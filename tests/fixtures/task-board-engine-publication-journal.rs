use super::*;
use crate::frozen_contracts::{
    ContractsBinding as RetainedContracts, admission as retained_admission,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_sdk::tools::ToolClients;
use ivy_store::rusqlite::Connection;
use ivy_task_board::checks::TaskChecks;
use ivy_task_board::engine_checks::{CheckPolicy, EngineChecks};
use ivy_task_board::engine_publication::RootChecks;
use ivy_task_board::journal_store::Contracts;
use ivy_task_board::{
    configuration::initial_configuration,
    engine_execution::{
        CachedExecution, ExecutionCategories, ExecutionComponents, ExecutionProgram,
        ExecutionProofs,
    },
    journal_store::{JournalStore, ReadScopes},
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    publication_coordination::PublicationCoordination,
    task_written::{TaskCommentNotices, TaskWriteObserver, TaskWriteProjection},
};
use std::cell::{Cell, RefCell};

// Exercise current creation and recovery on SQLite. This finite Task/field
// boundary does not claim complete current-registry schema qualification.
fn admission(name: &str, value: &Json) -> IvyResult<()> {
    if name == "TaskFields" {
        for key in [
            "title",
            "description",
            "acceptanceCriteria",
            "category",
            "control",
            "priority",
            "executionRequirement",
            "workspaceRequirement",
            "dueAt",
        ] {
            value.required(key)?;
        }
        assert!(value.get("nextReviewAt").is_none() && value.get("dependencies").is_none());
        return Ok(());
    }
    retained_admission(name, value)
}
struct ContractsBinding;
impl Contracts for ContractsBinding {
    fn version(&self, key: &str) -> IvyResult<String> {
        if key == "task-board/task" {
            Ok("3.1.0".into())
        } else {
            RetainedContracts.version(key)
        }
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        RetainedContracts.local(key)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &JsString::from(self.version(key)?))
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        admission(name, value)
    }
    fn validate_record(&self, key: &str, version: &JsString, value: &Json) -> IvyResult<()> {
        if key == "task-board/task" {
            admission("TaskFields", value.required("fields")?)?;
            assert!(value.get("waiting").is_none());
            for key in [
                "wakeAt",
                "runtimeWait",
                "dependencies",
                "observedDependencies",
            ] {
                value.required(key)?;
            }
            Ok(())
        } else if key == "task-board/operation" {
            let request = value.required("request")?;
            assert_eq!(request.required("action")?, &Json::from("create"));
            admission("TaskFields", request.required("fields")?)?;
            for key in [
                "operationId",
                "callerPrincipalId",
                "requestHash",
                "actor",
                "phase",
                "writes",
            ] {
                value.required(key)?;
            }
            Ok(())
        } else {
            RetainedContracts.validate_record(key, version, value)
        }
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
struct Policy {
    first: bool,
}
impl CheckPolicy for Policy {
    async fn aggregate(&self, _: &Json, _: &JsString) -> IvyResult<()> {
        panic!("Not an existing Task")
    }
    async fn fields(&self, fields: &Json, own: Option<&JsString>) -> IvyResult<()> {
        assert!(self.first, "Recovery must retain original checks");
        assert!(own.is_none());
        admission("TaskFields", fields)
    }
    async fn creation(&self, _: &Json, _: &Json, _: &Json, _: &JsString) -> IvyResult<()> {
        assert!(self.first, "Recovery must retain original state");
        Ok(())
    }
}
struct Unused;
impl PublicationRoutes<Json> for Unused {
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
        panic!("No lifecycle request")
    }
    async fn existing_task<P: LifecyclePublication>(
        &self,
        _: &Document,
        _: &mut P,
        _: Json,
        _: Option<&JsString>,
    ) -> IvyResult<Json> {
        panic!("No existing Task action")
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
// Projection is an explicit finite capture. Gate release, notice selection and
// operation-scoped publication use the production programs in this fixture.
#[derive(Default)]
struct Projection(RefCell<Vec<(JsString, f64, Json)>>);
impl TaskWriteProjection for Projection {
    fn observe(&self, id: &JsString, revision: f64, task: &Json) -> IvyResult<()> {
        self.0
            .borrow_mut()
            .push((id.clone(), revision, task.clone()));
        Ok(())
    }
}
struct LostFinalWrite<'a, W> {
    observer: &'a W,
    failed: &'a Cell<bool>,
    operation_id: &'a JsString,
}
impl<W: TaskWriteSink> TaskWriteSink for LostFinalWrite<'_, W> {
    async fn written(&self, operation: &Document, pin: &Json, task: &Json) -> IvyResult<()> {
        assert_eq!(s(&operation.pin, "objectId"), self.operation_id);
        assert_eq!(
            f(f(&operation.value, "request"), "operationId"),
            &Json::from("create-journal")
        );
        self.observer.written(operation, pin, task).await?;
        if f(task, "publication").is_null() && self.failed.replace(false) {
            return Err(
                IvyError::new("outcome_unknown", "Lost final creation callback.")
                    .with_outcome(Outcome::Unknown),
            );
        }
        Ok(())
    }
}
#[tokio::test(flavor = "current_thread")]
async fn actual_create_reopens_original_key_result_history_and_task_after_lost_final_callback() {
    let path = std::env::temp_dir().join(format!(
        "ivy-task-board-create-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let _cleanup = Cleanup(path.clone());
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let principal = JsString::from("board");
    let client = Arc::new(NoRpc);
    let mut operation_id: Option<JsString> = None;
    let mut original_stages = vec![];
    let mut original_task = Json::Null;
    let mut documents = 0;
    let mut revisions = 0;
    for round in 0..3 {
        let connection = Connection::open(&path).unwrap();
        initialize(&connection).unwrap();
        let read_scopes = ReadScopes::default();
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
            cache: Some(read_scopes.cache()),
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
            let fields = Json::object([
                ("title", "Manual creation".into()),
                ("description", "Retained manual final material".into()),
                ("acceptanceCriteria", Json::Array(vec![])),
                ("category", Json::Null),
                ("control", "user".into()),
                ("priority", Json::Number(2.0)),
                ("executionRequirement", Json::Null),
                (
                    "workspaceRequirement",
                    Json::object([
                        ("kind", "directory_path".into()),
                        ("path", "/fixture".into()),
                    ]),
                ),
                ("dueAt", Json::Null),
            ]);
            let content = Json::object([
                ("summary", "Original final material".into()),
                ("artifacts", Json::Array(vec![])),
                ("checks", Json::Array(vec![])),
                ("codeReferences", Json::Array(vec![])),
                ("repositoryResult", Json::Null),
                ("limitations", Json::Array(vec![])),
            ]);
            let request = Json::object([
                ("action", "create".into()),
                ("operationId", "create-journal".into()),
                ("fields", fields),
                ("workflowState", "review".into()),
                ("initialResult", content),
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
        let projection = Projection::default();
        let notices = TaskCommentNotices;
        let node = JsString::from("board-node");
        let observer = TaskWriteObserver {
            projection: &projection,
            gates: &gates,
            notices: &notices,
            client: client.clone(),
            service_node_id: &node,
        };
        let callback = LostFinalWrite {
            observer: &observer,
            failed: &failed,
            operation_id: &id,
        };
        let tools = ToolClients::new(client.clone());
        let policy = Policy { first: round == 0 };
        let checks = EngineChecks {
            policy: &policy,
            core: TaskChecks {
                store: &operations.store,
                catalogs: &catalogs,
                tools: &tools,
            },
        };
        let root_checks = RootChecks {
            journal: &journal,
            creation: &checks,
        };
        let unused = Unused;
        let publisher = ObservedPublication {
            operations: &operations,
            catalogs: &catalogs,
            checks: &root_checks,
            routes: &unused,
            observer: &callback,
        };
        let components = ExecutionComponents {
            categories: &unused,
            proofs: &unused,
            publication: &publisher,
            read_scope: &read_scopes,
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
        assert_eq!(writes.len(), 4);
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
                f(f(&writes[3], "outcome"), "objectId"),
                true,
            )
            .await
            .unwrap();
        assert_eq!(f(&task.pin, "revision"), &Json::Number(2.0));
        assert_eq!(f(&task.value, "taskKey"), &Json::from("TASK-0001"));
        assert_eq!(f(&task.value, "publication"), &Json::Null);
        assert_eq!(
            f(f(&task.value, "fields"), "userContact"),
            &Json::from("chat")
        );
        assert_eq!(
            f(f(&task.value, "fields"), "allowParallel"),
            &Json::Bool(false)
        );
        assert_eq!(f(&task.value, "latestResult"), f(&writes[1], "outcome"));
        assert_eq!(f(&task.value, "lastHistory"), f(&writes[2], "outcome"));
        assert_eq!(count(&connection, "active_gates"), 0);
        let observed = projection.0.borrow();
        assert_eq!(observed.len(), if round == 2 { 0 } else { 2 });
        if !observed.is_empty() {
            assert_eq!(observed.iter().map(|v| v.1).collect::<Vec<_>>(), [1.0, 2.0]);
            assert!(
                observed
                    .iter()
                    .all(|v| &Json::String(v.0.clone()) == f(&task.pin, "objectId"))
            );
        }
        if round == 0 {
            assert_eq!(result.unwrap_err().outcome, Outcome::Unknown);
            assert_eq!(f(&saved.value, "phase"), &Json::from("needs_attention"));
            assert_eq!(count(&connection, "cleanup_roots"), 0);
            // Deliberately change defaults before restart. Immutable prepared
            // Task bytes, checks, key allocation and receipt pins must win.
            let mut changed = initial_configuration();
            let mut defaults = f(&changed, "defaults").clone();
            defaults.insert("userContact", "phone".into());
            defaults.insert("allowParallel", true.into());
            changed.insert("defaults", defaults);
            journal
                .write(
                    "task-board/configuration",
                    &changed,
                    "changed-defaults",
                    &Json::object([(
                        "create",
                        Json::object([
                            ("parentId", "root".into()),
                            ("name", "TaskBoard configuration".into()),
                        ]),
                    )]),
                )
                .await
                .unwrap();
            original_stages = stages;
            original_task = task.value;
            documents = count(&connection, "documents");
            revisions = count(&connection, "revisions");
        } else {
            let outcome = result.unwrap();
            assert_eq!(f(&outcome, "task"), &task.pin);
            assert_eq!(stages, original_stages);
            assert_eq!(
                hash_json(&task.value).unwrap(),
                hash_json(&original_task).unwrap()
            );
            assert_eq!(f(&saved.value, "phase"), &Json::from("succeeded"));
            assert_eq!(count(&connection, "documents"), documents);
            assert_eq!(count(&connection, "revisions"), revisions + 1);
            assert_eq!(count(&connection, "cleanup_roots"), 1);
        }
    }
}
