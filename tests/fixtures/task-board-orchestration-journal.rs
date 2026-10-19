use super::*;
use ivy_contracts::builtin_schema::SchemaBundle;
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    inspection_journal::InspectionJournal,
    journal_store::{Contracts, JournalStore},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    native_comments::{NativeCommentStore, sync_native_comments},
    native_driver::{DriverRuntime, NativeDriver},
    native_inspector::{InspectionStore, NativeInspector},
    native_intent::TaskPrompts,
    native_orchestrator::NativeServices,
    native_turn_evidence::TurnEvidence,
    read_evidence::ReadEvidenceStore,
};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

fn validate(name: &str, v: &Json) -> IvyResult<()> {
    static B: OnceLock<SchemaBundle> = OnceLock::new();
    let b = B.get_or_init(|| {
        SchemaBundle::new(&[
            include_str!("../../specs/schemas/task-board.schema.json"),
            include_str!("../../specs/schemas/chat.schema.json"),
            include_str!("../../specs/schemas/agent.schema.json"),
            include_str!("../../specs/schemas/hive-wire.schema.json"),
        ])
        .unwrap()
    });
    canonical(v, MANAGEMENT_FRAME_BYTES)?;
    if b.matches(
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{name}"),
        v,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Unexpected finite fixture record.",
        ))
    }
}
/// Task/Run values are finite callback fixtures, not current engine admission.
struct Policy(RefCell<HashSet<String>>);
impl Policy {
    fn admit(&self, v: &Json) {
        self.0.borrow_mut().insert(hash_json(v).unwrap());
    }
}
impl Contracts for Policy {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, _: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &JsString::from("1.0.0"))
    }
    fn validate_record(&self, k: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        if ["task-board/task", "task-board/run"].contains(&k) {
            assert!(
                self.0.borrow().contains(&hash_json(v)?),
                "Unadmitted Task/Run"
            );
            return Ok(());
        }
        validate(
            match k {
                "task-board/native-read-evidence" => "NativeReadEvidence",
                "task-board/native-turn-snapshot" => "NativeTurnSnapshot",
                _ => panic!("Unexpected fixture contract {k}"),
            },
            v,
        )
    }
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        validate(n, v)
    }
}
struct Client {
    row: Json,
    catalog: Json,
    unsupported: bool,
    owner_valid: AtomicBool,
    methods: Mutex<Vec<Json>>,
    reads: Mutex<usize>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.methods
                .lock()
                .unwrap()
                .push(Json::Array(vec![method.into(), params.clone()]));
            if method == "serviceNodes.get" {
                assert_eq!(
                    params.required("serviceNodeId")?,
                    &Json::from("task-board-node")
                );
                return Ok(Json::object([
                    (
                        "principalId",
                        if self.owner_valid.load(Ordering::Relaxed) {
                            "caller"
                        } else {
                            "replacement"
                        }
                        .into(),
                    ),
                    ("serviceName", "task-board".into()),
                    ("connected", true.into()),
                    ("synced", true.into()),
                ]));
            }
            if method == "tools.list" {
                let name = params.required("namePrefix")?.as_str()?;
                return Ok(Json::object([
                    (
                        "provider",
                        Json::object([(
                            "node",
                            Json::object([("serviceNodeId", "native-agent".into())]),
                        )]),
                    ),
                    (
                        "items",
                        Json::Array(vec![Json::object([
                            ("qualifiedName", format!("agent.{name}").into()),
                            (
                                "definition",
                                Json::object([
                                    ("namespace", "agent".into()),
                                    ("name", name.into()),
                                    ("interfaceVersion", "1.0.0".into()),
                                    ("annotations", Json::object([("readOnlyHint", true.into())])),
                                    ("inputSchema", Json::Object(vec![])),
                                    ("outputSchema", Json::Object(vec![])),
                                ]),
                            ),
                            (
                                "definitionHash",
                                format!("sha256:{}", "a".repeat(64)).into(),
                            ),
                            ("catalogHash", format!("sha256:{}", "b".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", Json::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            let name = params.required("qualifiedName")?.as_str()?;
            match name.as_str() {
                "agent.status" => Ok(Json::object([
                    ("serviceNodeId", "native-agent".into()),
                    ("hostId", "fixture".into()),
                    ("nativeVersion", "0.159.2".into()),
                    (
                        "nativeExecutableHash",
                        self.catalog.required("nativeExecutableHash")?.clone(),
                    ),
                    (
                        "catalogHash",
                        self.row
                            .required("metadata")?
                            .required("catalogHash")?
                            .clone(),
                    ),
                    ("epoch", "epoch".into()),
                    ("pid", 42.0.into()),
                    ("state", "ready".into()),
                    ("observedAt", Json::String(s(data(), "time").clone())),
                    ("code", Json::Null),
                    ("initialized", Json::Object(vec![])),
                    ("pendingInputs", 0.0.into()),
                    (
                        "operations",
                        Json::object([
                            ("retained", 0.0.into()),
                            ("maximum", 100.0.into()),
                            ("bytes", 0.0.into()),
                            ("maximumBytes", 1048576.0.into()),
                        ]),
                    ),
                    ("observedMethods", Json::Array(vec![])),
                ])),
                "agent.inputs" => Ok(Json::object([
                    ("items", Json::Array(vec![])),
                    ("nextCursor", Json::Null),
                ])),
                "agent.read" => {
                    let a = params.required("arguments")?;
                    let method = s(a, "method");
                    let mut v = self.row.required("metadata")?.clone();
                    let mut count = self.reads.lock().unwrap();
                    *count += 1;
                    v.insert("observationId", format!("read-{}", *count).into());
                    v.insert("requestId", Json::Number(*count as f64));
                    v.insert("method", Json::String(method.clone()));
                    v.insert("params", a.required("params")?.clone());
                    v.insert(
                        "requestHash",
                        hash_json(&Json::object([
                            ("method", Json::String(method.clone())),
                            ("params", a.required("params")?.clone()),
                        ]))?
                        .into(),
                    );
                    // All observations follow the fixed snapshot clock.
                    v.insert(
                        "observedAt",
                        format!("2026-01-02T03:04:05.{:03}Z", 10 + *count).into(),
                    );
                    v.insert(
                        "reply",
                        match method.to_utf8().unwrap().as_str() {
                            "thread/read" => {
                                assert_eq!(
                                    a.required("params")?.get("includeTurns"),
                                    Some(&false.into())
                                );
                                self.row.required("metadata")?.required("reply")?.clone()
                            }
                            "thread/turns/list" => {
                                assert_eq!(
                                    a.required("params")?.get("itemsView"),
                                    Some(&"notLoaded".into())
                                );
                                self.row.required("turns")?.required("reply")?.clone()
                            }
                            "thread/items/list" => {
                                assert_eq!(a.required("params")?.get("limit"), Some(&1.0.into()));
                                assert_eq!(
                                    a.required("params")?.get("sortDirection"),
                                    Some(&"desc".into())
                                );
                                if self.unsupported {
                                    Json::object([(
                                        "error",
                                        Json::object([
                                            ("code", (-32601.0).into()),
                                            ("message", "unsupported".into()),
                                        ]),
                                    )])
                                } else {
                                    Json::object([(
                                        "result",
                                        Json::object([
                                            (
                                                "data",
                                                Json::Array(vec![Json::object([
                                                    ("turnId", "turn".into()),
                                                    (
                                                        "item",
                                                        Json::object([
                                                            ("id", "answer".into()),
                                                            ("type", "agentMessage".into()),
                                                            ("phase", "final_answer".into()),
                                                            ("text", "answer".into()),
                                                        ]),
                                                    ),
                                                ])]),
                                            ),
                                            ("nextCursor", Json::Null),
                                        ]),
                                    )])
                                }
                            }
                            other => panic!("Unbounded or unexpected native read {other}"),
                        },
                    );
                    Ok(v)
                }
                other => panic!("Unexpected native operation {other}"),
            }
        })
    }
}
struct Comments<'a, S>(&'a S);
impl<S: BlobStore<Pin = Json, Error = IvyError>> BlobStore for Comments<'_, S> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, k: ChunkKey, n: &str, p: &JsString, b: &[u8]) -> IvyResult<Json> {
        self.0.save(k, n, p, b)
    }
    fn read(&self, k: ChunkKey, p: &Json, r: &JsString) -> IvyResult<Vec<u8>> {
        self.0.read(k, p, r)
    }
}
impl<S: ReadEvidenceStore> ReadEvidenceStore for Comments<'_, S> {
    fn validate(&self, n: &str, v: &Json) -> IvyResult<()> {
        self.0.validate(n, v)
    }
    async fn read_document(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        self.0.read_document(k, a, p).await
    }
    async fn write_document(&self, k: &str, v: &Json, m: &str, d: &Json) -> IvyResult<Json> {
        self.0.write_document(k, v, m, d).await
    }
}
impl<S: InspectionStore> InspectionStore for Comments<'_, S> {
    async fn verify_owner(&self) -> IvyResult<()> {
        self.0.verify_owner().await
    }
    async fn named_document(&self, k: &str, n: &str, p: &JsString) -> IvyResult<Option<Document>> {
        self.0.named_document(k, n, p).await
    }
    fn tools(&self) -> &ToolClients {
        self.0.tools()
    }
    fn principal_id(&self) -> JsString {
        self.0.principal_id()
    }
    fn now(&self) -> JsString {
        self.0.now()
    }
}
impl<S: InspectionStore> NativeCommentStore for Comments<'_, S> {
    async fn native_comment(&self, _: &Json) -> IvyResult<()> {
        panic!("Empty native input fixture cannot publish comments")
    }
}
type Journal<'db> = JournalStore<'db, Client, Policy, fn() -> String, fn() -> JsString>;
type Inspected<'a, 'db> =
    InspectionJournal<'a, 'db, Client, Policy, fn() -> String, fn() -> JsString>;
struct Engine<'a, 'db> {
    store: &'a Journal<'db>,
    inspected: &'a Inspected<'a, 'db>,
    catalogs: &'a CheckedNativeContracts,
    summary: RefCell<Option<JsString>>,
    updates: RefCell<Vec<JsString>>,
}
impl NativeDriverRuntime for Engine<'_, '_> {
    async fn verify_owner(&self) -> IvyResult<()> {
        self.inspected.verify_owner().await
    }
    async fn scoped_operation_id(&self, _: &Json) -> IvyResult<JsString> {
        panic!("No terminal dispatch")
    }
    fn now(&self) -> JsString {
        s(data(), "time").clone()
    }
    async fn native_update(&self, q: &Json) -> IvyResult<Json> {
        validate("NativeUpdateRequest", q)?;
        self.verify_owner().await?;
        let run = Store::read(self.store, "task-board/run", q.required("run")?, true).await?;
        let task = Store::read(self.store, "task-board/task", q.required("taskId")?, true).await?;
        assert_eq!(
            task.pin.required("revision")?,
            q.required("expectedRevision")?
        );
        assert_eq!(task.value.required("claim")?.required("run")?, &run.pin);
        let change = q.required("change")?;
        let evidence = TurnEvidence {
            store: self.store,
            catalogs: self.catalogs,
            principal_id: &"caller".into(),
        };
        let mut value = run.value.clone();
        match s(change, "kind").to_utf8().unwrap().as_str() {
            "readSnapshot" => {
                let snapshot = evidence
                    .snapshot(&run.pin, change.required("snapshot")?)
                    .await?;
                let projected = project_native_state(
                    &run.value,
                    s(&snapshot.document.value, "status"),
                    snapshot.document.value.required("epoch")?,
                    &snapshot.thread,
                    &self.now(),
                    change.required("snapshot")?,
                )?;
                for k in ["phase", "externalOutcome"] {
                    value.insert(k, projected.required(k)?.clone());
                }
            }
            "completion" => {
                let completion = evidence
                    .completion(&run.pin, change.required("snapshot")?, change.get("output"))
                    .await?;
                assert_eq!(completion.status, JsString::from("completed"));
                *self.summary.borrow_mut() = Some(completion.summary);
                value.insert("phase", "completed".into());
            }
            k => panic!("Unimplemented current engine policy {k}"),
        }
        self.updates.borrow_mut().push(s(change, "kind").clone());
        self.store.contracts.admit(&value);
        self.store
            .contracts
            .validate_record("task-board/run", &"1.0.0".into(), &value)?;
        let next = self.store.journal.update(
            "task-board/run",
            s(&run.pin, "objectId"),
            n(&run.pin, "revision"),
            &value,
            &self.now(),
        )?;
        let mut task_value = task.value.clone();
        task_value.insert(
            "claim",
            if finished_run(&value) {
                Json::Null
            } else {
                Json::object([("run", next), ("phase", "running".into())])
            },
        );
        self.store.contracts.admit(&task_value);
        self.store
            .contracts
            .validate_record("task-board/task", &"1.0.0".into(), &task_value)?;
        self.store.journal.update(
            "task-board/task",
            s(&task.pin, "objectId"),
            n(&task.pin, "revision"),
            &task_value,
            &self.now(),
        )?;
        Ok(Json::Null)
    }
}
impl OrchestrationEngine for Engine<'_, '_> {
    fn now_millis(&self) -> f64 {
        0.0
    }
    fn run_handoff(&self, task: &Json, run: &JsString) -> IvyResult<bool> {
        Ok(f(task, "comments").as_array().unwrap().iter().any(|v| {
            f(v, "purpose") == &"handoff".into()
                && f(v, "run").get("objectId").and_then(Json::as_js_string) == Some(run)
        }))
    }
    fn claimed_workflow_state(&self, w: &Json) -> IvyResult<JsString> {
        assert!(
            ["publication", "external_outcome"]
                .contains(&s(w, "reason").to_utf8().unwrap().as_str())
        );
        Ok("in_progress".into())
    }
    async fn peer_state(&self, _: &Json, _: &JsString) -> IvyResult<JsString> {
        panic!("No terminal steering")
    }
    async fn sync_native_comments(&self, r: &JsString) -> IvyResult<()> {
        sync_native_comments(&Comments(self.inspected), r).await
    }
    async fn with_read_cache<T, F: Future<Output = IvyResult<T>>>(&self, f: F) -> IvyResult<T> {
        f.await
    }
}
struct NoPrompts;
impl TaskPrompts for NoPrompts {
    fn task_prompt(&self, _: &Json, _: &JsString, _: Option<&Json>) -> IvyResult<JsString> {
        panic!("No terminal prompt")
    }
    fn task_update_prompt(&self, _: &Json, _: &JsString, _: bool) -> IvyResult<JsString> {
        panic!("No terminal prompt")
    }
}
fn clock() -> String {
    s(data(), "time").to_utf8().unwrap()
}
fn fresh_id() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}

#[tokio::test(flavor = "current_thread")]
async fn concrete_orchestration_persists_verified_terminal_evidence_reopens_and_fences_owner_loss()
{
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let row = data()
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|r| s(r, "name") == &JsString::from("first terminal observation saves snapshot"))
        .unwrap();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let catalog = catalogs
        .checked_native_contract(&"0.159.2".into(), None)
        .unwrap()
        .catalog()
        .clone();
    for (unsupported, handoff) in [(false, false), (true, false), (false, true)] {
        let temp = Temp(std::env::temp_dir().join(format!(
            "task-board-orchestration-{}.sqlite",
            uuid::Uuid::new_v4()
        )));
        for round in 0..2 {
            let db = Connection::open(&temp.0).unwrap();
            local_journal::initialize(&db).unwrap();
            let client = Arc::new(Client {
                row: row.clone(),
                catalog: catalog.clone(),
                unsupported,
                owner_valid: AtomicBool::new(true),
                methods: Mutex::default(),
                reads: Mutex::new(0),
            });
            let mut run = row
                .required("run")
                .unwrap()
                .required("value")
                .unwrap()
                .clone();
            let mut target = run.required("target").unwrap().clone();
            target.insert("hostId", "fixture".into());
            run.insert("target", target);
            let mut task = row
                .required("task")
                .unwrap()
                .required("value")
                .unwrap()
                .clone();
            if handoff {
                task.insert(
                    "comments",
                    Json::Array(vec![Json::object([
                        ("purpose", "handoff".into()),
                        ("run", pin(200, 2.0)),
                    ])]),
                );
            }
            let policy = Policy(RefCell::default());
            policy.admit(&run);
            policy.admit(&task);
            let store = JournalStore {
                journal: LocalJournal {
                    connection: &db,
                    root_object_id: Some("root".into()),
                },
                client: client.clone(),
                contracts: policy,
                limits: HistoryLimits::default(),
                now: clock as fn() -> String,
                new_id: fresh_id as fn() -> JsString,
                cache: None,
            };
            if round == 0 {
                for (key, object, value) in [
                    ("task-board/run", id(200), &run),
                    ("task-board/task", id(100), &task),
                ] {
                    store
                        .journal
                        .create(
                            key,
                            &object,
                            Some(&"root".into()),
                            value,
                            &clock().into(),
                            &object,
                        )
                        .unwrap();
                    store
                        .journal
                        .update(key, &object, 1.0, value, &clock().into())
                        .unwrap();
                }
            } else {
                for key in ["task-board/run", "task-board/task"] {
                    let object = if key == "task-board/run" {
                        id(200)
                    } else {
                        id(100)
                    };
                    let d = store
                        .journal
                        .read(key, &Json::String(object), true)
                        .unwrap();
                    store.contracts.admit(&d.value);
                }
            }
            let inspected =
                InspectionJournal::new(&store, "caller".into(), "task-board-node".into());
            let engine = Engine {
                store: &store,
                inspected: &inspected,
                catalogs: &catalogs,
                summary: RefCell::default(),
                updates: RefCell::default(),
            };
            let driver_runtime = DriverRuntime::new(
                client.clone(),
                "caller".into(),
                "task-board-node".into(),
                |_: Json| std::future::ready(Err(IvyError::internal())),
                || "2026-01-02T03:04:05.006Z".into(),
            );
            let services = NativeServices {
                driver: NativeDriver {
                    store: &store,
                    runtime: &driver_runtime,
                    prompts: &NoPrompts,
                    catalogs: &catalogs,
                },
                inspector: NativeInspector {
                    store: &inspected,
                    catalogs: &catalogs,
                },
                evidence: TurnEvidence {
                    store: &store,
                    catalogs: &catalogs,
                    principal_id: &"caller".into(),
                },
            };
            let mut coordinator = NativeOrchestrator::new(&store, &engine, &services, &catalogs);
            if round == 0 {
                coordinator.drain(&id(200)).await.unwrap();
                assert_eq!(
                    *engine.updates.borrow(),
                    vec![JsString::from("readSnapshot"), JsString::from("completion")]
                );
                assert_eq!(
                    *engine.summary.borrow(),
                    Some(
                        if unsupported || handoff {
                            "Native turn completed."
                        } else {
                            "answer"
                        }
                        .into()
                    )
                );
                let final_run = Store::read(&store, "task-board/run", &Json::String(id(200)), true)
                    .await
                    .unwrap();
                assert!(finished_run(&final_run.value));
                assert_eq!(n(&final_run.pin, "revision"), 4.0);
                let methods = client.methods.lock().unwrap();
                let calls: Vec<_> = methods
                    .iter()
                    .filter(|v| v.as_array().unwrap()[0] == Json::from("tools.call"))
                    .collect();
                assert_eq!(
                    calls
                        .iter()
                        .filter(|v| s(&v.as_array().unwrap()[1], "qualifiedName")
                            == &JsString::from("agent.inputs"))
                        .count(),
                    1
                );
                let output_reads = calls
                    .iter()
                    .filter(|v| {
                        f(&v.as_array().unwrap()[1], "arguments").get("method")
                            == Some(&"thread/items/list".into())
                    })
                    .count();
                assert_eq!(output_reads, if handoff { 0 } else { 1 });
                assert_eq!(*client.reads.lock().unwrap(), if handoff { 6 } else { 7 });
            } else {
                let before = db.total_changes();
                coordinator.step(&id(200)).await.unwrap();
                assert_eq!(db.total_changes(), before);
                assert_eq!(*client.reads.lock().unwrap(), 0);
                assert_eq!(client.methods.lock().unwrap().len(), 1);
                client.owner_valid.store(false, Ordering::Relaxed);
                assert_eq!(
                    coordinator.step(&id(200)).await.err().unwrap().code,
                    "task_board_principal_mismatch"
                );
                assert_eq!(db.total_changes(), before);
                assert_eq!(*client.reads.lock().unwrap(), 0);
            }
            assert!(db.is_autocommit());
        }
    }
}
