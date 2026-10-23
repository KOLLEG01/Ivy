use super::*;
use ivy_contracts::builtin_schema::SchemaBundle;
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    journal_store::{Contracts, JournalStore},
    lifecycle_journal::LifecycleJournal,
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    operations::Operations,
    publication::Publication,
};
fn clock() -> String {
    "2026-01-02T03:04:05.006Z".into()
}
fn fresh() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}

// Finite current-shape checks for journal replay, deliberately separate from
// the frozen/current normative factory admission suites.
struct AdoptionPolicies {
    task: Json,
    previous: Json,
}
impl Contracts for AdoptionPolicies {
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
        admission(n, v)
    }
    fn validate_record(&self, k: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        match k {
            "task-board/operation" => {
                assert_eq!(f(f(v, "request"), "action"), &Json::from("nativeUpdate"));
                assert_eq!(
                    f(f(f(v, "request"), "change"), "kind"),
                    &Json::from("adoptTurn")
                );
                assert_eq!(f(v, "callerPrincipalId"), &Json::from("board"));
                assert_eq!(
                    f(v, "requestHash"),
                    &Json::from(hash_json(f(v, "request"))?)
                );
            }
            "task-board/task" => {
                let mutable = [
                    "publication",
                    "updatedAt",
                    "claim",
                    "workflowState",
                    "runtimeWait",
                    "attemptCount",
                    "lastRun",
                    "lastHistory",
                    "completionIssue",
                ];
                assert_eq!(
                    hash_json(&without(v, &mutable))?,
                    hash_json(&without(&self.task, &mutable))?
                );
                if !f(v, "claim").is_null() {
                    assert_eq!(f(v, "workflowState"), &Json::from("in_progress"));
                    assert_eq!(f(v, "attemptCount"), &Json::Number(3.0));
                }
            }
            "task-board/run" => {
                if hash_json(v)? == hash_json(&self.previous)? {
                    return Ok(());
                }
                assert_eq!(f(v, "taskId"), &Json::String(id(1)));
                assert_eq!(f(v, "phase"), &Json::from("running"));
                assert_eq!(f(v, "attempt"), &Json::Number(3.0));
                assert_eq!(f(v, "turnId"), &Json::from("direct"));
                for key in ["target", "workspace", "primaryResourceRef"] {
                    assert_eq!(hash_json(f(v, key))?, hash_json(f(&self.previous, key))?);
                }
                for key in ["thread", "turn", "interrupt"] {
                    assert_eq!(f(f(v, "calls"), key), &Json::Null);
                }
                for key in ["workflowState", "latestResult", "acceptedReview"] {
                    assert_eq!(f(f(v, "executionContext"), key), f(&self.task, key));
                }
                assert_eq!(
                    f(v, "requestHash"),
                    &Json::from(hash_json(f(v, "originalRequest"))?)
                );
            }
            "task-board/history" => admission("History", v)?,
            k => panic!("Unexpected adoption write: {k}"),
        }
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn adopted_turn_reopen_reuses_exact_journal_writes_and_preserves_later_ticket_edits() {
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let temp = Temp(std::env::temp_dir().join(format!(
        "task-board-adoption-{}.sqlite",
        uuid::Uuid::new_v4()
    )));
    let world = super::adoption::idle("review");
    let task = world.final_task();
    let previous = world.saved_run();
    let mut original: Option<JsString> = None;
    let mut counts = (0i64, 0i64);
    for round in 0..2 {
        let db = Connection::open(&temp.0).unwrap();
        initialize(&db).unwrap();
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: Arc::new(NoRpc),
            contracts: AdoptionPolicies {
                task: task.clone(),
                previous: previous.clone(),
            },
            limits: HistoryLimits::default(),
            now: clock as fn() -> String,
            new_id: fresh as fn() -> JsString,
            cache: None,
        };
        if round == 0 {
            for (key, value, p) in [
                ("task-board/task", &task, pin(1)),
                ("task-board/run", &previous, pin(2)),
            ] {
                journal
                    .journal
                    .create(
                        key,
                        &key.into(),
                        Some(&"root".into()),
                        value,
                        &clock().into(),
                        s(&p, "objectId"),
                    )
                    .unwrap();
            }
        }
        let operations = Operations {
            store: LifecycleJournal {
                journal: &journal,
                pages: &NoPages,
            },
            now: clock as fn() -> String,
        };
        let operation = if let Some(original) = &original {
            operations
                .store
                .read(
                    "task-board/operation",
                    &Json::String(original.clone()),
                    false,
                )
                .await
                .unwrap()
        } else {
            let (metadata, turns) = super::adoption::current_pair();
            let request = Json::object([
                ("action", "nativeUpdate".into()),
                ("operationId", "observed-direct-turn".into()),
                ("taskId", Json::String(id(1))),
                ("expectedRevision", Json::Number(1.0)),
                ("run", pin(2)),
                (
                    "change",
                    Json::object([
                        ("kind", "adoptTurn".into()),
                        ("metadata", metadata),
                        ("turns", turns),
                    ]),
                ),
            ]);
            let operation = operations.begin(&request, &actor("native")).await.unwrap();
            original = Some(s(&operation.pin, "objectId").clone());
            operation
        };
        let fail_final =
            |_: Json,
             v: Json|
             -> std::pin::Pin<Box<dyn std::future::Future<Output = IvyResult<()>>>> {
                Box::pin(async move {
                    if f(&v, "publication").is_null() {
                        Err(
                            IvyError::new("outcome_unknown", "Lost adopted Task callback")
                                .with_outcome(Outcome::Unknown),
                        )
                    } else {
                        Ok(())
                    }
                })
            };
        let mut steps = Publication {
            operations: &operations,
            operation: &operation,
            task_written: if round == 0 { Some(&fail_final) } else { None },
            index: 0.0,
        };
        let result = NativePublisher {
            store: &operations.store,
            engine: &world,
            evidence: &world,
        }
        .publish(&operation, &mut steps, None)
        .await;
        let current = operations
            .store
            .read("task-board/task", &Json::String(id(1)), false)
            .await
            .unwrap();
        assert_eq!(
            f(&current.value, "workflowState"),
            &Json::from("in_progress")
        );
        assert!(current.value.get("completionIssue").is_none());
        let count = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        if round == 0 {
            assert_eq!(result.unwrap_err().outcome, Outcome::Unknown);
            let mut later = current.value.clone();
            later.insert("updatedAt", "2026-01-02T03:04:06.000Z".into());
            journal
                .journal
                .update(
                    "task-board/task",
                    s(&current.pin, "objectId"),
                    3.0,
                    &later,
                    &clock().into(),
                )
                .unwrap();
            counts = (
                count("SELECT COUNT(*) FROM documents"),
                count("SELECT COUNT(*) FROM revisions"),
            );
        } else {
            let outcome = result.unwrap();
            assert_eq!(
                (
                    count("SELECT COUNT(*) FROM documents"),
                    count("SELECT COUNT(*) FROM revisions")
                ),
                counts
            );
            assert_eq!(f(&current.pin, "revision"), &Json::Number(4.0));
            assert_eq!(
                f(&current.value, "updatedAt"),
                &Json::from("2026-01-02T03:04:06.000Z")
            );
            assert_eq!(f(f(&current.value, "claim"), "run"), f(&outcome, "run"));
            assert_eq!(
                count(
                    "SELECT COUNT(*) FROM documents WHERE key='task-board/run' AND parent_id='root'"
                ),
                2
            );
            assert_eq!(
                count(
                    "SELECT COUNT(*) FROM documents WHERE key='task-board/history' AND parent_id='root'"
                ),
                1
            );
            assert_eq!(count("SELECT COUNT(*) FROM cleanup_roots"), 0);
        }
    }
}
fn admission(n: &str, v: &Json) -> IvyResult<()> {
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
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{n}"),
        v,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            format!("Fixture {n} admission failed."),
        ))
    }
}
fn without(v: &Json, keys: &[&str]) -> Json {
    let mut v = v.clone();
    for k in keys {
        v.remove(k);
    }
    v
}
struct Policies {
    task: Json,
    run: Json,
}
impl Contracts for Policies {
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
        admission(n, v)
    }
    fn validate_record(&self, k: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        match k {
            "task-board/operation" => admission("Operation", v),
            "task-board/result" => {
                // Built-in Result predates this content. Full current registry
                // qualification is separate from this retained-write test.
                assert_eq!(f(v, "kind"), &Json::from("intermediate"));
                assert!(f(v, "content").get("checks").is_none());
                assert!(f(f(v, "content"), "summary").as_js_string().is_some());
                Ok(())
            }
            "task-board/history" => admission("History", v),
            "task-board/task" => {
                let mutable = [
                    "publication",
                    "updatedAt",
                    "claim",
                    "workflowState",
                    "runtimeWait",
                    "openQuestionCount",
                    "primaryResourceRef",
                    "lastRun",
                    "latestResult",
                    "acceptedReview",
                    "lastHistory",
                    "commentDeliveries",
                ];
                assert_eq!(
                    hash_json(&without(v, &mutable))?,
                    hash_json(&without(&self.task, &mutable))?
                );
                Ok(())
            }
            "task-board/run" => {
                let mutable = [
                    "calls",
                    "phase",
                    "updatedAt",
                    "finishedAt",
                    "externalOutcome",
                    "result",
                ];
                assert_eq!(
                    hash_json(&without(v, &mutable))?,
                    hash_json(&without(&self.run, &mutable))?
                );
                Ok(())
            }
            "task-board/native-plan" => {
                assert_eq!(v, &Json::object([("nativeVersion", "native".into())]));
                Ok(())
            }
            k => panic!("Unexpected record {k}"),
        }
    }
}
struct NoRpc;
struct NoPages;
impl ivy_task_board::lifecycle_journal::LifecyclePages for NoPages {
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("No page scan is permitted")
    }
}
impl RpcClient for NoRpc {
    fn request<'a>(&'a self, m: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            if m == "system.status" {
                return Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )]));
            }
            panic!("No external effect permitted: {m}")
        })
    }
}
struct Engine;

#[test]
fn pending_and_failed_handoffs_are_selected_from_current_retained_operations() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = JournalStore {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        client: Arc::new(NoRpc),
        contracts: Policies {
            task: base_task(),
            run: base_run(),
        },
        limits: HistoryLimits::default(),
        now: clock as fn() -> String,
        new_id: fresh as fn() -> JsString,
        cache: None,
    };
    let store = LifecycleJournal {
        journal: &journal,
        pages: &NoPages,
    };
    let add = |phase: &str, run: &str, error: Json| {
        let operation = Json::object([
            ("handoffRun", Json::object([("objectId", run.into())])),
            ("phase", phase.into()),
            ("operationId", "handoff-original".into()),
            ("error", error),
        ]);
        journal
            .journal
            .create(
                "task-board/operation",
                &fresh(),
                Some(&"root".into()),
                &operation,
                &clock().into(),
                &fresh(),
            )
            .unwrap();
    };
    for phase in [
        "accepted",
        "preparing",
        "publishing",
        "needs_attention",
        "failed",
        "succeeded",
    ] {
        add(phase, "run", Json::Null);
    }
    add("accepted", "other-run", Json::Null);
    assert_eq!(store.pending_handoffs(&"run".into()).unwrap(), 4);
    assert!(store.failed_handoff(&"run".into()).unwrap().is_none());
    add(
        "failed",
        "run",
        Json::object([
            ("code", "storage_full".into()),
            ("message", "Original storage error".into()),
        ]),
    );
    let issue = store.failed_handoff(&"run".into()).unwrap().unwrap();
    assert_eq!(f(&issue, "code"), &Json::from("storage_full"));
    assert_eq!(f(&issue, "operationId"), &Json::from("handoff-original"));
    for _ in 0..13 {
        add("accepted", "run", Json::Null);
    }
    assert_eq!(
        store.pending_handoffs(&"run".into()).unwrap_err().code,
        "task_board_handoff_limit"
    );
}

impl NativePublicationPolicies for Engine {
    fn now(&self) -> JsString {
        clock().into()
    }
    async fn aggregate(&self, t: &Json, _: &JsString) -> IvyResult<()> {
        assert_eq!(f(t, "workflowState"), &Json::from("in_progress"));
        Ok(())
    }
}
impl NativePublicationEngine for Engine {
    fn phone_target(&self) -> &Json {
        static VALUE: Json = Json::Null;
        &VALUE
    }
    fn retry_at(&self, _: &Json, _: &JsString) -> IvyResult<Json> {
        panic!("No native turn is involved")
    }
    fn principal_id(&self) -> &JsString {
        static P: OnceLock<JsString> = OnceLock::new();
        P.get_or_init(|| "board".into())
    }
    async fn workspace(&self, _: &Json, _: &Json) -> IvyResult<Json> {
        Err(IvyError::new(
            "service_unavailable",
            "Finite workspace is unavailable.",
        ))
    }
}
impl NativePublicationEvidence for Engine {
    fn adoption(&self, _: &Json, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No direct turn observations")
    }
    async fn snapshot(&self, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No inferred native evidence")
    }
    async fn completion(&self, _: &Json, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No inferred native evidence")
    }
    fn identity(&self, _: &Json, _: &Json, _: &JsString) -> IvyResult<EvidenceIdentity> {
        panic!("No native call exists")
    }
    async fn read(&self, _: &Json, _: &Json, _: &EvidenceIdentity) -> IvyResult<Json> {
        panic!("No native call exists")
    }
    async fn intended(&self, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No native call exists")
    }
    fn resolve(&self, _: &Json) -> IvyResult<Json> {
        panic!("No native call exists")
    }
}
#[tokio::test(flavor = "current_thread")]
async fn actual_publication_reopen_reuses_records_and_retains_cleanup_dependencies() {
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let temp = Temp(std::env::temp_dir().join(format!(
        "task-board-native-publication-{}.sqlite",
        uuid::Uuid::new_v4()
    )));
    let task = base_task();
    let mut run = base_run();
    run.insert("cancellation", Json::object([("reason", "cancel".into())]));
    run.insert("turnId", Json::Null);
    let task_pin = pin(1);
    let run_pin = pin(2);
    let plan = f(f(&run, "originalRequest"), "intent").clone();
    let mut operation_id: Option<JsString> = None;
    let mut record_count = 0i64;
    let mut revision_count = 0i64;
    for round in 0..2 {
        let db = Connection::open(&temp.0).unwrap();
        initialize(&db).unwrap();
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: Arc::new(NoRpc),
            contracts: Policies {
                task: task.clone(),
                run: run.clone(),
            },
            limits: HistoryLimits::default(),
            now: clock as fn() -> String,
            new_id: fresh as fn() -> JsString,
            cache: None,
        };
        if round == 0 {
            for (k, v, p) in [
                ("task-board/task", &task, &task_pin),
                ("task-board/run", &run, &run_pin),
                (
                    "task-board/native-plan",
                    &Json::object([("nativeVersion", "native".into())]),
                    &plan,
                ),
            ] {
                store
                    .contracts
                    .validate_record(k, &"1.0.0".into(), v)
                    .unwrap();
                store
                    .journal
                    .create(
                        k,
                        &k.into(),
                        Some(&"root".into()),
                        v,
                        &clock().into(),
                        s(p, "objectId"),
                    )
                    .unwrap();
            }
        }
        let operations = Operations {
            store: LifecycleJournal {
                journal: &store,
                pages: &NoPages,
            },
            now: clock as fn() -> String,
        };
        let operation = if let Some(id) = &operation_id {
            operations
                .store
                .read("task-board/operation", &Json::String(id.clone()), false)
                .await
                .unwrap()
        } else {
            let request = Json::object([
                ("action", "nativeUpdate".into()),
                ("operationId", "native-publication-fixture".into()),
                ("taskId", f(&task_pin, "objectId").clone()),
                ("expectedRevision", Json::Number(1.0)),
                ("run", run_pin.clone()),
                ("change", Json::object([("kind", "cancelUnstarted".into())])),
            ]);
            let actor = Json::object([
                ("principalId", "board".into()),
                ("source", "native".into()),
                ("serviceNodeId", "board-node".into()),
                ("generation", Json::Number(1.0)),
            ]);
            let op = operations.begin(&request, &actor).await.unwrap();
            operation_id = Some(s(&op.pin, "objectId").clone());
            op
        };
        let fail_final =
            |_: Json,
             v: Json|
             -> std::pin::Pin<Box<dyn std::future::Future<Output = IvyResult<()>>>> {
                Box::pin(async move {
                    if f(&v, "publication") == &Json::Null {
                        Err(
                            IvyError::new("outcome_unknown", "Lost final Task callback.")
                                .with_outcome(Outcome::Unknown),
                        )
                    } else {
                        Ok(())
                    }
                })
            };
        let mut steps = Publication {
            operations: &operations,
            operation: &operation,
            task_written: if round == 0 { Some(&fail_final) } else { None },
            index: 0.0,
        };
        let result = NativePublisher {
            store: &operations.store,
            engine: &Engine,
            evidence: &Engine,
        }
        .publish(&operation, &mut steps, None)
        .await;
        let count = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        let current = store
            .read("task-board/task", f(&task_pin, "objectId"), false)
            .await
            .unwrap();
        assert_eq!(f(&current.value, "publication"), &Json::Null);
        assert_eq!(f(&current.value, "workflowState"), &Json::from("cancelled"));
        if round == 0 {
            let e = result.unwrap_err();
            assert_eq!(e.outcome, Outcome::Unknown);
            assert_eq!(count("SELECT COUNT(*) FROM cleanup_dependencies"), 0);
            assert_eq!(count("SELECT COUNT(*) FROM cleanup_roots"), 0);
            record_count = count("SELECT COUNT(*) FROM documents");
            revision_count = count("SELECT COUNT(*) FROM revisions");
        } else {
            let out = result.unwrap();
            assert_eq!(count("SELECT COUNT(*) FROM documents"), record_count);
            assert_eq!(count("SELECT COUNT(*) FROM revisions"), revision_count);
            assert_eq!(count("SELECT COUNT(*) FROM cleanup_dependencies"), 2);
            assert_eq!(count("SELECT COUNT(*) FROM cleanup_roots"), 2);
            let dependency: String = db
                .query_row(
                    "SELECT operation_id FROM cleanup_dependencies LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(dependency, s(&operation.pin, "objectId").to_utf8().unwrap());
            let original = operations
                .store
                .read("task-board/run", &run_pin, false)
                .await
                .unwrap();
            assert_eq!(
                hash_json(&original.value).unwrap(),
                hash_json(&run).unwrap()
            );
            let terminal = operations
                .store
                .read("task-board/run", f(&out, "run"), false)
                .await
                .unwrap();
            assert_eq!(f(&terminal.value, "phase"), &Json::from("cancelled"));
            assert_eq!(
                f(f(&terminal.value, "externalOutcome"), "evidence"),
                &Json::Null
            );
            let saved = operations
                .store
                .read("task-board/result", f(&out, "result"), false)
                .await
                .unwrap();
            assert!(
                f(f(&saved.value, "content"), "artifacts")
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            operations
                .finish(s(&operation.pin, "objectId"), &out)
                .await
                .unwrap();
            assert_eq!(count("SELECT COUNT(*) FROM cleanup_roots"), 3);
        }
    }
}
