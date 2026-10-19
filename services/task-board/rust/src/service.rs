//! SDK lifecycle binding for the local TaskBoard journal owner. The supplied
//! factory constructs the admitted engine graph in its own borrowed scope.
use crate::{
    generation::{Generation, GenerationClock, GenerationControl, GenerationRuntime, generation},
    journal_store::Contracts,
    service_bridge::LocalHandler,
};
use futures_util::FutureExt;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    service::{
        Dependency, InvocationContext, ServiceClient, ServiceConnection, ServiceFuture,
        ServiceOptions, ServiceState, ToolHandler,
    },
};
use std::{
    future::Future,
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};

pub struct ServiceSetup {
    pub component_id: JsString,
    pub public_base_url: String,
    pub credential: String,
    pub service_node_id: JsString,
    pub host_id: Option<JsString>,
    pub version: JsString,
    pub build_id: JsString,
    pub settings: Json,
    pub registry: Dependency<Json>,
    /// Exact names from the same owner registry/operation catalog.
    pub tools: Vec<JsString>,
}
#[allow(async_fn_in_trait)]
pub trait GenerationFactory {
    /// Keep all journal/engine references and their executor alive until this
    /// launch's activation.run returns. Never detach the local owner future.
    async fn run(&self, launch: GenerationLaunch) -> IvyResult<()>;
}
struct Current {
    identity: Arc<()>,
    control: GenerationControl,
    generation: u64,
}
struct Pending {
    identity: Arc<()>,
    generation: u64,
}
#[derive(Default)]
struct State {
    closed: bool,
    current: Option<Current>,
    pending: Option<Pending>,
    retirement_error: Option<IvyError>,
}
struct Reply(Mutex<Option<oneshot::Sender<IvyResult<()>>>>);
impl Reply {
    fn finish(&self, result: IvyResult<()>) {
        if let Some(reply) = self.0.lock().unwrap().take() {
            let _ = reply.send(result);
        }
    }
}
impl Drop for Reply {
    fn drop(&mut self) {
        if let Some(reply) = self.0.get_mut().unwrap().take() {
            let _ = reply.send(Err(interrupted()));
        }
    }
}
struct Attach {
    connection: ServiceConnection,
    identity: Arc<()>,
    reply: Arc<Reply>,
}
enum Command {
    Attach(Attach),
    Close,
}
#[derive(Clone)]
pub struct ServiceControl {
    state: Arc<Mutex<State>>,
    sender: mpsc::UnboundedSender<Command>,
    finished: watch::Receiver<Option<IvyResult<()>>>,
}
pub struct ServiceOwner {
    state: Arc<Mutex<State>>,
    receiver: mpsc::UnboundedReceiver<Command>,
    finished: watch::Sender<Option<IvyResult<()>>>,
    settings: Arc<Json>,
    interval_ms: f64,
}
pub struct TaskBoardService {
    pub client: ServiceClient,
    pub control: ServiceControl,
}
pub struct GenerationLaunch {
    pub connection: ServiceConnection,
    pub control: GenerationControl,
    pub generation: Generation,
    pub activation: Activation,
}
pub struct Activation {
    state: Arc<Mutex<State>>,
    identity: Arc<()>,
    reply: Arc<Reply>,
    connection: ServiceConnection,
    settings: Arc<Json>,
    installed: Arc<AtomicBool>,
}
fn not_ready() -> IvyError {
    IvyError::new(
        "service_not_ready",
        "TaskBoard has no reconciled current connection generation.",
    )
}
fn wrong_generation() -> IvyError {
    IvyError::new(
        "service_not_ready",
        "Workflow state is not attached to this generation.",
    )
}
fn interrupted() -> IvyError {
    IvyError::new("internal_error", "Service invocation failed.").with_outcome(Outcome::Unknown)
}
fn matches(identity: &Arc<()>, other: &Arc<()>) -> bool {
    Arc::ptr_eq(identity, other)
}
fn retire(state: &mut State) {
    if let Some(current) = state.current.take() {
        drop(current.control.retire());
    }
}
impl ServiceControl {
    /// Retire the previous slot eagerly. The owner serializes actual graph
    /// construction after the previous graph and its journal have drained.
    pub fn reconcile(&self, connection: ServiceConnection) -> ServiceFuture<()> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Box::pin(async { Err(not_ready()) });
        }
        if let Some(error) = state.retirement_error.clone() {
            return Box::pin(async { Err(error) });
        }
        retire(&mut state);
        let identity = Arc::new(());
        state.pending = Some(Pending {
            identity: identity.clone(),
            generation: connection.generation(),
        });
        let (sender, receiver) = oneshot::channel();
        let reply = Arc::new(Reply(Mutex::new(Some(sender))));
        let sent = self.sender.send(Command::Attach(Attach {
            connection,
            identity,
            reply,
        }));
        Box::pin(async move {
            sent.map_err(|_| interrupted())?;
            receiver.await.map_err(|_| interrupted())?
        })
    }
    pub fn handler(&self, tool: &str) -> ToolHandler {
        let selected = self.clone();
        let tool = tool.to_owned();
        Arc::new(move |args, context| {
            let state = selected.state.lock().unwrap();
            match state
                .current
                .as_ref()
                .filter(|c| !state.closed && c.generation == context.generation)
            {
                Some(current) => current.control.handler(&tool)(args, context),
                None => Box::pin(async { Err(not_ready()) }),
            }
        })
    }
    pub fn readiness(&self, connection: &ServiceConnection) -> ServiceFuture<Json> {
        let state = self.state.lock().unwrap();
        match state
            .current
            .as_ref()
            .filter(|c| !state.closed && c.generation == connection.generation())
        {
            Some(current) => current.control.readiness(InvocationContext {
                caller_principal_id: JsString::default(),
                operation_id: None,
                generation: connection.generation(),
                signal: connection.signal(),
            }),
            None => Box::pin(async { Err(wrong_generation()) }),
        }
    }
    pub fn notification(&self, value: &Json) {
        let state = self.state.lock().unwrap();
        if let Some(current) = state.current.as_ref().filter(|_| !state.closed) {
            current.control.notification(value);
        }
    }
    pub fn state(&self, value: &ServiceState) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        if ["offline", "stopped", "connecting"].contains(&value.status) {
            if value
                .generation
                .is_none_or(|g| state.pending.as_ref().is_some_and(|p| p.generation == g))
            {
                state.pending = None;
            }
            if value
                .generation
                .is_none_or(|g| state.current.as_ref().is_some_and(|c| c.generation == g))
            {
                retire(&mut state);
            }
        } else if let Some(current) = &state.current {
            current.control.state(value);
        }
    }
    pub fn close(&self) -> ServiceFuture<()> {
        let mut state = self.state.lock().unwrap();
        if !state.closed {
            state.closed = true;
            state.pending = None;
            retire(&mut state);
            let _ = self.sender.send(Command::Close);
        }
        let mut finished = self.finished.clone();
        Box::pin(async move {
            loop {
                if let Some(result) = finished.borrow_and_update().clone() {
                    return result;
                }
                finished.changed().await.map_err(|_| interrupted())?;
            }
        })
    }
}
impl Activation {
    pub fn settings(&self) -> &Json {
        &self.settings
    }
    /// Verify the current authenticated owner and optional root before exposing
    /// any slot or acknowledging the SDK reconcile callback.
    pub async fn run<H: LocalHandler, R: GenerationRuntime, C: GenerationClock>(
        &self,
        control: &GenerationControl,
        owner: Generation,
        handler: &H,
        runtime: &R,
        clock: &C,
        drain: impl Future<Output = IvyResult<()>>,
    ) -> IvyResult<()> {
        runtime.verify_owner().await?;
        if let Some(root) = self
            .settings
            .get("rootObjectId")
            .and_then(Json::as_js_string)
        {
            self.connection
                .request(
                    "objects.stat",
                    Json::object([("objectId", Json::String(root.clone()))]),
                    RequestOptions::default(),
                )
                .await?;
        }
        self.connection.check_open()?;
        {
            let mut state = self.state.lock().unwrap();
            if state.closed
                || !state
                    .pending
                    .as_ref()
                    .is_some_and(|p| matches(&p.identity, &self.identity))
            {
                return Err(not_ready());
            }
            state.pending = None;
            state.current = Some(Current {
                identity: self.identity.clone(),
                control: control.clone(),
                generation: self.connection.generation(),
            });
            self.installed.store(true, Ordering::Release);
        }
        self.reply.finish(Ok(()));
        let result = owner.run(handler, runtime, clock, drain).await;
        let mut state = self.state.lock().unwrap();
        if state
            .current
            .as_ref()
            .is_some_and(|c| matches(&c.identity, &self.identity))
        {
            retire(&mut state);
        }
        result
    }
}
impl Drop for ServiceOwner {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.pending = None;
        retire(&mut state);
        if self.finished.borrow().is_none() {
            self.finished.send_replace(Some(Err(interrupted())));
        }
    }
}
impl ServiceOwner {
    /// Drive this from the journal owner's local root future. The factory's
    /// borrowed graph cannot escape into the SDK's Send callback tasks.
    pub async fn run<F: GenerationFactory>(mut self, factory: &F) -> IvyResult<()> {
        while let Some(command) = self.receiver.recv().await {
            let attach = match command {
                Command::Close => break,
                Command::Attach(attach) => attach,
            };
            let allowed = {
                let state = self.state.lock().unwrap();
                if let Some(error) = &state.retirement_error {
                    Err(error.clone())
                } else if state.closed
                    || !state
                        .pending
                        .as_ref()
                        .is_some_and(|p| matches(&p.identity, &attach.identity))
                {
                    Err(not_ready())
                } else {
                    attach.connection.check_open()
                }
            };
            if let Err(error) = allowed {
                attach.reply.finish(Err(error));
                continue;
            }
            let (control, owner) = generation(attach.connection.generation(), self.interval_ms);
            let installed = Arc::new(AtomicBool::new(false));
            let activation = Activation {
                state: self.state.clone(),
                identity: attach.identity.clone(),
                reply: attach.reply.clone(),
                connection: attach.connection.clone(),
                settings: self.settings.clone(),
                installed: installed.clone(),
            };
            let launch = GenerationLaunch {
                connection: attach.connection.clone(),
                control,
                generation: owner,
                activation,
            };
            let result = AssertUnwindSafe(factory.run(launch))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(interrupted()));
            attach
                .reply
                .finish(Err(result.clone().err().unwrap_or_else(not_ready)));
            let mut state = self.state.lock().unwrap();
            if state
                .pending
                .as_ref()
                .is_some_and(|p| matches(&p.identity, &attach.identity))
            {
                state.pending = None;
            }
            if state
                .current
                .as_ref()
                .is_some_and(|c| matches(&c.identity, &attach.identity))
            {
                retire(&mut state);
            }
            if let Err(error) = result {
                if installed.load(Ordering::Acquire) {
                    state.retirement_error.get_or_insert(error.clone());
                }
                attach.connection.close_with(error);
            }
        }
        let result = self
            .state
            .lock()
            .unwrap()
            .retirement_error
            .clone()
            .map_or(Ok(()), Err);
        self.finished.send_replace(Some(result.clone()));
        result
    }
}
pub fn service<P: Contracts>(
    setup: ServiceSetup,
    contracts: &P,
) -> IvyResult<(TaskBoardService, ServiceOwner)> {
    contracts.validate_definition("Settings", &setup.settings)?;
    if setup.component_id != JsString::from("task-board") || setup.credential.is_empty() {
        return Err(IvyError::new(
            "invalid_arguments",
            "TaskBoard requires its own component identity and Hive credential.",
        ));
    }
    let interval_ms = setup
        .settings
        .required("scheduler")?
        .required("intervalMs")?
        .as_f64()
        .ok_or_else(IvyError::internal)?;
    let settings = Arc::new(setup.settings);
    let state = Arc::new(Mutex::new(State::default()));
    let (sender, receiver) = mpsc::unbounded_channel();
    let (finished, observer) = watch::channel(None);
    let control = ServiceControl {
        state: state.clone(),
        sender,
        finished: observer,
    };
    let selected = control.clone();
    let credential = setup.credential;
    let identity = Json::object([
        ("serviceNodeId", Json::String(setup.service_node_id)),
        ("serviceName", "task-board".into()),
        ("instanceMode", "singleton".into()),
        (
            "hostId",
            setup.host_id.map(Json::String).unwrap_or(Json::Null),
        ),
        ("version", Json::String(setup.version)),
        ("buildId", Json::String(setup.build_id)),
        ("hiveProtocol", Json::Number(1.0)),
    ]);
    let mut options = ServiceOptions::new(
        setup.public_base_url,
        Arc::new(move || {
            let value = credential.clone();
            Box::pin(async move { Ok(value) })
        }),
        identity,
        setup.registry,
        Arc::new(move |connection| selected.reconcile(connection)),
    );
    for name in setup.tools {
        let tool = name
            .to_utf8()
            .map_err(|_| IvyError::new("invalid_arguments", "Invalid TaskBoard tool name."))?;
        options
            .handlers
            .insert(format!("task-board.{tool}").into(), control.handler(&tool));
    }
    let selected = control.clone();
    options.readiness = Some(Arc::new(move |connection| selected.readiness(&connection)));
    let selected = control.clone();
    options.on_state = Some(Arc::new(move |state| selected.state(&state)));
    let selected = control.clone();
    options.on_notification = Some(Arc::new(move |value| selected.notification(value)));
    options.notification_filters = Some(Arc::new(|| {
        Box::pin(async {
            Ok(Json::Array(vec![Json::object([
                ("namespace", "agent".into()),
                ("name", "notification".into()),
                ("version", "1.0.0".into()),
            ])]))
        })
    }));
    options.heartbeat = Duration::from_millis(2000);
    let client = ServiceClient::new(options)?;
    Ok((
        TaskBoardService { client, control },
        ServiceOwner {
            state,
            receiver,
            finished,
            settings,
            interval_ms,
        },
    ))
}
impl TaskBoardService {
    pub fn start(&self) {
        self.client.start();
    }
    /// Both SDK shutdown and the local owner's drain survive a lost observer.
    pub fn close(&self) -> ServiceFuture<()> {
        let closed = self.control.close();
        let client = self.client.clone();
        let work = tokio::spawn(async move {
            client.stop().await;
            closed.await
        });
        Box::pin(async move { work.await.map_err(|_| interrupted())? })
    }
}
