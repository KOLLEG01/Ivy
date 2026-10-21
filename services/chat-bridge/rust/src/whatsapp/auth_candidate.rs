//! Unbound source candidate. Deliberately NOT exported by the production library.
//! Integration tests include this source; startup and deployment cannot opt in.
#[path = "auth_candidate_store.rs"]
pub mod store;
#[path = "auth_prototype_wire.rs"]
mod wire;
use ivy_chat_bridge::whatsapp::auth_node::AuthLaunch;
use ivy_contracts::{IvyJson as Json, IvyResult, hash::digest};
use ivy_host_runtime::{
    exec_file::SpawnOptions,
    process::{self, Command, LaunchContext, OwnedProcess},
};
use ivy_sdk::service::ServiceFuture;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use store::{Grant, Store, encode, refused, text, unknown};
use tokio::{
    io::AsyncReadExt,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

/// Explicit candidate/test parameters, never installed runtime defaults.
#[derive(Clone, Copy)]
pub struct CandidateBudget {
    pub initialize: Duration,
    pub close: Duration,
    pub cooperative: Duration,
}
impl CandidateBudget {
    pub fn source_fixture() -> Self {
        Self {
            initialize: Duration::from_secs(15),
            close: Duration::from_secs(10),
            cooperative: Duration::from_secs(8),
        }
    }
}
pub struct CandidateEvent {
    pub operation: Grant,
    pub value: Json,
}
type EventSink = Arc<dyn Fn(CandidateEvent) -> ServiceFuture<Json> + Send + Sync>;
#[derive(Default)]
struct CleanupStatus {
    decision_expired: bool,
    running: bool,
    unavailable: bool,
    tree_stopped: bool,
    proof: bool,
    retired: bool,
    worker_running: bool,
}
pub struct CandidateInitialization {
    result: watch::Receiver<Option<IvyResult<CandidateAccount>>>,
    status: Arc<Mutex<CleanupStatus>>,
}
impl CandidateInitialization {
    pub fn ready(&self) -> ServiceFuture<CandidateAccount> {
        Box::pin(observe(self.result.clone()))
    }
    pub fn cleanup_status(&self) -> Json {
        status_json(&self.status)
    }
}
fn status_json(status: &Mutex<CleanupStatus>) -> Json {
    let s = status.lock().unwrap();
    Json::object([
        ("decisionExpired", s.decision_expired.into()),
        ("cleanupRunning", s.running.into()),
        ("cleanupUnavailable", s.unavailable.into()),
        ("treeStopped", s.tree_stopped.into()),
        ("quiescenceProved", s.proof.into()),
        ("accountRetired", s.retired.into()),
        ("ownerWorkerRunning", s.worker_running.into()),
    ])
}

#[derive(Clone)]
pub enum Reply {
    Value(String),
    LibraryError(String),
}
pub struct Invocation {
    pub id: String,
    done: watch::Receiver<Option<IvyResult<Reply>>>,
}
impl Invocation {
    pub async fn result(self) -> IvyResult<Reply> {
        observe(self.done).await
    }
}
#[derive(Clone)]
pub struct QuiescenceProof(Json);
impl QuiescenceProof {
    pub fn value(&self) -> Json {
        self.0.clone()
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
    None,
    Prepared,
    Entered,
    CommitBefore,
    CommitAfter,
    FinishBefore,
    FinishAfter,
    ResultBody,
    ProofBefore,
    ProofAfter,
    RetentionAfter,
}
type Completion<T> = watch::Sender<Option<IvyResult<T>>>;
enum Control {
    Run {
        kind: String,
        method: String,
        args: Json,
        reply: oneshot::Sender<IvyResult<Invocation>>,
        credit: Option<OwnedSemaphorePermit>,
        parent: Option<Grant>,
        caller: Option<String>,
    },
    Read {
        operation: Option<String>,
        reply: oneshot::Sender<IvyResult<Json>>,
    },
    Fault(Fault, oneshot::Sender<()>),
    RejectInitialization(oneshot::Sender<()>),
    Close {
        done: Completion<QuiescenceProof>,
        deadline: Instant,
        cooperative: Instant,
    },
    Retire(Completion<()>),
    Expired(String),
    EventDone {
        callback: String,
        root: String,
        result: IvyResult<Json>,
    },
}
enum Output {
    Message(String, Json),
    Close,
}
struct Pending {
    grant: Grant,
    result: Completion<Reply>,
    _credit: Option<OwnedSemaphorePermit>,
}
struct Inner {
    control: mpsc::Sender<Control>,
    credits: Arc<Semaphore>,
    closing: Arc<AtomicBool>,
    budget: CandidateBudget,
    close_deadline: Mutex<Option<Instant>>,
    close: Mutex<Option<watch::Receiver<Option<IvyResult<QuiescenceProof>>>>>,
    retire: Mutex<Option<watch::Receiver<Option<IvyResult<()>>>>>,
    pub stats: Arc<Mutex<wire::Statistics>>,
    sink: Arc<Mutex<Option<EventSink>>>,
    cleanup: Arc<Mutex<CleanupStatus>>,
    _worker: JoinHandle<()>,
    cleanup_finished: watch::Receiver<bool>,
    actual_close: Mutex<Option<watch::Receiver<Option<IvyResult<QuiescenceProof>>>>>,
}
#[derive(Clone)]
pub struct CandidateAccount(Arc<Inner>);
impl CandidateAccount {
    pub fn statistics(&self) -> wire::Statistics {
        *self.0.stats.lock().unwrap()
    }
    pub fn initialize(
        store: Store,
        launch: AuthLaunch,
        budget: CandidateBudget,
    ) -> CandidateInitialization {
        let deadline = Instant::now() + budget.initialize;
        let status = Arc::new(Mutex::new(CleanupStatus::default()));
        let (actual, receive) = watch::channel(None);
        let owner_status = status.clone();
        tokio::spawn(async move {
            let result =
                Self::launch_owned(store, launch, budget, deadline, owner_status.clone()).await;
            if result.is_err() {
                owner_status.lock().unwrap().unavailable = true;
            }
            actual.send_replace(Some(result));
        });
        let (decision, result) = watch::channel(None);
        let observed = status.clone();
        tokio::spawn(async move {
            let result = match tokio::time::timeout_at(deadline, observe(receive)).await {
                Ok(value) if Instant::now() <= deadline => value,
                _ => {
                    observed.lock().unwrap().decision_expired = true;
                    Err(unknown())
                }
            };
            decision.send_replace(Some(result));
        });
        CandidateInitialization { result, status }
    }
    async fn launch_owned(
        mut store: Store,
        launch: AuthLaunch,
        budget: CandidateBudget,
        init_deadline: Instant,
        cleanup: Arc<Mutex<CleanupStatus>>,
    ) -> IvyResult<Self> {
        if budget.initialize.is_zero()
            || budget.close.is_zero()
            || budget.cooperative.is_zero()
            || budget.cooperative >= budget.close
        {
            return Err(refused());
        }
        let init = store.prepare("init", "initialize", None)?;
        store.enter(&init.id)?;
        store.hold_result(&init)?;
        let runtime = store.runtime_grant()?;
        let saved = store.saved_creds()?;
        let options = SpawnOptions::for_component(&launch.artifact_root, launch.helper_layout)?;
        let worker = ivy_host_runtime::paths::realpath(
            &launch
                .artifact_root
                .join("tests/rust/fixtures/chat-auth-candidate-worker.mjs"),
        )
        .map_err(|_| unknown())?;
        if !ivy_host_runtime::paths::inside(&launch.artifact_root, &worker)
            .map_err(|_| unknown())?
        {
            return Err(unknown());
        }
        // The owned launch is never cancelled by a readiness observer's deadline.
        let mut child = process::spawn(
            &Command {
                executable: "$node".into(),
                args: vec![worker.into_os_string(), store.epoch.clone().into()],
                cwd: None,
                timeout_ms: 0,
            },
            &launch.artifact_root,
            &BTreeMap::from([("$node".into(), launch.node.executable)]),
            LaunchContext {
                environment: launch.environment,
                job_launcher: options.job_launcher,
                raw_output: true,
                ..Default::default()
            },
        )
        .await?;
        let diagnostics = child.stderr.take().ok_or_else(unknown)?;
        let diagnostics = tokio::spawn(async move {
            let mut pipe = diagnostics;
            let mut buffer = [0; 8192];
            loop {
                match pipe.read(&mut buffer).await {
                    Ok(0) => return Ok(()),
                    Ok(_) => {}
                    Err(_) => return Err(unknown()),
                }
            }
        });
        let (reader, mut writer, stats) = wire::pipes(
            child.stdout.take().ok_or_else(unknown)?,
            child.stdin.take().ok_or_else(unknown)?,
            &store.epoch,
        );
        let (out, mut writes) = mpsc::channel(1);
        let (frames, reads) = mpsc::channel(1);
        let (control, commands) = mpsc::channel(1);
        let admission = Arc::new(AtomicBool::new(false));
        let (cleanup_finished, cleanup_observer) = watch::channel(false);
        let sink: Arc<Mutex<Option<EventSink>>> = Arc::new(Mutex::new(None));
        let (events, mut incoming) = mpsc::channel::<(String, String, CandidateEvent)>(4);
        let event_sink = sink.clone();
        let event_control = control.clone();
        tokio::spawn(async move {
            while let Some((callback, root, event)) = incoming.recv().await {
                let handler = event_sink.lock().unwrap().clone();
                let result = match handler {
                    Some(handler) => handler(event).await,
                    None => Ok(event.value),
                };
                if event_control
                    .send(Control::EventDone {
                        callback,
                        root,
                        result,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let writer = tokio::spawn(async move {
            while let Some(work) = writes.recv().await {
                match work {
                    Output::Message(id, value) => writer.send(&id, value).await?,
                    Output::Close => return writer.close().await,
                }
            }
            Err(unknown())
        });
        let reader = tokio::spawn(async move {
            let mut reader = reader;
            loop {
                let message = reader.receive().await;
                let ended = !matches!(&message, Ok(Some(_)));
                if frames.send(message).await.is_err() || ended {
                    break;
                }
            }
        });
        let (done, receive) = watch::channel(None);
        let init_id = init.id.clone();
        let initial = Json::object([
            ("kind", "command".into()),
            ("method", "initialize".into()),
            ("id", init.id.as_str().into()),
            ("token", init.token.as_str().into()),
            ("arguments", saved),
            (
                "runtime",
                Json::object([
                    ("id", runtime.id.as_str().into()),
                    ("token", runtime.token.as_str().into()),
                ]),
            ),
        ]);
        out.send(Output::Message(init.id.clone(), initial))
            .await
            .map_err(|_| unknown())?;
        let actor = Actor {
            store,
            child,
            diagnostics,
            writer,
            reader,
            out,
            reads,
            commands,
            pending: HashMap::from([(
                init.id.clone(),
                Pending {
                    grant: init,
                    result: done,
                    _credit: None,
                },
            )]),
            runtime,
            admission: admission.clone(),
            credits: Arc::new(Semaphore::new(4)),
            background: HashMap::new(),
            events,
            event_roots: std::collections::BTreeSet::new(),
            event_used: std::collections::BTreeSet::new(),
            cleanup: cleanup.clone(),
            cleanup_finished,
            reader_joined: false,
            writer_joined: false,
            diagnostics_joined: false,
            close_deadline: None,
            cooperative_deadline: None,
            next_callback: 0,
            close: None,
            close_result: None,
            stop_sent: false,
            stop_ack: false,
            raw_drained: false,
            retire_wait: None,
            fault: Fault::None,
            failed: false,
        };
        let credits = actor.credits.clone();
        // One serialized SQL/account worker keeps SQLite busy/commit waits off
        // the async executor. Independent pipes, events and deadline observers
        // continue; no SQL guard is held across callback-dependent waits.
        cleanup.lock().unwrap().worker_running = true;
        let worker_status = cleanup.clone();
        let executor = tokio::runtime::Handle::current();
        let worker = tokio::task::spawn_blocking(move || {
            executor.block_on(actor.run());
            worker_status.lock().unwrap().worker_running = false;
        });
        let owner = Self(Arc::new(Inner {
            control,
            credits,
            closing: admission,
            budget,
            close_deadline: Mutex::new(None),
            close: Mutex::new(None),
            retire: Mutex::new(None),
            stats,
            sink,
            cleanup,
            _worker: worker,
            cleanup_finished: cleanup_observer,
            actual_close: Mutex::new(None),
        }));
        let initialized = match tokio::time::timeout_at(init_deadline, observe(receive)).await {
            Ok(result) => result?,
            Err(_) => {
                let _ = owner.0.control.send(Control::Expired(init_id)).await;
                return Err(unknown());
            }
        };
        let ready = match initialized {
            Reply::Value(value) => ivy_contracts::json::parse(&value, usize::MAX)
                .ok()
                .is_some_and(|value| {
                    value.get("baileys") == Some(&Json::from("7.0.0-rc13"))
                        && value.get("libsignal") == Some(&Json::from("6.0.0"))
                        && value.get("liveSocket").and_then(Json::as_bool) == Some(false)
                        && value.get("network").and_then(Json::as_bool) == Some(false)
                        && value.get("candidate").and_then(Json::as_bool) == Some(true)
                        && value.get("rawBeforeWrapper").and_then(Json::as_bool) == Some(true)
                }),
            Reply::LibraryError(_) => false,
        };
        if !ready {
            let (done, wait) = oneshot::channel();
            let _ = owner
                .0
                .control
                .send(Control::RejectInitialization(done))
                .await;
            let _ = wait.await;
            return Err(unknown());
        }
        Ok(owner)
    }
    pub fn set_event_sink(&self, sink: EventSink) {
        *self.0.sink.lock().unwrap() = Some(sink);
    }
    pub fn cleanup_status(&self) -> Json {
        status_json(&self.0.cleanup)
    }
    pub fn start(&self, kind: &str, method: &str, args: Json) -> ServiceFuture<Invocation> {
        self.start_for(None, kind, method, args)
    }
    pub fn accepting(&self) -> bool {
        !self.0.closing.load(Ordering::Acquire)
    }
    fn start_for(
        &self,
        caller: Option<String>,
        kind: &str,
        method: &str,
        args: Json,
    ) -> ServiceFuture<Invocation> {
        let owner = self.clone();
        let kind = kind.to_owned();
        let method = method.to_owned();
        Box::pin(async move {
            if owner.0.closing.load(Ordering::Acquire) {
                return Err(refused());
            }
            let credit = owner
                .0
                .credits
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| unknown())?;
            if owner.0.closing.load(Ordering::Acquire) {
                return Err(refused());
            }
            let (reply, receive) = oneshot::channel();
            owner
                .0
                .control
                .send(Control::Run {
                    kind,
                    method,
                    args,
                    reply,
                    credit: Some(credit),
                    parent: None,
                    caller,
                })
                .await
                .map_err(|_| unknown())?;
            receive.await.map_err(|_| unknown())?
        })
    }
    pub async fn call(&self, kind: &str, method: &str, args: Json) -> IvyResult<(String, Reply)> {
        let call = self.start(kind, method, args).await?;
        let id = call.id.clone();
        let reply = observe(call.done).await?;
        Ok((id, reply))
    }
    pub async fn call_for(
        &self,
        caller: String,
        kind: &str,
        method: &str,
        args: Json,
    ) -> IvyResult<(String, Reply)> {
        let call = self.start_for(Some(caller), kind, method, args).await?;
        let id = call.id.clone();
        Ok((id, observe(call.done).await?))
    }
    pub async fn call_within(
        &self,
        parent: Grant,
        kind: &str,
        method: &str,
        args: Json,
    ) -> IvyResult<(String, Reply)> {
        if self.0.closing.load(Ordering::Acquire) {
            return Err(refused());
        }
        let (reply, receive) = oneshot::channel();
        self.0
            .control
            .send(Control::Run {
                kind: kind.into(),
                method: method.into(),
                args,
                reply,
                credit: None,
                parent: Some(parent),
                caller: None,
            })
            .await
            .map_err(|_| unknown())?;
        let call = receive.await.map_err(|_| unknown())??;
        let id = call.id.clone();
        Ok((id, observe(call.done).await?))
    }
    pub async fn read(&self, id: Option<&str>) -> IvyResult<Json> {
        let (reply, receive) = oneshot::channel();
        self.0
            .control
            .send(Control::Read {
                operation: id.map(str::to_owned),
                reply,
            })
            .await
            .map_err(|_| unknown())?;
        receive.await.map_err(|_| unknown())?
    }
    pub async fn fault(&self, fault: Fault) {
        let (reply, receive) = oneshot::channel();
        self.0
            .control
            .send(Control::Fault(fault, reply))
            .await
            .unwrap();
        receive.await.unwrap();
    }
    pub fn quiesce(&self) -> ServiceFuture<QuiescenceProof> {
        self.0.closing.store(true, Ordering::Release);
        let mut close = self.0.close.lock().unwrap();
        let receive = if let Some(value) = &*close {
            value.clone()
        } else {
            let (decision, receive) = watch::channel(None);
            *close = Some(receive.clone());
            let sender = self.0.control.clone();
            let now = Instant::now();
            let deadline = now + self.0.budget.close;
            let cooperative = now + self.0.budget.cooperative;
            *self.0.close_deadline.lock().unwrap() = Some(deadline);
            self.0.cleanup.lock().unwrap().running = true;
            let status = self.0.cleanup.clone();
            let (done, actual) = watch::channel(None);
            *self.0.actual_close.lock().unwrap() = Some(actual.clone());
            tokio::spawn(async move {
                if sender
                    .send(Control::Close {
                        done,
                        deadline,
                        cooperative,
                    })
                    .await
                    .is_err()
                {
                    decision.send_replace(Some(Err(unknown())));
                    return;
                }
                let result = match tokio::time::timeout_at(deadline, observe(actual)).await {
                    Ok(value) if Instant::now() <= deadline => value,
                    _ => {
                        status.lock().unwrap().decision_expired = true;
                        Err(unknown())
                    }
                };
                decision.send_replace(Some(result));
            });
            receive
        };
        Box::pin(observe(receive))
    }
    /// Preserve the original deadline decision, while the RuntimeSocket owner
    /// keeps journal resources open until its actual admitted callbacks finish.
    pub fn quiesce_settled(&self) -> ServiceFuture<()> {
        let decision = self.quiesce();
        let owner = self.clone();
        Box::pin(async move {
            let result = decision.await;
            let actual = owner
                .0
                .actual_close
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(unknown)?;
            let _ = observe(actual).await;
            let mut finished = owner.0.cleanup_finished.clone();
            while !*finished.borrow_and_update() {
                finished.changed().await.map_err(|_| unknown())?;
            }
            result.map(|_| ())
        })
    }
    pub fn retire(&self) -> ServiceFuture<()> {
        let mut retire = self.0.retire.lock().unwrap();
        let receive = if let Some(value) = &*retire {
            value.clone()
        } else {
            let (decision, receive) = watch::channel(None);
            *retire = Some(receive.clone());
            let sender = self.0.control.clone();
            let deadline = *self.0.close_deadline.lock().unwrap();
            let status = self.0.cleanup.clone();
            let (done, actual) = watch::channel(None);
            tokio::spawn(async move {
                if sender.send(Control::Retire(done)).await.is_err() {
                    decision.send_replace(Some(Err(unknown())));
                    return;
                }
                let result = if let Some(deadline) = deadline {
                    match tokio::time::timeout_at(deadline, observe(actual)).await {
                        Ok(value) if Instant::now() <= deadline => value,
                        _ => {
                            status.lock().unwrap().decision_expired = true;
                            Err(unknown())
                        }
                    }
                } else {
                    Err(unknown())
                };
                decision.send_replace(Some(result));
            });
            receive
        };
        Box::pin(observe(receive))
    }
}
async fn observe<T: Clone>(mut receive: watch::Receiver<Option<IvyResult<T>>>) -> IvyResult<T> {
    loop {
        if let Some(value) = receive.borrow_and_update().clone() {
            return value;
        }
        receive.changed().await.map_err(|_| unknown())?;
    }
}
struct Actor {
    store: Store,
    child: OwnedProcess,
    diagnostics: JoinHandle<IvyResult<()>>,
    writer: JoinHandle<IvyResult<()>>,
    reader: JoinHandle<()>,
    out: mpsc::Sender<Output>,
    reads: mpsc::Receiver<IvyResult<Option<(String, Json)>>>,
    commands: mpsc::Receiver<Control>,
    pending: HashMap<String, Pending>,
    runtime: Grant,
    admission: Arc<AtomicBool>,
    credits: Arc<Semaphore>,
    background: HashMap<String, OwnedSemaphorePermit>,
    events: mpsc::Sender<(String, String, CandidateEvent)>,
    event_roots: std::collections::BTreeSet<String>,
    event_used: std::collections::BTreeSet<String>,
    cleanup: Arc<Mutex<CleanupStatus>>,
    cleanup_finished: watch::Sender<bool>,
    reader_joined: bool,
    writer_joined: bool,
    diagnostics_joined: bool,
    close_deadline: Option<Instant>,
    cooperative_deadline: Option<Instant>,
    next_callback: u64,
    close: Option<(Grant, Completion<QuiescenceProof>)>,
    close_result: Option<IvyResult<QuiescenceProof>>,
    stop_sent: bool,
    stop_ack: bool,
    raw_drained: bool,
    retire_wait: Option<Completion<()>>,
    fault: Fault,
    failed: bool,
}
impl Actor {
    async fn run(mut self) {
        loop {
            let cooperative = self
                .cooperative_deadline
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(86400));
            tokio::select! {
                command=self.commands.recv()=>match command{Some(command)=>{if self.command(command).await.is_err(){self.fail().await;}},None=>{self.fail().await;break;}},
                frame=self.reads.recv(),if !self.raw_drained=>{match frame{Some(Ok(Some((id,value))))=>{if !self.failed&&self.message(id,value).await.is_err(){self.fail().await;}},Some(Ok(None))=>{self.raw_drained=true;if !self.failed&&self.finish_close().await.is_err(){self.fail().await;}},_=>{self.raw_drained=true;self.fail().await;}}},
                _=tokio::time::sleep_until(cooperative),if self.close.is_some()&&!self.failed&&self.close_result.is_none()=>{self.fail().await;}
                _=async {
                    if !self.reader_joined {let _=(&mut self.reader).await;self.reader_joined=true;}
                    if !self.writer_joined {let _=(&mut self.writer).await;self.writer_joined=true;}
                    if !self.diagnostics_joined {let _=(&mut self.diagnostics).await;self.diagnostics_joined=true;}
                },if self.failed&&!(self.reader_joined&&self.writer_joined&&self.diagnostics_joined)=>{}
            }
            if !self.failed && self.maybe_stop().await.is_err() {
                self.fail().await;
            }
            if self.failed
                && self.reader_joined
                && self.writer_joined
                && self.diagnostics_joined
                && self.event_roots.is_empty()
            {
                self.cleanup.lock().unwrap().running = false;
                self.cleanup_finished.send_replace(true);
            }
            let status = self.cleanup.lock().unwrap();
            if status.retired || (self.failed && !status.running && self.event_roots.is_empty()) {
                break;
            }
        }
        self.reader.abort();
    }
    fn send(&self, id: String, value: Json) -> ServiceFuture<()> {
        let output = self.out.clone();
        Box::pin(async move {
            output
                .send(Output::Message(id, value))
                .await
                .map_err(|_| unknown())
        })
    }
    async fn command(&mut self, command: Control) -> IvyResult<()> {
        match command {
            Control::Read { operation, reply } => {
                let _ = reply.send(
                    operation.map_or_else(|| self.store.owner(), |id| self.store.receipt(&id)),
                );
            }
            Control::Fault(fault, done) => {
                self.fault = fault;
                let _ = done.send(());
            }
            Control::RejectInitialization(done) => {
                self.fail().await;
                let _ = done.send(());
            }
            Control::Run {
                kind,
                method,
                args,
                reply,
                credit,
                parent,
                caller,
            } => {
                if self.failed || self.admission.load(Ordering::Acquire) || self.store.closing {
                    let _ = reply.send(Err(if self.failed { unknown() } else { refused() }));
                    return Ok(());
                }
                let parent = parent.unwrap_or_else(|| self.runtime.clone());
                let grant =
                    match self
                        .store
                        .prepare(&kind, &method, Some((&parent.id, &parent.token)))
                    {
                        Ok(grant) => grant,
                        Err(e) => {
                            let _ = reply.send(Err(e));
                            return Ok(());
                        }
                    };
                let (done, receive) = watch::channel(None);
                let id = grant.id.clone();
                let _ = reply.send(Ok(Invocation {
                    id: id.clone(),
                    done: receive,
                }));
                self.pending.insert(
                    id.clone(),
                    Pending {
                        grant,
                        result: done,
                        _credit: credit,
                    },
                );
                if self.fault == Fault::Prepared {
                    return Err(unknown());
                }
                if let Some(caller) = caller {
                    self.store.attach_caller(&id, &caller)?;
                }
                self.store.enter(&id)?;
                self.store.hold_result(&self.pending[&id].grant)?;
                if self.fault == Fault::Entered {
                    return Err(unknown());
                }
                let token = self.pending[&id].grant.token.clone();
                self.send(
                    id.clone(),
                    Json::object([
                        ("kind", "command".into()),
                        ("id", id.as_str().into()),
                        ("token", token.into()),
                        ("method", method.into()),
                        ("arguments", args),
                    ]),
                )
                .await?;
            }
            Control::Close {
                done,
                deadline,
                cooperative,
            } => {
                if let Some(result) = &self.close_result {
                    done.send_replace(Some(result.clone()));
                    return Ok(());
                }
                if self.failed {
                    done.send_replace(Some(Err(unknown())));
                    return Ok(());
                }
                let grant = self.store.start_close()?;
                let cleanup = self.store.token_index_cleanup(&grant)?;
                self.close_deadline = Some(deadline);
                self.cooperative_deadline = Some(cooperative);
                let closing = Json::object([
                    ("kind", "closing".into()),
                    (
                        "close",
                        Json::object([
                            ("id", grant.id.as_str().into()),
                            ("token", grant.token.as_str().into()),
                        ]),
                    ),
                    (
                        "cleanup",
                        Json::object([("id", cleanup.id.into()), ("token", cleanup.token.into())]),
                    ),
                ]);
                self.close = Some((grant, done));
                self.send("control".into(), closing).await?;
            }
            Control::Retire(done) => {
                if matches!(&self.close_result, Some(Ok(_))) {
                    let result = self.store.retire();
                    if result.is_ok() {
                        self.cleanup.lock().unwrap().retired = true;
                    }
                    done.send_replace(Some(result));
                } else if self.close_result.is_none() && self.close.is_some() && !self.failed {
                    self.retire_wait = Some(done);
                } else {
                    done.send_replace(Some(Err(unknown())));
                    if self.close.is_none() && !self.failed {
                        return Err(unknown());
                    }
                }
            }
            Control::Expired(id) => {
                if (id == "close" && self.close_result.is_none()) || self.pending.contains_key(&id)
                {
                    return Err(unknown());
                }
            }
            Control::EventDone {
                callback,
                root,
                result,
            } => {
                if !self.event_roots.remove(&root) {
                    return Err(unknown());
                }
                if !self.failed {
                    self.send(
                        callback,
                        Json::object([("kind", "response".into()), ("result", result?)]),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
    async fn message(&mut self, id: String, value: Json) -> IvyResult<()> {
        // A stop acknowledgement seals the protocol. Even a valid old grant
        // cannot admit another callback while the remaining pipe bytes drain.
        if self.stop_ack {
            return Err(unknown());
        }
        match text(&value, "kind")?.as_str() {
            "request" => {
                self.next_callback = self.next_callback.checked_add(1).ok_or_else(unknown)?;
                if id != format!("callback:{}", self.next_callback) {
                    return Err(unknown());
                }
                let request_method = text(&value, "method")?;
                let args = value.required("arguments").map_err(|_| unknown())?;
                let result = match request_method.as_str() {
                    "begin" | "reserve" => {
                        if self.admission.load(Ordering::Acquire) {
                            self.send(
                                id,
                                Json::object([
                                    ("kind", "response".into()),
                                    ("result", Json::object([("refused", true.into())])),
                                ]),
                            )
                            .await?;
                            return Ok(());
                        }
                        let parent = args.required("parent").map_err(|_| unknown())?;
                        let parent = if parent.is_null() {
                            None
                        } else {
                            Some((text(parent, "id")?, text(parent, "token")?))
                        };
                        let kind = text(args, "kind")?;
                        let method = text(args, "method")?;
                        if ![
                            "repository",
                            "raw-store",
                            "credential-save",
                            "inbound-crypto",
                            "background-prekey",
                            "app-state",
                        ]
                        .contains(&kind.as_str())
                        {
                            return Err(unknown());
                        }
                        let parent = parent.ok_or_else(unknown)?;
                        let root_credit = if parent.0 == self.runtime.id {
                            match self.credits.clone().try_acquire_owned() {
                                Ok(credit) => Some(credit),
                                Err(_) => {
                                    self.send(
                                        id,
                                        Json::object([
                                            ("kind", "response".into()),
                                            ("result", Json::object([("refused", true.into())])),
                                        ]),
                                    )
                                    .await?;
                                    return Ok(());
                                }
                            }
                        } else {
                            None
                        };
                        let grant = if request_method == "reserve" {
                            self.store.reserve_kind(
                                &Grant {
                                    id: parent.0.clone(),
                                    token: parent.1.clone(),
                                },
                                &kind,
                                &method,
                            )
                        } else {
                            self.store
                                .prepare(&kind, &method, Some((&parent.0, &parent.1)))
                        };
                        let grant = match grant {
                            Ok(grant) => grant,
                            Err(error) if error.outcome == ivy_contracts::Outcome::NotExecuted => {
                                self.send(
                                    id,
                                    Json::object([
                                        ("kind", "response".into()),
                                        ("result", Json::object([("refused", true.into())])),
                                    ]),
                                )
                                .await?;
                                return Ok(());
                            }
                            Err(error) => return Err(error),
                        };
                        if request_method == "begin" {
                            self.store.enter(&grant.id)?;
                        }
                        if let Some(credit) = root_credit {
                            self.background.insert(grant.id.clone(), credit);
                        }
                        Json::object([("id", grant.id.into()), ("token", grant.token.into())])
                    }
                    "activate" => {
                        let grant = Grant {
                            id: text(args, "id")?,
                            token: text(args, "token")?,
                        };
                        self.store
                            .activate_reserved(&grant, &text(args, "method")?)?;
                        Json::Null
                    }
                    "cancel" => {
                        let op = text(args, "id")?;
                        self.store.cancel_reserved(&Grant {
                            id: op.clone(),
                            token: text(args, "token")?,
                        })?;
                        if !self.store.has_live(&op) {
                            self.background.remove(&op);
                        }
                        Json::Null
                    }
                    "raw" => {
                        let kind = text(args, "kind")?;
                        let id = text(args, "id")?;
                        let token = text(args, "token")?;
                        if kind == "set" && self.fault == Fault::CommitBefore {
                            return Err(unknown());
                        }
                        let ordinal_text = text(args, "ordinal")?;
                        let ordinal = ordinal_text.parse::<u64>().map_err(|_| unknown())?;
                        if ordinal.to_string() != ordinal_text {
                            return Err(unknown());
                        }
                        let result = self.store.callback(
                            &id,
                            &token,
                            ordinal,
                            &kind,
                            &text(args, "encoded")?,
                            &text(args, "digest")?,
                        )?;
                        if kind == "set" && self.fault == Fault::CommitAfter {
                            return Err(unknown());
                        }
                        result
                    }
                    "finish" => {
                        if self.fault == Fault::FinishBefore {
                            return Err(unknown());
                        }
                        let op = text(args, "id")?;
                        let token = text(args, "token")?;
                        let outcome = text(args, "outcome")?;
                        let result_digest = text(args, "digest")?;
                        let name = args
                            .get("name")
                            .filter(|v| !v.is_null())
                            .map(Json::as_str)
                            .transpose()
                            .map_err(|_| unknown())?;
                        self.store.finish(
                            &op,
                            &token,
                            &outcome,
                            &result_digest,
                            name.as_deref(),
                        )?;
                        if !self.store.has_live(&op) {
                            self.background.remove(&op);
                        }
                        if self.fault == Fault::FinishAfter {
                            return Err(unknown());
                        }
                        self.event_used.remove(&op);
                        Json::Null
                    }
                    "event" => {
                        let operation = Grant {
                            id: text(args, "id")?,
                            token: text(args, "token")?,
                        };
                        self.store.check(&operation.id, &operation.token)?;
                        let receipt = self.store.receipt(&operation.id)?;
                        if text(&receipt, "kind")? != "inbound-crypto"
                            || text(&receipt, "method")? != "candidate.event"
                        {
                            self.send(
                                id,
                                Json::object([
                                    ("kind", "response".into()),
                                    ("result", Json::object([("refused", true.into())])),
                                ]),
                            )
                            .await?;
                            return Ok(());
                        }
                        if !self.event_used.insert(operation.id.clone()) {
                            return Err(unknown());
                        }
                        let root = text(&receipt, "finiteRoot")?;
                        if !self.event_roots.insert(root.clone()) {
                            return Err(unknown());
                        }
                        self.events
                            .try_send((
                                id,
                                root,
                                CandidateEvent {
                                    operation,
                                    value: args.required("value").map_err(|_| unknown())?.clone(),
                                },
                            ))
                            .map_err(|_| unknown())?;
                        return Ok(());
                    }
                    _ => return Err(unknown()),
                };
                self.send(
                    id,
                    Json::object([("kind", "response".into()), ("result", result)]),
                )
                .await?;
            }
            "result" | "error" => {
                if text(&value, "id")? != id {
                    return Err(unknown());
                }
                let pending = self.pending.get(&id).ok_or_else(unknown)?;
                let receipt = self.store.receipt(&id)?;
                if text(&receipt, "state")? != "completed" {
                    return Err(unknown());
                }
                let (kind, body) = if text(&value, "kind")? == "result" {
                    ("value", text(&value, "encoded")?)
                } else {
                    (
                        "library_error",
                        encode(&Json::object([("name", text(&value, "name")?.into())])),
                    )
                };
                if text(&receipt, "outcome")? != kind
                    || text(&receipt, "resultDigest")? != digest(body.as_bytes())
                {
                    return Err(unknown());
                }
                if self.fault == Fault::ResultBody {
                    return Err(unknown());
                }
                self.store.release_result(&pending.grant)?;
                if self.fault == Fault::RetentionAfter {
                    return Err(unknown());
                }
                let result = if kind == "value" {
                    Reply::Value(body)
                } else {
                    Reply::LibraryError(text(&value, "name")?)
                };
                pending.result.send_replace(Some(Ok(result)));
                self.pending.remove(&id);
            }
            "stopped" => {
                let (close, _) = self.close.as_ref().ok_or_else(unknown)?;
                if !self.stop_sent
                    || self.stop_ack
                    || text(&value, "scope")? != "candidate.inert.v1"
                    || id != close.id
                    || text(&value, "id")? != close.id
                    || !self.pending.is_empty()
                    || self.store.finite_active() != 1
                    || !self.event_roots.is_empty()
                {
                    return Err(unknown());
                }
                self.stop_ack = true;
                self.out.send(Output::Close).await.map_err(|_| unknown())?;
            }
            _ => return Err(unknown()),
        }
        Ok(())
    }
    async fn maybe_stop(&mut self) -> IvyResult<()> {
        if self.stop_sent
            || !self.pending.is_empty()
            || self.store.finite_active() != 1
            || !self.event_roots.is_empty()
        {
            return Ok(());
        }
        if let Some((grant, _)) = &self.close {
            self.stop_sent = true;
            self.send(
                grant.id.clone(),
                Json::object([
                    ("kind", "stop".into()),
                    ("id", grant.id.as_str().into()),
                    ("token", grant.token.as_str().into()),
                ]),
            )
            .await?;
        }
        Ok(())
    }
    async fn finish_close(&mut self) -> IvyResult<()> {
        if self.failed || !self.stop_ack {
            return Err(unknown());
        }
        let deadline = self.close_deadline.ok_or_else(unknown)?;
        let cooperative = self.cooperative_deadline.ok_or_else(unknown)?;
        let result = tokio::time::timeout_at(cooperative, self.child.completion.wait())
            .await
            .map_err(|_| unknown())?;
        if result.exit_code != Some(0)
            || result.error_code.is_some()
            || !tokio::time::timeout_at(deadline, process::owned_process_tree_stopped(&self.child))
                .await
                .map_err(|_| unknown())??
        {
            return Err(unknown());
        }
        tokio::time::timeout_at(deadline, &mut self.diagnostics)
            .await
            .map_err(|_| unknown())?
            .map_err(|_| unknown())??;
        self.diagnostics_joined = true;
        tokio::time::timeout_at(deadline, &mut self.writer)
            .await
            .map_err(|_| unknown())?
            .map_err(|_| unknown())??;
        self.writer_joined = true;
        tokio::time::timeout_at(deadline, &mut self.reader)
            .await
            .map_err(|_| unknown())?
            .map_err(|_| unknown())?;
        self.reader_joined = true;
        if !self.pending.is_empty()
            || !self.event_roots.is_empty()
            || self.store.finite_active() != 1
        {
            return Err(unknown());
        }
        let (grant, done) = self.close.as_ref().ok_or_else(unknown)?;
        let proof = Json::object([
            ("epoch", self.store.epoch.as_str().into()),
            ("closeOperation", grant.id.as_str().into()),
            ("scope", "candidate.inert.v1".into()),
            ("stopAck", true.into()),
            ("treeStopped", true.into()),
            ("protocolDrained", true.into()),
            ("diagnosticsDrained", true.into()),
            ("rootsSettled", self.pending.is_empty().into()),
            ("callbacksSettled", (self.store.finite_active() == 1).into()),
            (
                "eventsSettled",
                (self.pending.is_empty()
                    && self.event_roots.is_empty()
                    && self.store.finite_active() == 1)
                    .into(),
            ),
            (
                "process",
                Json::object([
                    ("exitCode", Json::Number(0.0)),
                    (
                        "signal",
                        result.signal.clone().map(Json::from).unwrap_or(Json::Null),
                    ),
                    ("errorCode", Json::Null),
                ]),
            ),
        ]);
        if self.fault == Fault::ProofBefore {
            return Err(unknown());
        }
        self.store.quiesce(grant, proof.clone())?;
        {
            let mut status = self.cleanup.lock().unwrap();
            status.tree_stopped = true;
            status.proof = true;
            status.running = false;
        }
        self.cleanup_finished.send_replace(true);
        if self.fault == Fault::ProofAfter {
            // Lost observer after durable proof must not erase that positive receipt.
            let result = Err(unknown());
            self.close_result = Some(result.clone());
            done.send_replace(Some(result));
            return Ok(());
        }
        let result = Ok(QuiescenceProof(proof));
        self.close_result = Some(result.clone());
        done.send_replace(Some(result));
        if let Some(done) = self.retire_wait.take() {
            let result = self.store.retire();
            if result.is_ok() {
                self.cleanup.lock().unwrap().retired = true;
            }
            done.send_replace(Some(result));
        }
        Ok(())
    }
    async fn fail(&mut self) {
        if self.failed {
            return;
        }
        self.failed = true;
        self.admission.store(true, Ordering::Release);
        self.store.fence();
        self.cleanup.lock().unwrap().running = true;
        for (_, pending) in self.pending.drain() {
            pending.result.send_replace(Some(Err(unknown())));
        }
        let result = Err(unknown());
        self.close_result = Some(result.clone());
        if let Some((_, done)) = &self.close {
            done.send_replace(Some(result));
        }
        if let Some(done) = self.retire_wait.take() {
            done.send_replace(Some(Err(unknown())));
        }
        self.background.clear();
        // Notification/caller deadlines do not cancel this original native stop.
        // Continue consuming failed protocol frames so its raw reader cannot
        // deadlock behind the bounded channel while the child/tree is reaped.
        let stopped = {
            let stop = self.child.stop(Duration::ZERO);
            tokio::pin!(stop);
            loop {
                tokio::select! {result=&mut stop=>break result,frame=self.reads.recv(),if !self.raw_drained=>{if !matches!(frame,Some(Ok(Some(_)))){self.raw_drained=true;}}}
            }
        };
        let tree = if stopped.is_ok() {
            process::owned_process_tree_stopped(&self.child)
                .await
                .unwrap_or(false)
        } else {
            false
        };
        {
            let mut status = self.cleanup.lock().unwrap();
            status.tree_stopped = tree;
            status.unavailable = !tree;
        }
        let _ = self.out.send(Output::Close).await;
        self.cleanup.lock().unwrap().running = !(self.reader.is_finished()
            && self.writer.is_finished()
            && self.diagnostics.is_finished());
    }
}
