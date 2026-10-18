use ivy_agent_manager::{
    local_state::{LocalAgentState, Pin, SavedState},
    native_journal::{Identity, Limits, NativeJournal, Owner},
    native_operations::{
        NativeOperationStore, NativeOperations, SqliteOperationStore, StoreFuture,
    },
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let out = Command::new("node")
            .arg("tests/fixtures/agent-native-operations-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        parse(std::str::from_utf8(&out.stdout).unwrap(), 4 * 1024 * 1024).unwrap()
    })
}
fn field<'a>(v: &'a IvyJson, k: &str) -> &'a IvyJson {
    v.get(k).unwrap()
}
fn text<'a>(v: &'a IvyJson, k: &str) -> &'a JsString {
    field(v, k).as_js_string().unwrap()
}
fn string(v: &IvyJson, k: &str) -> String {
    text(v, k).to_utf8().unwrap()
}
fn boolean(v: &IvyJson, k: &str) -> bool {
    v.get(k) == Some(&IvyJson::Bool(true))
}
fn identity(v: &IvyJson) -> Identity {
    Identity::from_operation(v).unwrap()
}
fn id(operation_id: &str) -> Identity {
    Identity {
        caller_principal_id: "caller".into(),
        operation_id: operation_id.into(),
    }
}
fn outcome(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("result", value)]),
        Err(error) => IvyJson::object([("error", error.to_wire())]),
    }
}
fn equal(actual: &IvyJson, expected: &IvyJson, label: &str) {
    assert_eq!(
        canonical(actual, MANAGEMENT_FRAME_BYTES).unwrap(),
        canonical(expected, MANAGEMENT_FRAME_BYTES).unwrap(),
        "{label}"
    );
}
fn pin(value: &Pin) -> IvyJson {
    IvyJson::object([
        ("objectId", value.object_id.clone().into()),
        ("revision", IvyJson::Number(value.revision as f64)),
    ])
}
struct Failure {
    code: String,
    after_commit: bool,
}
#[derive(Default)]
struct StoreState {
    rows: BTreeMap<String, SavedState>,
    failure: Option<Failure>,
    fail_read: bool,
    writes: usize,
}
#[derive(Default)]
struct Store {
    state: Mutex<StoreState>,
    service_node_id: JsString,
}
impl Store {
    fn new() -> Self {
        Self {
            service_node_id: "agent".into(),
            ..Self::default()
        }
    }
}
impl NativeOperationStore for Store {
    fn service_node_id(&self) -> &JsString {
        &self.service_node_id
    }
    fn read(&self, key: String) -> StoreFuture<'_, Option<SavedState>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if std::mem::take(&mut state.fail_read) {
                return Err(IvyError::new(
                    "service_unavailable",
                    "Fixture read unavailable.",
                ));
            }
            Ok(state.rows.get(&key).cloned())
        })
    }
    fn write(
        &self,
        key: String,
        value: IvyJson,
        previous: Option<Pin>,
    ) -> StoreFuture<'_, SavedState> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.writes += 1;
            let failure = state.failure.take();
            if let Some(f) = &failure {
                if !f.after_commit {
                    return Err(IvyError::new(&f.code, "Fixture write unavailable."));
                }
            }
            let prior = state.rows.get(&key);
            if prior.map(|p| &p.pin) != previous.as_ref() {
                return Err(IvyError::new(
                    "revision_conflict",
                    "Another owner claimed the operation.",
                ));
            }
            let saved = SavedState {
                value,
                pin: Pin {
                    object_id: key.clone(),
                    revision: prior.map_or(1, |p| p.pin.revision + 1),
                },
            };
            state.rows.insert(key, saved.clone());
            if let Some(f) = failure {
                return Err(IvyError::new(f.code, "Fixture write unavailable."));
            }
            Ok(saved)
        })
    }
}
struct Owned {
    manager: NativeOperations,
    memory: Arc<Mutex<NativeJournal>>,
}
fn memory(epoch: Option<&JsString>) -> Arc<Mutex<NativeJournal>> {
    let source = field(data(), "owner");
    let mut memory = NativeJournal::with_clock(
        Owner {
            service_node_id: text(source, "serviceNodeId").clone(),
            native_version: text(source, "nativeVersion").clone(),
            native_executable_hash: text(source, "nativeExecutableHash").clone(),
        },
        Limits {
            max_operations: 100,
            max_journal_bytes: 128 * 1024 * 1024,
            max_pending_inputs: 16,
        },
        Arc::new(|| text(data(), "fixedTime").clone()),
    )
    .unwrap();
    if let Some(epoch) = epoch {
        memory.begin_epoch(epoch).unwrap();
    }
    Arc::new(Mutex::new(memory))
}
impl Owned {
    fn new(store: Arc<dyn NativeOperationStore>, epoch: Option<&JsString>) -> Self {
        let memory = memory(epoch);
        Self {
            manager: NativeOperations::new(store, memory.clone()),
            memory,
        }
    }
}
#[derive(Clone)]
struct Barrier {
    entered: watch::Sender<bool>,
    opened: watch::Sender<bool>,
}
impl Barrier {
    fn new() -> Self {
        Self {
            entered: watch::channel(false).0,
            opened: watch::channel(false).0,
        }
    }
    async fn wait(sender: &watch::Sender<bool>) {
        let mut receiver = sender.subscribe();
        loop {
            if *receiver.borrow_and_update() {
                return;
            }
            receiver.changed().await.unwrap();
        }
    }
    async fn block(&self) {
        self.entered.send_replace(true);
        Self::wait(&self.opened).await;
    }
}
struct Handle {
    result: watch::Receiver<Option<IvyJson>>,
}
impl Handle {
    fn track(work: impl Future<Output = IvyResult<IvyJson>> + Send + 'static) -> Self {
        let (sender, result) = watch::channel(None);
        tokio::spawn(async move {
            sender.send_replace(Some(outcome(work.await)));
        });
        Self { result }
    }
    async fn wait(&self) -> IvyJson {
        let mut receiver = self.result.clone();
        loop {
            if let Some(value) = receiver.borrow_and_update().clone() {
                return value;
            }
            receiver.changed().await.unwrap();
        }
    }
}
struct Fixture {
    store: Arc<Store>,
    owners: HashMap<String, Owned>,
    handles: HashMap<String, Handle>,
    gates: HashMap<String, Barrier>,
    effects: Arc<Mutex<Vec<IvyJson>>>,
    epoch: Option<JsString>,
}
impl Fixture {
    fn new(row: &IvyJson) -> Self {
        let epoch = match row.get("epoch") {
            Some(IvyJson::Null) => None,
            Some(v) => Some(v.as_js_string().unwrap().clone()),
            None => Some("native-epoch".into()),
        };
        let mut f = Self {
            store: Arc::new(Store::new()),
            owners: HashMap::new(),
            handles: HashMap::new(),
            gates: HashMap::new(),
            effects: Arc::new(Mutex::new(Vec::new())),
            epoch,
        };
        if let Some(names) = row.get("owners") {
            for name in names.as_array().unwrap() {
                f.add_owner(name.as_str().unwrap());
            }
        } else {
            f.add_owner("a".into());
        }
        f
    }
    fn add_owner(&mut self, name: String) {
        self.owners
            .insert(name, Owned::new(self.store.clone(), self.epoch.as_ref()));
    }
    fn barrier(&mut self, name: &str) -> Barrier {
        self.gates
            .entry(name.into())
            .or_insert_with(Barrier::new)
            .clone()
    }
    async fn execute(&mut self, c: &IvyJson) -> IvyResult<IvyJson> {
        let owner = c
            .get("owner")
            .map(|v| v.as_str().unwrap())
            .unwrap_or("a".into());
        let owned = &self.owners[&owner];
        let manager = owned.manager.clone();
        let memory = owned.memory.clone();
        match string(c, "action").as_str() {
            "owner" => self.add_owner(string(c, "name")),
            "start" => {
                let c = c.clone();
                let store = self.store.clone();
                let effects = self.effects.clone();
                let barrier = c.get("gate").map(|v| self.barrier(&v.as_str().unwrap()));
                let action_manager = manager.clone();
                let action_memory = memory.clone();
                let operation_identity = identity(field(&c, "identity"));
                let action_c = c.clone();
                let action = move || async move {
                    let c = action_c;
                    let identity = identity(field(&c, "identity"));
                    let key = hash_json(&identity.value())?;
                    let intent = store
                        .state
                        .lock()
                        .unwrap()
                        .rows
                        .get(&key)
                        .expect("native action requires a durable claim")
                        .value
                        .clone();
                    effects.lock().unwrap().push(IvyJson::object([
                        ("owner", field(&c, "owner").clone()),
                        ("identity", identity.value()),
                        ("intent", intent),
                    ]));
                    let behavior = string(&c, "behavior");
                    let epoch = action_memory.lock().unwrap().epoch().cloned();
                    let request_id = IvyJson::from(action_manager.request_id(&identity)?);
                    let failed = || IvyError::new("fixture_lost", "Fixture action failed.");
                    if behavior == "reject" {
                        return Err(IvyError::new(
                            "service_not_ready",
                            "Provider connection is closed.",
                        ));
                    }
                    if behavior == "clearThrow" {
                        action_memory.lock().unwrap().close();
                        return Err(failed());
                    }
                    if behavior == "raw" {
                        return Ok(field(&c, "raw").clone());
                    }
                    if boolean(&c, "beforeGate") {
                        if let Some(b) = &barrier {
                            b.block().await;
                        }
                    }
                    let epoch = epoch.ok_or_else(IvyError::internal)?;
                    if behavior == "answer" {
                        let params = field(&c, "params");
                        action_memory.lock().unwrap().dispatch_answer(
                            &identity,
                            field(params, "identity"),
                            field(params, "reply"),
                        )?;
                    } else {
                        action_memory
                            .lock()
                            .unwrap()
                            .dispatch(&identity, &epoch, &request_id)?;
                    }
                    if !boolean(&c, "beforeGate") {
                        if let Some(b) = &barrier {
                            b.block().await;
                        }
                    }
                    match behavior.as_str() {
                        "lost" => return Err(failed()),
                        "forgetThrow" => {
                            action_memory.lock().unwrap().forget(&identity)?;
                            return Err(failed());
                        }
                        "unknownThrow" => {
                            action_memory
                                .lock()
                                .unwrap()
                                .lose_epoch(&epoch, &"fixture_lost".into())?;
                            return Err(failed());
                        }
                        "returnLive" => {
                            return action_memory
                                .lock()
                                .unwrap()
                                .get(&identity)
                                .map(Option::unwrap);
                        }
                        _ => {}
                    }
                    let reply = IvyJson::object([(
                        "result",
                        IvyJson::object([
                            ("turn", IvyJson::object([("id", "managed-turn".into())])),
                            ("thread", "original".into()),
                        ]),
                    )]);
                    let id = if behavior == "answer" {
                        field(field(field(&c, "params"), "identity"), "requestId")
                    } else {
                        &request_id
                    };
                    let value = action_memory
                        .lock()
                        .unwrap()
                        .finish(&identity, &epoch, id, &reply)?;
                    if behavior == "terminalWriteFailure" {
                        store.state.lock().unwrap().failure = Some(Failure {
                            code: "service_unavailable".into(),
                            after_commit: false,
                        });
                    }
                    if behavior == "terminalThrow" {
                        return Err(failed());
                    }
                    Ok(value)
                };
                let run = manager.run(
                    operation_identity,
                    text(&c, "method").clone(),
                    field(&c, "params").clone(),
                    action,
                    boolean(&c, "prevent"),
                )?;
                self.handles
                    .insert(string(&c, "handle"), Handle::track(run.wait()));
            }
            "wait" => return Ok(self.handles[&string(c, "handle")].wait().await),
            "entered" => Barrier::wait(&self.barrier(&string(c, "gate")).entered).await,
            "release" => {
                self.barrier(&string(c, "gate")).opened.send_replace(true);
            }
            "pending" => {
                // Let ready work settle, without releasing the gate under test.
                tokio::time::sleep(Duration::from_millis(2)).await;
                return Ok(IvyJson::Bool(
                    self.handles[&string(c, "handle")].result.borrow().is_none(),
                ));
            }
            "drop" => {
                self.handles.remove(&string(c, "handle"));
            }
            "settle" => {
                let identity = identity(field(c, "identity"));
                self.handles.insert(
                    string(c, "handle"),
                    Handle::track(async move {
                        manager.settle(&identity).await?;
                        Ok(IvyJson::Null)
                    }),
                );
            }
            "settleTurn" => {
                let thread_id = text(c, "threadId").clone();
                self.handles.insert(
                    string(c, "handle"),
                    Handle::track(async move {
                        manager.settle_turn(&thread_id).await;
                        Ok(IvyJson::Null)
                    }),
                );
            }
            "read" => {
                return manager
                    .read(&identity(field(c, "identity")))
                    .await
                    .map(|v| v.unwrap_or(IvyJson::Null));
            }
            "clear" => memory.lock().unwrap().close(),
            "failure" => {
                self.store.state.lock().unwrap().failure = Some(Failure {
                    code: string(c, "code"),
                    after_commit: boolean(c, "afterCommit"),
                })
            }
            "readFailure" => self.store.state.lock().unwrap().fail_read = true,
            "patch" => {
                let key = hash_json(&id(&string(c, "id")).value())?;
                let mut state = self.store.state.lock().unwrap();
                let saved = state.rows.get_mut(&key).unwrap();
                for (key, value) in field(c, "patch").as_object().unwrap() {
                    saved.value.insert(key.to_utf8().unwrap(), value.clone());
                }
                if boolean(c, "bump") {
                    saved.pin.revision += 1;
                }
            }
            "memoryAccept" => {
                memory.lock().unwrap().accept(
                    &identity(field(c, "identity")),
                    &"thread/start".into(),
                    &IvyJson::object([] as [(&str, IvyJson); 0]),
                )?;
            }
            "observe" => {
                memory.lock().unwrap().observe_input(
                    field(c, "identity"),
                    &"item/tool/requestUserInput".into(),
                    &IvyJson::object([("threadId", "thread".into())]),
                )?;
            }
            "snapshot" => {
                let state = self.store.state.lock().unwrap();
                return Ok(IvyJson::object([
                    (
                        "rows",
                        IvyJson::Array(
                            state
                                .rows
                                .iter()
                                .map(|(key, saved)| {
                                    IvyJson::object([
                                        ("key", key.clone().into()),
                                        ("value", saved.value.clone()),
                                        ("pin", pin(&saved.pin)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "effects",
                        IvyJson::Array(self.effects.lock().unwrap().clone()),
                    ),
                    ("writes", IvyJson::Number(state.writes as f64)),
                ]));
            }
            action => panic!("unknown fixture action {action}"),
        }
        Ok(IvyJson::Null)
    }
}

#[tokio::test]
async fn durable_operation_traces_match_original_coordinator() {
    let rows = field(data(), "cases").as_array().unwrap();
    assert_eq!(rows.len(), 41);
    for row in rows {
        let name = string(row, "name");
        let mut fixture = Fixture::new(row);
        for (index, command) in field(row, "commands")
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let label = format!("{name}, step {index}: {}", string(command, "action"));
            let result = tokio::time::timeout(Duration::from_secs(5), fixture.execute(command))
                .await
                .expect(&label);
            let actual = if string(command, "action") == "wait" {
                result.unwrap()
            } else {
                outcome(result)
            };
            equal(
                &actual,
                &field(row, "expected").as_array().unwrap()[index],
                &label,
            );
        }
        for handle in fixture.handles.values() {
            tokio::time::timeout(Duration::from_secs(5), handle.wait())
                .await
                .unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_claims_precede_effects_and_completion_owner_survives_restart() {
    let f = super::Fixture::new();
    let state = Arc::new(Mutex::new(f.open(10, 128 * 1024 * 1024)));
    let store = Arc::new(SqliteOperationStore::new(state.clone()).unwrap());
    let owned = Owned::new(store.clone(), Some(&"native-epoch".into()));
    let identity = id(&format!(
        "epoch:{}:turn",
        chrono::Utc::now().timestamp_millis()
    ));
    let manager = owned.manager.clone();
    let memory = owned.memory.clone();
    let action_id = identity.clone();
    let action_store = store.clone();
    let action_manager = manager.clone();
    let barrier = Barrier::new();
    let action_gate = barrier.clone();
    let params = IvyJson::object([("threadId", "managed-thread".into())]);
    let run = manager
        .run(
            identity.clone(),
            "turn/start".into(),
            params.clone(),
            move || async move {
                let saved = action_store
                    .read(hash_json(&action_id.value())?)
                    .await?
                    .unwrap();
                assert_eq!(saved.value.get("phase"), Some(&IvyJson::from("dispatched")));
                assert_eq!(
                    saved.value.get("requestId"),
                    Some(&IvyJson::from(action_manager.request_id(&action_id)?))
                );
                let request_id = action_manager.request_id(&action_id)?.into();
                memory
                    .lock()
                    .unwrap()
                    .dispatch(&action_id, &"native-epoch".into(), &request_id)?;
                action_gate.block().await;
                let reply = IvyJson::object([(
                    "result",
                    IvyJson::object([("turn", IvyJson::object([("id", "managed-turn".into())]))]),
                )]);
                let result = memory.lock().unwrap().finish(
                    &action_id,
                    &"native-epoch".into(),
                    &request_id,
                    &reply,
                )?;
                Ok(result)
            },
            false,
        )
        .unwrap();
    Barrier::wait(&barrier.entered).await;
    let settled = Arc::new(AtomicBool::new(false));
    let complete = settled.clone();
    let completion_manager = manager.clone();
    let completion = tokio::spawn(async move {
        completion_manager
            .settle_turn(&"managed-thread".into())
            .await;
        complete.store(true, Ordering::SeqCst);
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!settled.load(Ordering::SeqCst));
    let duplicate = manager
        .run(
            identity.clone(),
            "turn/start".into(),
            params.clone(),
            || async { panic!("duplicate must not execute") },
            false,
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(duplicate.get("phase"), Some(&IvyJson::from("dispatched")));
    barrier.opened.send_replace(true);
    let saved = run.wait().await.unwrap();
    completion.await.unwrap();
    assert_eq!(
        state
            .lock()
            .unwrap()
            .completion_caller("managed-thread", "managed-turn")
            .unwrap()
            .as_deref(),
        Some("caller")
    );
    assert_eq!(
        state
            .lock()
            .unwrap()
            .completion_caller("managed-thread", "other")
            .unwrap(),
        None
    );
    drop(owned);
    drop(manager);
    drop(store);
    close_state(state).await;
    let state = Arc::new(Mutex::new(f.open(10, 128 * 1024 * 1024)));
    assert_eq!(
        state
            .lock()
            .unwrap()
            .completion_caller("managed-thread", "managed-turn")
            .unwrap()
            .as_deref(),
        Some("caller")
    );
    let restarted = Owned::new(
        Arc::new(SqliteOperationStore::new(state.clone()).unwrap()),
        Some(&"replacement".into()),
    );
    let replay = restarted
        .manager
        .run(
            identity,
            "turn/start".into(),
            params,
            || async { panic!("replay must not execute") },
            false,
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    equal(&replay, &saved, "persisted result across SQLite reopen");
    drop(restarted);
    close_state(state).await;
}

async fn close_state(state: Arc<Mutex<LocalAgentState>>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        // The receipt is published just before the owner task drops its captures.
        while Arc::strong_count(&state) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    Arc::try_unwrap(state)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap()
        .close()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn separate_sqlite_connections_allow_one_dispatch_and_restart_keeps_uncertainty() {
    let f = super::Fixture::new();
    let first_state = Arc::new(Mutex::new(f.open(10, 128 * 1024 * 1024)));
    let second_state = Arc::new(Mutex::new(f.open(10, 128 * 1024 * 1024)));
    let first = Owned::new(
        Arc::new(SqliteOperationStore::new(first_state.clone()).unwrap()),
        Some(&"first".into()),
    );
    let second = Owned::new(
        Arc::new(SqliteOperationStore::new(second_state.clone()).unwrap()),
        Some(&"second".into()),
    );
    let identity = id(&format!(
        "epoch:{}:race",
        chrono::Utc::now().timestamp_millis()
    ));
    let sends = Arc::new(AtomicUsize::new(0));
    let race = Arc::new(tokio::sync::Barrier::new(2));
    let mut work = Vec::new();
    for owned in [first, second] {
        let race = race.clone();
        let sends = sends.clone();
        let identity = identity.clone();
        work.push(tokio::spawn(async move {
            race.wait().await;
            let manager = owned.manager.clone();
            let request_id = manager.request_id(&identity)?.into();
            let memory = owned.memory.clone();
            let action_id = identity.clone();
            let run = manager.run(
                identity,
                "thread/start".into(),
                IvyJson::object([] as [(&str, IvyJson); 0]),
                move || async move {
                    sends.fetch_add(1, Ordering::SeqCst);
                    let epoch = memory.lock().unwrap().epoch().unwrap().clone();
                    memory
                        .lock()
                        .unwrap()
                        .dispatch(&action_id, &epoch, &request_id)?;
                    Err(IvyError::internal()) // Simulated owner loss after dispatch.
                },
                false,
            )?;
            let _ = run.wait().await;
            Ok::<_, IvyError>(())
        }));
    }
    for call in work {
        call.await.unwrap().unwrap();
    }
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    close_state(first_state).await;
    close_state(second_state).await;
    let state = Arc::new(Mutex::new(f.open(10, 128 * 1024 * 1024)));
    let owner = Owned::new(
        Arc::new(SqliteOperationStore::new(state.clone()).unwrap()),
        Some(&"new".into()),
    );
    let result = owner.manager.read(&identity).await.unwrap().unwrap();
    assert_eq!(result.get("phase"), Some(&IvyJson::from("outcome_unknown")));
    assert_eq!(
        result.get("code"),
        Some(&IvyJson::from("native_connection_lost"))
    );
    let key = hash_json(&identity.value()).unwrap();
    assert_eq!(
        state
            .lock()
            .unwrap()
            .read("operation", &key)
            .unwrap()
            .unwrap()
            .value
            .get("phase"),
        Some(&IvyJson::from("dispatched"))
    );
    drop(owner);
    close_state(state).await;
}
