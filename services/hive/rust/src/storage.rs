//! One SQLite owner on a dedicated thread. Commands move through a bounded
//! channel; neither payloads nor results cross a serialization boundary.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    runtime::Handle,
    sync::{mpsc, oneshot, watch},
};

pub const MAX_PENDING: usize = 512;
pub const MAX_QUEUED_BYTES: usize = 64 * 1024 * 1024;
pub type Reply<T = IvyJson> = Pin<Box<dyn Future<Output = IvyResult<T>> + Send>>;
pub type BackupTask = Pin<Box<dyn Future<Output = IvyResult<IvyJson>>>>;
pub type Initializer = Box<dyn FnOnce(IvyJson) -> IvyResult<Box<dyn StorageService>> + Send>;

/// Dynamic RPC data and native internal commands move to the DB thread.
#[derive(Debug)]
pub enum Payload {
    Json(IvyJson),
    Execute {
        context: IvyJson,
        request: IvyJson,
    },
    ConfigureCredentials(Arc<Vec<crate::store::Credential>>),
    Authenticate(crate::store::AuthContext),
    CreateSession(JsString),
    ResolveSession(JsString),
    EndSession(crate::store::AuthContext),
}
#[cfg(test)]
#[path = "../../../../tests/rust/hive_storage_payload_memory.rs"]
mod payload_tests;
impl From<IvyJson> for Payload {
    fn from(value: IvyJson) -> Self {
        Self::Json(value)
    }
}
impl Payload {
    fn retained_bytes(&self) -> usize {
        let json_heap = |value: &IvyJson| {
            value.retained_bytes().saturating_sub(std::mem::size_of::<IvyJson>())
        };
        match self {
            Self::Json(value) => json_heap(value),
            Self::Execute { context, request } => {
                json_heap(context).saturating_add(json_heap(request))
            }
            Self::Authenticate(context) | Self::EndSession(context) => context
                .credential_digest.0.capacity().saturating_mul(2)
                .saturating_add(context.session_digest.as_ref().map_or(0, |value| {
                    value.0.capacity().saturating_mul(2)
                })),
            Self::CreateSession(value) | Self::ResolveSession(value) => {
                value.0.capacity().saturating_mul(2)
            }
            Self::ConfigureCredentials(values) => {
                // The effects hold another reference to this same allocation.
                let mut bytes = 2 * std::mem::size_of::<usize>()
                    + std::mem::size_of::<Vec<crate::store::Credential>>();
                bytes = bytes.saturating_add(
                    values.capacity().saturating_mul(std::mem::size_of::<crate::store::Credential>()),
                );
                for value in values.iter() {
                    bytes = bytes.saturating_add(value.principal_id.0.capacity().saturating_mul(2))
                        .saturating_add(value.digest.0.capacity().saturating_mul(2));
                }
                bytes
            }
        }
    }
}

/// Network state that must be checked or applied between consecutive DB actions.
/// These synchronous effects run on the DB owner with the network runtime entered.
pub trait RequestEffects: Send {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of_val(self)
    }
    fn prepare(&self, _payload: &mut Payload) -> IvyResult<()> {
        Ok(())
    }
    fn complete(self: Box<Self>, result: IvyResult<IvyJson>) -> IvyResult<IvyJson> {
        result
    }
}
pub struct Request {
    payload: Payload,
    effects: Option<Box<dyn RequestEffects>>,
}
impl Request {
    pub fn new(payload: impl Into<Payload>) -> Self {
        Self { payload: payload.into(), effects: None }
    }
    pub fn with_effects(payload: impl Into<Payload>, effects: impl RequestEffects + 'static) -> Self {
        Self { payload: payload.into(), effects: Some(Box::new(effects)) }
    }
    pub fn prepare(&mut self) -> IvyResult<()> {
        if let Some(effects) = &self.effects {
            effects.prepare(&mut self.payload)?;
        }
        Ok(())
    }
    pub fn take_payload(&mut self) -> Payload {
        std::mem::replace(&mut self.payload, Payload::Json(IvyJson::Null))
    }
    pub fn complete(self, result: IvyResult<IvyJson>) -> IvyResult<IvyJson> {
        match self.effects {
            Some(effects) => effects.complete(result),
            None => result,
        }
    }
    fn retained_bytes(&self) -> usize {
        self.payload.retained_bytes().saturating_add(
            self.effects.as_ref().map_or(0, |effects| effects.retained_bytes()),
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RequestOptions {
    pub timeout_ms: u64,
    /// Preserve time already spent waiting for network dispatch.
    pub deadline: Option<Instant>,
    pub terminate_on_timeout: bool,
    pub reserved_bytes: u64,
}
impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            timeout_ms: 30_000,
            deadline: None,
            terminate_on_timeout: true,
            reserved_bytes: 4096,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingUpdate {
    pub node: IvyJson,
    pub generation: Option<f64>,
    pub catalog: Option<Arc<crate::registry_catalog::Catalog>>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Notification {
    Routing(RoutingUpdate),
    EventsAvailable(IvyJson),
    Changes(Vec<JsString>),
}
#[derive(Debug, Clone)]
pub enum Event {
    Notification(Notification),
    Failure(IvyError),
}
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;
pub enum Completion {
    Reply(IvyResult<IvyJson>),
    Failed(IvyError),
}
pub struct Dispatch {
    pub notifications: Vec<Notification>,
    pub completion: Completion,
}
/// The connection and its non-Send domains are created and destroyed on the DB
/// thread. Backups yield between copy batches on that same connection.
pub trait StorageService {
    fn handle(&mut self, payload: Payload) -> IvyResult<IvyJson>;
    fn backup(&mut self, destination: JsString) -> IvyResult<BackupTask>;
    fn notifications(&mut self) -> Vec<Notification>;
    fn close(&mut self) -> IvyResult<()>;
    fn dispatch(&mut self, payload: Payload) -> Dispatch {
        let result = self.handle(payload);
        Dispatch {
            notifications: self.notifications(),
            completion: Completion::Reply(result),
        }
    }
}

fn unavailable() -> IvyError {
    IvyError::new(
        "storage_unavailable",
        "Hive storage is unavailable; restart Hive.",
    )
}
fn expired(outcome: Outcome) -> IvyError {
    IvyError::new(
        "deadline_exceeded",
        "Hive storage deadline expired; reconcile operation identity.",
    )
    .with_outcome(outcome)
}
#[derive(Clone)]
pub struct LifecycleWaiter(watch::Receiver<Option<IvyResult<()>>>);
impl LifecycleWaiter {
    pub async fn wait(&self) -> IvyResult<()> {
        let mut receiver = self.0.clone();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver.changed().await.map_err(|_| unavailable())?;
        }
    }
}
#[derive(Default)]
struct Usage {
    count: usize,
    bytes: usize,
}
struct Reservation {
    usage: Arc<Mutex<Usage>>,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut usage = self.usage.lock().unwrap();
        usage.count -= 1;
        usage.bytes -= self.bytes;
    }
}
enum Command {
    Action(Request),
    Backup(JsString),
}
enum WorkPhase {
    Queued,
    Running,
    Expired,
}
struct Work {
    command: Command,
    deadline: Option<Instant>,
    phase: Arc<Mutex<WorkPhase>>,
    reply: oneshot::Sender<IvyResult<IvyJson>>,
    _reservation: Reservation,
}
struct Startup {
    options: IvyJson,
    initialize: Initializer,
    receiver: mpsc::Receiver<Work>,
}
struct State {
    sender: Option<mpsc::Sender<Work>>,
    startup: Option<Startup>,
}
struct Shared {
    state: Mutex<State>,
    usage: Arc<Mutex<Usage>>,
    ready: watch::Sender<Option<IvyResult<()>>>,
    closed: watch::Sender<Option<IvyResult<()>>>,
    failure: Arc<Mutex<Option<IvyError>>>,
    events: EventSink,
}
#[derive(Clone)]
pub struct StorageClient(Arc<Shared>);
impl StorageClient {
    pub fn new(options: IvyJson, initialize: Initializer, events: EventSink) -> Self {
        let (sender, receiver) = mpsc::channel(MAX_PENDING);
        Self(Arc::new(Shared {
            state: Mutex::new(State {
                sender: Some(sender),
                startup: Some(Startup {
                    options,
                    initialize,
                    receiver,
                }),
            }),
            usage: Arc::new(Mutex::new(Usage::default())),
            ready: watch::channel(None).0,
            closed: watch::channel(None).0,
            failure: Arc::new(Mutex::new(None)),
            events,
        }))
    }
    pub fn pending_count(&self) -> usize {
        self.0.usage.lock().unwrap().count
    }
    pub fn queued_bytes(&self) -> usize {
        self.0.usage.lock().unwrap().bytes
    }
    pub fn is_ready(&self) -> bool {
        matches!(*self.0.ready.borrow(), Some(Ok(())))
            && self.0.state.lock().unwrap().sender.is_some()
            && self.failure().is_none()
    }
    pub fn failure(&self) -> Option<IvyError> {
        self.0.failure.lock().unwrap().clone()
    }
    pub fn start(&self) -> LifecycleWaiter {
        if self.0.state.lock().unwrap().sender.is_none() || self.failure().is_some() {
            return LifecycleWaiter(watch::channel(Some(Err(unavailable()))).1);
        }
        let waiter = LifecycleWaiter(self.0.ready.subscribe());
        let Some(startup) = self.0.state.lock().unwrap().startup.take() else {
            return waiter;
        };
        let ready = self.0.ready.clone();
        let closed = self.0.closed.clone();
        let failure = self.0.failure.clone();
        let events = self.0.events.clone();
        let network = Handle::current();
        let thread = std::thread::Builder::new()
            .name("hive-storage".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| unavailable())?;
                    runtime.block_on(serve(startup, &ready, &events, &network))
                }))
                .unwrap_or_else(|_| Err(unavailable().with_outcome(Outcome::Unknown)));
                if ready.borrow().is_none() {
                    ready.send_replace(Some(outcome.clone()));
                }
                if let Err(error) = &outcome {
                    failure.lock().unwrap().get_or_insert_with(|| error.clone());
                    let _network = network.enter();
                    events(Event::Failure(error.clone()));
                }
                // serve has dropped the connection and all queued reservations.
                closed.send_replace(Some(outcome));
            });
        if thread.is_err() {
            let error = unavailable();
            self.0.ready.send_replace(Some(Err(error.clone())));
            self.0.closed.send_replace(Some(Err(error.clone())));
            self.0.failure.lock().unwrap().replace(error);
        }
        waiter
    }
    /// Admission is eager. Dropping the observer cannot cancel a posted action;
    /// its payload remains charged until the DB owner consumes it.
    pub fn request(&self, payload: impl Into<Payload>, options: RequestOptions) -> Reply {
        self.dispatch(Request::new(payload), options)
    }
    pub fn dispatch(&self, mut request: Request, options: RequestOptions) -> Reply {
        let bytes = request
            .retained_bytes()
            .saturating_add(std::mem::size_of::<Work>())
            .saturating_add(std::mem::size_of::<Mutex<WorkPhase>>() + 2 * std::mem::size_of::<usize>())
            .max(usize::try_from(options.reserved_bytes).unwrap_or(usize::MAX));
        let result = (|| {
            let state = self.0.state.lock().unwrap();
            if !matches!(*self.0.ready.borrow(), Some(Ok(()))) || self.failure().is_some() {
                return Err(unavailable());
            }
            let sender = state.sender.as_ref().ok_or_else(unavailable)?;
            let mut usage = self.0.usage.lock().unwrap();
            if usage.count >= MAX_PENDING || bytes > MAX_QUEUED_BYTES.saturating_sub(usage.bytes) {
                return Err(IvyError::new(
                    "limit_exceeded",
                    "Hive storage request queue is full.",
                ));
            }
            usage.count += 1;
            usage.bytes += bytes;
            drop(usage);
            let reservation = Reservation {
                usage: self.0.usage.clone(),
                bytes,
            };
            let deadline = options.deadline.or_else(|| {
                Instant::now().checked_add(Duration::from_millis(options.timeout_ms))
            });
            let phase = Arc::new(Mutex::new(WorkPhase::Queued));
            let command = match &mut request.payload {
                Payload::Json(payload) if payload.get("action") == Some(&"backup".into()) => {
                    if request.effects.is_some() {
                        return Err(IvyError::internal());
                    }
                    Command::Backup(
                        payload
                            .remove("destination")
                            .and_then(|v| v.as_js_string().cloned())
                            .ok_or_else(IvyError::internal)?,
                    )
                }
                _ => Command::Action(request),
            };
            let (reply, receiver) = oneshot::channel();
            sender
                .try_send(Work {
                    command,
                    deadline,
                    phase: phase.clone(),
                    reply,
                    _reservation: reservation,
                })
                .map_err(|_| unavailable())?;
            Ok((receiver, deadline, phase))
        })();
        let failure = self.0.failure.clone();
        Box::pin(async move {
            let (receiver, deadline, phase) = result?;
            let observed = match deadline {
                Some(at) => {
                    tokio::time::timeout_at(tokio::time::Instant::from_std(at), receiver).await
                }
                None => Ok(receiver.await),
            };
            match observed {
                Ok(result) => {
                    result.unwrap_or_else(|_| Err(unavailable().with_outcome(Outcome::Unknown)))
                }
                Err(_) => {
                    // The DB owner and timeout agree whether work has started.
                    // Expiring queued work guarantees it can never execute later.
                    let outcome = {
                        let mut phase = phase.lock().unwrap();
                        match *phase {
                            WorkPhase::Running => Outcome::Unknown,
                            WorkPhase::Queued | WorkPhase::Expired => {
                                *phase = WorkPhase::Expired;
                                Outcome::NotExecuted
                            }
                        }
                    };
                    let error = expired(outcome);
                    if options.terminate_on_timeout && outcome == Outcome::Unknown {
                        failure.lock().unwrap().get_or_insert_with(|| error.clone());
                    }
                    Err(error)
                }
            }
        })
    }
    /// Closing the only sender drains accepted work and the active backup, then
    /// closes SQLite. There is no replacement owner or in-process restart.
    pub fn close(&self) -> LifecycleWaiter {
        let mut state = self.0.state.lock().unwrap();
        state.sender.take();
        if state.startup.take().is_some() {
            self.0.ready.send_replace(Some(Err(unavailable())));
            self.0.closed.send_replace(Some(Ok(())));
        }
        LifecycleWaiter(self.0.closed.subscribe())
    }
}
fn publish(events: &EventSink, network: &Handle, notifications: Vec<Notification>) {
    let _network = network.enter();
    for value in notifications {
        events(Event::Notification(value));
    }
}
struct Backup {
    task: BackupTask,
    work: Work,
}
async fn serve(
    mut startup: Startup,
    ready: &watch::Sender<Option<IvyResult<()>>>,
    events: &EventSink,
    network: &Handle,
) -> IvyResult<()> {
    let mut service = (startup.initialize)(startup.options)?;
    ready.send_replace(Some(Ok(())));
    let mut backup: Option<Backup> = None;
    loop {
        let work = if let Some(running) = backup.as_mut() {
            tokio::select! {
                biased;
                result = &mut running.task => {
                    let completed = backup.take().unwrap();
                    let _ = completed.work.reply.send(result);
                    continue;
                }
                work = startup.receiver.recv() => work,
            }
        } else {
            startup.receiver.recv().await
        };
        let Some(work) = work else {
            break;
        };
        let expired_before_start = {
            let mut phase = work.phase.lock().unwrap();
            if matches!(*phase, WorkPhase::Expired)
                || work.deadline.is_some_and(|deadline| Instant::now() >= deadline)
            {
                *phase = WorkPhase::Expired;
                true
            } else {
                *phase = WorkPhase::Running;
                false
            }
        };
        if expired_before_start {
            let _ = work.reply.send(Err(expired(Outcome::NotExecuted)));
            continue;
        }
        match &work.command {
            Command::Backup(destination) => {
                let result = if backup.is_some() {
                    Err(IvyError::new(
                        "limit_exceeded",
                        "A storage backup is already active.",
                    ))
                } else {
                    service.backup(destination.clone())
                };
                match result {
                    Ok(task) => backup = Some(Backup { task, work }),
                    Err(error) => {
                        let _ = work.reply.send(Err(error));
                    }
                }
            }
            Command::Action(_) => {
                let Work {
                    command,
                    reply,
                    _reservation,
                    ..
                } = work;
                let Command::Action(mut request) = command else {
                    unreachable!()
                };
                let prepared = {
                    let _network = network.enter();
                    request.prepare()
                };
                if let Err(error) = prepared {
                    let _ = reply.send(Err(error));
                    continue;
                }
                let dispatch = service.dispatch(request.take_payload());
                publish(events, network, dispatch.notifications);
                match dispatch.completion {
                    Completion::Reply(result) => {
                        let result = {
                            let _network = network.enter();
                            request.complete(result)
                        };
                        let _ = reply.send(result);
                    }
                    Completion::Failed(error) => {
                        let _ = reply.send(Err(error.clone().with_outcome(Outcome::Unknown)));
                        return Err(error);
                    }
                }
            }
        }
    }
    if let Some(backup) = backup {
        let _ = backup.work.reply.send(backup.task.await);
    }
    service.close()
}
