//! Owned storage-child I/O. Policy is kept in WorkerClient; Host owns OS launch,
//! process trees and definite retirement. No posted request is ever replayed.
use crate::{
    worker_client::{
        Effects, Generation, Lifecycle, MAX_PENDING, RequestId, RequestOptions, STARTUP_TIMEOUT_MS,
        WorkerClient,
    },
    worker_protocol::{FrameReader, Message, Notification, Request, write_frame},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, Outcome};
use ivy_host_runtime::process::{self, Command, LaunchContext, OwnedProcess};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{Notify, mpsc, oneshot, watch},
    task::AbortHandle,
    time::Instant,
};

#[derive(Clone)]
pub struct WorkerLaunch {
    pub root: PathBuf,
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub options: IvyJson,
    pub job_launcher: Option<PathBuf>,
    pub environment: BTreeMap<OsString, OsString>,
}
#[derive(Debug, Clone)]
pub enum Event {
    Notification(Notification),
    Failure(IvyError),
}
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;
fn connection_failed() -> IvyError {
    IvyError::new("storage_unavailable", "Hive storage connection failed.")
        .with_outcome(Outcome::Unknown)
}
fn unconfirmed_stop() -> IvyError {
    IvyError::new("storage_unavailable", "Hive storage stop is unconfirmed.")
        .with_outcome(Outcome::Unknown)
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
            if receiver.changed().await.is_err() {
                return Err(connection_failed());
            }
        }
    }
}
struct Signal {
    sender: watch::Sender<Option<IvyResult<()>>>,
    waiter: LifecycleWaiter,
}
impl Signal {
    fn new() -> Self {
        let (sender, receiver) = watch::channel(None);
        Self {
            sender,
            waiter: LifecycleWaiter(receiver),
        }
    }
    fn finish(&self, result: IvyResult<()>) {
        if self.sender.borrow().is_none() {
            self.sender.send_replace(Some(result));
        }
    }
}
#[derive(Default)]
struct State {
    policy: WorkerClient,
    pending: BTreeMap<RequestId, oneshot::Sender<IvyResult<IvyJson>>>,
    startup: Option<Signal>,
    closing: Option<Signal>,
    process: Option<(Generation, Arc<OwnedProcess>)>,
    writer: Option<mpsc::Sender<Request>>,
    pumps: Vec<AbortHandle>,
    launching: bool,
    stopping: bool,
}
struct Client {
    launch: WorkerLaunch,
    state: Mutex<State>,
    changed: Arc<Notify>,
    epoch: Instant,
    events: EventSink,
}
/// One writer generation. All queue reservations survive caller abandonment or
/// timeout until reply or confirmed retirement, exactly as in WorkerClient.
#[derive(Clone)]
pub struct StorageClient(Arc<Client>);
impl StorageClient {
    pub fn new(launch: WorkerLaunch, events: EventSink) -> Self {
        Self(Arc::new(Client {
            launch,
            state: Mutex::new(State::default()),
            changed: Arc::new(Notify::new()),
            epoch: Instant::now(),
            events,
        }))
    }
    fn now(&self) -> u64 {
        self.0.epoch.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
    pub fn lifecycle(&self) -> Lifecycle {
        self.0.state.lock().unwrap().policy.lifecycle()
    }
    pub fn pending_count(&self) -> usize {
        self.0.state.lock().unwrap().policy.pending_count()
    }
    pub fn queued_bytes(&self) -> u64 {
        self.0.state.lock().unwrap().policy.queued_bytes()
    }
    /// Starts eagerly; concurrent calls share their exact result. An abandoned
    /// waiter cannot cancel launch or bypass a previous unconfirmed owner.
    pub fn start(&self) -> LifecycleWaiter {
        let mut state = self.0.state.lock().unwrap();
        if matches!(
            state.policy.lifecycle(),
            Lifecycle::Starting(_) | Lifecycle::Ready(_)
        ) {
            return state.startup.as_ref().unwrap().waiter.clone();
        }
        let signal = Signal::new();
        let generation = match state.policy.start(self.now()) {
            Ok(generation) => generation,
            Err(error) => {
                signal.finish(Err(error));
                return signal.waiter;
            }
        };
        let waiter = signal.waiter.clone();
        state.startup = Some(signal);
        state.closing = None;
        state.stopping = false;
        state.launching = true;
        let timer = self.timer(generation);
        state.pumps.push(timer);
        drop(state);
        let weak = Arc::downgrade(&self.0);
        let launch = self.0.launch.clone();
        tokio::spawn(async move {
            let result = process::spawn(
                &Command {
                    executable: "$storage".into(),
                    args: launch.args,
                    cwd: None,
                    timeout_ms: STARTUP_TIMEOUT_MS,
                },
                &launch.root,
                &BTreeMap::from([("$storage".into(), launch.executable)]),
                LaunchContext {
                    raw_output: true,
                    job_launcher: launch.job_launcher,
                    environment: launch.environment,
                    without_job: false,
                    allow_windows_breakaway: false,
                    ..Default::default()
                },
            )
            .await;
            let Some(client) = weak.upgrade().map(StorageClient) else {
                return;
            };
            match result {
                Ok(process) => client.bind(generation, process),
                Err(error) => {
                    client.0.state.lock().unwrap().launching = false;
                    let absent = error.outcome == Outcome::NotExecuted;
                    client.fail(
                        generation,
                        IvyError::new("storage_unavailable", "Hive storage worker failed.")
                            .with_outcome(Outcome::Unknown),
                        absent,
                    );
                    // A timer may already have fenced this still-pending launch.
                    client.retire(generation, absent);
                }
            }
        });
        waiter
    }
    pub fn request(
        &self,
        payload: IvyJson,
        options: RequestOptions,
    ) -> oneshot::Receiver<IvyResult<IvyJson>> {
        self.request_for(payload, options, None)
    }
    fn request_for(
        &self,
        payload: IvyJson,
        mut options: RequestOptions,
        generation: Option<Generation>,
    ) -> oneshot::Receiver<IvyResult<IvyJson>> {
        options.backup = payload
            .get("action")
            .and_then(|v| v.as_str().ok())
            .as_deref()
            == Some("backup");
        let (sender, receiver) = oneshot::channel();
        let mut state = self.0.state.lock().unwrap();
        if generation.is_some_and(|g| state.policy.lifecycle() != Lifecycle::Ready(g)) {
            let _ = sender.send(Err(IvyError::new(
                "service_unavailable",
                "Hive storage is not ready.",
            )));
            return receiver;
        }
        let id = match state.policy.post(self.now(), options) {
            Ok(id) => id,
            Err(error) => {
                let _ = sender.send(Err(error));
                return receiver;
            }
        };
        state.pending.insert(id, sender);
        let failed = state
            .writer
            .as_ref()
            .unwrap()
            .try_send(Request {
                id: id.sequence,
                payload,
            })
            .is_err();
        drop(state);
        self.0.changed.notify_one();
        if failed {
            self.fail(id.generation, connection_failed(), false);
        }
        receiver
    }
    pub fn close(&self) -> LifecycleWaiter {
        let mut state = self.0.state.lock().unwrap();
        if let Some(signal) = &state.closing {
            return signal.waiter.clone();
        }
        let signal = Signal::new();
        let waiter = signal.waiter.clone();
        state.stopping = true;
        state.closing = Some(signal);
        let generation = match state.policy.lifecycle() {
            Lifecycle::Stopped => {
                state.closing.as_ref().unwrap().finish(Ok(()));
                return waiter;
            }
            Lifecycle::StopUnconfirmed(_) => {
                state
                    .closing
                    .as_ref()
                    .unwrap()
                    .finish(Err(unconfirmed_stop()));
                return waiter;
            }
            Lifecycle::Retiring(_) => return waiter,
            Lifecycle::Starting(_) => {
                let effects = state.policy.shutdown();
                self.deliver(state, effects, false);
                return waiter;
            }
            Lifecycle::Ready(generation) => generation,
        };
        drop(state);
        // Bind cooperative Close to the owner observed above. A stale shutdown
        // continuation must never close an explicitly restarted generation.
        let cooperative = self.request_for(
            IvyJson::object([("action", "close".into())]),
            RequestOptions {
                timeout_ms: 5000,
                ..Default::default()
            },
            Some(generation),
        );
        let client = self.clone();
        tokio::spawn(async move {
            let _ = cooperative.await;
            let mut state = client.0.state.lock().unwrap();
            let effects = if state.policy.lifecycle() == Lifecycle::Ready(generation) {
                state.policy.shutdown()
            } else {
                Effects::default()
            };
            client.deliver(state, effects, false);
        });
        waiter
    }
    fn timer(&self, generation: Generation) -> AbortHandle {
        let weak = Arc::downgrade(&self.0);
        let changed = self.0.changed.clone();
        let epoch = self.0.epoch;
        tokio::spawn(async move {
            loop {
                let notified = changed.notified();
                let deadline = {
                    let Some(client) = weak.upgrade() else { return; };
                    let state = client.state.lock().unwrap();
                    if !matches!(state.policy.lifecycle(), Lifecycle::Starting(g) | Lifecycle::Ready(g) if g == generation) { return; }
                    state.policy.next_deadline()
                };
                if let Some(deadline) = deadline {
                    tokio::select! { _ = notified => continue, _ = tokio::time::sleep_until(epoch + Duration::from_millis(deadline)) => {} }
                    let Some(client) = weak.upgrade().map(StorageClient) else { return; };
                    let mut state = client.0.state.lock().unwrap();
                    let effects = state.policy.expire(client.now());
                    client.deliver(state, effects, false);
                } else { notified.await; }
            }
        }).abort_handle()
    }
    fn bind(&self, generation: Generation, mut process: OwnedProcess) {
        let stdin = process.stdin.take().expect("Host raw stdin");
        let stdout = process.stdout.take().expect("Host raw stdout");
        let mut stderr = process.stderr.take().expect("Host raw stderr");
        let completion = process.completion.clone();
        let process = Arc::new(process);
        let mut state = self.0.state.lock().unwrap();
        state.launching = false;
        state.process = Some((generation, process));
        if !matches!(state.policy.lifecycle(), Lifecycle::Starting(g) if g == generation) {
            drop(state);
            self.retire(generation, false);
            return;
        }
        let (sender, mut requests) = mpsc::channel::<Request>(MAX_PENDING);
        state.writer = Some(sender);
        let weak = Arc::downgrade(&self.0);
        let options = self.0.launch.options.clone();
        let writer = tokio::spawn(async move {
            let mut stdin = stdin;
            let result = async {
                write_frame(&mut stdin, &options).await?;
                while let Some(request) = requests.recv().await {
                    write_frame(&mut stdin, &request.to_json()).await?;
                }
                Ok::<_, std::io::Error>(())
            }
            .await;
            if result.is_err() {
                if let Some(client) = weak.upgrade().map(StorageClient) {
                    client.fail(generation, connection_failed(), false);
                }
            }
        });
        let weak = Arc::downgrade(&self.0);
        let reader = tokio::spawn(async move {
            let mut reader = FrameReader::new(stdout);
            loop {
                let message = match reader.next().await {
                    Ok(Some(value)) => Message::from_json(value),
                    _ => break,
                };
                let Some(client) = weak.upgrade().map(StorageClient) else {
                    return;
                };
                match message {
                    Ok(Some(message)) => client.message(generation, message),
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
            if let Some(client) = weak.upgrade().map(StorageClient) {
                client.fail(generation, connection_failed(), false);
            }
        });
        let drain = tokio::spawn(async move {
            let mut bytes = [0; 8192];
            while stderr.read(&mut bytes).await.is_ok_and(|n| n != 0) {}
        });
        state.pumps.extend([
            writer.abort_handle(),
            reader.abort_handle(),
            drain.abort_handle(),
        ]);
        drop(state);
        let weak = Arc::downgrade(&self.0);
        tokio::spawn(async move {
            let result = completion.wait().await;
            if let Some(client) = weak.upgrade().map(StorageClient) {
                client.fail(
                    generation,
                    IvyError::new(
                        "storage_unavailable",
                        format!(
                            "Hive storage worker exited ({}).",
                            result.exit_code.unwrap_or(1)
                        ),
                    )
                    .with_outcome(Outcome::Unknown),
                    false,
                );
                client.retired(generation, result.error_code.is_none());
            }
        });
    }
    fn message(&self, generation: Generation, message: Message) {
        if let Message::Fatal(error) = message {
            self.fail(generation, error, false);
            return;
        }
        let mut state = self.0.state.lock().unwrap();
        let mut notification = None;
        match message {
            Message::Ready => {
                if state.policy.started(generation) {
                    state.startup.as_ref().unwrap().finish(Ok(()));
                }
            }
            Message::Started(sequence) => state.policy.request_started(
                RequestId {
                    generation,
                    sequence,
                },
                self.now(),
            ),
            Message::Progress(sequence) => state.policy.progress(
                RequestId {
                    generation,
                    sequence,
                },
                self.now(),
            ),
            Message::Reply { id, result } => {
                if let Some(settlement) = state.policy.reply(
                    RequestId {
                        generation,
                        sequence: id,
                    },
                    result,
                ) {
                    if let Some(sender) = state.pending.remove(&settlement.id) {
                        let _ = sender.send(settlement.result);
                    }
                }
            }
            Message::Notification(value) => {
                if state.policy.lifecycle() == Lifecycle::Ready(generation) {
                    notification = Some(value);
                }
            }
            Message::Fatal(_) => unreachable!(),
        }
        drop(state);
        self.0.changed.notify_one();
        if let Some(value) = notification {
            (self.0.events)(Event::Notification(value));
        }
    }
    fn fail(&self, generation: Generation, error: IvyError, absent: bool) {
        let mut state = self.0.state.lock().unwrap();
        let effects = state.policy.fail(generation, error);
        self.deliver(state, effects, absent);
    }
    // Keep the policy transition and its settlements in one critical section.
    // A simultaneous proof/restart cannot redirect old effects to a new owner.
    fn deliver(&self, mut state: MutexGuard<'_, State>, effects: Effects, absent: bool) {
        for settlement in effects.settlements {
            if let Some(sender) = state.pending.remove(&settlement.id) {
                let _ = sender.send(settlement.result);
            }
        }
        if let Some(error) = &effects.failure {
            if let Some(signal) = &state.startup {
                signal.finish(Err(error.clone()));
            }
        }
        let report = effects.failure.filter(|_| !state.stopping);
        if effects.retire.is_some() {
            if state.stopping {
                if let Some(signal) = &state.startup {
                    signal.finish(Err(IvyError::new(
                        "service_unavailable",
                        "Hive is shutting down.",
                    )
                    .with_outcome(Outcome::Unknown)));
                }
            }
            state.writer = None;
            for pump in state.pumps.drain(..) {
                pump.abort();
            }
        }
        drop(state);
        if let Some(generation) = effects.retire {
            self.retire(generation, absent);
        }
        if let Some(error) = report {
            (self.0.events)(Event::Failure(error));
        }
    }
    fn retire(&self, generation: Generation, absent: bool) {
        let state = self.0.state.lock().unwrap();
        if !matches!(state.policy.lifecycle(), Lifecycle::Retiring(g) | Lifecycle::StopUnconfirmed(g) if g == generation)
        {
            return;
        }
        let process = state
            .process
            .as_ref()
            .filter(|(g, _)| *g == generation)
            .map(|(_, p)| p.clone());
        let launching = state.launching;
        drop(state);
        if let Some(process) = process {
            let weak = Arc::downgrade(&self.0);
            tokio::spawn(async move {
                let confirmed = process
                    .stop(Duration::ZERO)
                    .await
                    .is_ok_and(|r| r.error_code.is_none());
                if let Some(client) = weak.upgrade().map(StorageClient) {
                    client.retired(generation, confirmed);
                }
            });
        } else if !launching {
            self.retired(generation, absent);
        }
    }
    fn retired(&self, generation: Generation, confirmed: bool) {
        let mut state = self.0.state.lock().unwrap();
        if !matches!(state.policy.lifecycle(), Lifecycle::Retiring(g) | Lifecycle::StopUnconfirmed(g) if g == generation)
        {
            return;
        }
        state.policy.retired(generation, confirmed);
        if confirmed {
            state.process = None;
            state.pending.clear();
        }
        if state.stopping {
            if let Some(signal) = &state.closing {
                signal.finish(if confirmed {
                    Ok(())
                } else {
                    Err(unconfirmed_stop())
                });
            }
        }
    }
}
