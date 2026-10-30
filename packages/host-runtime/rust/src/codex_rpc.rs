//! Native JSONL over caller-owned streams. No process launch, retry or daemon kill.
//! One actor serializes protocol state and durable hooks. Dropping a request
//! waiter does not cancel dispatch or its eventual before-resolve commit.
use super::codex_admission::{NativeAdmission, PendingRequest, Priority, ResponseOwner, Timeout};
use super::codex_framing::{FrameError, NativeLines};
use ivy_contracts::agent_schema::{NativeFrameKind, validate_agent, validate_agent_frame};
use ivy_contracts::json::{parse, stringify};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

pub use ivy_contracts::limits::{
    NATIVE_ANSWER_FRAME_BYTES, NATIVE_FRAME_BYTES, NATIVE_REQUEST_FRAME_BYTES,
};
pub type HookFuture = Pin<Box<dyn Future<Output = IvyResult<()>> + Send>>;
pub type BeforeSend = Arc<dyn Fn(IvyJson) -> HookFuture + Send + Sync>;
pub type BeforeResolve = Arc<dyn Fn(IvyJson, IvyJson) -> HookFuture + Send + Sync>;

#[derive(Clone)]
pub struct RequestOptions {
    pub request_id: Option<JsString>,
    pub timeout_ms: u64,
    pub priority: Priority,
    pub detach_on_timeout: bool,
    pub before_send: Option<BeforeSend>,
    pub before_resolve: Option<BeforeResolve>,
}
impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            request_id: None,
            timeout_ms: 120_000,
            priority: Priority::Background,
            detach_on_timeout: false,
            before_send: None,
            before_resolve: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NativeRequest {
    pub id: IvyJson,
    pub method: JsString,
    pub params: IvyJson,
}
#[derive(Clone, Debug)]
pub struct NativeNotification {
    pub method: JsString,
    pub params: IvyJson,
}
/// Callbacks commit observations in wire order. They must not await another
/// command on this same actor. Return an error to close the connection.
#[derive(Clone)]
pub struct Events {
    pub on_request: Arc<dyn Fn(NativeRequest) -> HookFuture + Send + Sync>,
    pub on_notification: Arc<dyn Fn(NativeNotification) -> HookFuture + Send + Sync>,
    pub on_close: Arc<dyn Fn(String) -> HookFuture + Send + Sync>,
}
impl Default for Events {
    fn default() -> Self {
        Self {
            on_request: Arc::new(|_| Box::pin(async { Ok(()) })),
            on_notification: Arc::new(|_| Box::pin(async { Ok(()) })),
            on_close: Arc::new(|_| Box::pin(async { Ok(()) })),
        }
    }
}

type Answer<T> = oneshot::Sender<IvyResult<T>>;
enum Command {
    Request(JsString, IvyJson, RequestOptions, Answer<IvyJson>),
    Notify(JsString, IvyJson, Answer<()>),
    Answer(JsString, IvyJson, IvyJson, BeforeSend, Answer<()>),
    Close(String),
}
enum Incoming {
    Frame(IvyJson),
    Close(String),
}
struct Pending {
    key: String,
    id: IvyJson,
    method: JsString,
    options: RequestOptions,
    deadline: Instant,
    response: Answer<IvyJson>,
}
struct Tasks(Vec<JoinHandle<()>>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

#[derive(Clone)]
pub struct NativeRpc {
    commands: mpsc::Sender<Command>,
    state: watch::Receiver<Option<String>>,
    catalog: Arc<IvyJson>,
}
impl NativeRpc {
    pub fn start<W, R>(
        catalog: Arc<IvyJson>,
        input: W,
        output: R,
        events: Events,
    ) -> IvyResult<Self>
    where
        W: AsyncWrite + Unpin + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
    {
        validate_agent("Catalog", &catalog)?;
        let definitions = |name| -> IvyResult<BTreeMap<JsString, MethodParameters>> {
            let mut map = BTreeMap::new();
            for definition in catalog
                .get(name)
                .and_then(IvyJson::as_array)
                .ok_or_else(IvyError::internal)?
            {
                let method = definition
                    .get("method")
                    .and_then(IvyJson::as_js_string)
                    .ok_or_else(IvyError::internal)?
                    .clone();
                let parameters = MethodParameters {
                    required: definition.get("paramsRequired") == Some(&IvyJson::Bool(true)),
                    absent: definition.get("paramsAbsent") == Some(&IvyJson::Bool(true)),
                };
                if map.insert(method, parameters).is_some() {
                    return Err(IvyError::new(
                        "native_catalog_invalid",
                        "Native catalog has duplicate methods.",
                    ));
                }
            }
            Ok(map)
        };
        let requests = definitions("clientRequests")?;
        let server = definitions("serverRequests")?;
        let notifications = definitions("serverNotifications")?;
        let client_notifications = definitions("clientNotifications")?;
        let (commands, receiver) = mpsc::channel(64);
        let (state_tx, state) = watch::channel(None);
        let (incoming_tx, incoming) = mpsc::channel(1);
        let (writes, write_rx) = mpsc::unbounded_channel();
        let buffered = Arc::new(AtomicUsize::new(0));
        let tasks = Tasks(vec![
            tokio::spawn(read_stream(output, incoming_tx.clone())),
            tokio::spawn(write_stream(input, write_rx, incoming_tx, buffered.clone())),
        ]);
        let actor = Actor {
            admission: NativeAdmission::default(),
            pending: Vec::new(),
            observation_prefix: format!("observation-{}:", uuid::Uuid::new_v4()),
            observation_sequence: 0,
            requests,
            server,
            notifications,
            client_notifications,
            events,
            writes,
            buffered,
            state: state_tx,
        };
        tokio::spawn(async move {
            let _tasks = tasks;
            actor.run(receiver, incoming).await;
        });
        Ok(Self {
            commands,
            state,
            catalog,
        })
    }
    pub fn catalog(&self) -> &IvyJson {
        self.catalog.as_ref()
    }
    pub fn connected(&self) -> bool {
        self.state.borrow().is_none() && !self.commands.is_closed()
    }
    pub async fn closed(&self) -> String {
        let mut state = self.state.clone();
        loop {
            if let Some(code) = state.borrow().clone() {
                return code;
            }
            if state.changed().await.is_err() {
                return "native_connection_closed".into();
            }
        }
    }
    pub async fn close(&self, code: impl Into<String>) {
        let _ = self.commands.send(Command::Close(code.into())).await;
        self.closed().await;
    }
    pub async fn request(
        &self,
        method: JsString,
        params: IvyJson,
        options: RequestOptions,
    ) -> IvyResult<IvyJson> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Request(method, params, options, tx))
            .await
            .map_err(|_| unavailable())?;
        rx.await
            .unwrap_or_else(|_| Err(unknown("native_connection_closed")))
    }
    pub async fn notify(&self, method: JsString, params: IvyJson) -> IvyResult<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Notify(method, params, tx))
            .await
            .map_err(|_| unavailable())?;
        rx.await
            .unwrap_or_else(|_| Err(unknown("native_connection_closed")))
    }
    /// The owner checks current epoch/request authority before calling this.
    pub async fn answer(
        &self,
        method: JsString,
        id: IvyJson,
        reply: IvyJson,
        before_send: BeforeSend,
    ) -> IvyResult<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Answer(method, id, reply, before_send, tx))
            .await
            .map_err(|_| unavailable())?;
        rx.await
            .unwrap_or_else(|_| Err(unknown("native_connection_closed")))
    }
}
fn unavailable() -> IvyError {
    IvyError::new("native_unavailable", "Native connection is unavailable.")
}
fn unknown(code: &str) -> IvyError {
    IvyError::new(
        code,
        "Native connection ended; reconcile the original operation.",
    )
    .with_outcome(Outcome::Unknown)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn key(id: &IvyJson) -> String {
    stringify(id, usize::MAX).expect("validated request ID")
}
fn encoded(value: &IvyJson, limit: usize) -> IvyResult<Vec<u8>> {
    let mut bytes = stringify(value, limit)?.into_bytes();
    bytes.push(b'\n');
    Ok(bytes)
}
async fn read_stream<R: AsyncRead + Unpin>(mut output: R, events: mpsc::Sender<Incoming>) {
    let mut lines = NativeLines::new(NATIVE_FRAME_BYTES);
    let mut buffer = vec![0; 65_536];
    let code = loop {
        let count = match output.read(&mut buffer).await {
            Ok(0) => break lines.finish().to_owned(),
            Ok(n) => n,
            Err(_) => break "native_read_failed".into(),
        };
        let mut frames = Vec::new();
        let result = lines.receive(&buffer[..count], |text| {
            frames.push(parse(text, NATIVE_FRAME_BYTES).map_err(|_| ())?);
            Ok(())
        });
        // Valid frames earlier in the same chunk precede any framing failure.
        for frame in frames {
            if events.send(Incoming::Frame(frame)).await.is_err() {
                return;
            }
        }
        if let Err(error) = result {
            break match error {
                FrameError::TooLarge => "native_frame_too_large",
                FrameError::InvalidUtf8 | FrameError::Consumer(()) => "native_invalid_frame",
            }
            .into();
        }
    };
    let _ = events.send(Incoming::Close(code)).await;
}
async fn write_stream<W: AsyncWrite + Unpin>(
    mut input: W,
    mut writes: mpsc::UnboundedReceiver<Vec<u8>>,
    events: mpsc::Sender<Incoming>,
    buffered: Arc<AtomicUsize>,
) {
    while let Some(bytes) = writes.recv().await {
        let result = input.write_all(&bytes).await;
        buffered.fetch_sub(bytes.len(), Ordering::SeqCst);
        if result.is_err() {
            let _ = events
                .send(Incoming::Close("native_write_failed".into()))
                .await;
            return;
        }
    }
}
// The actor needs names and envelope rules, not copies of the catalog schemas.
struct MethodParameters {
    required: bool,
    absent: bool,
}
struct Actor {
    admission: NativeAdmission,
    pending: Vec<Pending>,
    observation_prefix: String,
    observation_sequence: u64,
    requests: BTreeMap<JsString, MethodParameters>,
    server: BTreeMap<JsString, MethodParameters>,
    notifications: BTreeMap<JsString, MethodParameters>,
    client_notifications: BTreeMap<JsString, MethodParameters>,
    events: Events,
    writes: mpsc::UnboundedSender<Vec<u8>>,
    buffered: Arc<AtomicUsize>,
    state: watch::Sender<Option<String>>,
}
impl Actor {
    fn observation(&self, id: &IvyJson) -> bool {
        id.as_js_string()
            .and_then(|s| s.to_utf8().ok())
            .and_then(|s| s.strip_prefix(&self.observation_prefix).map(str::to_owned))
            .is_some_and(|suffix| {
                suffix.parse::<u64>().is_ok_and(|n| {
                    n > 0 && n <= self.observation_sequence && n.to_string() == suffix
                })
            })
    }
    fn writable(&self) -> IvyResult<()> {
        require(
            !self.writes.is_closed(),
            "native_unavailable",
            "Native connection is unavailable.",
        )?;
        require(
            self.buffered.load(Ordering::SeqCst) < NATIVE_ANSWER_FRAME_BYTES,
            "native_capacity",
            "Native write buffer is full; no new native request was sent.",
        )
    }
    fn write(&self, bytes: Vec<u8>) -> IvyResult<()> {
        let len = bytes.len();
        self.buffered.fetch_add(len, Ordering::SeqCst);
        self.writes.send(bytes).map_err(|_| {
            self.buffered.fetch_sub(len, Ordering::SeqCst);
            unknown("native_write_failed")
        })
    }
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut incoming: mpsc::Receiver<Incoming>,
    ) {
        let code = loop {
            let deadline = self.pending.iter().map(|p| p.deadline).min();
            let timer = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                message = incoming.recv() => match message {
                    Some(Incoming::Frame(frame)) => if let Err(error) = self.frame(frame).await { break error.code; },
                    Some(Incoming::Close(code)) => break code,
                    None => break "native_connection_closed".into(),
                },
                command = commands.recv() => match command {
                    Some(Command::Close(code)) => break code,
                    Some(command) => if let Some(code) = self.command(command).await { break code; },
                    None => break "native_connection_closed".into(),
                },
                _ = timer => {
                    let Some(index) = self.pending.iter().position(|p| p.deadline <= Instant::now()) else { continue; };
                    let id = self.pending[index].key.clone();
                    match self.admission.timeout(&id) {
                        Timeout::CloseConnection => break "native_deadline_exceeded".into(),
                        Timeout::Detached(_) => {
                            let p = self.pending.remove(index);
                            if self.observation(&p.id) { self.admission.complete(&id); }
                            let error = IvyError::new("native_observation_deadline", "Background observation expired; active turns remain connected.")
                                .with_outcome(Outcome::Unknown).with_details(details(&p));
                            let _ = p.response.send(Err(error));
                        }, Timeout::Inactive => {},
                    }
                }
            }
        };
        // Close admission before the durable close callback; pending outcomes
        // are rejected only after that callback has persisted recovery evidence.
        commands.close();
        let code = if (self.events.on_close)(code.clone()).await.is_err() {
            "native_recovery_storage_failed".into()
        } else {
            code
        };
        self.admission.close();
        for p in self.pending.drain(..) {
            let error = unknown(&code).with_details(details(&p));
            let _ = p.response.send(Err(error));
        }
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Request(_, _, _, tx) => {
                    let _ = tx.send(Err(unavailable()));
                }
                Command::Notify(_, _, tx) | Command::Answer(_, _, _, _, tx) => {
                    let _ = tx.send(Err(unavailable()));
                }
                Command::Close(_) => {}
            }
        }
        self.state.send_replace(Some(code));
    }
    async fn command(&mut self, command: Command) -> Option<String> {
        match command {
            Command::Request(method, params, options, response) => {
                let mut prepare = || -> IvyResult<(IvyJson, String, Vec<u8>)> {
                    self.writable()?;
                    self.admission
                        .check_capacity(options.detach_on_timeout, options.priority)
                        .map_err(|e| IvyError::new(e.code(),
                            if self.admission.pending_count() >= if options.priority == Priority::Interaction {64} else {56} {
                                "Native request capacity is full; foreground slots remain reserved."
                            } else { "Native observation capacity is full." }))?;
                    require(
                        !options.detach_on_timeout
                            || options.request_id.is_none()
                            || self.admission.expired_count() < 64,
                        "native_capacity",
                        "Native unresolved request identity capacity is full.",
                    )?;
                    require(
                        self.requests.contains_key(&method),
                        "native_method_unavailable",
                        "Method is absent from this installed native catalog.",
                    )?;
                    require(
                        options.timeout_ms > 0 && options.timeout_ms <= 600_000,
                        "invalid_arguments",
                        "Native observation deadline must be finite and at most ten minutes.",
                    )?;
                    require(
                        !options.request_id.as_ref().is_some_and(|id| {
                            id.to_utf8()
                                .is_ok_and(|s| s.starts_with(&self.observation_prefix))
                        }),
                        "native_state_conflict",
                        "Native observation request IDs cannot be reused.",
                    )?;
                    let id = IvyJson::String(match options.request_id.clone() {
                        Some(id) => id,
                        None if options.detach_on_timeout => {
                            self.observation_sequence += 1;
                            format!("{}{}", self.observation_prefix, self.observation_sequence)
                                .into()
                        }
                        None => uuid::Uuid::new_v4().to_string().into(),
                    });
                    validate_agent_frame(&id, NativeFrameKind::RequestId)?;
                    let id_key = key(&id);
                    require(
                        matches!(
                            self.admission.response_owner(&id_key),
                            ResponseOwner::Unknown
                        ),
                        "native_state_conflict",
                        "Native request ID is already in use.",
                    )?;
                    let bytes = encoded(
                        &IvyJson::object([
                            ("id", id.clone()),
                            ("method", IvyJson::String(method.clone())),
                            ("params", params),
                        ]),
                        NATIVE_REQUEST_FRAME_BYTES,
                    )?;
                    Ok((id, id_key, bytes))
                };
                let (id, id_key, bytes) = match prepare() {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = response.send(Err(e));
                        return None;
                    }
                };
                self.admission
                    .admit(
                        id_key.clone(),
                        PendingRequest {
                            method: String::from_utf16_lossy(&method.0),
                            timeout_ms: options.timeout_ms,
                            detach_on_timeout: options.detach_on_timeout,
                        },
                        options.priority,
                    )
                    .expect("checked admission");
                let deadline = Instant::now() + Duration::from_millis(options.timeout_ms);
                if let Some(hook) = &options.before_send {
                    if let Err(error) = hook(id.clone()).await {
                        self.admission.cancel_before_send(&id_key);
                        let _ = response.send(Err(error));
                        return None;
                    }
                }
                self.pending.push(Pending {
                    key: id_key,
                    id,
                    method,
                    options,
                    deadline,
                    response,
                });
                if self.write(bytes).is_err() {
                    return Some("native_write_failed".into());
                }
            }
            Command::Notify(method, params, response) => {
                let result = (|| {
                    self.writable()?;
                    let definition = self.client_notifications.get(&method).ok_or_else(|| {
                        IvyError::new(
                            "native_method_unavailable",
                            "Client notification is absent from the installed native catalog.",
                        )
                    })?;
                    let mut frame = IvyJson::object([("method", IvyJson::String(method))]);
                    if !definition.absent {
                        frame.insert("params", params);
                    }
                    let bytes = encoded(&frame, NATIVE_ANSWER_FRAME_BYTES)?;
                    self.write(bytes)
                })();
                let close = result
                    .as_ref()
                    .is_err_and(|e| e.outcome == Outcome::Unknown);
                let _ = response.send(result);
                if close {
                    return Some("native_write_failed".into());
                }
            }
            Command::Answer(method, id, reply, hook, response) => {
                let prepare = || {
                    self.writable()?;
                    validate_agent_frame(&id, NativeFrameKind::RequestId)?;
                    validate_agent_frame(&reply, NativeFrameKind::Reply)?;
                    require(
                        self.server.contains_key(&method),
                        "native_method_unavailable",
                        "Server request is absent from the installed native catalog.",
                    )?;
                    let mut frame = IvyJson::object([("id", id.clone())]);
                    for (name, value) in reply.as_object().expect("validated object") {
                        frame.insert(name.clone(), value.clone());
                    }
                    encoded(&frame, NATIVE_ANSWER_FRAME_BYTES)
                };
                let result = match prepare() {
                    Ok(bytes) => match hook(id).await {
                        Ok(()) => self.write(bytes),
                        Err(e) => Err(e),
                    },
                    Err(e) => Err(e),
                };
                // A persistence error, even unknown, does not imply a write failure.
                let close = self.writes.is_closed();
                let _ = response.send(result);
                if close {
                    return Some("native_write_failed".into());
                }
            }
            Command::Close(_) => unreachable!(),
        }
        None
    }
    async fn frame(&mut self, frame: IvyJson) -> IvyResult<()> {
        require(
            frame.as_object().is_some(),
            "native_invalid_frame",
            "Native frame must be one protocol object.",
        )?;
        require(
            frame
                .get("jsonrpc")
                .is_none_or(|v| v == &IvyJson::from("2.0")),
            "native_invalid_frame",
            "Native JSON-RPC version is invalid.",
        )?;
        if let Some(method) = frame.get("method") {
            require(
                method.as_js_string().is_some()
                    && frame.get("result").is_none()
                    && frame.get("error").is_none(),
                "native_invalid_frame",
                "Native request/notification variants cannot mix.",
            )?;
            let method = method.as_js_string().unwrap();
            let has_id = frame.get("id").is_some();
            let definition = (if has_id {
                &self.server
            } else {
                &self.notifications
            })
            .get(method)
            .ok_or_else(|| {
                IvyError::new(
                    "native_catalog_mismatch",
                    "Native method is absent from the pinned generated catalog.",
                )
            })?;
            require(
                !definition.required || frame.get("params").is_some(),
                "native_invalid_frame",
                "Native method is missing required parameters.",
            )?;
            let params = frame.get("params").cloned().unwrap_or(IvyJson::Null);
            if let Some(id) = frame.get("id") {
                validate_agent_frame(id, NativeFrameKind::RequestId)?;
                (self.events.on_request)(NativeRequest {
                    id: id.clone(),
                    method: method.clone(),
                    params,
                })
                .await?;
            } else {
                (self.events.on_notification)(NativeNotification {
                    method: method.clone(),
                    params,
                })
                .await?;
            }
            return Ok(());
        }
        let missing_id = IvyJson::Null;
        let id = frame.get("id").unwrap_or(&missing_id);
        validate_agent_frame(id, NativeFrameKind::RequestId)?;
        require(
            id.as_js_string().is_some(),
            "native_unknown_response",
            "Native response does not identify an outstanding client request.",
        )?;
        let id_key = key(id);
        require(
            self.observation(id)
                || !matches!(
                    self.admission.response_owner(&id_key),
                    ResponseOwner::Unknown
                ),
            "native_unknown_response",
            "Native response has no outstanding client request.",
        )?;
        require(
            frame.get("result").is_some() != frame.get("error").is_some(),
            "native_invalid_frame",
            "Native response requires exactly one result or error.",
        )?;
        let reply = match frame.get("result") {
            Some(result) => IvyJson::object([("result", result.clone())]),
            None => IvyJson::object([("error", frame.get("error").unwrap().clone())]),
        };
        validate_agent_frame(&reply, NativeFrameKind::Reply)?;
        if let Some(index) = self.pending.iter().position(|p| p.key == id_key) {
            if let Some(hook) = &self.pending[index].options.before_resolve {
                hook(self.pending[index].id.clone(), reply.clone())
                    .await
                    .map_err(|_| {
                        IvyError::new(
                            "native_outcome_commit_failed",
                            "Native outcome commit failed.",
                        )
                    })?;
            }
            let p = self.pending.remove(index);
            self.admission.complete(&id_key);
            let _ = p.response.send(Ok(reply));
        } else {
            self.admission.complete(&id_key);
        }
        Ok(())
    }
}
fn details(p: &Pending) -> IvyJson {
    IvyJson::object([
        ("method", IvyJson::String(p.method.clone())),
        ("timeoutMs", IvyJson::Number(p.options.timeout_ms as f64)),
    ])
}
