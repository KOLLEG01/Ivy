//! ChatBridge's provider lifecycle. Host owns configuration/health file I/O;
//! the native WhatsApp owner supplies its existing transport lifecycle boundary.
use crate::{
    chat_main::ChatMain,
    collector::ChatCollector,
    delivery::ChatDelivery,
    diagnostic_notifications::DIAGNOSTIC_NOTIFICATION_POLL_MS,
    generation::{ChatGeneration, ChatGenerationConfig, ChatGenerationLease, settled},
    registry::{chat_registry, chat_tools},
    whatsapp::{
        journal_owner::Journal,
        messages::Translator,
        runtime::{
            RuntimeAssembly, RuntimeMedia, RuntimeParts, RuntimeResources, RuntimeSocket,
            WhatsAppRuntime,
        },
    },
    worker::ChatWorker,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, chat_schema::validate_chat, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::portable_native_files::NativeHelperLayout;
use ivy_sdk::{
    clock::{Clock, SystemClock, date_to_iso},
    service::{ServiceClient, ServiceFuture, ServiceOptions, ServiceState},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
};

type Completion = watch::Receiver<Option<IvyResult<()>>>;
fn complete(result: IvyResult<()>) -> Completion {
    let (_, receiver) = watch::channel(Some(result));
    receiver
}
#[derive(Clone)]
pub struct HealthObservation {
    pub ready: bool,
    pub generation: Option<u64>,
    pub details: JsString,
}
/// These callbacks delegate to the shared Host HealthFile; they do not implement
/// configuration, file replacement or boot/launch control matching in this app.
pub struct ChatHealth {
    pub write: Arc<dyn Fn(HealthObservation) -> ServiceFuture<()> + Send + Sync>,
    pub control: Arc<dyn Fn() -> ServiceFuture<bool> + Send + Sync>,
}

pub(crate) fn service_settings(config: &Json) -> IvyResult<&Json> {
    let settings = config.required("settings")?;
    validate_chat("Settings", settings)?;
    if config.get("componentId") != Some(&Json::from("chat-bridge"))
        || !config
            .get("credential")
            .and_then(Json::as_js_string)
            .is_some_and(|value| !value.0.is_empty())
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "ChatBridge requires its own component identity and Hive credential.",
        ));
    }
    Ok(settings)
}
/// The account and media owners open these resources before service startup.
/// Component admission and helper layout remain explicit at this boundary.
pub struct WhatsAppServiceParts {
    pub config: Json,
    pub journal: Journal,
    pub socket: Arc<dyn RuntimeSocket>,
    pub media: Arc<dyn RuntimeMedia>,
    pub resources: RuntimeResources,
    pub admitted_artifact: PathBuf,
    pub helper_layout: NativeHelperLayout,
}
/// The service opens the real native transport before constructing these parts.
/// Every accepted transport operation that uses Main must retain its generation
/// lease; settled/close retain the transport's own journal and socket work.
pub trait ChatTransport: Send + Sync {
    fn files(&self) -> Option<crate::files::ChatFiles> { None }
    fn start(&self);
    fn attach(&self, generation: Option<Arc<ChatGeneration>>) -> IvyResult<()>;
    fn changed(&self);
    fn notification(&self, frame: &Json);
    fn state(&self) -> JsString;
    fn created_main(&self, binding_id: &str) -> IvyResult<()>;
    fn settled(&self) -> ServiceFuture<()>;
    fn close(&self) -> ServiceFuture<()>;
}
struct HealthJob {
    value: HealthObservation,
    done: watch::Sender<Option<IvyResult<()>>>,
}
struct HealthWrites {
    sender: mpsc::UnboundedSender<HealthJob>,
    last: Mutex<Completion>,
}
impl HealthWrites {
    fn new(write: Arc<dyn Fn(HealthObservation) -> ServiceFuture<()> + Send + Sync>) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<HealthJob>();
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                // Like healthWork.catch(...).then(...), a failed previous write
                // does not prevent the next admitted write from running.
                job.done.send_replace(Some(write(job.value).await));
            }
        });
        Self {
            sender,
            last: Mutex::new(complete(Ok(()))),
        }
    }
    fn write(
        &self,
        ready: bool,
        generation: Option<u64>,
        details: impl Into<JsString>,
    ) -> Completion {
        let (done, receiver) = watch::channel(None);
        let mut last = self.last.lock().unwrap();
        let job = HealthJob {
            value: HealthObservation {
                ready,
                generation,
                details: details.into(),
            },
            done,
        };
        if let Err(error) = self.sender.send(job) {
            error.0.done.send_replace(Some(Err(IvyError::internal())));
        }
        *last = receiver.clone();
        receiver
    }
    fn pending(&self) -> Completion {
        self.last.lock().unwrap().clone()
    }
}

#[derive(Clone, Default)]
struct WorkerState {
    idle: bool,
    runnable: bool,
}
struct WorkerHandle {
    sender: mpsc::UnboundedSender<oneshot::Sender<(IvyResult<()>, WorkerState)>>,
    state: Arc<Mutex<WorkerState>>,
    turn: Arc<Mutex<Option<JsString>>>,
    connection: ivy_sdk::service::ServiceConnection,
}
impl WorkerHandle {
    async fn start(
        lease: ChatGenerationLease,
        owner: Weak<Core>,
        generation: Arc<ChatGeneration>,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        let (sender, mut receiver) =
            mpsc::unbounded_channel::<oneshot::Sender<(IvyResult<()>, WorkerState)>>();
        let (ready, started) = oneshot::channel();
        let state = Arc::new(Mutex::new(WorkerState::default()));
        let turn = Arc::new(Mutex::new(None));
        let progress = turn.clone();
        let observed = state.clone();
        let connection = lease.connection.clone();
        tokio::spawn(async move {
            let (native, operations) =
                match lease.native().and_then(|n| Ok((n, lease.operations()?))) {
                    Ok(parts) => parts,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
            let main =
                match ChatMain::new(&operations, &native, &lease.config().notice_instructions) {
                    Ok(main) => main,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
            let delivery = ChatDelivery::new(&main);
            let evidence = match native.evidence() {
                Ok(e) => e,
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            let collector =
                ChatCollector::new(&operations, &evidence, &delivery, &lease.config().catalogs);
            let mut worker = match ChatWorker::with_clock(&delivery, &collector, clock) {
                Ok(w) => w,
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            worker.observe_turns(Arc::new(move |value| *progress.lock().unwrap() = value));
            let _ = ready.send(Ok(()));
            // This future keeps one borrowed graph, including the collector's
            // caches, for the entire connection lifetime. It never aborts a drain.
            loop {
                tokio::select! { biased;
                    _ = lease.connection.closed() => break,
                    command = receiver.recv() => {
                        let Some(done) = command else { break; };
                        let result = worker.drain(|| owner.upgrade().is_some_and(|c| c.should_continue(&generation))).await;
                        let value = WorkerState { idle: worker.idle, runnable: worker.runnable };
                        *observed.lock().unwrap() = value.clone();
                        let _ = done.send((result, value));
                    }
                }
            }
        });
        started.await.map_err(|_| IvyError::internal())??;
        Ok(Self {
            sender,
            state,
            turn,
            connection,
        })
    }
    async fn drain(&self) -> (IvyResult<()>, WorkerState) {
        let (done, receive) = oneshot::channel();
        if self.sender.send(done).is_ok() {
            if let Ok(value) = receive.await {
                return value;
            }
        }
        let result = if self.connection.signal().is_cancelled() {
            Ok(())
        } else {
            Err(IvyError::internal())
        };
        (result, self.state.lock().unwrap().clone())
    }
}

struct Attached {
    generation: Arc<ChatGeneration>,
    worker: WorkerHandle,
}
#[derive(Default)]
struct State {
    closed: bool,
    ready: bool,
    runtime: Option<Arc<Attached>>,
    started_generation: Option<u64>,
    active: Option<Completion>,
    diagnostic_work: Option<Completion>,
    diagnostic: Option<Json>,
    diagnostic_notice_error: Option<Json>,
    wake_pending: bool,
    timer: Option<JoinHandle<()>>,
    retired: HashMap<usize, Completion>,
}
struct Core {
    config: Arc<ChatGenerationConfig>,
    state: Mutex<State>,
    poll: Duration,
    health: HealthWrites,
    transport: Option<Arc<dyn ChatTransport>>,
    clock: Arc<dyn Clock>,
    stopped: watch::Sender<bool>,
}
impl Core {
    fn current(&self) -> Option<Arc<Attached>> {
        self.state.lock().unwrap().runtime.clone()
    }
    fn ready(state: &State) -> bool {
        state.ready
            && state
                .runtime
                .as_ref()
                .is_some_and(|r| r.generation.connection.ready())
    }
    fn should_continue(&self, generation: &Arc<ChatGeneration>) -> bool {
        let s = self.state.lock().unwrap();
        !s.closed
            && Self::ready(&s)
            && s.runtime
                .as_ref()
                .is_some_and(|r| Arc::ptr_eq(&r.generation, generation))
    }
    fn selected(&self, generation: u64, message: &str) -> IvyResult<ChatGenerationLease> {
        let s = self.state.lock().unwrap();
        s.runtime
            .as_ref()
            .filter(|r| r.generation.generation() == generation)
            .ok_or_else(|| IvyError::new("service_not_ready", message))?
            .generation
            .accept()
    }
    async fn reconcile(
        self: &Arc<Self>,
        connection: ivy_sdk::service::ServiceConnection,
    ) -> IvyResult<()> {
        let generation =
            ChatGeneration::with_clock(self.config.clone(), connection.clone(), self.clock.clone())
                .await?;
        let worker = WorkerHandle::start(
            generation.accept()?,
            Arc::downgrade(self),
            generation.clone(),
            self.clock.clone(),
        )
        .await;
        let worker = match worker {
            Ok(w) => w,
            Err(error) => {
                settled(generation.retire()).await?;
                return Err(error);
            }
        };
        let next = Arc::new(Attached { generation, worker });
        let attachment = {
            let mut s = self.state.lock().unwrap();
            match connection.throw_if_aborted() {
                Ok(()) => {
                    s.runtime = Some(next.clone());
                    s.diagnostic = None;
                    Ok(())
                }
                Err(error) => Err(error),
            }
        };
        if let Err(error) = attachment {
            settled(next.generation.retire()).await?;
            return Err(error);
        }
        self.retire_after_close(next.clone());
        if let Some(transport) = &self.transport {
            transport.attach(Some(next.generation.clone()))?;
        }
        Ok(())
    }
    fn retire_after_close(self: &Arc<Self>, selected: Arc<Attached>) {
        let id = Arc::as_ptr(&selected) as usize;
        let (done, receive) = watch::channel(None);
        self.state.lock().unwrap().retired.insert(id, receive);
        let owner = Arc::downgrade(self);
        let transport = self.transport.clone();
        tokio::spawn(async move {
            selected.generation.connection.closed().await;
            if let Some(core) = owner.upgrade() {
                let detached = {
                    let mut s = core.state.lock().unwrap();
                    if s.runtime
                        .as_ref()
                        .is_some_and(|r| Arc::ptr_eq(r, &selected))
                    {
                        s.runtime = None;
                        true
                    } else {
                        false
                    }
                };
                if detached {
                    if let Some(t) = &transport {
                        let _ = t.attach(None);
                    }
                }
            }
            // Match allSettled for transport work. Generation read leases cover
            // accepted API, worker and diagnostic futures without cancelling them.
            if let Some(t) = &transport {
                let _ = t.settled().await;
            }
            let result = settled(selected.generation.retire()).await;
            done.send_replace(Some(result));
            if let Some(core) = owner.upgrade() {
                core.state.lock().unwrap().retired.remove(&id);
            }
        });
    }
    async fn readiness(&self, connection: ivy_sdk::service::ServiceConnection) -> IvyResult<Json> {
        let lease = self.selected(
            connection.generation(),
            "ChatBridge storage is not attached to this connection.",
        )?;
        lease.verify_owner().await?;
        let mut details = {
            let s = self.state.lock().unwrap();
            String::from(if s.diagnostic_notice_error.is_some() {
                "Operational diagnostic notifications need reconciliation."
            } else if s.diagnostic.is_some() {
                "Saved chat is available; original work needs reconciliation."
            } else {
                "Authenticated chat storage and original work reconciliation are available."
            })
        };
        if let Some(t) = &self.transport {
            details.push_str(&format!(" WhatsApp: {}.", t.state().to_utf8()?));
        }
        settled(
            self.health
                .write(true, Some(connection.generation()), details),
        )
        .await?;
        let s = self.state.lock().unwrap();
        Ok(Json::object([
            ("ready", true.into()),
            (
                "diagnostics",
                Json::Array(
                    s.diagnostic
                        .iter()
                        .chain(s.diagnostic_notice_error.iter())
                        .cloned()
                        .collect(),
                ),
            ),
        ]))
    }
    fn on_state(self: &Arc<Self>, state: ServiceState) {
        let mut start = false;
        let mut offline = false;
        {
            let mut s = self.state.lock().unwrap();
            s.ready = state.status == "ready";
            if state.status == "ready" && state.generation != s.started_generation {
                s.started_generation = state.generation;
                start = true;
            } else if ["offline", "stopped", "connecting"].contains(&state.status) {
                s.runtime = None;
                s.diagnostic = None;
                s.started_generation = None;
                if let Some(timer) = s.timer.take() {
                    timer.abort();
                }
                offline = true;
            }
        }
        if start {
            self.kick();
            self.monitor_diagnostics();
        }
        if offline {
            if let Some(t) = &self.transport {
                let _ = t.attach(None);
            }
            self.health.write(
                false,
                state.generation,
                "Hive ChatBridge connection is not ready.",
            );
        }
    }
    fn now(&self) -> IvyResult<JsString> {
        date_to_iso(self.clock.utc_millis() as f64)
    }
    fn kick(self: &Arc<Self>) {
        let (selected, done) = {
            let mut s = self.state.lock().unwrap();
            if s.closed || !Self::ready(&s) {
                return;
            }
            if s.active.is_some() {
                s.wake_pending = true;
                return;
            }
            if let Some(timer) = s.timer.take() {
                timer.abort();
            }
            let selected = s.runtime.as_ref().unwrap().clone();
            let (done, receive) = watch::channel(None);
            s.active = Some(receive);
            (selected, done)
        };
        let core = self.clone();
        tokio::spawn(async move {
            let (result, worker) = selected.worker.drain().await;
            let mut completion = Ok(());
            let mut s = core.state.lock().unwrap();
            if s.runtime
                .as_ref()
                .is_some_and(|r| Arc::ptr_eq(r, &selected))
            {
                match result {
                    Ok(()) => s.diagnostic = None,
                    Err(error) => match core.now() {
                        Ok(at) => {
                            let code = Json::String(error.code_js());
                            let first = s
                                .diagnostic
                                .as_ref()
                                .filter(|v| v.get("code") == Some(&code))
                                .and_then(|v| v.get("firstObservedAt"))
                                .cloned()
                                .unwrap_or(Json::String(at.clone()));
                            s.diagnostic = Some(Json::object([
                                ("code", code), ("resource", Json::object([("workspaceId", core.config.admission.definition().required("workspaceId").unwrap().clone())])),
                                ("severity", "warning".into()), ("source", "chat-bridge".into()), ("firstObservedAt", first),
                                ("lastObservedAt", Json::String(at)), ("status", "current".into()),
                                ("message", "Chat work is waiting for its original storage or native outcome. Saved messages remain available.".into()),
                            ]));
                        }
                        Err(e) => completion = Err(e),
                    },
                }
            }
            s.active = None;
            if !s.closed && Self::ready(&s) {
                let delay = if s.wake_pending || worker.runnable {
                    Duration::ZERO
                } else if worker.idle {
                    Duration::from_secs(30)
                } else {
                    core.poll
                };
                let owner = Arc::downgrade(&core);
                s.timer = Some(tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    if let Some(core) = owner.upgrade() {
                        core.kick();
                    }
                }));
            }
            s.wake_pending = false;
            done.send_replace(Some(completion));
        });
    }
    fn monitor_diagnostics(self: &Arc<Self>) {
        let (selected, lease, done) = {
            let mut s = self.state.lock().unwrap();
            if s.closed || !Self::ready(&s) || s.diagnostic_work.is_some() {
                return;
            }
            let selected = s.runtime.as_ref().unwrap().clone();
            let Ok(lease) = selected.generation.accept() else {
                return;
            };
            let (done, receive) = watch::channel(None);
            s.diagnostic_work = Some(receive);
            (selected, lease, done)
        };
        let core = self.clone();
        tokio::spawn(async move {
            let result = lease
                .monitor_diagnostics_with_clock(core.clock.clone())
                .await;
            let aborted = lease.connection.signal().is_cancelled();
            drop(lease);
            let mut kick = false;
            let mut completion = Ok(());
            {
                let mut s = core.state.lock().unwrap();
                if s.runtime
                    .as_ref()
                    .is_some_and(|r| Arc::ptr_eq(r, &selected))
                {
                    match result {
                        Ok(()) => {
                            s.diagnostic_notice_error = None;
                            kick = true;
                        }
                        Err(error) if !aborted => match core.now() {
                            Ok(at) => {
                                let first = s
                                    .diagnostic_notice_error
                                    .as_ref()
                                    .and_then(|v| v.get("firstObservedAt"))
                                    .cloned()
                                    .unwrap_or(Json::String(at.clone()));
                                s.diagnostic_notice_error = Some(Json::object([
                                    ("code", "chat_diagnostics_unavailable".into()), ("source", "chat-bridge".into()),
                                    ("severity", "error".into()), ("status", "current".into()),
                                    ("resource", Json::object([("cause", Json::String(error.code_js()))])),
                                    ("firstObservedAt", first), ("lastObservedAt", Json::String(at)),
                                    ("message", "Operational notices could not be reconciled with their original Main receipts.".into()),
                                ]));
                            }
                            Err(e) => completion = Err(e),
                        },
                        Err(_) => {}
                    }
                }
                // Success calls kick before the original finally clears this slot.
            }
            if kick {
                core.kick();
            }
            core.state.lock().unwrap().diagnostic_work = None;
            done.send_replace(Some(completion));
        });
    }
    fn notification(self: &Arc<Self>, frame: &Json) {
        if let Some(t) = &self.transport {
            t.notification(frame);
        }
        let payload = frame.get("params").and_then(|v| v.get("payload"));
        let turn = payload
            .and_then(|v| v.get("params"))
            .and_then(|v| v.get("turn"))
            .and_then(|v| v.get("id"))
            .and_then(Json::as_js_string);
        let selected = self.current();
        if payload.and_then(|v| v.get("method")) == Some(&Json::from("turn/completed"))
            && turn.is_some()
            && selected.is_some_and(|r| r.worker.turn.lock().unwrap().as_ref() == turn)
        {
            self.kick();
        }
    }
}

struct Inner {
    service: ServiceClient,
    core: Arc<Core>,
    health: ChatHealth,
    closing: Mutex<Option<Completion>>,
}
#[derive(Clone)]
pub struct ChatService(Arc<Inner>);
impl ChatService {
    /// Compose the admitted desktop factory and WhatsApp controller from the
    /// same owners before the ordinary provider lifecycle can start.
    pub async fn start_with_whatsapp(
        config: &Json,
        generation: Arc<ChatGenerationConfig>,
        health: ChatHealth,
        parts: WhatsAppServiceParts,
    ) -> IvyResult<Self> {
        Self::validate_parts(config, &generation, true)?;
        let host_id = config
            .required("hostId")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?;
        let translator = Translator::new(Some(&generation.language));
        let factory = Arc::new(RuntimeAssembly::for_component(
            parts.journal.clone(),
            parts.config.clone(),
            parts.media.clone(),
            parts.admitted_artifact,
            parts.helper_layout,
            translator,
        )?);
        let transport = Arc::new(WhatsAppRuntime::new(RuntimeParts {
            config: parts.config,
            host_id,
            journal: parts.journal,
            socket: parts.socket,
            media: parts.media,
            factory,
            resources: parts.resources,
            translator,
        }));
        Self::start_admitted(
            config,
            generation,
            health,
            Some(transport),
            Arc::new(SystemClock),
        )
        .await
    }
    /// Compose already admitted Host/transport parts. The executable adapter must
    /// load them through their owners before calling this service constructor.
    pub async fn start(
        config: &Json,
        generation: Arc<ChatGenerationConfig>,
        health: ChatHealth,
        transport: Option<Arc<dyn ChatTransport>>,
    ) -> IvyResult<Self> {
        Self::with_clock(config, generation, health, transport, Arc::new(SystemClock)).await
    }
    pub async fn with_clock(
        config: &Json,
        generation: Arc<ChatGenerationConfig>,
        health: ChatHealth,
        transport: Option<Arc<dyn ChatTransport>>,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        Self::validate_parts(config, &generation, transport.is_some())?;
        Self::start_admitted(config, generation, health, transport, clock).await
    }
    fn validate_parts(
        config: &Json,
        generation: &ChatGenerationConfig,
        transport: bool,
    ) -> IvyResult<()> {
        let settings = service_settings(config)?;
        if canonical(settings.required("definition")?, MANAGEMENT_FRAME_BYTES)?
            != canonical(generation.admission.definition(), MANAGEMENT_FRAME_BYTES)?
            || config.get("serviceNodeId")
                != Some(&Json::String(generation.service_node_id.clone()))
            || settings
                .get("whatsappConfigPath")
                .is_some_and(|value| !value.is_null())
                != transport
        {
            return Err(IvyError::new(
                "chat_scope_mismatch",
                "Chat service parts must preserve their immutable configuration and configured transport.",
            ));
        }
        Ok(())
    }
    async fn start_admitted(
        config: &Json,
        generation: Arc<ChatGenerationConfig>,
        health: ChatHealth,
        transport: Option<Arc<dyn ChatTransport>>,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        let settings = config.required("settings")?;
        let (stopped, _) = watch::channel(false);
        let core = Arc::new(Core {
            config: generation,
            state: Mutex::new(State::default()),
            poll: Duration::from_millis(settings.required("pollMs")?.as_f64().unwrap() as u64),
            health: HealthWrites::new(health.write.clone()),
            transport,
            clock,
            stopped,
        });
        let credential = config.required("credential")?.as_str()?;
        let identity = Json::object([
            ("serviceNodeId", config.required("serviceNodeId")?.clone()),
            ("serviceName", "chat-bridge".into()),
            ("hostId", config.required("hostId")?.clone()),
            ("version", config.required("version")?.clone()),
            ("buildId", config.required("buildId")?.clone()),
            ("hiveProtocol", Json::Number(1.0)),
            ("instanceMode", "singleton".into()),
        ]);
        let reconcile = core.clone();
        let mut options = ServiceOptions::new(
            config.required("publicBaseUrl")?.as_str()?,
            Arc::new(move || {
                let value = credential.clone();
                Box::pin(async move { Ok(value) })
            }),
            identity,
            Arc::new(|| Box::pin(async { chat_registry() })),
            Arc::new(move |connection| {
                let core = reconcile.clone();
                Box::pin(async move { core.reconcile(connection).await })
            }),
        );
        options.heartbeat = Duration::from_millis(2000);
        let ready = core.clone();
        options.readiness = Some(Arc::new(move |connection| {
            let core = ready.clone();
            Box::pin(async move { core.readiness(connection).await })
        }));
        let observer = core.clone();
        options.on_state = Some(Arc::new(move |state| observer.on_state(state)));
        let notifications = core.clone();
        options.on_notification = Some(Arc::new(move |frame| notifications.notification(frame)));
        let native_service = core
            .config
            .admission
            .definition()
            .required("project")?
            .required("serviceNodeId")?
            .clone();
        options.notification_filters = Some(Arc::new(move || {
            let native_service = native_service.clone();
            Box::pin(async move {
                Ok(Json::Array(vec![Json::object([
                    ("namespace", "agent".into()),
                    ("name", "notification".into()),
                    ("version", "1.0.0".into()),
                    ("serviceNodeId", native_service),
                ])]))
            })
        }));
        for tool in chat_tools()? {
            let name = tool.required("name")?.as_str()?;
            let read_only = tool
                .get("annotations")
                .and_then(|v| v.get("readOnlyHint"))
                .and_then(Json::as_bool)
                .unwrap_or(false);
            let owner = core.clone();
            options.handlers.insert(
                format!("chat.{name}").into(),
                Arc::new(move |args, context| {
                    let lease = owner.selected(
                        context.generation,
                        "ChatBridge has no verified current Hive connection.",
                    );
                    let core = owner.clone();
                    let name = name.clone();
                    Box::pin(async move {
                        let result = {
                            let lease = lease?;
                            let files = core.transport.as_ref().and_then(|t| t.files());
                            lease.invoke_with_files(&name, &args, &context, files.as_ref()).await?
                        };
                        if let Some(t) = &core.transport {
                            if name == "createMain"
                                && result.get("phase") == Some(&Json::from("succeeded"))
                                && result.get("outcome").and_then(|v| v.get("action"))
                                    == Some(&Json::from("createMain"))
                                && result.get("request").and_then(|v| v.get("action"))
                                    == Some(&Json::from("createMain"))
                                && !result
                                    .get("request")
                                    .and_then(|v| v.get("resumeMain"))
                                    .is_some_and(|value| !value.is_null())
                            {
                                t.created_main(
                                    &result
                                        .required("outcome")?
                                        .required("binding")?
                                        .required("object")?
                                        .required("objectId")?
                                        .as_str()?,
                                )?;
                            }
                        }
                        if !read_only {
                            if let Some(t) = &core.transport {
                                t.changed();
                            }
                            core.kick();
                        }
                        Ok(result)
                    })
                }),
            );
        }
        let service = ServiceClient::new(options)?;
        let app = Self(Arc::new(Inner {
            service,
            core,
            health,
            closing: Mutex::new(None),
        }));
        settled(app.0.core.health.write(
            false,
            None,
            "Connecting ChatBridge and verifying its exact workspace contracts.",
        ))
        .await?;
        app.start_control();
        app.0.service.start();
        app.start_diagnostics();
        if let Some(t) = &app.0.core.transport {
            t.start();
        }
        Ok(app)
    }
    pub fn service(&self) -> &ServiceClient {
        &self.0.service
    }
    pub fn runtime(&self) -> Option<Arc<ChatGeneration>> {
        self.0.core.current().map(|r| r.generation.clone())
    }
    /// Wait for an identity-matched Host control request or another close caller,
    /// including completion of pending work and the final health write.
    pub async fn wait(&self) -> IvyResult<()> {
        let mut stopped = self.0.core.stopped.subscribe();
        stopped
            .wait_for(|stopped| *stopped)
            .await
            .map_err(|_| IvyError::internal())?;
        self.close().await
    }
    fn start_control(&self) {
        let owner = Arc::downgrade(&self.0);
        let mut stopped = self.0.core.stopped.subscribe();
        let checking = Arc::new(AtomicBool::new(false));
        tokio::spawn(async move {
            let mut timer = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_millis(500),
                Duration::from_millis(500),
            );
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {biased;
                    _ = stopped.changed() => break,
                    _ = timer.tick() => {
                        let Some(app) = owner.upgrade().map(ChatService) else {break;};
                        if app.0.core.state.lock().unwrap().closed {break;}
                        // Keep interval ticks independent of an accepted slow
                        // read, like the original checkingControl guard. A late
                        // read neither overlaps nor creates an immediate retry.
                        if checking.swap(true, Ordering::AcqRel) {continue;}
                        let checking = checking.clone();
                        tokio::spawn(async move {
                            if app.0.health.control.as_ref()().await == Ok(true) {let _ = app.close().await;}
                            checking.store(false, Ordering::Release);
                        });
                    }
                }
            }
        });
    }
    fn start_diagnostics(&self) {
        let owner = Arc::downgrade(&self.0.core);
        let mut stopped = self.0.core.stopped.subscribe();
        tokio::spawn(async move {
            let delay = Duration::from_millis(DIAGNOSTIC_NOTIFICATION_POLL_MS as u64);
            let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + delay, delay);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {biased;
                    _ = stopped.changed() => break,
                    _ = timer.tick() => {let Some(core) = owner.upgrade() else {break;}; core.monitor_diagnostics();}
                }
            }
        });
    }
    pub fn close(&self) -> ServiceFuture<()> {
        let mut closing = self.0.closing.lock().unwrap();
        if let Some(value) = &*closing {
            let value = value.clone();
            return Box::pin(async move { settled(value).await });
        }
        {
            let mut s = self.0.core.state.lock().unwrap();
            s.closed = true;
            if let Some(timer) = s.timer.take() {
                timer.abort();
            }
        }
        self.0.core.stopped.send_replace(true);
        let (done, receive) = watch::channel(None);
        *closing = Some(receive.clone());
        let app = self.clone();
        tokio::spawn(async move {
            let result: IvyResult<()> = async {
                let transport_error = if let Some(t) = &app.0.core.transport {
                    t.close().await.err()
                } else {
                    None
                };
                // Preserve the original finally ordering and failure result. In
                // particular a transport close error still skips final health.
                app.0.service.stop().await;
                let active = app.0.core.state.lock().unwrap().active.clone();
                if let Some(active) = active {
                    settled(active).await?;
                }
                let diagnostic = app.0.core.state.lock().unwrap().diagnostic_work.clone();
                if let Some(diagnostic) = diagnostic {
                    settled(diagnostic).await?;
                }
                let retired: Vec<_> = app
                    .0
                    .core
                    .state
                    .lock()
                    .unwrap()
                    .retired
                    .values()
                    .cloned()
                    .collect();
                let mut waits = JoinSet::new();
                for work in retired {
                    waits.spawn(settled(work));
                }
                while let Some(result) = waits.join_next().await {
                    if let Err(error) = result.map_err(|_| IvyError::internal()).and_then(|v| v) {
                        waits.detach_all();
                        return Err(error);
                    }
                }
                if let Some(error) = transport_error {
                    return Err(error);
                }
                let _ = settled(app.0.core.health.pending()).await;
                (app.0.health.write)(HealthObservation {
                    ready: false,
                    generation: None,
                    details: "ChatBridge stopped.".into(),
                })
                .await
            }
            .await;
            done.send_replace(Some(result));
        });
        Box::pin(async move { settled(receive).await })
    }
}
