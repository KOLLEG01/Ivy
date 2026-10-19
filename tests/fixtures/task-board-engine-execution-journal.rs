use super::*;
use ivy_contracts::builtin_schema::SchemaBundle;
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    journal_store::{Contracts, JournalStore},
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    local_workspaces::{LocalWorkspaces, workspace_key},
    publication::Publication,
};
use std::cell::Cell;
fn clock() -> String {
    "2026-01-02T03:04:05.006Z".into()
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
struct Policies {
    task: Json,
}
impl Contracts for Policies {
    fn version(&self, k: &str) -> IvyResult<String> {
        Ok(if k == "task-board/task" {
            "1.8.0"
        } else {
            "1.0.0"
        }
        .into())
    }
    // Public Task routing/admission is a finite test binding; retained Operation
    // and Coordination records use their exact frozen schemas and actual journal.
    fn local(&self, k: &str) -> IvyResult<bool> {
        assert!(matches!(
            k,
            "task-board/task" | "task-board/coordination" | "task-board/operation"
        ));
        Ok(true)
    }
    fn readable_version(&self, k: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &JsString::from(self.version(k)?))
    }
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        admission(n, v)
    }
    fn validate_record(&self, k: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        match k {
            "task-board/operation" => admission("Operation", v),
            "task-board/coordination" => admission("Coordination", v),
            "task-board/task" => {
                let mut v = v.clone();
                v.insert("publication", Json::Null);
                assert_eq!(hash_json(&v)?, hash_json(&self.task)?);
                Ok(())
            }
            k => panic!("Unexpected record {k}"),
        }
    }
}
struct Client;
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            if m == "system.status" {
                Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )]))
            } else {
                panic!("No external effect permitted: {m}")
            }
        })
    }
}
struct NoPages;
impl LifecyclePages for NoPages {
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("No page scan permitted")
    }
}
struct Bindings;
impl ExecutionCategories for Bindings {
    async fn normalize(&self, _: &JsString) -> IvyResult<JsString> {
        unreachable!()
    }
    async fn revalidate(&self, _: &JsString) -> IvyResult<JsString> {
        unreachable!()
    }
}
impl ExecutionProofs for Bindings {
    type Transcript = Json;
    async fn owner_stopped(&self, _: &Json) -> IvyResult<bool> {
        unreachable!()
    }
    async fn verify_transcript(&self, _: &Json, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
}
impl ExecutionReadScope for Bindings {
    async fn with_read_cache<F: Future<Output = IvyResult<Json>>>(
        &self,
        work: F,
    ) -> IvyResult<Json> {
        work.await
    }
}
struct Publisher<'a, S, C> {
    operations: &'a Operations<S, C>,
    task: &'a Json,
    task_pin: &'a Json,
    principal: &'a JsString,
    fail_final: Cell<bool>,
}
impl<S: ExecutionStore, C: Fn() -> String> ExecutionPublication<Json> for Publisher<'_, S, C> {
    async fn publish(
        &self,
        operation: &Document,
        _: Option<&Json>,
        _: Option<&JsString>,
    ) -> IvyResult<Json> {
        let gates = PublicationCoordination {
            store: &self.operations.store,
            principal_id: self.principal,
            now: &self.operations.now,
        };
        let id = s(&operation.pin, "objectId").clone();
        let written = |_: Json, value: Json| -> Pin<Box<dyn Future<Output = IvyResult<()>> + '_>> {
            let gates = &gates;
            let id = &id;
            let failed = &self.fail_final;
            Box::pin(async move {
                if f(f(&value, "publication"), "objectId").as_js_string() == Some(id) {
                    gates.release(id, true).await?
                }
                if f(&value, "publication").is_null() && failed.replace(false) {
                    return Err(
                        IvyError::new("outcome_unknown", "Lost final Task callback.")
                            .with_outcome(Outcome::Unknown),
                    );
                }
                Ok(())
            })
        };
        let mut steps = Publication {
            operations: self.operations,
            operation,
            task_written: Some(&written),
            index: 0.0,
        };
        let mut reserved = self.task.clone();
        reserved.insert("publication", steps.pin()?);
        let first = steps
            .write(
                "task-board/task",
                &reserved,
                &Json::object([
                    ("objectId", self.task_pin.required("objectId")?.clone()),
                    (
                        "expectedRevision",
                        self.task_pin.required("revision")?.clone(),
                    ),
                ]),
            )
            .await?;
        let final_task = steps
            .write(
                "task-board/task",
                self.task,
                &Json::object([
                    ("objectId", first.required("objectId")?.clone()),
                    ("expectedRevision", first.required("revision")?.clone()),
                ]),
            )
            .await?;
        Ok(Json::object([
            (
                "operationId",
                operation.value.required("operationId")?.clone(),
            ),
            ("task", final_task),
            ("plan", Json::Null),
            ("run", Json::Null),
            ("history", Json::Null),
            ("result", Json::Null),
            ("review", Json::Null),
            ("attachment", Json::Null),
        ]))
    }
}
struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
fn count(c: &Connection, table: &str) -> i64 {
    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[tokio::test(flavor = "current_thread")]
async fn actual_gates_and_cached_execution_reopen_lost_final_callback_without_new_stages() {
    let path = std::env::temp_dir().join(format!(
        "ivy-task-board-execution-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let _cleanup = Cleanup(path.clone());
    let principal = JsString::from("board");
    let client = Arc::new(Client);
    let task = Json::object([
        (
            "fields",
            Json::object([
                ("category", Json::Null),
                ("dependencies", Json::Array(vec![])),
            ]),
        ),
        ("publication", Json::Null),
    ]);
    let mut task_pin: Option<Json> = None;
    let mut operation_id: Option<JsString> = None;
    let mut documents = 0;
    let mut revisions = 0;
    let mut prepared = vec![];
    for round in 0..3 {
        let c = Connection::open(&path).unwrap();
        initialize(&c).unwrap();
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &c,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Policies { task: task.clone() },
            limits: HistoryLimits {
                count: 10000,
                bytes: 256 * 1024 * 1024,
            },
            now: clock as fn() -> String,
            new_id: || uuid::Uuid::new_v4().to_string().into(),
            cache: None,
        };
        let task_pin = match &task_pin {
            Some(p) => p.clone(),
            None => {
                let p = journal
                    .write(
                        "task-board/task",
                        &task,
                        "seed-task",
                        &Json::object([(
                            "create",
                            Json::object([("parentId", "root".into()), ("name", "Task".into())]),
                        )]),
                    )
                    .await
                    .unwrap();
                task_pin = Some(p.clone());
                p
            }
        };
        let pages = NoPages;
        let operations = Operations {
            store: LifecycleJournal {
                journal: &journal,
                pages: &pages,
            },
            now: clock as fn() -> String,
        };
        let operation = if let Some(id) = &operation_id {
            operations
                .store
                .read("task-board/operation", &Json::String(id.clone()), true)
                .await
                .unwrap()
        } else {
            let request = Json::object([
                ("action", "transition".into()),
                ("operationId", "local-execution".into()),
                ("taskId", f(&task_pin, "objectId").clone()),
                ("expectedRevision", Json::Number(1.0)),
                ("workflowState", "todo".into()),
                ("detail", Json::Null),
            ]);
            let actor = Json::object([
                ("principalId", "caller".into()),
                ("serviceNodeId", "board-node".into()),
                ("generation", Json::Number(1.0)),
                ("source", "user".into()),
            ]);
            let operation = operations.begin(&request, &actor).await.unwrap();
            operation_id = Some(s(&operation.pin, "objectId").clone());
            let gates = PublicationCoordination {
                store: &operations.store,
                principal_id: &principal,
                now: &operations.now,
            };
            gates.acquire(&operation).await.unwrap();
            assert_eq!(count(&c, "active_gates"), 2);
            assert_eq!(
                operations
                    .store
                    .operation_gates(s(&operation.pin, "objectId"))
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                f(&gates.pending().unwrap().unwrap(), "objectId"),
                f(&operation.pin, "objectId")
            );
            assert!(
                gates
                    .release_unstarted(s(&operation.pin, "objectId"))
                    .await
                    .unwrap()
            );
            assert_eq!(count(&c, "active_gates"), 0);
            LocalWorkspaces {
                connection: &c,
                store: &journal,
                normalize: workspace_key,
            }
            .reserve(
                &"host".into(),
                &"/fixture/task".into(),
                s(&task_pin, "objectId"),
                s(&operation.pin, "objectId"),
            )
            .unwrap();
            operation
        };
        let publisher = Publisher {
            operations: &operations,
            task: &task,
            task_pin: &task_pin,
            principal: &principal,
            fail_final: Cell::new(round == 0),
        };
        let bindings = Bindings;
        let components = ExecutionComponents {
            categories: &bindings,
            proofs: &bindings,
            publication: &publisher,
            read_scope: &bindings,
        };
        let cached = CachedExecution {
            operations: &operations,
            principal_id: &principal,
            services: &components,
        };
        let executor = Executor::default();
        let queue = ExecutionQueue::new(&cached, &executor);
        let result = executor.drive(queue.execute(operation).unwrap()).await;
        executor.drive(queue.settled()).await;
        let saved = operations
            .store
            .read(
                "task-board/operation",
                &Json::String(operation_id.clone().unwrap()),
                true,
            )
            .await
            .unwrap();
        let current = operations
            .store
            .read("task-board/task", f(&task_pin, "objectId"), true)
            .await
            .unwrap();
        assert_eq!(f(&current.value, "publication"), &Json::Null);
        assert_eq!(f(&current.pin, "revision"), &Json::Number(3.0));
        assert_eq!(count(&c, "active_gates"), 0);
        let stages = f(&saved.value, "writes")
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                (
                    f(f(w, "payload"), "object").clone(),
                    f(w, "outcome").clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(stages.len(), 2);
        if round == 0 {
            assert_eq!(result.unwrap_err().outcome, Outcome::Unknown);
            assert_eq!(f(&saved.value, "phase"), &Json::from("needs_attention"));
            assert_eq!(count(&c, "cleanup_roots"), 0);
            assert_eq!(count(&c, "workspace_reservations"), 1);
            documents = count(&c, "documents");
            revisions = count(&c, "revisions");
            prepared = stages;
        } else {
            let result = result.unwrap();
            assert_eq!(f(&result, "task"), &current.pin);
            assert_eq!(f(&saved.value, "phase"), &Json::from("succeeded"));
            assert_eq!(count(&c, "documents"), documents);
            assert_eq!(count(&c, "revisions"), revisions + 1);
            assert_eq!(stages, prepared);
            assert_eq!(count(&c, "cleanup_roots"), 1);
        }
    }
}
