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
            "task-board/result" => admission("Result", v),
            "task-board/history" => admission("History", v),
            "task-board/task" => {
                let mutable = [
                    "publication",
                    "updatedAt",
                    "claim",
                    "workflowState",
                    "waiting",
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
impl NativePublicationPolicies for Engine {
    fn now(&self) -> JsString {
        clock().into()
    }
    fn handoff(&self, _: &Json, _: &JsString) -> IvyResult<Json> {
        panic!("No completion prompt is involved")
    }
    fn retry_at(&self, _: &Json, _: &JsString) -> IvyResult<Json> {
        panic!("No native turn is involved")
    }
    fn shared_project(&self, _: &Json, _: &Json) -> IvyResult<bool> {
        panic!("Cancellation already releases its reservation")
    }
    async fn aggregate(&self, t: &Json, _: &JsString) -> IvyResult<()> {
        assert_eq!(f(t, "workflowState"), &Json::from("in_progress"));
        Ok(())
    }
    async fn finish_active<P: LifecyclePublication>(&self, c: &Json, _: &mut P) -> IvyResult<Json> {
        assert_eq!(f(f(c, "completion"), "state"), &Json::from("cancelled"));
        let mut t = f(c, "finalTask").clone();
        t.insert("claim", Json::Null);
        t.insert("workflowState", "cancelled".into());
        t.insert("waiting", Json::Null);
        t.insert("lastRun", f(f(c, "outcome"), "run").clone());
        t.insert("latestResult", f(f(c, "outcome"), "result").clone());
        t.insert("acceptedReview", Json::Null);
        Ok(t)
    }
}
impl NativePublicationEngine for Engine {
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
    let row = data()
        .as_array()
        .unwrap()
        .iter()
        .find(|r| s(r, "name") == &JsString::from("cancelUnstarted active"))
        .unwrap();
    let mut task = f(f(f(row, "initial"), "task"), "value").clone();
    task.insert("attachments", Json::Array(vec![]));
    let mut run = f(f(f(row, "initial"), "run"), "value").clone();
    run.insert("owner", Json::object([("source", "scheduler".into())]));
    let task_pin = f(f(f(row, "initial"), "task"), "pin").clone();
    let run_pin = f(f(f(row, "initial"), "run"), "pin").clone();
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
