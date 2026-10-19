use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome, json::parse};
use ivy_sdk::{
    clock::SystemClock,
    rpc::{RequestOptions, RpcClient},
    service::{InvocationContext, ServiceConnection, ServiceState},
};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    engine_invocations::EngineInvocations,
    engine_service::RetainedService,
    generation::GenerationRuntime,
    journal_store::Contracts,
    local_executor::LocalExecutor,
    local_journal::{LocalJournal, initialize},
    service::{GenerationFactory, GenerationLaunch, ServiceSetup, service as make_service},
    service_bridge::LocalHandler,
};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::Notify,
};
#[path = "task-board-service-engine.rs"]
mod engine;

fn json(text: &str) -> Json {
    parse(text, 32 * 1024 * 1024).unwrap()
}
fn n(value: &Json, key: &str) -> u64 {
    value.get(key).and_then(Json::as_f64).unwrap() as u64
}
async fn bounded<T>(work: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), work)
        .await
        .expect("TaskBoard service fixture did not settle")
}
async fn until(mut condition: impl FnMut() -> bool) {
    bounded(async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
}
struct Peer {
    child: Child,
    url: String,
    registry: Json,
    frames: Arc<Mutex<Vec<Json>>>,
    reader: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn start() -> Self {
        Self::start_with_engine(false).await
    }
    async fn start_with_engine(engine: bool) -> Self {
        let mut child = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-service-peer.mjs",
            ])
            .args(if engine { vec!["--engine"] } else { vec![] })
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let config = json(
            &bounded(lines.next_line())
                .await
                .unwrap()
                .expect("Loopback peer exited before returning its configuration; inspect stderr"),
        );
        let frames = Arc::new(Mutex::new(vec![]));
        let selected = frames.clone();
        let reader = tokio::spawn(async move {
            while let Some(line) = lines.next_line().await.unwrap() {
                selected.lock().unwrap().push(json(&line));
            }
        });
        Self {
            child,
            url: config.get("url").unwrap().as_str().unwrap(),
            registry: config.get("registry").unwrap().clone(),
            frames,
            reader,
        }
    }
    fn methods(&self, generation: u64) -> Vec<String> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|v| n(v, "generation") == generation)
            .filter_map(|v| v.get("method").and_then(|value| value.as_str().ok()))
            .collect()
    }
    async fn finish(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
        bounded(self.reader).await.unwrap();
    }
}
// Required finite admission port. This test never advertises the current owner
// schema/registry or exercises domain action validation through this port.
struct Admission {
    calls: Cell<usize>,
    refuse: bool,
}
impl Contracts for Admission {
    fn version(&self, _: &str) -> IvyResult<String> {
        panic!("No record version lookup in service construction")
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        panic!("No local record lookup in service construction")
    }
    fn readable_version(&self, _: &str, _: &JsString) -> IvyResult<bool> {
        panic!("No retained record lookup in service construction")
    }
    fn validate_record(&self, _: &str, _: &JsString, _: &Json) -> IvyResult<()> {
        panic!("No record admission in service construction")
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        assert_eq!(name, "Settings");
        self.calls.set(self.calls.get() + 1);
        if self.refuse {
            Err(failure("settings"))
        } else {
            assert_eq!(
                value.get("scheduler").unwrap().get("intervalMs"),
                Some(&Json::Number(1000.0))
            );
            Ok(())
        }
    }
}
fn failure(phase: &str) -> IvyError {
    IvyError::new("fixture_refusal", format!("Refused {phase}."))
        .with_details(Json::object([("phase", phase.into())]))
        .with_outcome(Outcome::Unknown)
}
fn setup(peer: &Peer, root: bool) -> ServiceSetup {
    let registry = peer.registry.clone();
    ServiceSetup {
        component_id: "task-board".into(),
        public_base_url: peer.url.clone(),
        credential: "fixture-token".into(),
        service_node_id: "fixture-task-board".into(),
        host_id: Some("fixture-host".into()),
        version: "1.0.0".into(),
        build_id: format!("sha256:{}", "a".repeat(64)).into(),
        settings: Json::object([
            (
                "scheduler",
                Json::object([("intervalMs", Json::Number(1000.0))]),
            ),
            (
                "rootObjectId",
                if root {
                    "fixture-root".into()
                } else {
                    Json::Null
                },
            ),
        ]),
        registry: Arc::new(move || {
            let value = registry.clone();
            Box::pin(async move { Ok(value) })
        }),
        tools: vec!["workspace".into()],
    }
}
#[derive(Default)]
struct Gate {
    open: Cell<bool>,
    changed: Notify,
}
impl Gate {
    fn release(&self) {
        self.open.set(true);
        self.changed.notify_waiters();
    }
    async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            if self.open.get() {
                return;
            }
            changed.await;
        }
    }
}
#[derive(Default)]
struct Plan {
    verify: Gate,
    call: Gate,
    tick: Gate,
    stop: Gate,
    drain: Gate,
    owner_error: bool,
    drain_error: bool,
}
impl Plan {
    fn opened() -> Self {
        let plan = Self::default();
        plan.verify.release();
        plan.call.release();
        plan.tick.release();
        plan.stop.release();
        plan.drain.release();
        plan
    }
}
struct Factory {
    plans: Vec<Plan>,
    events: RefCell<Vec<(u64, &'static str)>>,
    connections: RefCell<Vec<ServiceConnection>>,
}
impl Factory {
    fn event(&self, generation: u64, name: &'static str) {
        self.events.borrow_mut().push((generation, name));
    }
    fn count(&self, generation: u64, name: &str) -> usize {
        self.events
            .borrow()
            .iter()
            .filter(|(g, e)| *g == generation && *e == name)
            .count()
    }
    fn new(plans: Vec<Plan>) -> Self {
        Self {
            plans,
            events: RefCell::default(),
            connections: RefCell::default(),
        }
    }
}
struct Runtime<'a> {
    factory: &'a Factory,
    plan: &'a Plan,
    connection: &'a ServiceConnection,
    stopping: Cell<bool>,
}
impl Runtime<'_> {
    fn event(&self, name: &'static str) {
        self.factory.event(self.connection.generation(), name);
    }
}
impl GenerationRuntime for Runtime<'_> {
    fn recovered(&self) -> bool {
        true
    }
    fn diagnostics(&self) -> Vec<Json> {
        vec!["fixture diagnostic".into()]
    }
    fn task_changed(&self) {
        self.event("changed");
    }
    fn notification(&self, value: &Json) -> bool {
        assert_eq!(value.get("method"), Some(&"notifications.provider".into()));
        let params = value.get("params").unwrap();
        assert_eq!(params.get("namespace"), Some(&"agent".into()));
        assert_eq!(params.get("name"), Some(&"notification".into()));
        self.event("notification");
        true
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        self.event("verify");
        self.plan.verify.wait().await;
        if self.plan.owner_error {
            return Err(failure("owner"));
        }
        self.connection
            .request(
                "serviceNodes.get",
                Json::object([("serviceNodeId", "fixture-task-board".into())]),
                RequestOptions::default(),
            )
            .await?;
        Ok(())
    }
    async fn tick(&self) {
        assert!(!self.stopping.get());
        self.event("tick");
        self.plan.tick.wait().await;
        self.event("tick.done");
    }
    fn stop_admissions(&self) {
        self.stopping.set(true);
    }
    async fn stop(&self) {
        assert!(self.stopping.get());
        self.event("stop");
        self.plan.stop.wait().await;
        self.event("stop.done");
    }
}
struct Public<'a> {
    factory: &'a Factory,
    plan: &'a Plan,
    journal: &'a LocalJournal<'a>,
    generation: u64,
    calls: Cell<usize>,
}
impl LocalHandler for Public<'_> {
    async fn handle(&self, tool: &str, args: Json, context: InvocationContext) -> IvyResult<Json> {
        assert_eq!(tool, "workspace");
        assert_eq!(context.generation, self.generation);
        // Unpaired UTF16 reaches the handler exactly, without Rust UTF8 replacement.
        assert_eq!(context.caller_principal_id.0.last(), Some(&0xd800));
        assert_eq!(
            context.operation_id.as_ref().unwrap().0.last(),
            Some(&0xdfff)
        );
        self.calls.set(self.calls.get() + 1);
        let index = self.calls.get();
        self.factory.event(self.generation, "call");
        self.plan.call.wait().await;
        let id: JsString = format!("00000000-0000-4000-8000-{:012}", index).into();
        let name: JsString = format!("invocation-{index}").into();
        let pin = self.journal.create(
            "TaskBoard.Operation",
            &name,
            None,
            &args,
            &"2026-01-02T00:00:00.000Z".into(),
            &id,
        )?;
        let saved = self.journal.read("TaskBoard.Operation", &pin, true)?;
        assert_eq!(saved.value, args);
        self.factory.event(self.generation, "write");
        Ok(Json::object([
            ("args", args),
            ("generation", Json::Number(self.generation as f64)),
            ("caller", Json::String(context.caller_principal_id)),
            ("operation", Json::String(context.operation_id.unwrap())),
        ]))
    }
}
impl GenerationFactory for Factory {
    async fn run(&self, launch: GenerationLaunch) -> IvyResult<()> {
        let GenerationLaunch {
            connection,
            control,
            generation,
            activation,
        } = launch;
        let number = connection.generation();
        let plan = &self.plans[number as usize - 1];
        self.event(number, "create");
        self.connections.borrow_mut().push(connection.clone());
        let database = Connection::open_in_memory().unwrap();
        initialize(&database)?;
        let journal = LocalJournal {
            connection: &database,
            root_object_id: None,
        };
        let public = Public {
            factory: self,
            plan,
            journal: &journal,
            generation: number,
            calls: Cell::new(0),
        };
        let runtime = Runtime {
            factory: self,
            plan,
            connection: &connection,
            stopping: Cell::new(false),
        };
        let jobs = LocalExecutor::default();
        let invocations = EngineInvocations::new(&jobs);
        let handler = RetainedService {
            handler: &public,
            invocations: &invocations,
        };
        assert_eq!(
            activation
                .settings()
                .get("scheduler")
                .unwrap()
                .get("intervalMs"),
            Some(&Json::Number(1000.0))
        );
        let result = jobs
            .drive(activation.run(
                &control,
                generation,
                &handler,
                &runtime,
                &SystemClock,
                async {
                    invocations.stop().await;
                    self.event(number, "drain");
                    plan.drain.wait().await;
                    self.event(number, "drain.done");
                    if plan.drain_error {
                        Err(failure("drain"))
                    } else {
                        Ok(())
                    }
                },
            ))
            .await;
        drop(handler);
        drop(invocations);
        drop(jobs);
        drop(public);
        drop(journal);
        database.close().unwrap();
        self.event(number, "release");
        result
    }
}
fn context(generation: u64) -> InvocationContext {
    let mut caller = "caller-".encode_utf16().collect::<Vec<_>>();
    caller.push(0xd800);
    let mut operation = "operation-".encode_utf16().collect::<Vec<_>>();
    operation.push(0xdfff);
    InvocationContext {
        caller_principal_id: JsString(caller),
        operation_id: Some(JsString(operation)),
        generation,
        signal: RequestOptions::default().cancellation,
    }
}
async fn rpc(connection: &ServiceConnection, method: &str, args: Json) -> IvyResult<Json> {
    connection
        .request(
            method,
            args,
            RequestOptions {
                timeout: Duration::from_secs(5),
                ..Default::default()
            },
        )
        .await
}

#[tokio::test(flavor = "current_thread")]
async fn sdk_reconnect_keeps_prior_sqlite_owner_until_calls_tick_stop_and_drain_finish() {
    let peer = Peer::start().await;
    let admission = Admission {
        calls: Cell::new(0),
        refuse: false,
    };
    let (service, owner) = make_service(setup(&peer, true), &admission).unwrap();
    assert_eq!(admission.calls.get(), 1);
    fn send_sync<T: Send + Sync>(_: &T) {}
    send_sync(&service.control);
    send_sync(&service.control.handler("workspace"));
    let factory = Factory::new(vec![Plan::default(), Plan::opened()]);
    let script = async {
        service.start();
        until(|| factory.count(1, "verify") == 1).await;
        assert!(!service.client.ready());
        assert!(!peer.methods(1).contains(&"objects.stat".into()));
        let checking = factory.connections.borrow()[0].clone();
        assert_eq!(
            service
                .control
                .readiness(&checking)
                .await
                .unwrap_err()
                .message_js(),
            "Workflow state is not attached to this generation.".into()
        );
        let refused = service.control.handler("workspace")(Json::Null, context(1))
            .await
            .unwrap_err();
        assert_eq!(
            refused.message_js(),
            "TaskBoard has no reconciled current connection generation.".into()
        );
        factory.plans[0].verify.release();
        let first = bounded(service.client.wait_ready(None)).await.unwrap();
        until(|| factory.count(1, "tick") == 1).await;
        let remote = {
            let first = first.clone();
            tokio::spawn(async move {
                rpc(
                    &first,
                    "test.invoke",
                    Json::object([("value", "socket invocation".into())]),
                )
                .await
            })
        };
        until(|| factory.count(1, "call") == 1).await;
        let control = service.control.clone();
        std::thread::spawn(move || {
            drop(control.handler("workspace")(
                Json::object([("lost", true.into())]),
                context(1),
            ))
        })
        .join()
        .unwrap();
        until(|| factory.count(1, "call") == 2).await;
        rpc(
            &first,
            "test.notification",
            Json::object([("matched", true.into())]),
        )
        .await
        .unwrap();
        until(|| factory.count(1, "notification") == 1).await;
        assert_eq!(factory.count(1, "tick"), 1);
        rpc(&first, "test.close", Json::Null).await.unwrap();
        bounded(first.closed()).await;
        // SDK proceeds through the next real connection and registry sync while
        // the first local owner still retains its accepted work.
        until(|| peer.methods(2).contains(&"notifications.subscribe".into())).await;
        assert_eq!(factory.count(2, "create"), 0);
        assert_eq!(factory.count(1, "release"), 0);
        assert!(remote.await.unwrap().is_err());
        factory.plans[0].call.release();
        until(|| factory.count(1, "write") == 2 && factory.count(1, "stop") == 1).await;
        assert_eq!(factory.count(1, "drain"), 0);
        factory.plans[0].stop.release();
        until(|| factory.count(1, "stop.done") == 1).await;
        assert_eq!(factory.count(1, "drain"), 0);
        factory.plans[0].tick.release();
        until(|| factory.count(1, "drain") == 1).await;
        assert_eq!(factory.count(2, "create"), 0);
        factory.plans[0].drain.release();
        let second = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(second.generation(), 2);
        assert_eq!(factory.count(1, "release"), 1);
        assert_eq!(factory.count(2, "create"), 1);
        service.control.state(&ServiceState {
            status: "offline",
            generation: Some(1),
            code: None,
            message: None,
            phase: None,
        });
        let result = rpc(
            &second,
            "test.invoke",
            Json::object([("value", "next owner".into())]),
        )
        .await
        .unwrap();
        assert_eq!(n(result.get("result").unwrap(), "generation"), 2);
        let methods = peer.methods(1);
        let position = |name: &str| methods.iter().position(|s| s == name).unwrap();
        assert!(position("registry.sync") < position("notifications.subscribe"));
        assert!(position("notifications.subscribe") < position("serviceNodes.get"));
        assert!(position("serviceNodes.get") < position("objects.stat"));
        assert!(position("objects.stat") < position("service.heartbeat"));
        let frames = peer.frames.lock().unwrap();
        let open = frames
            .iter()
            .find(|v| v.get("kind") == Some(&"open".into()))
            .unwrap();
        assert_eq!(
            open.get("authorization"),
            Some(&"Bearer fixture-token".into())
        );
        assert_eq!(open.get("path"), Some(&"/ivy/ws".into()));
        let connect = frames
            .iter()
            .find(|v| v.get("method") == Some(&"service.connect".into()))
            .unwrap()
            .get("params")
            .unwrap();
        assert_eq!(connect.get("serviceName"), Some(&"task-board".into()));
        assert_eq!(connect.get("instanceMode"), Some(&"singleton".into()));
        let filter = frames
            .iter()
            .find(|v| v.get("method") == Some(&"notifications.subscribe".into()))
            .unwrap()
            .get("params")
            .unwrap()
            .get("filters")
            .unwrap();
        assert_eq!(
            filter,
            &json(r#"[{"namespace":"agent","name":"notification","version":"1.0.0"}]"#)
        );
        drop(frames);
        // Eager retirement and SDK stop continue after a lost close observer.
        drop(service.close());
        assert!(
            service.control.handler("workspace")(Json::Null, context(2))
                .await
                .is_err()
        );
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    result.unwrap();
    assert_eq!(factory.count(2, "release"), 1);
    let events = factory.events.borrow();
    assert!(
        events.iter().position(|v| *v == (1, "release")).unwrap()
            < events.iter().position(|v| *v == (2, "create")).unwrap()
    );
    drop(events);
    peer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn startup_refusal_and_failed_retirement_preserve_exact_errors_without_replacing_owner() {
    let peer = Peer::start().await;
    let admission = Admission {
        calls: Cell::new(0),
        refuse: true,
    };
    let mut invalid = setup(&peer, true);
    invalid.component_id = "wrong".into();
    assert_eq!(
        make_service(invalid, &admission).err().unwrap(),
        failure("settings")
    );
    assert_eq!(admission.calls.get(), 1);
    let admission = Admission {
        calls: Cell::new(0),
        refuse: false,
    };
    let mut invalid = setup(&peer, false);
    invalid.credential.clear();
    assert_eq!(
        make_service(invalid, &admission)
            .err()
            .unwrap()
            .message_js(),
        "TaskBoard requires its own component identity and Hive credential.".into()
    );
    let (service, owner) = make_service(setup(&peer, true), &admission).unwrap();
    let mut first = Plan::opened();
    first.owner_error = true;
    let mut second = Plan::opened();
    second.drain_error = true;
    let factory = Factory::new(vec![first, second]);
    let script = async {
        service.start();
        let ready = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(ready.generation(), 2);
        assert_eq!(factory.count(1, "release"), 1);
        assert!(!peer.methods(1).contains(&"objects.stat".into()));
        assert!(!peer.methods(1).contains(&"service.heartbeat".into()));
        let refused = factory.connections.borrow()[0].close_reason().unwrap();
        assert_eq!(refused, failure("owner"));
        rpc(&ready, "test.close", Json::Null).await.unwrap();
        until(|| factory.count(2, "release") == 1).await;
        until(|| peer.methods(3).contains(&"notifications.subscribe".into())).await;
        assert_eq!(factory.connections.borrow().len(), 2);
        assert_eq!(
            service.control.reconcile(ready).await.unwrap_err(),
            failure("drain")
        );
        assert_eq!(
            bounded(service.close()).await.unwrap_err(),
            failure("drain")
        );
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    assert_eq!(result.unwrap_err(), failure("drain"));
    assert_eq!(factory.count(2, "release"), 1);
    peer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn lost_reconcile_observer_still_installs_and_dropped_root_reports_unknown() {
    let peer = Peer::start().await;
    let admission = Admission {
        calls: Cell::new(0),
        refuse: false,
    };
    let (service, owner) = make_service(setup(&peer, false), &admission).unwrap();
    let factory = Factory::new(vec![Plan::opened()]);
    let script = async {
        service.start();
        let ready = bounded(service.client.wait_ready(None)).await.unwrap();
        // Reconcile a current connection directly, dropping the observation.
        // The local root retires and reconstructs the graph without a waiter.
        drop(service.control.reconcile(ready.clone()));
        until(|| factory.count(1, "create") == 2).await;
        bounded(async {
            loop {
                match service.control.readiness(&ready).await {
                    Ok(value) => {
                        assert_eq!(value.get("ready"), Some(&true.into()));
                        break;
                    }
                    Err(error) if error.code_js() == JsString::from("service_not_ready") => {
                        tokio::time::sleep(Duration::from_millis(2)).await
                    }
                    Err(error) => panic!("Unexpected readiness refusal: {error:?}"),
                }
            }
        })
        .await;
        let reply = service.control.handler("workspace")(
            Json::object([("lostReconcile", true.into())]),
            context(1),
        );
        assert_eq!(n(&reply.await.unwrap(), "generation"), 1);
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    result.unwrap();
    let (service, owner) = make_service(setup(&peer, false), &admission).unwrap();
    let closed = service.control.close();
    drop(owner);
    let error = closed.await.unwrap_err();
    assert_eq!(error.code_js(), "internal_error".into());
    assert_eq!(error.outcome, Outcome::Unknown);
    bounded(service.close()).await.unwrap_err();
    peer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn startup_root_refusal_prevents_readiness_and_preserves_rpc_error_before_reconnect() {
    let peer = Peer::start().await;
    let admission = Admission {
        calls: Cell::new(0),
        refuse: false,
    };
    let mut options = setup(&peer, true);
    options
        .settings
        .insert("rootObjectId", "missing-root".into());
    let (service, owner) = make_service(options, &admission).unwrap();
    let factory = Factory::new(vec![Plan::opened(), Plan::opened()]);
    let script = async {
        service.start();
        let ready = bounded(service.client.wait_ready(None)).await.unwrap();
        assert_eq!(ready.generation(), 2);
        assert_eq!(factory.count(1, "verify"), 1);
        assert_eq!(factory.count(1, "tick"), 0);
        assert_eq!(factory.count(1, "release"), 1);
        let error = factory.connections.borrow()[0].close_reason().unwrap();
        let expected = failure("root");
        assert_eq!(error.code_js(), expected.code_js());
        assert_eq!(error.message_js(), expected.message_js());
        assert_eq!(error.outcome, expected.outcome);
        assert_eq!(error.details, expected.details);
        let methods = peer.methods(1);
        assert!(methods.contains(&"objects.stat".into()));
        assert!(!methods.contains(&"service.heartbeat".into()));
        bounded(service.close()).await.unwrap();
    };
    let (result, ()) = bounded(async { tokio::join!(owner.run(&factory), script) }).await;
    result.unwrap();
    peer.finish().await;
}
