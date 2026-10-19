use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    checked_native_contract::CheckedNativeContracts,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    journal_store::{Contracts, JournalStore},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    native_driver::{DriverRuntime, NativeDriver, NativeDriverRuntime, NativeDriverStore},
    native_intent::{NativeSlot, TaskPrompts},
    operations::{Document, Store},
    read_evidence::ReadEvidenceStore,
};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{Arc, Mutex},
};
const TIME: &str = "2026-01-02T03:04:05.006Z";
fn id(n: u64) -> JsString {
    format!("00000000-0000-4000-8000-{n:012}").into()
}
fn pin(n: u64, r: f64) -> Json {
    Json::object([
        ("objectId", Json::String(id(n))),
        ("revision", Json::Number(r)),
    ])
}
fn s(v: &Json, k: &str) -> JsString {
    v.required(k).unwrap().as_js_string().unwrap().clone()
}
fn merge(v: &Json, p: Option<&Json>) -> Json {
    let mut v = v.clone();
    if let Some(p) = p.and_then(Json::as_object) {
        for (k, x) in p {
            v.insert(k.clone(), x.clone());
        }
    }
    v
}
fn event(log: &RefCell<Vec<Json>>, items: impl IntoIterator<Item = Json>) {
    log.borrow_mut()
        .push(Json::Array(items.into_iter().collect()));
}
fn failure(code: JsString) -> IvyError {
    let c = code.to_utf8().unwrap();
    IvyError::new(&c, "Fixture driver unavailable.").with_outcome(if c == "outcome_unknown" {
        Outcome::Unknown
    } else {
        Outcome::NotExecuted
    })
}
struct Policy {
    allowed: Rc<RefCell<HashSet<String>>>,
}
impl Contracts for Policy {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, _: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &"1.0.0".into())
    }
    fn validate_record(&self, key: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        if key == "task-board/native-call" {
            super::native_intent::validate("NativeCall", v)
        } else if self.allowed.borrow().contains(&hash_json(v)?) {
            Ok(())
        } else {
            Err(IvyError::new(
                "invalid_arguments",
                "Fixture record not admitted.",
            ))
        }
    }
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        super::native_intent::validate(n, v)
    }
}
struct NoRpc;
impl RpcClient for NoRpc {
    fn request<'a>(&'a self, m: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        panic!("Driver fixture made unexpected native/Hive RPC: {m}")
    }
}
struct State {
    log: RefCell<Vec<Json>>,
    fault: RefCell<Json>,
    scopes: RefCell<HashMap<String, JsString>>,
    identities: Cell<u64>,
    named: Cell<u64>,
    operation: Option<JsString>,
}
impl State {
    fn take(&self, key: &str) -> Option<Json> {
        self.fault.borrow_mut().remove(key)
    }
}
impl NativeDriverRuntime for State {
    async fn verify_owner(&self) -> IvyResult<()> {
        event(&self.log, ["owner".into()]);
        if let Some(v) = self.fault.borrow().get("owner") {
            return Err(failure(v.as_js_string().unwrap().clone()));
        }
        Ok(())
    }
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString> {
        event(&self.log, ["scope".into(), scope.clone()]);
        if let Some(v) = self.fault.borrow().get("scope") {
            return Err(failure(v.as_js_string().unwrap().clone()));
        }
        let key = canonical(scope, 65536)?;
        let mut scopes = self.scopes.borrow_mut();
        Ok(scopes
            .entry(key)
            .or_insert_with(|| {
                let n = self.identities.get() + 1;
                self.identities.set(n);
                self.operation
                    .clone()
                    .unwrap_or_else(|| format!("native-operation-{n}").into())
            })
            .clone())
    }
    async fn native_update(&self, r: &Json) -> IvyResult<Json> {
        event(&self.log, ["update".into(), r.clone()]);
        if let Some(v) = self.take("update") {
            return Err(failure(v.as_js_string().unwrap().clone()));
        }
        Ok(Json::Null)
    }
    fn now(&self) -> JsString {
        event(&self.log, ["now".into()]);
        TIME.into()
    }
}
struct Trace<'a, S> {
    inner: &'a S,
    state: &'a State,
}
impl<S: NativeDriverStore> NativeDriverStore for Trace<'_, S> {
    async fn named_call(&self, name: &JsString) -> IvyResult<Option<Document>> {
        event(
            &self.state.log,
            [
                "named".into(),
                "task-board/native-call".into(),
                Json::String(name.clone()),
                Json::Null,
            ],
        );
        let n = self.state.named.get() + 1;
        self.state.named.set(n);
        if let Some(v) = self.state.take("named") {
            return Err(failure(v.as_js_string().unwrap().clone()));
        }
        if n > 1 {
            if let Some(v) = self.state.take("namedAfter") {
                return Err(failure(v.as_js_string().unwrap().clone()));
            }
        }
        self.inner.named_call(name).await
    }
}
impl<S: Store + ReadEvidenceStore> Store for Trace<'_, S> {
    fn root_object_id(&self) -> Option<JsString> {
        self.inner.root_object_id()
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        panic!("Unexpected driver version lookup")
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        panic!("Unexpected retired operation")
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        panic!("Unexpected local key lookup")
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("Unexpected workflow completion")
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        panic!("Driver lost UTF16 name")
    }
    async fn read(&self, key: &str, a: &Json, up: bool) -> IvyResult<Document> {
        event(
            &self.state.log,
            ["read".into(), key.into(), a.clone(), up.into()],
        );
        if key == "task-board/native-call" && a.as_js_string().is_none() {
            if let Some(v) = self.state.take("read") {
                return Err(failure(v.as_js_string().unwrap().clone()));
            }
        }
        Store::read(self.inner, key, a, up).await
    }
    async fn write(&self, key: &str, v: &Json, m: &str, d: &Json) -> IvyResult<Json> {
        event(
            &self.state.log,
            ["write".into(), key.into(), v.clone(), m.into(), d.clone()],
        );
        let f = self.state.take("write");
        if let Some(f) = f
            .as_ref()
            .filter(|f| f.get("commit") == Some(&Json::Bool(false)))
        {
            return Err(failure(s(f, "code")));
        }
        let p = Store::write(self.inner, key, v, m, d).await?;
        if let Some(f) = f {
            return Err(failure(s(&f, "code")));
        }
        Ok(p)
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("Unexpected mutation lookup")
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        panic!("Unexpected prepared write")
    }
}
impl<S: ReadEvidenceStore> BlobStore for Trace<'_, S> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<Json> {
        panic!("Preparation wrote blob")
    }
    fn read(&self, _: ChunkKey, _: &Json, _: &JsString) -> IvyResult<Vec<u8>> {
        panic!("Preparation read blob")
    }
}
impl<S: Store + ReadEvidenceStore> ReadEvidenceStore for Trace<'_, S> {
    fn validate(&self, n: &str, v: &Json) -> IvyResult<()> {
        event(&self.state.log, ["validate".into(), n.into()]);
        self.inner.validate(n, v)
    }
    async fn read_document(&self, _: &str, _: &Json, _: bool) -> IvyResult<Document> {
        panic!("Preparation read evidence")
    }
    async fn write_document(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Preparation wrote evidence")
    }
}
struct Prompts<'a>(&'a State);
impl TaskPrompts for Prompts<'_> {
    fn task_prompt(&self, t: &Json, o: &JsString, c: Option<&Json>) -> IvyResult<JsString> {
        event(
            &self.0.log,
            [
                "prompt".into(),
                "task".into(),
                t.clone(),
                Json::String(o.clone()),
                c.cloned()
                    .unwrap_or_else(|| Json::object([("undefined", true.into())])),
            ],
        );
        Ok(JsString(
            "driver prompt".encode_utf16().chain([0xd800]).collect(),
        ))
    }
    fn task_update_prompt(&self, t: &Json, o: &JsString, c: bool) -> IvyResult<JsString> {
        event(
            &self.0.log,
            [
                "prompt".into(),
                "update".into(),
                t.clone(),
                Json::String(o.clone()),
                c.into(),
            ],
        );
        Ok(JsString(
            "driver prompt".encode_utf16().chain([0xd800]).collect(),
        ))
    }
}
fn data() -> Json {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-native-driver-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn slot(v: &Json) -> NativeSlot {
    match s(v, "slot").to_utf8().unwrap().as_str() {
        "thread" => NativeSlot::Thread,
        "turn" => NativeSlot::Turn,
        "steer" => NativeSlot::Steer,
        "interrupt" => NativeSlot::Interrupt,
        _ => panic!("Unknown driver slot"),
    }
}
fn seed(
    j: &LocalJournal<'_>,
    allowed: &RefCell<HashSet<String>>,
    key: &str,
    n: u64,
    r: f64,
    v: &Json,
) {
    allowed.borrow_mut().insert(hash_json(v).unwrap());
    if r == 1.0 {
        j.create(key, &id(n), Some(&"root".into()), v, &TIME.into(), &id(n))
            .unwrap();
    } else {
        j.update(key, &id(n), r - 1.0, v, &TIME.into()).unwrap();
    }
}
fn calls(db: &Connection) -> Json {
    let mut stmt=db.prepare("SELECT d.id,d.name,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='task-board/native-call' ORDER BY d.rowid").unwrap();
    Json::Array(
        stmt.query_map([], |r| {
            let n: f64 = r.get(2)?;
            let body: String = r.get(3)?;
            Ok(Json::object([
                (
                    "pin",
                    Json::object([
                        ("objectId", Json::from(r.get::<_, String>(0)?)),
                        ("revision", Json::Number(n)),
                    ]),
                ),
                ("name", Json::from(r.get::<_, String>(1)?)),
                ("value", parse(&body, MANAGEMENT_FRAME_BYTES).unwrap()),
            ]))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap(),
    )
}
#[tokio::test(flavor = "current_thread")]
async fn durable_preparation_matches_original_replays_races_guards_and_uncertainty() {
    let groups = data();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut count = 0;
    for group in groups.as_array().unwrap() {
        let base = group.required("base").unwrap();
        for row in group.required("rows").unwrap().as_array().unwrap() {
            count += 1;
            let input = row.required("input").unwrap();
            let db = Connection::open_in_memory().unwrap();
            local_journal::initialize(&db).unwrap();
            let allowed = Rc::new(RefCell::new(HashSet::new()));
            let journal = LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            };
            let mut run = merge(base.required("run").unwrap(), input.get("run"));
            let task = merge(base.required("task").unwrap(), input.get("task"));
            if input.get("attached") == Some(&Json::Bool(true)) {
                let mut cs = run.required("calls").unwrap().clone();
                cs.insert("thread", pin(300, 1.0));
                run.insert("calls", cs);
            }
            seed(&journal, &allowed, "task-board/run", 200, 1.0, &run);
            seed(&journal, &allowed, "task-board/run", 200, 2.0, &run);
            seed(
                &journal,
                &allowed,
                "task-board/task",
                100,
                1.0,
                base.required("initial").unwrap(),
            );
            seed(&journal, &allowed, "task-board/task", 100, 2.0, &task);
            let existing = input.get("existing") == Some(&Json::Bool(true));
            let next = Cell::new(if existing { 301 } else { 300 });
            let store = JournalStore {
                journal,
                client: Arc::new(NoRpc),
                contracts: Policy {
                    allowed: allowed.clone(),
                },
                limits: HistoryLimits::default(),
                now: || TIME.to_owned(),
                new_id: || {
                    let n = next.get();
                    next.set(n + 1);
                    id(n)
                },
                cache: None,
            };
            let state = State {
                log: RefCell::default(),
                fault: RefCell::new(
                    input
                        .get("fault")
                        .cloned()
                        .unwrap_or_else(|| Json::object([] as [(&str, Json); 0])),
                ),
                scopes: RefCell::default(),
                identities: Cell::new(0),
                named: Cell::new(0),
                operation: input
                    .get("operationId")
                    .and_then(Json::as_js_string)
                    .cloned(),
            };
            let trace = Trace {
                inner: &store,
                state: &state,
            };
            let prompts = Prompts(&state);
            if existing {
                let intent = ivy_task_board::native_intent::NativeIntent {
                    store: &trace,
                    catalogs: &catalogs,
                };
                let mut value = intent
                    .intended_call(&run, NativeSlot::Thread, None, &prompts)
                    .await
                    .unwrap();
                let op = state
                    .operation
                    .clone()
                    .unwrap_or_else(|| "native-operation-1".into());
                let request = intent
                    .retain_native_request(value.required("request").unwrap(), &op)
                    .unwrap();
                for (k, v) in [
                    ("request", request),
                    ("origin", Json::object([("kind", "initial".into())])),
                    ("operationId", Json::String(op.clone())),
                    ("state", "prepared".into()),
                    ("preparedAt", TIME.into()),
                    ("observedAt", Json::Null),
                    ("ownerPhase", Json::Null),
                    ("epoch", Json::Null),
                    ("evidence", Json::Null),
                    ("code", Json::Null),
                ] {
                    value.insert(k, v);
                }
                let mut name = JsString::from("Native call ");
                name.0.extend(op.0);
                store
                    .journal
                    .create(
                        "task-board/native-call",
                        &name,
                        Some(&"root".into()),
                        &value,
                        &TIME.into(),
                        &id(300),
                    )
                    .unwrap();
            }
            state.log.borrow_mut().clear();
            let driver = NativeDriver {
                store: &trace,
                runtime: &state,
                prompts: &prompts,
                catalogs: &catalogs,
            };
            let mut outcomes = vec![];
            for step in input.required("steps").unwrap().as_array().unwrap() {
                match s(step, "kind").to_utf8().unwrap().as_str() {
                    "resetScope" => {
                        state.scopes.borrow_mut().clear();
                        event(&state.log, ["resetScope".into()]);
                    }
                    "attach" => {
                        let r = store
                            .journal
                            .read("task-board/run", &Json::String(id(200)), true)
                            .unwrap();
                        let t = store
                            .journal
                            .read("task-board/task", &Json::String(id(100)), true)
                            .unwrap();
                        let rev = r.pin.required("revision").unwrap().as_f64().unwrap() + 1.0;
                        let mut v = r.value;
                        let mut cs = v.required("calls").unwrap().clone();
                        cs.insert("thread", pin(300, 1.0));
                        v.insert("calls", cs);
                        seed(&store.journal, &allowed, "task-board/run", 200, rev, &v);
                        let mut v = t.value;
                        v.insert("claim", Json::object([("run", pin(200, rev))]));
                        seed(
                            &store.journal,
                            &allowed,
                            "task-board/task",
                            100,
                            t.pin.required("revision").unwrap().as_f64().unwrap() + 1.0,
                            &v,
                        );
                        event(&state.log, ["attach".into()]);
                    }
                    "prepare" => {
                        let r = driver
                            .prepare(&id(200), slot(step), step.get("origin"))
                            .await;
                        outcomes.push(match r {
                            Ok(v) => Json::object([("ok", v)]),
                            Err(e) => Json::object([(
                                "error",
                                Json::object([
                                    ("code", Json::String(e.code_js())),
                                    ("message", Json::String(e.message_js())),
                                    ("outcome", e.outcome.as_str().into()),
                                ]),
                            )]),
                        });
                    }
                    _ => panic!("Unknown driver step"),
                }
            }
            let actual = Json::object([
                ("outcomes", Json::Array(outcomes)),
                ("trace", Json::Array(state.log.into_inner())),
                ("calls", calls(&db)),
            ]);
            assert_eq!(
                hash_json(&actual).unwrap(),
                s(row, "expectedHash").to_utf8().unwrap(),
                "{} / {}: {}",
                s(group, "path").to_utf8().unwrap(),
                s(row, "name").to_utf8().unwrap(),
                canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
            );
            assert!(db.is_autocommit());
        }
    }
    assert_eq!(count, 164);
}

struct Client {
    requests: Mutex<Vec<Json>>,
}
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, p: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap()
                .push(Json::Array(vec![m.into(), p.clone()]));
            Ok(match m {
                "serviceNodes.get" => Json::object([
                    ("principalId", "workflow-owner".into()),
                    ("serviceName", "task-board".into()),
                    ("connected", true.into()),
                    ("synced", true.into()),
                ]),
                "system.status" => Json::object([("runtimeEpoch", "epoch".into())]),
                _ => panic!("Unexpected runtime/native RPC: {m}"),
            })
        })
    }
}
struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("ivy-native-driver-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn real_runtime_identity_survives_journal_reopen_on_its_live_connection() {
    let groups = data();
    let base = groups.as_array().unwrap()[0].required("base").unwrap();
    let temp = Temporary::new();
    let path = temp.0.join("journal.sqlite");
    let client = Arc::new(Client {
        requests: Mutex::default(),
    });
    let updates = Arc::new(Mutex::new(Vec::<Json>::new()));
    let runtime = DriverRuntime {
        client: client.clone(),
        principal_id: "workflow-owner".into(),
        service_node_id: "workflow-service".into(),
        native_update: |r: Json| {
            updates.lock().unwrap().push(r);
            std::future::ready(Ok(Json::Null))
        },
        now: || JsString::from(TIME),
    };
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let state = State {
        log: RefCell::default(),
        fault: RefCell::new(Json::Null),
        scopes: RefCell::default(),
        identities: Cell::new(0),
        named: Cell::new(0),
        operation: None,
    };
    let prompts = Prompts(&state);
    let mut original = None;
    for round in 0..2 {
        let db = Connection::open(&path).unwrap();
        local_journal::initialize(&db).unwrap();
        let allowed = Rc::new(RefCell::new(HashSet::new()));
        let journal = LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        };
        for (key, n, v) in [
            ("task-board/run", 200, base.required("run").unwrap()),
            ("task-board/task", 100, base.required("task").unwrap()),
        ] {
            allowed.borrow_mut().insert(hash_json(v).unwrap());
            if round == 0 {
                seed(&journal, &allowed, key, n, 1.0, v);
                seed(&journal, &allowed, key, n, 2.0, v);
            }
        }
        allowed
            .borrow_mut()
            .insert(hash_json(base.required("initial").unwrap()).unwrap());
        let store = JournalStore {
            journal,
            client: client.clone(),
            contracts: Policy { allowed },
            limits: HistoryLimits::default(),
            now: || TIME.to_owned(),
            new_id: || id(300),
            cache: None,
        };
        // Preserve a distinct exact Task revision for initial request preparation.
        if round == 0 {
            db.execute(
                "UPDATE revisions SET value_json=? WHERE id=? AND revision=1",
                ivy_store::rusqlite::params![
                    canonical(base.required("initial").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
                    id(100).to_utf8().unwrap()
                ],
            )
            .unwrap();
        }
        let driver = NativeDriver {
            store: &store,
            runtime: &runtime,
            prompts: &prompts,
            catalogs: &catalogs,
        };
        let before = db.total_changes();
        let result = driver
            .prepare(&id(200), NativeSlot::Thread, None)
            .await
            .unwrap();
        if let Some(old) = &original {
            assert_eq!(&result, old);
            assert_eq!(db.total_changes(), before);
        } else {
            original = Some(result);
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM documents WHERE key='task-board/native-call'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(calls(&db).as_array().unwrap().len(), 1);
    }
    let updates = updates.lock().unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(
        canonical(&updates[0], MANAGEMENT_FRAME_BYTES).unwrap(),
        canonical(&updates[1], MANAGEMENT_FRAME_BYTES).unwrap()
    );
    let scope = Json::Array(vec![
        "task-board-native-call".into(),
        "run-operation".into(),
        "thread".into(),
        Json::object([("kind", "initial".into())]),
    ]);
    let retained = runtime.scoped_operation_id(&scope).await.unwrap();
    let fresh = DriverRuntime {
        client: Arc::new(Client {
            requests: Mutex::default(),
        }),
        principal_id: "workflow-owner".into(),
        service_node_id: "workflow-service".into(),
        native_update: |_: Json| std::future::ready(Ok(Json::Null)),
        now: || JsString::from(TIME),
    };
    let fresh_id = fresh.scoped_operation_id(&scope).await.unwrap();
    assert_ne!(
        retained, fresh_id,
        "A new client must not be mistaken for the old live owner's cache"
    );
    {
        let requests = client.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.as_array().unwrap()[0] == Json::from("system.status"))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.as_array().unwrap()[0] == Json::from("serviceNodes.get"))
                .count(),
            2
        );
    }
    let wrong = DriverRuntime {
        client: client.clone(),
        principal_id: "wrong-owner".into(),
        service_node_id: "workflow-service".into(),
        native_update: |_: Json| std::future::ready(Ok(Json::Null)),
        now: || JsString::from(TIME),
    };
    assert_eq!(
        wrong.verify_owner().await.unwrap_err().code,
        "task_board_principal_mismatch"
    );
}
