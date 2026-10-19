use super::*;
use ivy_contracts::builtin_schema::SchemaBundle;
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    inspection_journal::InspectionJournal,
    journal_store::{Contracts, JournalStore},
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    native_inspector::InspectionStore,
    operations::{Operations, mutation},
    scheduler_journal::SchedulerJournalBindings,
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
    if b.matches(
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{name}"),
        v,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            format!("Fixture {name} admission failed."),
        ))
    }
}
struct Policy;
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
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        validate(n, v)
    }
    fn validate_record(&self, k: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        validate(
            match k {
                "task-board/operation" => "Operation",
                "task-board/coordination" => "Coordination",
                k => panic!("Unexpected {k}"),
            },
            v,
        )
    }
}
struct Client {
    owner: std::sync::atomic::AtomicBool,
    calls: std::sync::Mutex<Vec<Json>>,
}
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(Json::Array(vec![m.into(), a]));
            match m {
                "system.status" => Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )])),
                "serviceNodes.get" => Ok(Json::object([
                    (
                        "principalId",
                        if self.owner.load(std::sync::atomic::Ordering::Relaxed) {
                            "caller"
                        } else {
                            "replacement"
                        }
                        .into(),
                    ),
                    ("serviceName", "task-board".into()),
                    ("connected", true.into()),
                    ("synced", true.into()),
                ])),
                _ => panic!("Reconciliation must not issue native/remote effects: {m}"),
            }
        })
    }
}
struct Pages<'a>(&'a Connection);
impl LifecyclePages for Pages<'_> {
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        let mut items = vec![];
        if k == "task-board/operation" {
            let records=self.0.prepare("SELECT documents.id,current_revision,value_json FROM documents JOIN revisions ON revisions.id=documents.id AND revisions.revision=documents.current_revision WHERE documents.key='task-board/operation' ORDER BY documents.id").unwrap()
            .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,String>(2)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
            for (id, rev, v) in records {
                let v = parse(&v, MANAGEMENT_FRAME_BYTES)?;
                if ["accepted", "preparing", "publishing", "needs_attention"]
                    .iter()
                    .any(|p| f(&v, "phase") == &Json::from(*p))
                {
                    items.push(Json::object([
                        ("objectId", id.into()),
                        ("revision", Json::Number(rev as f64)),
                    ]));
                }
            }
        }
        assert!(f(o, "cursor").is_null());
        assert_eq!(num(o, "limit"), 2.0);
        assert!(items.len() <= 2);
        Ok(Json::object([
            ("items", Json::Array(items)),
            ("nextCursor", Json::Null),
        ]))
    }
}
impl SchedulerJournalBindings for Pages<'_> {
    fn free_bytes(&self) -> IvyResult<Option<f64>> {
        Ok(Some(1073741824.0))
    }
}
struct Engine<'a, S, C, I> {
    operations: &'a Operations<S, C>,
    inspection: &'a I,
    gates: &'a HashMap<JsString, JsString>,
    first: &'a JsString,
    ready: &'a Cell<bool>,
    seen: RefCell<Vec<JsString>>,
    service: JsString,
}
impl<S: Store, C: Fn() -> String, I: InspectionStore> ReconcileEngine for Engine<'_, S, C, I> {
    fn service_node_id(&self) -> &JsString {
        &self.service
    }
    fn page_size(&self) -> f64 {
        2.0
    }
    fn now_millis(&self) -> f64 {
        1767312245006.0
    }
    fn now(&self) -> JsString {
        clock().into()
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        self.inspection.verify_owner().await
    }
    async fn recover_operation(&self, id: &JsString) -> IvyResult<()> {
        self.seen.borrow_mut().push(id.clone());
        if id == self.first && !self.ready.get() {
            return Err(
                IvyError::new("outcome_unknown", "Original recovery still pending.")
                    .with_outcome(Outcome::Unknown),
            );
        }
        let refusal = IvyError::new("not_found", "Exact original native binding is unavailable.");
        self.operations.failure(id, &refusal, false).await?;
        let gate = self
            .operations
            .store
            .read(
                "task-board/coordination",
                &Json::String(self.gates[id].clone()),
                true,
            )
            .await?;
        if !f(&gate.value, "activeOperation").is_null() {
            let mut value = gate.value.clone();
            value.insert("activeOperation", Json::Null);
            self.operations
                .store
                .write(
                    "task-board/coordination",
                    &value,
                    &mutation(id, "fixture-gate-release")?,
                    &Json::object([
                        ("objectId", gate.pin.required("objectId")?.clone()),
                        ("expectedRevision", gate.pin.required("revision")?.clone()),
                    ]),
                )
                .await?;
        }
        Err(refusal)
    }
    async fn sync_native_comments(&self, _: &JsString) -> IvyResult<()> {
        panic!("No Runs")
    }
    async fn drain_native(&self, _: &JsString) -> IvyResult<bool> {
        panic!("No Runs")
    }
    async fn native_signals(&self, _: &JsString) -> IvyResult<()> {
        panic!("No Runs")
    }
    async fn schedule(&self, _: &JsString) -> IvyResult<()> {
        panic!("No Tasks")
    }
    async fn delivery(&self, _: &JsString) -> IvyResult<()> {
        panic!("No deliveries")
    }
}
fn clock() -> String {
    "2026-01-02T03:04:05.006Z".to_owned()
}
fn fresh() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
#[tokio::test(flavor = "current_thread")]
async fn actual_journal_gates_terminal_refusal_cleanup_reopen_and_owner_fences() {
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let temp = Temp(std::env::temp_dir().join(format!(
        "task-board-reconciler-{}.sqlite",
        uuid::Uuid::new_v4()
    )));
    let client = Arc::new(Client {
        owner: std::sync::atomic::AtomicBool::new(true),
        calls: Default::default(),
    });
    let mut gates = HashMap::new();
    let mut originals = vec![];
    let ready = Cell::new(false);
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);
    for round in 0..2 {
        let db = Connection::open(&temp.0).unwrap();
        local_journal::initialize(&db).unwrap();
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Policy,
            limits: HistoryLimits::default(),
            now: clock as fn() -> String,
            new_id: fresh as fn() -> JsString,
            cache: None,
        };
        let pages = Pages(&db);
        let operations = Operations {
            store: LifecycleJournal {
                journal: &store,
                pages: &pages,
            },
            now: clock as fn() -> String,
        };
        let inspection = InspectionJournal::new(&store, "caller".into(), "task-board-node".into());
        if round == 0 {
            for n in 0..2 {
                let actor = Json::object([
                    ("principalId", "caller".into()),
                    ("serviceNodeId", "task-board-node".into()),
                    ("generation", Json::Number(1.0)),
                    ("source", "user".into()),
                ]);
                let request = Json::object([
                    ("action", "archive".into()),
                    ("operationId", format!("original-{n}").into()),
                    ("taskId", Json::String(id(100 + n))),
                    ("expectedRevision", Json::Number(2.0)),
                    ("archived", true.into()),
                ]);
                let operation = operations.begin(&request, &actor).await.unwrap();
                let gate = id(500 + n);
                let value = Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("principalId", "caller".into()),
                    ("activeOperation", operation.pin.clone()),
                    ("updatedAt", clock().into()),
                ]);
                operations
                    .store
                    .write(
                        "task-board/coordination",
                        &value,
                        &format!("gate-{n}"),
                        &Json::object([(
                            "create",
                            Json::object([
                                ("parentId", "root".into()),
                                ("name", Json::String(gate)),
                            ]),
                        )]),
                    )
                    .await
                    .unwrap();
                let written = operations
                    .store
                    .named(
                        "task-board/coordination",
                        &id(500 + n).to_utf8().unwrap(),
                        None,
                    )
                    .await
                    .unwrap()
                    .unwrap();
                gates.insert(
                    s(&operation.pin, "objectId").clone(),
                    s(&written.pin, "objectId").clone(),
                );
                originals.push(operation);
            }
        }
        let engine = Engine {
            operations: &operations,
            inspection: &inspection,
            gates: &gates,
            first: s(&originals[0].pin, "objectId"),
            ready: &ready,
            seen: RefCell::default(),
            service: "task-board-node".into(),
        };
        let tasks = Tasks::default();
        let subject = Reconciler::new(&operations.store, &engine, &tasks);
        let before = db.total_changes();
        for _ in 0..10 {
            let mut tick = Box::pin(subject.tick());
            assert!(tick.as_mut().poll(&mut cx).is_ready());
            for _ in 0..8 {
                tasks.pump(&mut cx);
            }
        }
        if round == 0 {
            assert!(
                subject
                    .diagnostics()
                    .iter()
                    .any(|d| f(d, "code") == &Json::from("outcome_unknown"))
            );
            assert!(
                engine
                    .seen
                    .borrow()
                    .contains(s(&originals[1].pin, "objectId"))
            );
            assert_eq!(
                operations
                    .store
                    .pending_coordination_page(None, 10)
                    .unwrap()
                    .len(),
                1
            );
            ready.set(true);
            for _ in 0..8 {
                let mut tick = Box::pin(subject.tick());
                assert!(tick.as_mut().poll(&mut cx).is_ready());
                for _ in 0..8 {
                    tasks.pump(&mut cx);
                }
            }
            assert!(subject.diagnostics().is_empty());
            assert!(
                operations
                    .store
                    .pending_coordination_page(None, 10)
                    .unwrap()
                    .is_empty()
            );
        } else {
            assert!(subject.recovered());
            assert!(subject.diagnostics().is_empty());
            assert!(engine.seen.borrow().is_empty());
            assert_eq!(db.total_changes(), before);
            client
                .owner
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let mut tick = Box::pin(subject.tick());
            assert!(tick.as_mut().poll(&mut cx).is_ready());
            assert_eq!(
                s(&subject.diagnostics()[0], "code"),
                &JsString::from("task_board_principal_mismatch")
            );
            assert_eq!(db.total_changes(), before);
        }
        for original in &originals {
            let retained = store
                .read("task-board/operation", &original.pin, true)
                .await
                .unwrap();
            assert_eq!(f(&retained.value, "phase"), &Json::from("accepted"));
            let latest = store
                .read(
                    "task-board/operation",
                    original.pin.required("objectId").unwrap(),
                    true,
                )
                .await
                .unwrap();
            assert_eq!(f(&latest.value, "phase"), &Json::from("failed"));
        }
        assert!(tasks.0.borrow().is_empty());
        assert!(db.is_autocommit());
    }
    assert!(client.calls.lock().unwrap().iter().all(|c| {
        ["system.status", "serviceNodes.get"]
            .contains(&c.as_array().unwrap()[0].as_str().unwrap().as_str())
    }));
}
