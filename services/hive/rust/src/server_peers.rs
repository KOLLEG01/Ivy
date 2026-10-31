//! Network-side peer lifetime, provider correlation and request dispatch.
//! The transport owns socket readiness, flushing, close events and publication
//! turns; storage, routing, diagnostics and admission retain their own owners.
//! DB request effects share its scheduling boundary; direct provider calls stay
//! on the network executor.
use crate::{
    admission::{RequestBudget, RequestLease},
    http_common,
    live_routing::{LiveRouting, PreparedCall},
    rpc,
    server_rpc::{Clock, Reply, Storage, require_fresh, validate_direct_tool_request},
    storage::{Payload, Request, RequestEffects, RequestOptions},
    tool_binding_diagnostics::ToolBindingDiagnostics,
    ws_send::{self, Socket},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    core_schema::{validate_input, validate_output},
    json::stringify,
    limits::{CONNECTION_IN_FLIGHT_REQUESTS, MANAGEMENT_SOCKET_BUFFER_BYTES},
    validation::{validate_response_frame, validate_service_notification_frame},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::oneshot, task::AbortHandle, time::Instant};

pub trait PeerSocket: Socket + Send {
    fn ping(&mut self) -> IvyResult<()>;
}
struct NotificationFilter {
    namespace: JsString,
    name: JsString,
    version: JsString,
    service_node_id: Option<JsString>,
}
impl NotificationFilter {
    fn from_validated(value: &IvyJson) -> IvyResult<Self> {
        Ok(Self {
            namespace: text(value, "namespace")?,
            name: text(value, "name")?,
            version: text(value, "version")?,
            service_node_id: value.get("serviceNodeId")
                .map(|_| text(value, "serviceNodeId")).transpose()?,
        })
    }
    fn matches(&self, params: &IvyJson) -> bool {
        params.get("namespace").and_then(IvyJson::as_js_string) == Some(&self.namespace)
            && params.get("name").and_then(IvyJson::as_js_string) == Some(&self.name)
            && params.get("version").and_then(IvyJson::as_js_string) == Some(&self.version)
            && self.service_node_id.as_ref().is_none_or(|id|
                params.get("serviceNodeId").and_then(IvyJson::as_js_string) == Some(id))
    }
}
struct Session {
    active: usize,
    alive: bool,
    diagnosed_at: f64,
    filters: Vec<NotificationFilter>,
    changes: Vec<JsString>,
    expiry: Option<AbortHandle>,
}
struct Connection {
    socket: Mutex<Box<dyn PeerSocket>>,
    context: Mutex<IvyJson>,
    session: Mutex<Session>,
}
/// Identity is the connection itself, never a caller-supplied ID or node name.
#[derive(Clone)]
pub struct Peer(Arc<Connection>);
impl Peer {
    pub fn context(&self) -> IvyJson {
        self.0.context.lock().unwrap().clone()
    }
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    fn send(&self, value: &IvyJson, encoded: Option<&str>) -> IvyResult<()> {
        ws_send::send(&mut **self.0.socket.lock().unwrap(), value, encoded)
    }
    fn terminate(&self) -> IvyResult<()> {
        self.0.socket.lock().unwrap().terminate()
    }
}
struct BrowserPeer {
    peer: Peer,
    context: IvyJson,
}
impl crate::browser_session::Peer for BrowserPeer {
    fn context(&self) -> &IvyJson {
        &self.context
    }
    fn terminate(&self) -> IvyResult<()> {
        self.peer.terminate()
    }
}
impl crate::browser_session::Peers for Peers {
    fn iter(&self) -> Box<dyn Iterator<Item = Arc<dyn crate::browser_session::Peer>> + '_> {
        let peers = self.0.state.lock().unwrap().peers.clone();
        Box::new(peers.into_iter().map(|peer| {
            Arc::new(BrowserPeer {
                context: peer.context(),
                peer,
            }) as Arc<dyn crate::browser_session::Peer>
        }))
    }
}
/// The real direct dispatcher is entered synchronously after its freshness
/// check. Its tool/discovery validator remains a separate contract owner.
pub type Direct =
    Arc<dyn Fn(&Peers, &IvyJson, &mut IvyJson, &Peer) -> IvyResult<Option<Reply>> + Send + Sync>;
pub type Ids = Arc<dyn Fn() -> JsString + Send + Sync>;
pub fn native_ids() -> Ids {
    Arc::new(|| uuid::Uuid::new_v4().to_string().into())
}
/// A validated provider failure passed to the public error constructor.
pub struct ProviderError {
    pub code: JsString,
    pub message: JsString,
    pub outcome: IvyJson,
    pub details: Option<IvyJson>,
}
pub type ErrorConstructor = Arc<dyn Fn(ProviderError) -> IvyError + Send + Sync>;
fn provider_response(mut frame: IvyJson, provider_error: &ErrorConstructor) -> IvyResult<IvyResult<IvyJson>> {
    validate_response_frame(&frame)?;
    // The admitted envelope owns its payload. Keep the raw outcome and optional
    // details while moving them across the provider error boundary.
    if let Some(mut error) = frame.remove("error") {
        let mut data = error.remove("data").unwrap();
        Ok(Err(provider_error(ProviderError {
            code: data.remove("code").and_then(IvyJson::into_js_string).unwrap(),
            message: error.remove("message").and_then(IvyJson::into_js_string).unwrap(),
            outcome: data.remove("outcome").unwrap(),
            details: data.remove("details"),
        })))
    } else {
        Ok(Ok(frame.remove("result").unwrap()))
    }
}
fn notification_frame(mut params: IvyJson, context: &IvyJson) -> IvyResult<IvyJson> {
    params.insert("serviceNodeId", context.required("serviceNodeId")?.clone());
    params.insert("generation", context.required("generation")?.clone());
    Ok(IvyJson::object([
        ("jsonrpc", "2.0".into()),
        ("method", "notifications.provider".into()),
        ("params", params),
    ]))
}
#[derive(Clone, Copy)]
pub enum UnhandledKind {
    Callback,
    Promise,
}
pub struct Owners {
    pub storage: Arc<dyn Storage>,
    pub routing: Arc<Mutex<LiveRouting>>,
    pub diagnostics: Arc<Mutex<ToolBindingDiagnostics>>,
    pub requests: RequestBudget,
    pub direct: Direct,
    pub now: Clock,
    pub ids: Ids,
    pub provider_error: ErrorConstructor,
    pub wake_browser_push: Arc<dyn Fn() + Send + Sync>,
    pub clear_authentication: Arc<dyn Fn() + Send + Sync>,
    /// Bind the runtime's original uncaught-callback/unhandled-promise policy.
    /// Transport failures must not disappear in a detached native task.
    pub unhandled: Arc<dyn Fn(UnhandledKind, IvyError) + Send + Sync>,
}
#[derive(Default)]
pub struct Options {
    pub call_timeout_ms: Option<f64>,
    pub validate_messages: bool,
}
#[derive(Clone)]
struct Settlement(Arc<Mutex<Option<oneshot::Sender<IvyResult<IvyJson>>>>>);
impl Settlement {
    fn finish(&self, result: IvyResult<IvyJson>) {
        if let Some(sender) = self.0.lock().unwrap().take() {
            let _ = sender.send(result);
        }
    }
}
#[derive(Clone)]
struct Pending {
    peer: Peer,
    generation: f64,
    reply: Settlement,
    timer: AbortHandle,
}
#[derive(Default)]
struct State {
    peers: Vec<Peer>,
    // Map insertion order also orders rejects and completion continuations.
    forwarded: Vec<(JsString, Pending)>,
    generation: f64,
    available: f64,
    availability_scheduled: bool,
    stopped: bool,
}
struct Parts {
    owners: Owners,
    options: Options,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct Peers(Arc<Parts>);

struct PeerEffects {
    peers: Peers,
    peer: Peer,
    connecting: bool,
    delivery: Arc<Mutex<PeerDelivery>>,
}
struct PeerDelivery {
    peer: Peer,
    id: IvyJson,
    lease: RequestLease,
    finished: bool,
    error: Option<IvyError>,
}
impl PeerDelivery {
    fn reject(&mut self, error: IvyError) {
        if self.finished {
            return;
        }
        self.finished = true;
        let sent = self.peer.send(&rpc::failure(&error, self.id.clone()), None);
        self.settle(sent);
    }
    fn finish(&mut self, result: IvyResult<IvyJson>) {
        if self.finished {
            return;
        }
        self.finished = true;
        let sent = match result {
            Ok(value) => self.peer.send(&result_frame(self.id.clone(), value), None),
            Err(error) => Err(error),
        };
        // Preserve the transport's existing fallback and unhandled-error policy.
        let sent = sent.or_else(|error| self.peer.send(&rpc::failure(&error, self.id.clone()), None));
        let sent = sent.or_else(|error| self.peer.send(&rpc::failure(&error, self.id.clone()), None));
        self.settle(sent);
    }
    fn settle(&mut self, sent: IvyResult<()>) {
        match sent {
            Ok(()) => {
                self.peer.0.session.lock().unwrap().active -= 1;
                self.lease.release();
            }
            Err(error) => self.error = Some(error),
        }
    }
}
impl RequestEffects for PeerEffects {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + std::mem::size_of::<Mutex<PeerDelivery>>() + 2 * std::mem::size_of::<usize>()
            + self.delivery.lock().unwrap().id.retained_bytes()
    }
    fn prepare(&self, payload: &mut Payload) -> IvyResult<()> {
        let Payload::Execute { context: prepared, .. } = payload else { return Err(IvyError::internal()); };
        let mut context = self.peer.context();
        require_fresh(&context, &*self.peers.0.owners.now)?;
        if !self.connecting
            && context.get("serviceNodeId").and_then(IvyJson::as_js_string)
                .is_some_and(|v| !v.0.is_empty())
        {
            self.peers.0.owners.routing.lock().unwrap().caller(&context)?;
        }
        if self.connecting {
            let mut state = self.peers.0.state.lock().unwrap();
            state.generation = if state.generation == 9_007_199_254_740_991.0 {
                1.0
            } else {
                state.generation + 1.0
            };
            context.insert("generation", IvyJson::Number(state.generation));
        }
        *prepared = context;
        Ok(())
    }
    fn complete(self: Box<Self>, result: IvyResult<IvyJson>) -> IvyResult<IvyJson> {
        let result = self.bind(result);
        self.delivery.lock().unwrap().finish(result);
        Ok(IvyJson::Null)
    }
}
impl PeerEffects {
    fn bind(&self, result: IvyResult<IvyJson>) -> IvyResult<IvyJson> {
        let mut prepared = result?;
        let connected = self.connecting && prepared.get("kind") == Some(&"result".into());
        let value = prepared.remove("value").ok_or_else(IvyError::internal)?;
        if connected {
            {
                let mut context = self.peer.0.context.lock().unwrap();
                for (key, value) in value.as_object().ok_or_else(IvyError::internal)? {
                    context.insert(key.clone(), value.clone());
                }
            }
            for old in self.peers.members() {
                if !old.same(&self.peer)
                    && old.context().get("serviceNodeId") == value.get("serviceNodeId")
                {
                    old.terminate()?;
                }
            }
        }
        Ok(value)
    }
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn delay(milliseconds: f64) -> Duration {
    // The original timer truncates valid delays and schedules other values on the next turn.
    let milliseconds = if (1.0..=f64::from(i32::MAX)).contains(&milliseconds) {
        milliseconds.trunc() as u64
    } else {
        1
    };
    Duration::from_millis(milliseconds)
}
fn ready<T: Send + 'static>(value: IvyResult<T>) -> Reply<T> {
    Box::pin(std::future::ready(value))
}
fn receive<T: Send + 'static>(reply: oneshot::Receiver<IvyResult<T>>) -> Reply<T> {
    Box::pin(async move { reply.await.map_err(|_| IvyError::internal())? })
}
fn result_frame(id: IvyJson, result: IvyJson) -> IvyJson {
    IvyJson::object([("jsonrpc", "2.0".into()), ("id", id), ("result", result)])
}
fn failed_connection() -> IvyError {
    IvyError::new(
        "outcome_unknown",
        "Provider connection was lost after dispatch.",
    )
    .with_outcome(Outcome::Unknown)
}
fn deadline() -> IvyError {
    IvyError::new(
        "deadline_exceeded",
        "Provider response deadline exceeded; execution outcome is unknown.",
    )
    .with_outcome(Outcome::Unknown)
}
impl Peers {
    pub fn new(owners: Owners, options: Options) -> Self {
        Self(Arc::new(Parts {
            owners,
            options,
            state: Mutex::new(State::default()),
        }))
    }
    fn members(&self) -> Vec<Peer> {
        self.0.state.lock().unwrap().peers.clone()
    }
    fn forget(&self, payload: IvyJson) {
        let reply = self
            .0
            .owners
            .storage
            .request(payload, RequestOptions::default());
        tokio::spawn(async move {
            let _ = reply.await;
        });
    }
    pub fn connected(&self, socket: Box<dyn PeerSocket>, context: IvyJson) -> Peer {
        let peer = Peer(Arc::new(Connection {
            socket: Mutex::new(socket),
            context: Mutex::new(context.clone()),
            session: Mutex::new(Session {
                active: 0,
                alive: true,
                diagnosed_at: 0.0,
                filters: vec![],
                changes: vec![],
                expiry: None,
            }),
        }));
        if let Some(expires) = context.get("expiresAt") {
            let duration = expires
                .as_f64()
                .map_or(0.0, |expires| (expires - (self.0.owners.now)()).max(0.0));
            let due = Instant::now() + delay(duration);
            let timed = peer.clone();
            let unhandled = self.0.owners.unhandled.clone();
            let timer = tokio::spawn(async move {
                tokio::time::sleep_until(due).await;
                if let Err(error) = timed.terminate() {
                    unhandled(UnhandledKind::Callback, error);
                }
            });
            peer.0.session.lock().unwrap().expiry = Some(timer.abort_handle());
        }
        self.0.state.lock().unwrap().peers.push(peer.clone());
        peer
    }
    pub fn pong(&self, peer: &Peer) {
        peer.0.session.lock().unwrap().alive = true;
    }
    /// Only the transport's actual close event calls this method. Termination
    /// requests do not invent a close event or cancel already accepted requests.
    pub fn closed(&self, peer: &Peer) -> IvyResult<()> {
        if let Some(timer) = peer.0.session.lock().unwrap().expiry.take() {
            timer.abort();
        }
        self.0.state.lock().unwrap().peers.retain(|p| !p.same(peer));
        let context = peer.context();
        if context
            .get("serviceNodeId")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
            && context
                .get("generation")
                .and_then(IvyJson::as_f64)
                .is_some_and(|v| v.is_finite() && v > 0.0)
            && self.0.owners.routing.lock().unwrap().disconnect(
                &text(&context, "serviceNodeId")?,
                context
                    .required("generation")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)?,
            )
        {
            self.forget(IvyJson::object([
                ("action", "disconnect".into()),
                ("serviceNodeId", context.required("serviceNodeId")?.clone()),
            ]));
        }
        let removed = {
            let mut state = self.0.state.lock().unwrap();
            let mut removed = vec![];
            let mut index = 0;
            while index < state.forwarded.len() {
                if state.forwarded[index].1.peer.same(peer) {
                    removed.push(state.forwarded.remove(index).1);
                } else {
                    index += 1;
                }
            }
            removed
        };
        for pending in removed {
            pending.timer.abort();
            pending.reply.finish(Err(failed_connection()));
        }
        Ok(())
    }
    pub fn protocol_diagnostic(&self, peer: &Peer, code: &str, message: &str) {
        if (self.0.owners.now)() - peer.0.session.lock().unwrap().diagnosed_at < 5000.0 {
            return;
        }
        peer.0.session.lock().unwrap().diagnosed_at = (self.0.owners.now)();
        self.forget(IvyJson::object([
            ("action", "protocol.diagnostic".into()),
            ("context", peer.context()),
            ("code", code.into()),
            ("message", message.into()),
        ]));
    }
    pub fn call(&self, context: &IvyJson, params: IvyJson) -> IvyResult<Reply> {
        let call = self
            .0
            .owners
            .routing
            .lock()
            .unwrap()
            .prepare(context, params)?;
        Ok(self.forward(context, call))
    }
    pub fn forward(&self, context: &IvyJson, call: PreparedCall) -> Reply {
        match self.start_forward(context, call) {
            Ok(reply) => reply,
            Err(error) => ready(Err(error)),
        }
    }
    pub fn dashboard_image(
        &self,
        context: &IvyJson,
        node: &JsString,
        args: IvyJson,
    ) -> IvyResult<Reply> {
        let call = self
            .0
            .owners
            .routing
            .lock()
            .unwrap()
            .prepare_dashboard_image(context, node, args)?;
        Ok(self.forward(context, call))
    }
    fn start_forward(&self, context: &IvyJson, call: PreparedCall) -> IvyResult<Reply> {
        require_fresh(context, &*self.0.owners.now)?;
        let PreparedCall {
            context: call,
            definition_hash,
            arguments,
            operation_id,
        } = call;
        let mut state = self.0.state.lock().unwrap();
        let peer = state
            .peers
            .iter()
            .find(|peer| {
                let context = peer.context();
                context.get("serviceNodeId") == Some(&IvyJson::String(call.service_node_id.clone()))
                    && context.get("generation") == Some(&IvyJson::Number(call.generation))
                    && peer.0.socket.lock().unwrap().is_open()
            })
            .cloned()
            .ok_or_else(|| {
                IvyError::new(
                    "service_unavailable",
                    "No live socket for the selected provider generation.",
                )
            })?;
        require(
            state.forwarded.len() < 512,
            "limit_exceeded",
            "Hive live call limit reached.",
        )?;
        require(
            state
                .forwarded
                .iter()
                .filter(|(_, p)| p.peer.same(&peer))
                .count()
                < 64,
            "limit_exceeded",
            "Provider connection call/buffer limit reached.",
        )?;
        let mut id = JsString::from("hive-call-");
        id.0.extend((self.0.owners.ids)().0);
        let mut params = IvyJson::object([
            (
                "qualifiedName",
                IvyJson::String(call.qualified_name.clone()),
            ),
            ("arguments", arguments),
            ("definitionHash", IvyJson::String(definition_hash)),
            ("generation", IvyJson::Number(call.generation)),
            (
                "callerPrincipalId",
                IvyJson::String(call.caller_principal_id.clone()),
            ),
        ]);
        if operation_id
            .as_ref()
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        {
            params.insert("operationId", operation_id.unwrap());
        }
        let request = IvyJson::object([
            ("jsonrpc", "2.0".into()),
            ("id", IvyJson::String(id.clone())),
            ("method", "provider.invoke".into()),
            ("params", params),
        ]);
        let encoded = ws_send::encode(&request)?;
        require(
            peer.0.socket.lock().unwrap().buffered_amount() + encoded.len() as f64
                <= MANAGEMENT_SOCKET_BUFFER_BYTES as f64,
            "limit_exceeded",
            "Provider connection call/buffer limit reached.",
        )?;
        let (sender, reply) = oneshot::channel();
        let settlement = Settlement(Arc::new(Mutex::new(Some(sender))));
        let due = Instant::now() + delay(self.0.options.call_timeout_ms.unwrap_or(30_000.0));
        let weak = Arc::downgrade(&self.0);
        let expired_id = id.clone();
        let expired_reply = settlement.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep_until(due).await;
            if let Some(parts) = weak.upgrade() {
                parts
                    .state
                    .lock()
                    .unwrap()
                    .forwarded
                    .retain(|(id, _)| id != &expired_id);
            }
            expired_reply.finish(Err(deadline()));
        });
        let pending = Pending {
            peer: peer.clone(),
            generation: call.generation,
            reply: settlement,
            timer: timer.abort_handle(),
        };
        if let Some((_, previous)) = state.forwarded.iter_mut().find(|(key, _)| key == &id) {
            *previous = pending;
        } else {
            state.forwarded.push((id, pending));
        }
        drop(state);
        // A synchronous send failure rejects the promise but does not undo the
        // earlier pending insertion or timer. Close/result/deadline still clears it.
        peer.send(&request, Some(&encoded))?;
        let this = self.clone();
        let (result, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let work = async {
                let value = reply.await.map_err(|_| IvyError::internal())??;
                let completed = this
                    .0
                    .owners
                    .routing
                    .lock()
                    .unwrap()
                    .complete(&call, value)?;
                this.0
                    .owners
                    .diagnostics
                    .lock()
                    .unwrap()
                    .completed(&IvyJson::object([
                        (
                            "callerPrincipalId",
                            IvyJson::String(call.caller_principal_id.clone()),
                        ),
                        (
                            "serviceNodeId",
                            IvyJson::String(call.service_node_id.clone()),
                        ),
                        (
                            "qualifiedName",
                            IvyJson::String(call.qualified_name.clone()),
                        ),
                    ]))?;
                Ok(completed)
            }
            .await;
            let _ = result.send(work);
        });
        Ok(receive(receiver))
    }
    pub fn provider_result(&self, peer: &Peer, frame: IvyJson) {
        let Some(id) = frame.get("id").and_then(IvyJson::as_js_string) else {
            self.protocol_diagnostic(
                peer,
                "stale_generation",
                "Unmatched provider response was ignored.",
            );
            return;
        };
        let pending = self
            .0
            .state
            .lock()
            .unwrap()
            .forwarded
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, p)| p.clone());
        let Some(pending) = pending.filter(|p| {
            p.peer.same(peer)
                && peer.context().get("generation") == Some(&IvyJson::Number(p.generation))
        }) else {
            self.protocol_diagnostic(
                peer,
                "stale_generation",
                "Unmatched provider response was ignored.",
            );
            return;
        };
        let id = id.clone();
        let result = provider_response(frame, &self.0.owners.provider_error);
        pending.reply.finish(result.unwrap_or_else(|_| {
            Err(IvyError::new(
                "provider_contract_error",
                "Provider sent an invalid response envelope.",
            )
            .with_outcome(Outcome::Unknown))
        }));
        pending.timer.abort();
        self.0
            .state
            .lock()
            .unwrap()
            .forwarded
            .retain(|(key, _)| key != &id);
    }
    pub fn reject_all(&self, error: IvyError) {
        let pending = std::mem::take(&mut self.0.state.lock().unwrap().forwarded);
        for (_, pending) in pending {
            pending.timer.abort();
            pending.reply.finish(Err(error.clone()));
        }
    }
    pub fn subscribe(
        &self,
        peer: Option<&Peer>,
        context: &IvyJson,
        params: &IvyJson,
    ) -> IvyResult<Reply> {
        validate_input("notifications.subscribe", params)?;
        require(
            peer.is_some() && context.get("transport") == Some(&"ws".into()),
            "invalid_arguments",
            "Notification subscriptions require the current WebSocket session.",
        )?;
        let peer = peer.unwrap();
        let filters = params
            .required("filters")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(NotificationFilter::from_validated)
            .collect::<IvyResult<Vec<_>>>()?;
        let changes = params
            .get("changes")
            .and_then(IvyJson::as_array)
            .unwrap_or(&[])
            .iter()
            .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
            .collect::<IvyResult<Vec<_>>>()?;
        let mut result = IvyJson::object([("subscribed", IvyJson::Number(filters.len() as f64))]);
        if params.get("changes").is_some() {
            result.insert("changes", IvyJson::Number(changes.len() as f64));
        }
        {
            let mut session = peer.0.session.lock().unwrap();
            session.filters = filters;
            session.changes = changes;
        }
        let validate = self.0.options.validate_messages;
        Ok(Box::pin(async move {
            if validate {
                validate_output("notifications.subscribe", &result)?;
            }
            Ok(result)
        }))
    }
    fn subscribed(peer: &Peer, params: &IvyJson) -> bool {
        peer.0.session.lock().unwrap().filters.iter().any(|filter| filter.matches(params))
    }
    fn notification(&self, peer: &Peer, mut frame: IvyJson) {
        let delivered = (|| -> IvyResult<()> {
            validate_service_notification_frame(&frame)?;
            let params = frame.remove("params").unwrap();
            let context = peer.context();
            self.0
                .owners
                .routing
                .lock()
                .unwrap()
                .notification(&context, &params)?;
            let value = notification_frame(params, &context)?;
            let encoded = stringify(&value, 1024 * 1024)?;
            let params = value.get("params").unwrap();
            for client in self.members() {
                if !client.same(peer) && Self::subscribed(&client, params) {
                    client.send(&value, Some(&encoded))?;
                }
            }
            Ok(())
        })();
        if delivered.is_err() {
            self.protocol_diagnostic(
                peer,
                "invalid_frame",
                "Transient provider frame was rejected.",
            );
        }
    }
    /// The executor calls this for complete WS text/binary messages. Its WS
    /// implementation enforces the original maxPayload before this boundary.
    pub fn message(&self, peer: &Peer, bytes: &[u8], binary: bool) -> IvyResult<()> {
        let parsed = require(
            !binary,
            "invalid_frame",
            "Hive WebSocket frames must be JSON text.",
        )
        .and_then(|()| http_common::json_body(bytes));
        let mut frame = match parsed {
            Ok(frame) => frame,
            Err(error) => return peer.send(&rpc::failure(&error, IvyJson::Null), None),
        };
        if frame.as_object().is_some() {
            if (frame.get("result").is_some() || frame.get("error").is_some())
                && frame.get("id").and_then(IvyJson::as_js_string).is_some()
            {
                self.provider_result(peer, frame);
                return Ok(());
            }
            if frame.get("id").is_none() {
                if frame.get("method") == Some(&"service.notification".into()) {
                    self.notification(peer, frame);
                }
                return Ok(());
            }
        }
        let context = peer.context();
        let id = rpc::valid_request_id(frame.get("id"));
        if let Err(error) = require_fresh(&context, &*self.0.owners.now) {
            peer.send(&rpc::failure(&error, id), None)?;
            return peer.terminate();
        }
        if peer.0.session.lock().unwrap().active >= CONNECTION_IN_FLIGHT_REQUESTS {
            return peer.send(
                &rpc::failure(
                    &IvyError::new("limit_exceeded", "Too many in-flight calls."),
                    id,
                ),
                None,
            );
        }
        let mut lease = match self.0.owners.requests.acquire(
            &text(&context, "credentialDigest")?,
            Some(1024 + bytes.len()),
        ) {
            Ok(lease) => lease,
            Err(error) => return peer.send(&rpc::failure(&error, id), None),
        };
        peer.0.session.lock().unwrap().active += 1;
        let direct = (|| {
            require_fresh(&context, &*self.0.owners.now)?;
            validate_direct_tool_request(&frame)?;
            let direct = (self.0.owners.direct)(self, &context, &mut frame, peer)?;
            if direct.is_none() {
                let mut routing = self.0.owners.routing.lock().unwrap();
                if context
                    .get("serviceNodeId")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| !v.0.is_empty())
                {
                    routing.caller(&context)?;
                }
                if frame.get("method") == Some(&"registry.sync".into())
                    && context
                        .get("serviceNodeId")
                        .and_then(IvyJson::as_js_string)
                        .is_some_and(|v| !v.0.is_empty())
                    && context
                        .get("generation")
                        .and_then(IvyJson::as_f64)
                        .is_some_and(|v| v.is_finite() && v > 0.0)
                {
                    routing.suspend(
                        &text(&context, "serviceNodeId")?,
                        context
                            .required("generation")?
                            .as_f64()
                            .ok_or_else(IvyError::internal)?,
                    );
                }
            }
            Ok(direct)
        })();
        match direct {
            Err(error) => {
                peer.send(&rpc::failure(&error, id), None)?;
                peer.0.session.lock().unwrap().active -= 1;
                lease.release();
            }
            Ok(Some(work)) => {
                let peer = peer.clone();
                let unhandled = self.0.owners.unhandled.clone();
                tokio::spawn(async move {
                    let sent = match work.await {
                        Ok(value) => peer.send(&result_frame(id, value), None),
                        Err(error) => peer.send(&rpc::failure(&error, id), None),
                    };
                    peer.0.session.lock().unwrap().active -= 1;
                    lease.release();
                    if let Err(error) = sent {
                        unhandled(UnhandledKind::Promise, error);
                    }
                });
            }
            Ok(None) => self.queued(peer, frame, bytes.len(), lease),
        }
        Ok(())
    }
    fn queued(&self, peer: &Peer, frame: IvyJson, bytes: usize, lease: RequestLease) {
        let delivery = Arc::new(Mutex::new(PeerDelivery {
            peer: peer.clone(), id: rpc::valid_request_id(frame.get("id")), lease,
            finished: false, error: None,
        }));
        let connecting = frame.get("method") == Some(&"service.connect".into());
        let reply = self.0.owners.storage.dispatch(
            Request::with_effects(
                Payload::Execute { context: IvyJson::Null, request: frame },
                PeerEffects {
                    peers: self.clone(), peer: peer.clone(), connecting,
                    delivery: delivery.clone(),
                },
            ),
            RequestOptions { reserved_bytes: bytes as u64, ..Default::default() },
        );
        let unhandled = self.0.owners.unhandled.clone();
        tokio::spawn(async move {
            let result = reply.await;
            let error = {
                let mut delivery = delivery.lock().unwrap();
                if let Err(error) = result {
                    delivery.reject(error);
                }
                delivery.error.take()
            };
            if let Some(error) = error {
                unhandled(UnhandledKind::Promise, error);
            }
        });
    }
    pub fn changes(&self, scopes: &[JsString]) -> IvyResult<()> {
        fn below(a: &JsString, b: &JsString) -> bool {
            a.0.starts_with(&b.0) && a.0.get(b.0.len()) == Some(&47)
        }
        for peer in self.members() {
            let session = peer.0.session.lock().unwrap();
            let selected: Vec<_> = scopes
                .iter()
                .filter(|scope| {
                    session.changes
                        .iter()
                        .any(|watch| scope == &watch || below(scope, watch) || below(watch, scope))
                })
                .cloned()
                .map(IvyJson::String)
                .collect();
            drop(session);
            if !selected.is_empty() {
                peer.send(
                    &IvyJson::object([
                        ("jsonrpc", "2.0".into()),
                        ("method", "notifications.changed".into()),
                        (
                            "params",
                            IvyJson::object([("scopes", IvyJson::Array(selected))]),
                        ),
                    ]),
                    None,
                )?;
            }
        }
        Ok(())
    }
    pub fn health(&self) -> IvyResult<()> {
        for peer in self.members() {
            let (alive, changes) = {
                let mut session = peer.0.session.lock().unwrap();
                let alive = session.alive;
                if alive {
                    session.alive = false;
                }
                (alive, !session.changes.is_empty())
            };
            if !alive {
                peer.terminate()?;
                continue;
            }
            peer.0.socket.lock().unwrap().ping()?;
            if changes {
                peer.send(
                    &IvyJson::object([
                        ("jsonrpc", "2.0".into()),
                        ("method", "notifications.changed".into()),
                        (
                            "params",
                            IvyJson::object([("scopes", IvyJson::Array(vec![]))]),
                        ),
                    ]),
                    None,
                )?;
            }
        }
        Ok(())
    }
    pub fn clear_authentication(&self) {
        (self.0.owners.clear_authentication)();
    }
    pub fn restore_diagnostics(&self, values: &IvyJson) -> IvyResult<()> {
        self.0
            .owners
            .diagnostics
            .lock()
            .unwrap()
            .restore(values.as_array().ok_or_else(IvyError::internal)?)
    }
    /// The storage client already owns private notification admission.
    pub fn routing_update(&self, value: &crate::storage::RoutingUpdate) -> IvyResult<()> {
        self.0.owners.routing.lock().unwrap().update(
            &value.node,
            value.generation,
            value.catalog.as_deref(),
        )
    }
    /// Invoked only after storage accepts the replacement and the HTTP cache,
    /// configured credentials and OAuth indexes have changed in that order.
    pub fn retire_credentials(&self, credentials: &[crate::store::Credential]) -> IvyResult<()> {
        self.0
            .owners
            .routing
            .lock()
            .unwrap()
            .retire_unconfigured_principals(
                &credentials
                    .iter()
                    .map(|value| value.principal_id.clone())
                    .collect::<Vec<_>>(),
            );
        for peer in self.members() {
            let context = peer.context();
            if !credentials.iter().any(|value| {
                context.get("credentialDigest") == Some(&IvyJson::String(value.digest.clone()))
            }) {
                if context
                    .get("serviceNodeId")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| !v.0.is_empty())
                    && context
                        .get("generation")
                        .and_then(IvyJson::as_f64)
                        .is_some_and(|v| v.is_finite() && v > 0.0)
                {
                    if let (Some(id), Some(generation)) = (
                        context.get("serviceNodeId").and_then(IvyJson::as_js_string),
                        context.get("generation").and_then(IvyJson::as_f64),
                    ) {
                        self.0
                            .owners
                            .routing
                            .lock()
                            .unwrap()
                            .disconnect(id, generation);
                    }
                }
                peer.terminate()?;
            }
        }
        Ok(())
    }
    /// Returns true only when the executor must schedule a publication turn.
    /// Browser push is awakened even for an invalid hint or a stopped server.
    pub fn events_available(&self, through: f64) -> bool {
        (self.0.owners.wake_browser_push)();
        let mut state = self.0.state.lock().unwrap();
        if !through.is_finite()
            || through.fract() != 0.0
            || !(1.0..=9_007_199_254_740_991.0).contains(&through)
            || state.stopped
        {
            return false;
        }
        state.available = state.available.max(through);
        if state.availability_scheduled {
            return false;
        }
        state.availability_scheduled = true;
        true
    }
    pub fn publish_events(&self) -> IvyResult<()> {
        let available = {
            let mut state = self.0.state.lock().unwrap();
            state.availability_scheduled = false;
            let available = state.available;
            state.available = 0.0;
            if state.stopped { 0.0 } else { available }
        };
        if available == 0.0 {
            return Ok(());
        }
        let value = IvyJson::object([
            ("jsonrpc", "2.0".into()),
            ("method", "events.available".into()),
            (
                "params",
                IvyJson::object([("throughSequence", IvyJson::Number(available))]),
            ),
        ]);
        let encoded = ws_send::encode(&value)?;
        for peer in self.members() {
            let context = peer.context();
            if context
                .get("serviceNodeId")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|v| !v.0.is_empty())
                && context
                    .get("generation")
                    .and_then(IvyJson::as_f64)
                    .is_some_and(|v| v.is_finite() && v > 0.0)
            {
                peer.send(&value, Some(&encoded))?;
            }
        }
        Ok(())
    }
    /// Called at close entry, before awaiting browser-push shutdown.
    pub fn stop_publication(&self) {
        self.0.state.lock().unwrap().stopped = true;
    }
    /// Called after browser-push close and timer cancellation, before closing
    /// the WS/HTTP owners and finally the storage worker.
    pub fn reject_for_shutdown(&self) -> IvyResult<()> {
        self.reject_all(
            IvyError::new(
                "outcome_unknown",
                "Hive is shutting down; reconcile the original operation.",
            )
            .with_outcome(Outcome::Unknown),
        );
        for peer in self.members() {
            peer.terminate()?;
        }
        Ok(())
    }
    /// The HTTP owner supplies cache invalidation at its original position;
    /// recovery scheduling follows these synchronous failure effects.
    pub fn storage_failed(&self) -> IvyResult<()> {
        self.0.owners.routing.lock().unwrap().clear();
        (self.0.owners.clear_authentication)();
        for peer in self.members() {
            peer.terminate()?;
        }
        self.reject_all(
            IvyError::new(
                "outcome_unknown",
                "Hive storage restarted; reconcile the original operation.",
            )
            .with_outcome(Outcome::Unknown),
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/hive_peer_delivery.rs"]
mod delivery_tests;
