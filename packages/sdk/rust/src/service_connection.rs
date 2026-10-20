use crate::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    rpc::{RequestFuture, RequestOptions, RpcClient, base_url, response_value},
};
use futures_util::{FutureExt, SinkExt, StreamExt};
use ivy_contracts::{
    hash::tool_definition_hash,
    json::{js_string, parse, stringify},
    limits::*,
    validation::*,
    wire_schema::validate_shared,
};
use std::{
    collections::HashMap,
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;
#[path = "service_header.rs"]
mod header;

pub type ServiceFuture<T> = Pin<Box<dyn Future<Output = IvyResult<T>> + Send + 'static>>;
pub type ToolHandler =
    Arc<dyn Fn(IvyJson, InvocationContext) -> ServiceFuture<IvyJson> + Send + Sync>;
pub type NotificationObserver = Arc<dyn Fn(&IvyJson) + Send + Sync>;
#[derive(Clone)]
pub struct InvocationContext {
    pub caller_principal_id: JsString,
    pub operation_id: Option<JsString>,
    pub generation: u64,
    pub signal: CancellationToken,
    close_reason: Arc<OnceLock<IvyError>>,
}
impl InvocationContext {
    /// Construct a caller-owned context for independent handlers and tests.
    /// Cancellation uses the supplied reason. Provider handlers must use their
    /// delivered context to retain the connection generation's first close error.
    pub fn detached(
        caller_principal_id: JsString,
        operation_id: Option<JsString>,
        generation: u64,
        signal: CancellationToken,
        abort_reason: IvyError,
    ) -> Self {
        Self {
            caller_principal_id,
            operation_id,
            generation,
            signal,
            close_reason: Arc::new(OnceLock::from(abort_reason)),
        }
    }
    /// Match AbortSignal.throwIfAborted while retaining the owning generation's
    /// first close error, including exact UTF-16 text, outcome and details.
    pub fn throw_if_aborted(&self) -> IvyResult<()> {
        throw_if_aborted(&self.signal, &self.close_reason)
    }
}
/// Share this across socket generations. Closing a socket does not free accepted handlers.
#[derive(Clone)]
pub struct InvocationCapacity(Arc<Semaphore>);
impl Default for InvocationCapacity {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(CONNECTION_IN_FLIGHT_REQUESTS)))
    }
}
#[derive(Clone)]
pub struct ConnectionOptions {
    pub service_node_id: JsString,
    pub handlers: HashMap<JsString, ToolHandler>,
    pub json_handlers: HashMap<JsString, ToolHandler>,
    pub capacity: InvocationCapacity,
    pub on_notification: Option<NotificationObserver>,
}
impl ConnectionOptions {
    pub fn new(service_node_id: JsString) -> Self {
        Self {
            service_node_id,
            handlers: HashMap::new(),
            json_handlers: HashMap::new(),
            capacity: InvocationCapacity::default(),
            on_notification: None,
        }
    }
}
#[derive(Clone)]
struct Binding {
    hash: String,
    handler: ToolHandler,
}
type Reply = oneshot::Sender<IvyResult<IvyJson>>;
struct State {
    pending: HashMap<String, Reply>,
    bindings: HashMap<JsString, Binding>,
    owner: Option<OwnerObservation>,
}
#[derive(Clone)]
struct OwnerObservation {
    identity: Arc<()>,
    future: futures_util::future::Shared<ServiceFuture<IvyJson>>,
}
struct Inner {
    options: ConnectionOptions,
    state: Mutex<State>,
    signal: CancellationToken,
    close_reason: Arc<OnceLock<IvyError>>,
    generation: AtomicU64,
    ready: AtomicBool,
    outbound: mpsc::UnboundedSender<String>,
    buffered: AtomicUsize,
    available: AtomicU64,
    events: Notify,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.close_reason.get_or_init(stopping);
        self.signal.cancel();
    }
}
/// One socket generation. Requests and handler bindings never migrate to its replacement.
#[derive(Clone)]
pub struct ServiceConnection(Arc<Inner>);
fn unknown(code: &str, message: &str) -> IvyError {
    IvyError::new(code, message).with_outcome(Outcome::Unknown)
}
fn websocket_failed() -> IvyError {
    unknown("service_unavailable", "Hive WebSocket failed.")
}
fn socket_closed() -> IvyError {
    unknown(
        "outcome_unknown",
        "Hive connection closed; reconcile accepted operations.",
    )
}
fn stopping() -> IvyError {
    unknown("service_unavailable", "Service connection is stopping.")
}
fn throw_if_aborted(signal: &CancellationToken, reason: &OnceLock<IvyError>) -> IvyResult<()> {
    if signal.is_cancelled() {
        Err(reason.get().cloned().unwrap_or_else(stopping))
    } else {
        Ok(())
    }
}

pub(crate) enum ConnectionPreparationError {
    Ivy(IvyError),
    NativeHeader,
}
impl From<IvyError> for ConnectionPreparationError {
    fn from(error: IvyError) -> Self {
        Self::Ivy(error)
    }
}
impl ConnectionPreparationError {
    fn into_error(self) -> IvyError {
        match self {
            Self::Ivy(error) => error,
            // Match IvyError.from for the synchronous Node TypeError.
            Self::NativeHeader => IvyError::internal(),
        }
    }
    pub(crate) fn into_state_error(self) -> IvyError {
        match self {
            Self::Ivy(error) => error,
            // ServiceClient.run hides non-Ivy errors from its state observer.
            Self::NativeHeader => IvyError::new("dependency_unavailable", ""),
        }
    }
}

impl ServiceConnection {
    pub async fn connect(
        public_base_url: &str,
        credential: &str,
        options: ConnectionOptions,
        cancellation: CancellationToken,
    ) -> IvyResult<Self> {
        Self::connect_js(public_base_url, &credential.into(), options, cancellation).await
    }
    pub async fn connect_js(
        public_base_url: &str,
        credential: &JsString,
        options: ConnectionOptions,
        cancellation: CancellationToken,
    ) -> IvyResult<Self> {
        let request = Self::prepare_js(public_base_url, credential)
            .map_err(ConnectionPreparationError::into_error)?;
        Self::connect_prepared(request, options, cancellation).await
    }
    pub(crate) fn prepare_js(
        public_base_url: &str,
        credential: &JsString,
    ) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, ConnectionPreparationError> {
        let base = base_url(public_base_url)?;
        let origin = base.origin().ascii_serialization();
        let mut url = base.join("ws").map_err(|_| websocket_failed())?;
        url.set_scheme(if base.scheme() == "https" {
            "wss"
        } else {
            "ws"
        })
        .map_err(|_| websocket_failed())?;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| websocket_failed())?;
        request.headers_mut().insert(
            "Authorization",
            header::authorization(credential)
                .map_err(|_| ConnectionPreparationError::NativeHeader)?,
        );
        request
            .headers_mut()
            .insert("Origin", origin.parse().map_err(|_| websocket_failed())?);
        Ok(request)
    }
    pub(crate) async fn connect_prepared(
        request: tokio_tungstenite::tungstenite::http::Request<()>,
        options: ConnectionOptions,
        cancellation: CancellationToken,
    ) -> IvyResult<Self> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MANAGEMENT_FRAME_BYTES))
            .max_frame_size(Some(MANAGEMENT_FRAME_BYTES))
            .write_buffer_size(0);
        let (socket, _) = tokio::select! {
            biased;
            _=cancellation.cancelled()=>return Err(stopping()),
            result=tokio::time::timeout(Duration::from_secs(10),connect_async_with_config(request,Some(config),true))=>result.map_err(|_|websocket_failed())?.map_err(|_|websocket_failed())?,
        };
        let (outbound, mut queue) = mpsc::unbounded_channel::<String>();
        let connection = Self(Arc::new(Inner {
            options,
            state: Mutex::new(State {
                pending: HashMap::new(),
                bindings: HashMap::new(),
                owner: None,
            }),
            signal: CancellationToken::new(),
            close_reason: Arc::new(OnceLock::new()),
            generation: AtomicU64::new(0),
            ready: AtomicBool::new(false),
            outbound,
            buffered: AtomicUsize::new(0),
            available: AtomicU64::new(0),
            events: Notify::new(),
        }));
        let (mut sink, mut stream) = socket.split();
        let weak = Arc::downgrade(&connection.0);
        let signal = connection.signal();
        tokio::spawn(async move {
            loop {
                let encoded = tokio::select! {biased;_=signal.cancelled()=>break,value=queue.recv()=>match value{Some(v)=>v,None=>break}};
                let size = encoded.len();
                let result = tokio::select! {biased;_=signal.cancelled()=>break,result=sink.send(Message::Text(encoded.into()))=>result};
                if let Some(inner) = weak.upgrade() {
                    inner.buffered.fetch_sub(size, Ordering::AcqRel);
                    if result.is_err() {
                        Self(inner).close_with(unknown("outcome_unknown", "Hive write failed."));
                        break;
                    }
                } else {
                    break;
                }
            }
        });
        let weak = Arc::downgrade(&connection.0);
        let signal = connection.signal();
        tokio::spawn(async move {
            loop {
                let item =
                    tokio::select! {biased;_=signal.cancelled()=>break,item=stream.next()=>item};
                let Some(inner) = weak.upgrade() else { break };
                let connection = Self(inner);
                match item {
                    Some(Ok(Message::Text(text))) => {
                        if connection.receive(text.as_str()).is_err() {
                            connection.close_with(unknown(
                                "invalid_frame",
                                "Hive sent malformed protocol data.",
                            ));
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        connection.close_with(unknown(
                            "invalid_frame",
                            "Hive sent a binary protocol frame.",
                        ));
                        break;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        connection.close_with(socket_closed());
                        break;
                    }
                    Some(Err(_)) => {
                        connection.close_with(websocket_failed());
                        break;
                    }
                    _ => {}
                }
            }
        });
        Ok(connection)
    }
    pub fn service_node_id(&self) -> &JsString {
        &self.0.options.service_node_id
    }
    pub(crate) async fn bound_owner(&self) -> IvyResult<IvyJson> {
        self.check_open()?;
        let pending = {
            let mut state = self.0.state.lock().unwrap();
            if let Some(pending) = &state.owner {
                pending.clone()
            } else {
                let identity = Arc::new(());
                let token = identity.clone();
                let connection = self.clone();
                let future: ServiceFuture<IvyJson> = Box::pin(async move {
                    let result = connection
                        .request(
                            "serviceNodes.get",
                            IvyJson::object([(
                                "serviceNodeId",
                                IvyJson::String(connection.service_node_id().clone()),
                            )]),
                            RequestOptions::default(),
                        )
                        .await;
                    if !result.as_ref().is_ok_and(|node| {
                        node.get("connected").and_then(IvyJson::as_bool) == Some(true)
                            && node.get("synced").and_then(IvyJson::as_bool) == Some(true)
                    }) {
                        let mut state = connection.0.state.lock().unwrap();
                        if state
                            .owner
                            .as_ref()
                            .is_some_and(|p| Arc::ptr_eq(&p.identity, &token))
                        {
                            state.owner = None;
                        }
                    }
                    result
                });
                let pending = OwnerObservation {
                    identity,
                    future: future.shared(),
                };
                state.owner = Some(pending.clone());
                let driver = pending.future.clone();
                tokio::spawn(async move {
                    let _ = driver.await;
                });
                pending
            }
        };
        let result = pending.future.await?;
        self.check_open()?;
        Ok(result)
    }
    pub fn signal(&self) -> CancellationToken {
        self.0.signal.clone()
    }
    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }
    pub fn set_generation(&self, generation: u64) {
        self.0.generation.store(generation, Ordering::Release);
    }
    pub fn ready(&self) -> bool {
        self.0.ready.load(Ordering::Acquire) && !self.0.signal.is_cancelled()
    }
    pub fn set_ready(&self, ready: bool) {
        self.0.ready.store(ready, Ordering::Release);
    }
    pub fn close_reason(&self) -> Option<IvyError> {
        self.0.close_reason.get().cloned()
    }
    /// Return the original abort reason for this connection generation.
    pub fn throw_if_aborted(&self) -> IvyResult<()> {
        throw_if_aborted(&self.0.signal, &self.0.close_reason)
    }
    pub fn check_open(&self) -> IvyResult<()> {
        match self.close_reason() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    pub async fn closed(&self) {
        self.0.signal.cancelled().await;
    }
    pub fn close(&self) {
        self.close_with(stopping());
    }
    pub fn close_with(&self, error: IvyError) {
        let pending = {
            let mut state = self.0.state.lock().unwrap();
            if self.0.close_reason.set(error.clone()).is_err() {
                return;
            }
            self.0.ready.store(false, Ordering::Release);
            std::mem::take(&mut state.pending)
        };
        self.0.signal.cancel();
        for (_, reply) in pending {
            let _ = reply.send(Err(error.clone()));
        }
    }
    fn registry_bindings(&self, registry: &IvyJson) -> IvyResult<HashMap<JsString, Binding>> {
        let mut bindings = HashMap::new();
        for namespace in registry.get("namespaces").unwrap().as_array().unwrap() {
            for definition in namespace.get("tools").unwrap().as_array().unwrap() {
                let mut name = definition
                    .get("namespace")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone();
                name.0.push(46);
                name.0
                    .extend_from_slice(&definition.get("name").unwrap().as_js_string().unwrap().0);
                let object = self.0.options.handlers.get(&name);
                let json = self.0.options.json_handlers.get(&name);
                if bindings.contains_key(&name) || object.is_some() == json.is_some() {
                    return Err(IvyError::new(
                        "registry_invalid",
                        "Every declared tool needs exactly one concrete object or JSON handler.",
                    ));
                }
                bindings.insert(
                    name,
                    Binding {
                        hash: tool_definition_hash(definition)?,
                        handler: object.or(json).unwrap().clone(),
                    },
                );
            }
        }
        Ok(bindings)
    }
    pub fn set_registry(&self, registry: &IvyJson) -> IvyResult<()> {
        validate_shared("RegistrySync", registry)?;
        let bindings = self.registry_bindings(registry)?;
        self.0.state.lock().unwrap().bindings = bindings;
        Ok(())
    }
    pub fn prepare_registry(&self, registry: &IvyJson) -> IvyResult<IvyJson> {
        self.check_open()?;
        self.set_registry(registry)?;
        Ok(registry.clone())
    }
    fn send(&self, value: &IvyJson) -> IvyResult<()> {
        let _state = self.0.state.lock().unwrap();
        if self.0.close_reason.get().is_some() {
            return Err(IvyError::new(
                "service_unavailable",
                "Hive connection is unavailable.",
            ));
        }
        let encoded = stringify(value, MANAGEMENT_FRAME_BYTES)?;
        let size = encoded.len();
        if self
            .0
            .buffered
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(size)
                    .filter(|n| *n <= MANAGEMENT_SOCKET_BUFFER_BYTES)
            })
            .is_err()
        {
            return Err(IvyError::new(
                "limit_exceeded",
                "Service socket buffer is full.",
            ));
        }
        if self.0.outbound.send(encoded).is_err() {
            self.0.buffered.fetch_sub(size, Ordering::AcqRel);
            return Err(IvyError::new(
                "service_unavailable",
                "Hive connection is unavailable.",
            ));
        }
        Ok(())
    }
    pub fn notification(
        &self,
        namespace: JsString,
        name: JsString,
        version: JsString,
        payload: IvyJson,
    ) -> IvyResult<()> {
        self.send(&IvyJson::object([
            ("jsonrpc", "2.0".into()),
            ("method", "service.notification".into()),
            (
                "params",
                IvyJson::object([
                    ("namespace", IvyJson::String(namespace)),
                    ("name", IvyJson::String(name)),
                    ("version", IvyJson::String(version)),
                    ("payload", payload),
                ]),
            ),
        ]))
    }
    fn receive(&self, text: &str) -> IvyResult<()> {
        let frame = parse(text, MANAGEMENT_FRAME_BYTES)?;
        if frame.as_object().is_none() {
            return Err(IvyError::internal());
        }
        if equal(frame.get("method"), "provider.invoke") {
            self.invoke(frame);
        } else if equal(frame.get("method"), "events.available") && frame.get("id").is_none() {
            validate_event_availability_frame(&frame)?;
            let sequence = frame
                .get("params")
                .unwrap()
                .get("throughSequence")
                .unwrap()
                .as_f64()
                .unwrap() as u64;
            self.0.available.fetch_max(sequence, Ordering::AcqRel);
            self.0.events.notify_waiters();
        } else if equal(frame.get("method"), "notifications.provider") && frame.get("id").is_none()
        {
            validate_provider_notification_frame(&frame)?;
            if let Some(observer) = &self.0.options.on_notification {
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| observer(&frame)));
            }
        } else {
            let id = frame
                .get("id")
                .map(js_string)
                .unwrap_or_else(|| "undefined".into());
            if let Ok(id) = id.to_utf8() {
                let reply = self.0.state.lock().unwrap().pending.remove(&id);
                if let Some(reply) = reply {
                    let _ = reply.send(response_value(&frame, &id));
                }
            }
        }
        Ok(())
    }
    fn invoke(&self, frame: IvyJson) {
        // Invalid requests have no trusted correlation identity and are ignored.
        if validate_provider_call_frame(&frame).is_err() {
            return;
        }
        let id = frame.get("id").unwrap().clone();
        let params = frame.get("params").unwrap();
        let generation = params.get("generation").unwrap().as_f64().unwrap() as u64;
        let binding = {
            let state = self.0.state.lock().unwrap();
            if self.0.close_reason.get().is_some()
                || !self.ready()
                || generation != self.generation()
            {
                Err(IvyError::new(
                    "stale_generation",
                    "Invocation generation is not ready.",
                ))
            } else {
                state
                    .bindings
                    .get(params.get("qualifiedName").unwrap().as_js_string().unwrap())
                    .filter(|b| equal(params.get("definitionHash"), &b.hash))
                    .cloned()
                    .ok_or_else(|| {
                        IvyError::new(
                            "tool_definition_changed",
                            "Invocation does not match this registered definition.",
                        )
                    })
            }
        };
        let binding = match binding {
            Ok(binding) => binding,
            Err(error) => {
                self.invocation_reply(id, Err(error));
                return;
            }
        };
        let permit = match self.0.options.capacity.0.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.invocation_reply(
                    id,
                    Err(IvyError::new(
                        "limit_exceeded",
                        "Service handler capacity is full.",
                    )),
                );
                return;
            }
        };
        let context = InvocationContext {
            caller_principal_id: params
                .get("callerPrincipalId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            operation_id: params
                .get("operationId")
                .and_then(IvyJson::as_js_string)
                .cloned(),
            generation,
            signal: self.signal(),
            close_reason: self.0.close_reason.clone(),
        };
        let arguments = params.get("arguments").unwrap().clone();
        let connection = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            // Cancellation only informs the handler; accepted work retains capacity until completion.
            let result =
                AssertUnwindSafe(async move { (binding.handler)(arguments, context).await })
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        Err(unknown("internal_error", "Service invocation failed."))
                    });
            connection.invocation_reply(id, result);
        });
    }
    fn invocation_reply(&self, id: IvyJson, result: IvyResult<IvyJson>) {
        if self.0.signal.is_cancelled() {
            return;
        }
        let result = result.and_then(|result| {
            self.send(&IvyJson::object([
                ("jsonrpc", "2.0".into()),
                ("id", id.clone()),
                ("result", result),
            ]))
        });
        if let Err(error) = result {
            if self
                .send(&IvyJson::object([
                    ("jsonrpc", "2.0".into()),
                    ("id", id),
                    ("error", error.to_wire()),
                ]))
                .is_err()
            {
                self.close();
            }
        }
    }
    pub async fn wait_for_events(&self, after_sequence: u64, timeout: Duration) -> IvyResult<()> {
        let timer = tokio::time::sleep(timeout.max(Duration::from_millis(25)));
        tokio::pin!(timer);
        loop {
            let notified = self.0.events.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.0.available.load(Ordering::Acquire) > after_sequence {
                return Ok(());
            }
            self.check_open()?;
            tokio::select! {biased;_=self.0.signal.cancelled()=>return self.check_open(),_=&mut timer=>return Ok(()),_=notified=>{}}
        }
    }
}

struct PendingObservation {
    inner: Weak<Inner>,
    id: String,
}
impl Drop for PendingObservation {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.state.lock().unwrap().pending.remove(&self.id);
        }
    }
}
impl RpcClient for ServiceConnection {
    fn service_connection(&self) -> Option<&Self> {
        Some(self)
    }
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let id = uuid::Uuid::new_v4().to_string();
            let (reply, result) = oneshot::channel();
            {
                let mut state = self.0.state.lock().unwrap();
                if self.0.close_reason.get().is_some() {
                    return Err(IvyError::new(
                        "service_unavailable",
                        "Hive service connection is unavailable.",
                    ));
                }
                if options.cancellation.is_cancelled() {
                    return Err(options.cancel_error);
                }
                if state.pending.len() >= CONNECTION_IN_FLIGHT_REQUESTS {
                    return Err(IvyError::new(
                        "limit_exceeded",
                        "Service in-flight request limit reached.",
                    ));
                }
                state.pending.insert(id.clone(), reply);
            }
            let _observation = PendingObservation {
                inner: Arc::downgrade(&self.0),
                id: id.clone(),
            };
            self.send(&IvyJson::object([
                ("jsonrpc", "2.0".into()),
                ("id", id.into()),
                ("method", method.into()),
                ("params", params),
            ]))?;
            tokio::select! {
                biased;
                result=result=>result.unwrap_or_else(|_|Err(self.close_reason().unwrap_or_else(socket_closed))),
                _=options.cancellation.cancelled()=>Err(unknown("outcome_unknown","Request observation was cancelled; its execution is not presumed cancelled.")),
                _=tokio::time::sleep(options.timeout)=>Err(unknown("deadline_exceeded","Hive request timed out; reconcile its operation identity.")),
            }
        })
    }
}
