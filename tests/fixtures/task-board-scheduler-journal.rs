use super::*;
use ivy_contracts::builtin_schema::SchemaBundle;
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    journal_store::{Contracts, JournalStore},
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    operations::Operations,
    scheduler::SchedulerRuntime,
    scheduler_journal::SchedulerJournalBindings,
};
use std::{
    cell::Cell,
    sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
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
struct Policy(Json);
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
        match k {
            "task-board/native-plan" => validate("NativePlan", v),
            "task-board/operation" => validate("Operation", v),
            "task-board/scheduler-attempt" => validate("SchedulerAttempt", v),
            "task-board/configuration" => validate("Configuration", v),
            // This static Task fixture is admitted exactly; full current Task
            // overlay admission remains a required engine integration binding.
            "task-board/task" => {
                assert_eq!(hash_json(v)?, hash_json(&self.0)?);
                Ok(())
            }
            other => panic!("Unexpected contract {other}"),
        }
    }
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        validate(n, v)
    }
}
struct NativeClient {
    replay: Replay,
    owner: AtomicBool,
}
impl RpcClient for NativeClient {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            match m {
                "system.status" => Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )])),
                "serviceNodes.get" => {
                    assert_eq!(f(&a, "serviceNodeId"), &Json::from("task-board-node"));
                    Ok(Json::object([
                        (
                            "principalId",
                            if self.owner.load(AtomicOrdering::Relaxed) {
                                "caller"
                            } else {
                                "replacement"
                            }
                            .into(),
                        ),
                        ("serviceName", "task-board".into()),
                        ("connected", true.into()),
                        ("synced", true.into()),
                    ]))
                }
                _ => self.replay.call("rpc", [m.into(), self.replay.rpc_args(a)]),
            }
        })
    }
}
struct Pages;
impl LifecyclePages for Pages {
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        assert_eq!(k, "task-board/run");
        assert_eq!(f(o, "limit"), &Json::Number(50.0));
        Ok(Json::object([
            ("items", Json::Array(vec![])),
            ("nextCursor", Json::Null),
        ]))
    }
}
impl SchedulerJournalBindings for Pages {
    fn free_bytes(&self) -> IvyResult<Option<f64>> {
        Ok(Some(1073741824.0))
    }
}
struct Policies<'a, S, C> {
    operations: &'a Operations<S, C>,
    now: &'a Cell<bool>,
    dispatches: Cell<u32>,
    recoveries: Cell<u32>,
}
impl<S, C> TaskPrompts for Policies<'_, S, C> {
    fn task_prompt(&self, _: &Json, _: &JsString, _: Option<&Json>) -> IvyResult<JsString> {
        Ok(JsString(
            "task prompt".encode_utf16().chain([0xd800]).collect(),
        ))
    }
    fn task_update_prompt(&self, _: &Json, _: &JsString, _: bool) -> IvyResult<JsString> {
        panic!("Unexpected update prompt")
    }
}
impl<S: Store, C: Fn() -> String> SchedulerPolicies for Policies<'_, S, C> {
    fn enabled(&self) -> bool {
        true
    }
    fn interval_ms(&self) -> f64 {
        1000.0
    }
    fn now(&self) -> JsString {
        if self.now.get() {
            "2026-01-02T03:04:05.006Z"
        } else {
            "2026-01-02T03:03:05.006Z"
        }
        .into()
    }
    fn compare_default(&self, _: &JsString, _: &JsString) -> Ordering {
        panic!("Only one eligible host")
    }
    fn coordination_pending(&self, _: &Json) -> IvyResult<bool> {
        Ok(false)
    }
    fn peer_comment(&self, _: &Json) -> IvyResult<bool> {
        panic!("No queued coordination")
    }
    async fn forward_delegated_review(&self, _: &Document) -> IvyResult<()> {
        Ok(())
    }
    async fn unstarted_missing_context(&self, _: &Document) -> IvyResult<bool> {
        Ok(false)
    }
    async fn unstarted_archived_context(&self, _: &Document) -> IvyResult<bool> {
        panic!("No prior wait")
    }
    async fn failed_rate_limit_retry_at(&self, _: &Document) -> IvyResult<Option<JsString>> {
        panic!("No prior wait")
    }
    async fn check_dependencies(&self, _: &Json, _: &JsString, d: bool) -> IvyResult<()> {
        assert!(d);
        Ok(())
    }
    async fn check_blocker(&self, b: &Json) -> IvyResult<()> {
        assert!(b.is_null());
        Ok(())
    }
    async fn schedule(&self, r: &Json) -> IvyResult<Json> {
        self.dispatches.set(self.dispatches.get() + 1);
        let actor = Json::object([
            ("principalId", "caller".into()),
            ("serviceNodeId", "task-board-node".into()),
            ("generation", Json::Number(1.0)),
            ("source", "scheduler".into()),
        ]);
        self.operations.begin(r, &actor).await?;
        Err(
            IvyError::new("outcome_unknown", "Dispatch reply lost after acceptance.")
                .with_outcome(Outcome::Unknown),
        )
    }
    async fn recover_operation(&self, id: &JsString) -> IvyResult<Json> {
        self.recoveries.set(self.recoveries.get() + 1);
        let operation = self
            .operations
            .store
            .read("task-board/operation", &Json::String(id.clone()), true)
            .await?;
        assert_eq!(f(&operation.value, "phase"), &Json::from("accepted"));
        Ok(operation.pin)
    }
}
struct RetainedAllocation {
    value: Json,
    calls: Cell<u32>,
}
impl SchedulerAllocation for RetainedAllocation {
    async fn allocate(&self, _: &Document, _: &JsString, _: Option<&JsString>) -> IvyResult<Json> {
        self.calls.set(self.calls.get() + 1);
        Ok(self.value.clone())
    }
}
fn old() -> String {
    "2026-01-02T03:03:05.006Z".to_owned()
}
fn fresh() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
#[tokio::test(flavor = "current_thread")]
async fn actual_allocation_and_scheduler_journal_reopen_keep_uncertain_operation_and_successor_proof()
 {
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let temp = Temp(std::env::temp_dir().join(format!(
        "task-board-scheduler-{}.sqlite",
        uuid::Uuid::new_v4()
    )));
    let group = data()
        .as_array()
        .unwrap()
        .iter()
        .find(|g| {
            f(g, "mode") == &Json::from("allocate")
                && s(g, "path").to_utf8().unwrap().contains("0.159.2")
        })
        .unwrap();
    let row = &f(group, "rows").as_array().unwrap()[0];
    let task = doc(f(f(row, "initial"), "task"));
    let initial_steps = f(row, "boundaries").as_array().unwrap();
    let preference = f(
        initial_steps
            .iter()
            .find(|e| f(e, "kind") == &Json::from("named"))
            .unwrap(),
        "value",
    );
    let expected_plan = f(
        initial_steps
            .iter()
            .find(|e| f(e, "kind") == &Json::from("write"))
            .unwrap(),
        "args",
    )
    .as_array()
    .unwrap()[1]
        .clone();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let catalogs = CheckedNativeContracts::new(&root);
    let current = Cell::new(false);
    let mut allocation = None;
    let mut first_attempt = None;
    let mut successor = None;
    let client = Arc::new(NativeClient {
        owner: AtomicBool::new(true),
        replay: Replay {
            name: "actual SQLite allocation".into(),
            steps: Mutex::new(
                initial_steps
                    .iter()
                    .filter(|v| f(v, "kind") == &Json::from("rpc"))
                    .cloned()
                    .collect(),
            ),
            aliases: Mutex::default(),
        },
    });
    for round in 0..3 {
        let db = Connection::open(&temp.0).unwrap();
        local_journal::initialize(&db).unwrap();
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Policy(task.value.clone()),
            limits: HistoryLimits::default(),
            now: old as fn() -> String,
            new_id: fresh as fn() -> JsString,
            cache: None,
        };
        let pages = Pages;
        let operations = Operations {
            store: LifecycleJournal {
                journal: &store,
                pages: &pages,
            },
            now: old as fn() -> String,
        };
        let engine = SchedulerRuntime::new(
            client.clone(),
            "caller".into(),
            "task-board-node".into(),
            Policies {
                operations: &operations,
                now: &current,
                dispatches: Cell::new(0),
                recoveries: Cell::new(0),
            },
        );
        let task_id = s(&task.pin, "objectId");
        if round == 0 {
            store
                .journal
                .create(
                    "task-board/task",
                    task_id,
                    Some(&"root".into()),
                    &task.value,
                    &old().into(),
                    task_id,
                )
                .unwrap();
            store
                .journal
                .update("task-board/task", task_id, 1.0, &task.value, &old().into())
                .unwrap();
            let config = f(preference, "value");
            store
                .contracts
                .validate_record("task-board/configuration", &"1.0.0".into(), config)
                .unwrap();
            store
                .journal
                .create(
                    "task-board/configuration",
                    &"TaskBoard configuration".into(),
                    Some(&"root".into()),
                    config,
                    &old().into(),
                    &fresh(),
                )
                .unwrap();
            let result = Allocator {
                store: &operations.store,
                engine: &engine,
                catalogs: &catalogs,
            }
            .allocate(&task, &"original".into(), None)
            .await
            .unwrap();
            let saved = store
                .read("task-board/native-plan", f(&result, "intent"), true)
                .await
                .unwrap();
            assert_eq!(
                hash_json(&saved.value).unwrap(),
                hash_json(&expected_plan).unwrap()
            );
            assert!(client.replay.steps.lock().unwrap().is_empty());
            allocation = Some(result);
        }
        let retained = RetainedAllocation {
            value: allocation.clone().unwrap(),
            calls: Cell::new(0),
        };
        let scheduler = Scheduler {
            store: &operations.store,
            engine: &engine,
            allocator: &retained,
            operations: &operations,
        };
        let name = format!("Scheduled task {} revision 2", task_id.to_utf8().unwrap());
        if round == 0 {
            assert_eq!(
                scheduler.task(task_id).await.err().unwrap().code,
                "outcome_unknown"
            );
            first_attempt = operations
                .store
                .named("task-board/scheduler-attempt", &name, Some(task_id))
                .await
                .unwrap();
            assert_eq!(
                f(&first_attempt.as_ref().unwrap().value, "sequence"),
                &Json::Number(1.0)
            );
            assert_eq!(retained.calls.get(), 1);
            assert_eq!(engine.policies.dispatches.get(), 1);
        } else {
            let before = db.total_changes();
            let recovered = scheduler.task(task_id).await.unwrap();
            assert_eq!(db.total_changes(), before);
            assert_eq!(retained.calls.get(), 0);
            assert_eq!(engine.policies.recoveries.get(), 1);
            assert_eq!(engine.policies.dispatches.get(), 0);
            if round == 1 {
                let failed = operations
                    .failure(
                        s(&recovered, "objectId"),
                        &IvyError::new("fixture_failed", "Conclusive unapplied failure."),
                        false,
                    )
                    .await
                    .unwrap();
                current.set(true);
                assert_eq!(
                    scheduler.task(task_id).await.err().unwrap().code,
                    "outcome_unknown"
                );
                let next = operations
                    .store
                    .named("task-board/scheduler-attempt", &name, Some(task_id))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(f(&next.value, "sequence"), &Json::Number(2.0));
                assert_eq!(
                    f(&next.value, "previous"),
                    &first_attempt.as_ref().unwrap().pin
                );
                assert_eq!(f(&next.value, "previousOperation"), &failed.pin);
                assert_eq!(retained.calls.get(), 1);
                assert_eq!(engine.policies.dispatches.get(), 1);
                successor = Some(next);
            } else {
                let next = operations
                    .store
                    .named("task-board/scheduler-attempt", &name, Some(task_id))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(next.pin, successor.as_ref().unwrap().pin);
                client.owner.store(false, AtomicOrdering::Relaxed);
                let before = db.total_changes();
                assert_eq!(
                    scheduler.task(task_id).await.err().unwrap().code,
                    "task_board_principal_mismatch"
                );
                assert_eq!(db.total_changes(), before);
            }
        }
        let plan = store
            .read(
                "task-board/native-plan",
                f(allocation.as_ref().unwrap(), "intent"),
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            hash_json(&plan.value).unwrap(),
            hash_json(&expected_plan).unwrap()
        );
        assert!(db.is_autocommit());
    }
}
