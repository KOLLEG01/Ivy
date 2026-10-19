use super::*;
use ivy_contracts::builtin_schema::SchemaBundle;
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    engine_execution::{
        CachedExecution, ExecutionCategories, ExecutionComponents, ExecutionProgram,
        ExecutionProofs, ExecutionReadScope,
    },
    engine_publication::{
        CreationChecks, EnginePublisher, PublicationRoutes, RootPublicationChecks,
    },
    journal_store::{Contracts, JournalStore},
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    native_lifecycle::LifecyclePublication,
    publication_coordination::PublicationCoordination,
};
use std::{cell::Cell, future::Future, pin::Pin};

fn clock() -> String {
    "2026-01-02T03:04:05.006Z".into()
}
fn admission(name: &str, value: &Json) -> IvyResult<()> {
    static B: OnceLock<SchemaBundle> = OnceLock::new();
    let bundle = B.get_or_init(|| {
        SchemaBundle::new(&[
            include_str!("../../specs/schemas/task-board.schema.json"),
            include_str!("../../specs/schemas/chat.schema.json"),
            include_str!("../../specs/schemas/agent.schema.json"),
            include_str!("../../specs/schemas/hive-wire.schema.json"),
            include_str!("../../specs/schemas/hive-operations.schema.json"),
        ])
        .unwrap()
    });
    if bundle.matches(
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{name}"),
        value,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            format!("Frozen {name} admission failed."),
        ))
    }
}
struct ContractsBinding;
impl Contracts for ContractsBinding {
    fn version(&self, key: &str) -> IvyResult<String> {
        Ok(match key {
            "task-board/task" => "1.8.0",
            "task-board/configuration" => "1.2.0",
            "task-board/run" | "task-board/delivery" => "1.1.0",
            _ => "1.0.0",
        }
        .into())
    }
    // Only local public routing is finite. Every retained value is validated
    // against its complete frozen schema, including the Operation union.
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &JsString::from(self.version(key)?))
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        admission(name, value)
    }
    fn validate_record(&self, key: &str, _: &JsString, value: &Json) -> IvyResult<()> {
        admission(
            match key {
                "task-board/task" => "Task",
                "task-board/operation" => "Operation",
                "task-board/coordination" => "Coordination",
                "task-board/history" => "History",
                "task-board/configuration" => "Configuration",
                key => panic!("Unexpected record {key}"),
            },
            value,
        )
    }
}
struct NoRpc;
impl RpcClient for NoRpc {
    fn request<'a>(&'a self, method: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "system.status" {
                Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )]))
            } else {
                panic!("No external effect allowed: {method}")
            }
        })
    }
}
struct NoPages;
impl LifecyclePages for NoPages {
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("No page scan allowed")
    }
}
struct Checks {
    first: bool,
}
impl ActionPolicies for Checks {
    async fn aggregate(&self, task: &Json, _: &JsString) -> IvyResult<()> {
        assert!(self.first, "Recovery must retain original admission");
        admission("Task", task)
    }
    async fn validate_action(
        &self,
        request: &Json,
        task: &Json,
        actor: &Json,
        at: &JsString,
        _: &JsString,
    ) -> IvyResult<()> {
        assert!(self.first, "Recovery must retain original admission");
        admission("CommentActionRequest", request)?;
        // Finite semantic policy for this one admitted user comment. The
        // owner's current action/question/handoff policy remains required.
        assert_eq!(f(request, "expectedRevision"), &Json::Number(1.0));
        assert_eq!(f(task, "workflowState"), &Json::from("waiting"));
        assert_eq!(f(actor, "source"), &Json::from("user"));
        assert_eq!(at, &JsString::from(clock()));
        Ok(())
    }
}
impl ActionRestore for Checks {
    async fn restore_for_execution(&self, _: &Document, _: &Document) -> IvyResult<()> {
        panic!("No continuation")
    }
}
struct Unused;
impl CreationChecks for Unused {
    async fn fields(&self, _: &Json) -> IvyResult<()> {
        panic!("No creation")
    }
    async fn initial_state(&self, _: &Json, _: &Json, _: &Json, _: &JsString) -> IvyResult<Json> {
        panic!("No creation")
    }
    async fn execution_options(&self, _: &Json, _: bool) -> IvyResult<()> {
        panic!("No creation")
    }
}
impl RootPublicationChecks for Unused {
    async fn configuration(&self, _: &Json) -> IvyResult<()> {
        panic!("No configuration update")
    }
}
struct Routes<'a, A>(&'a A);
impl<A: ExistingTaskPublication> PublicationRoutes<Json> for Routes<'_, A> {
    async fn native_update<P: LifecyclePublication>(
        &self,
        _: &Document,
        _: &mut P,
        _: Option<&Json>,
    ) -> IvyResult<Json> {
        panic!("No native update")
    }
    async fn lifecycle<P: LifecyclePublication>(
        &self,
        _: &Document,
        _: &mut P,
        _: Json,
    ) -> IvyResult<Json> {
        panic!("No lifecycle update")
    }
    async fn existing_task<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        outcome: Json,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json> {
        self.0.publish(operation, steps, outcome, normalized).await
    }
}
impl ExecutionCategories for Unused {
    async fn normalize(&self, _: &JsString) -> IvyResult<JsString> {
        panic!("Uncategorized Task")
    }
    async fn revalidate(&self, _: &JsString) -> IvyResult<JsString> {
        panic!("Uncategorized Task")
    }
}
impl ExecutionProofs for Unused {
    type Transcript = Json;
    async fn owner_stopped(&self, _: &Json) -> IvyResult<bool> {
        panic!("No native Run")
    }
    async fn verify_transcript(&self, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No native transcript")
    }
}
impl ExecutionReadScope for Unused {
    async fn with_read_cache<F: Future<Output = IvyResult<Json>>>(
        &self,
        work: F,
    ) -> IvyResult<Json> {
        work.await
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
fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn initial_task() -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskKey", "TASK-0001".into()),
        (
            "fields",
            Json::object([
                ("title", "Original title".into()),
                ("description", "A retained comment".into()),
                ("acceptanceCriteria", Json::Array(vec![])),
                ("category", Json::Null),
                ("control", "agent".into()),
                ("priority", Json::Number(2.0)),
                ("executionRequirement", Json::Null),
                (
                    "workspaceRequirement",
                    Json::object([
                        ("kind", "directory_path".into()),
                        ("path", "/fixture".into()),
                    ]),
                ),
                ("dependencies", Json::Array(vec![])),
                ("userContact", "ticket".into()),
                ("allowParallel", false.into()),
                ("nextReviewAt", Json::Null),
                ("dueAt", Json::Null),
            ]),
        ),
        ("workflowState", "waiting".into()),
        (
            "waiting",
            Json::object([
                ("reason", "user".into()),
                ("detail", "Waiting for input".into()),
                ("since", clock().into()),
            ]),
        ),
        ("publication", Json::Null),
        ("primaryResourceRef", Json::Null),
        ("claim", Json::Null),
        ("attemptCount", Json::Number(0.0)),
        ("lastRun", Json::Null),
        ("lastHistory", Json::Null),
        ("latestResult", Json::Null),
        ("acceptedReview", Json::Null),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        ("agentCommentCount", Json::Number(0.0)),
        ("workRevision", Json::Number(0.0)),
        ("attachments", Json::Array(vec![])),
        ("createdAt", clock().into()),
        ("updatedAt", clock().into()),
    ])
}
#[tokio::test(flavor = "current_thread")]
async fn comment_recovery_preserves_a_later_writer_and_returns_the_original_receipt() {
    let path = std::env::temp_dir().join(format!(
        "ivy-task-board-comment-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let _cleanup = Cleanup(path.clone());
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let principal = JsString::from("board");
    let client = Arc::new(NoRpc);
    let mut operation_id: Option<JsString> = None;
    let mut original_stages = vec![];
    let mut later_task = Json::Null;
    let mut documents = 0;
    let mut revisions = 0;
    for round in 0..3 {
        let connection = Connection::open(&path).unwrap();
        initialize(&connection).unwrap();
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &connection,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: ContractsBinding,
            limits: HistoryLimits::default(),
            now: clock as fn() -> String,
            new_id: || uuid::Uuid::new_v4().to_string().into(),
            cache: None,
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
            let task = journal
                .write(
                    "task-board/task",
                    &initial_task(),
                    "initial-task",
                    &Json::object([(
                        "create",
                        Json::object([("parentId", "root".into()), ("name", "Task".into())]),
                    )]),
                )
                .await
                .unwrap();
            let request = Json::object([
                ("action", "comment".into()),
                ("operationId", "comment-journal".into()),
                ("taskId", f(&task, "objectId").clone()),
                ("expectedRevision", Json::Number(1.0)),
                ("commentId", "comment".into()),
                ("body", "Please continue the work.".into()),
                ("requests", Json::Array(vec![])),
                ("responses", Json::Array(vec![])),
                ("attachments", Json::Array(vec![])),
                ("replyTo", Json::Null),
                ("authorKind", "user".into()),
                ("purpose", "update".into()),
                ("agentDelivery", "queue".into()),
                ("contactUser", false.into()),
            ]);
            let actor = Json::object([
                ("principalId", "caller".into()),
                ("serviceNodeId", "board-node".into()),
                ("generation", Json::Number(1.0)),
                ("source", "user".into()),
            ]);
            let operation = operations.begin(&request, &actor).await.unwrap();
            operation_id = Some(s(&operation.pin, "objectId").clone());
            operation
        };
        let gates = PublicationCoordination {
            store: &operations.store,
            principal_id: &principal,
            now: &operations.now,
        };
        let failed = Cell::new(round == 0);
        let id = s(&operation.pin, "objectId").clone();
        let callback = |_: Json, task: Json| -> Pin<Box<dyn Future<Output = IvyResult<()>> + '_>> {
            let gates = &gates;
            let failed = &failed;
            let id = &id;
            Box::pin(async move {
                if f(f(&task, "publication"), "objectId").as_js_string() == Some(id) {
                    gates.release(id, true).await?;
                }
                if f(&task, "publication").is_null() && failed.replace(false) {
                    return Err(
                        IvyError::new("outcome_unknown", "Lost final comment callback.")
                            .with_outcome(Outcome::Unknown),
                    );
                }
                Ok(())
            })
        };
        let checks = Checks { first: round == 0 };
        let unused = Unused;
        let settings = Json::object([("phoneTarget", Json::Null)]);
        let actions = TaskActions {
            store: &operations.store,
            settings: &settings,
            catalogs: &catalogs,
            policies: &checks,
            lifecycle: &checks,
        };
        let routes = Routes(&actions);
        let publisher = EnginePublisher {
            operations: &operations,
            catalogs: &catalogs,
            checks: &unused,
            routes: &routes,
            task_written: &callback,
        };
        let components = ExecutionComponents {
            categories: &unused,
            proofs: &unused,
            publication: &publisher,
            read_scope: &unused,
        };
        let cached = CachedExecution {
            operations: &operations,
            principal_id: &principal,
            services: &components,
        };
        let result = cached.run(operation).await;
        let saved = operations
            .store
            .read(
                "task-board/operation",
                &Json::String(operation_id.clone().unwrap()),
                true,
            )
            .await
            .unwrap();
        let writes = f(&saved.value, "writes").as_array().unwrap();
        assert_eq!(writes.len(), 3);
        let stages = writes
            .iter()
            .map(|w| {
                (
                    f(f(w, "payload"), "object").clone(),
                    f(w, "outcome").clone(),
                )
            })
            .collect::<Vec<_>>();
        let task = operations
            .store
            .read(
                "task-board/task",
                f(f(&writes[2], "outcome"), "objectId"),
                true,
            )
            .await
            .unwrap();
        assert_eq!(f(&task.value, "publication"), &Json::Null);
        assert_eq!(f(&task.value, "workflowState"), &Json::from("todo"));
        assert_eq!(f(&task.value, "waiting"), &Json::Null);
        assert_eq!(f(&task.value, "workRevision"), &Json::Number(1.0));
        assert_eq!(f(&task.value, "comments").as_array().unwrap().len(), 1);
        assert_eq!(
            f(&task.value, "commentDeliveries")
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(f(&task.value, "lastHistory"), f(&writes[1], "outcome"));
        assert_eq!(count(&connection, "active_gates"), 0);
        if round == 0 {
            assert_eq!(result.unwrap_err().outcome, Outcome::Unknown);
            assert_eq!(f(&saved.value, "phase"), &Json::from("needs_attention"));
            assert_eq!(f(&task.pin, "revision"), &Json::Number(3.0));
            assert_eq!(count(&connection, "cleanup_roots"), 0);
            let mut changed = task.value;
            let mut fields = f(&changed, "fields").clone();
            fields.insert("title", "Concurrent title".into());
            changed.insert("fields", fields);
            // A different writer commits after the final effect but before
            // recovery. Returning the old action receipt must not rewrite it.
            let changed_pin = journal
                .write(
                    "task-board/task",
                    &changed,
                    "other-writer",
                    &Json::object([(
                        "update",
                        Json::object([
                            ("objectId", f(&task.pin, "objectId").clone()),
                            ("expectedRevision", Json::Number(3.0)),
                        ]),
                    )]),
                )
                .await
                .unwrap();
            assert_eq!(f(&changed_pin, "revision"), &Json::Number(4.0));
            original_stages = stages;
            later_task = changed;
            documents = count(&connection, "documents");
            revisions = count(&connection, "revisions");
        } else {
            let outcome = result.unwrap();
            assert_eq!(f(&outcome, "task"), f(&writes[2], "outcome"));
            assert_eq!(f(f(&outcome, "task"), "revision"), &Json::Number(3.0));
            assert_eq!(f(&task.pin, "revision"), &Json::Number(4.0));
            assert_eq!(stages, original_stages);
            assert_eq!(
                hash_json(&task.value).unwrap(),
                hash_json(&later_task).unwrap()
            );
            assert_eq!(f(&saved.value, "phase"), &Json::from("succeeded"));
            assert_eq!(count(&connection, "documents"), documents);
            assert_eq!(count(&connection, "revisions"), revisions + 1);
            assert_eq!(count(&connection, "cleanup_roots"), 1);
        }
    }
}
