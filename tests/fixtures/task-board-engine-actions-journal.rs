use super::*;
use ivy_contracts::builtin_schema::SchemaBundle;
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    category_projection::{CategoryLocale, CategoryProjection},
    engine_admission::{AdmissionRuntime, EngineAdmission, EngineOwner},
    engine_execution::{
        CachedExecution, ExecutionCategories, ExecutionComponents, ExecutionProgram,
        ExecutionProofs, ExecutionQueue,
    },
    engine_invocations::EngineInvocations,
    engine_publication::{
        CreationChecks, EnginePublisher, PublicationRoutes, RootPublicationChecks,
    },
    engine_read::ReadContext,
    engine_validation::{ActionValidation, ActionValidationChecks, check_aggregate},
    journal_store::{Contracts, JournalStore, ReadScopes},
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    native_lifecycle::LifecyclePublication,
    publication_coordination::PublicationCoordination,
    reconciler::ReconcileExecutor,
    task_written::{TaskCommentNotices, TaskWriteObserver},
};
use std::{
    cell::{Cell, RefCell},
    cmp::Ordering as SortOrder,
    collections::HashMap,
    future::{Future, poll_fn},
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Poll, Waker},
};

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
#[derive(Default)]
struct NoRpc {
    owner_checks: AtomicUsize,
    fail_notice: AtomicBool,
    notices: Mutex<HashMap<JsString, Json>>,
    notice_calls: Mutex<Vec<Json>>,
}
impl RpcClient for NoRpc {
    fn request<'a>(&'a self, method: &'a str, args: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "serviceNodes.get" {
                assert_eq!(args, Json::object([("serviceNodeId", "board-node".into())]));
                self.owner_checks.fetch_add(1, Ordering::SeqCst);
                Ok(Json::object([
                    ("principalId", "board".into()),
                    ("serviceName", "task-board".into()),
                    ("connected", true.into()),
                    ("synced", true.into()),
                ]))
            } else if method == "system.status" {
                Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )]))
            } else if method == "events.head" || method == "events.ack" {
                Ok(Json::object([("throughSequence", Json::Number(0.0))]))
            } else if method == "events.subscribe" {
                Ok(Json::object([
                    ("items", Json::Array(vec![])),
                    ("throughSequence", Json::Number(0.0)),
                    ("gap", Json::Null),
                ]))
            } else if method == "events.publish" {
                assert_eq!(
                    f(&args, "topic"),
                    &Json::from("task-board.browser-notification")
                );
                assert_eq!(f(&args, "topicVersion"), &Json::from("1.0.0"));
                static NOTICE: OnceLock<SchemaBundle> = OnceLock::new();
                let schema = NOTICE.get_or_init(|| {
                    SchemaBundle::new(&[concat!(
                        r#"{"$id":"https://ivy.invalid/test/task-board-notice","allOf":["#,
                        include_str!("../../specs/schemas/browser-notice.schema.json"),
                        "]}"
                    )])
                    .unwrap()
                });
                assert!(schema.matches(
                    "https://ivy.invalid/test/task-board-notice",
                    f(&args, "payload")
                )?);
                self.notice_calls.lock().unwrap().push(args.clone());
                let mut notices = self.notices.lock().unwrap();
                let key = s(&args, "mutationId").clone();
                if let Some(prior) = notices.get(&key) {
                    assert_eq!(hash_json(prior)?, hash_json(f(&args, "payload"))?);
                } else {
                    notices.insert(key, f(&args, "payload").clone());
                }
                drop(notices);
                if self.fail_notice.swap(false, Ordering::SeqCst) {
                    return Err(IvyError::new(
                        "outcome_unknown",
                        "Lost committed browser notice reply.",
                    )
                    .with_outcome(Outcome::Unknown));
                }
                Ok(Json::object(Vec::<(&str, Json)>::new()))
            } else {
                panic!("No external effect allowed: {method}")
            }
        })
    }
}
struct NoPages;
impl LifecyclePages for NoPages {
    async fn page(&self, key: &str, options: &Json) -> IvyResult<Json> {
        assert_eq!(key, "task-board/task");
        assert_eq!(
            f(options, "select"),
            &Json::Array(vec![
                "data:/fields/category".into(),
                "data:/createdAt".into()
            ])
        );
        // Finite empty initial snapshot. Subsequent observations and old-replay
        // revision fences use the actual CategoryProjection implementation.
        Ok(Json::object([
            ("items", Json::Array(vec![])),
            ("nextCursor", Json::Null),
        ]))
    }
}
struct Locale;
impl CategoryLocale for Locale {
    fn lowercase_default(&self, value: &JsString) -> JsString {
        let value = value.to_utf8().unwrap();
        assert!(value.is_ascii());
        value.to_ascii_lowercase().into()
    }
    fn compare_default(&self, left: &JsString, right: &JsString) -> SortOrder {
        assert!(left.to_utf8().unwrap().is_ascii() && right.to_utf8().unwrap().is_ascii());
        left.cmp(right)
    }
}
#[derive(Default)]
struct Executor<'a>(
    RefCell<VecDeque<Pin<Box<dyn Future<Output = ()> + 'a>>>>,
    RefCell<Option<Waker>>,
);
impl<'a> ReconcileExecutor<'a> for Executor<'a> {
    fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + 'a>>) {
        self.0.borrow_mut().push_back(work);
        if let Some(waiter) = self.1.borrow().as_ref() {
            waiter.wake_by_ref();
        }
    }
}
impl Executor<'_> {
    async fn drive<F: Future>(&self, work: F) -> F::Output {
        let mut work = Box::pin(work);
        let mut result = None;
        poll_fn(|cx| {
            *self.1.borrow_mut() = Some(cx.waker().clone());
            if result.is_none() {
                if let Poll::Ready(value) = work.as_mut().poll(cx) {
                    result = Some(value);
                }
            }
            let mut jobs = std::mem::take(&mut *self.0.borrow_mut());
            let mut pending = VecDeque::new();
            while let Some(mut job) = jobs.pop_front() {
                if job.as_mut().poll(cx).is_pending() {
                    pending.push_back(job);
                }
            }
            self.0.borrow_mut().extend(pending);
            if result.is_some() && self.0.borrow().is_empty() {
                Poll::Ready(result.take().unwrap())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}
struct Checks<'a, S> {
    first: bool,
    store: &'a S,
}
impl<S: Store> ActionValidationChecks for Checks<'_, S> {
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()> {
        assert!(self.first, "Recovery must retain original admission");
        admission("Task", task)?;
        check_aggregate(self.store, task, id).await
    }
    async fn fields(&self, _: &Json, _: &JsString) -> IvyResult<()> {
        panic!("No field edit")
    }
    async fn execution_options(&self, _: &Json, _: Option<&Json>) -> IvyResult<()> {
        panic!("No execution options")
    }
    async fn execution(
        &self,
        _: &Json,
        _: &Json,
        _: &JsString,
        _: &JsString,
        _: &Json,
        _: &Json,
        _: bool,
    ) -> IvyResult<Json> {
        panic!("No execution")
    }
    async fn deferral(&self, _: &Json, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("No deferral")
    }
    async fn content(&self, _: &Json) -> IvyResult<()> {
        panic!("No final material")
    }
    async fn artifact(&self, _: &Json) -> IvyResult<()> {
        panic!("No attachment")
    }
    fn coordination_pending(&self, _: &Json) -> IvyResult<bool> {
        panic!("No scheduling")
    }
    async fn failed_rate_limit_retry_at(&self, _: &Document) -> IvyResult<Option<JsString>> {
        panic!("No recovery proof")
    }
    async fn unstarted_archived_context(&self, _: &Document) -> IvyResult<bool> {
        panic!("No recovery proof")
    }
}
impl<S> ActionRestore for Checks<'_, S> {
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
                ("category", "Original category".into()),
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
    check_comment_recovery(false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn agent_question_recovery_replays_original_notice_and_preserves_projection_revision() {
    check_comment_recovery(true).await;
}
async fn check_comment_recovery(question: bool) {
    let path = std::env::temp_dir().join(format!(
        "ivy-task-board-comment-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let _cleanup = Cleanup(path.clone());
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let principal = JsString::from("board");
    let client = Arc::new(NoRpc::default());
    client.fail_notice.store(question, Ordering::SeqCst);
    let mut operation_id: Option<JsString> = None;
    let mut original_stages = vec![];
    let mut later_task = Json::Null;
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
            let mut initial = initial_task();
            if question {
                let mut fields = f(&initial, "fields").clone();
                fields.insert("userContact", "chat".into());
                initial.insert("fields", fields);
            }
            let task = journal
                .write(
                    "task-board/task",
                    &initial,
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
                (
                    "body",
                    if question {
                        "Which input is needed?"
                    } else {
                        "Please continue the work."
                    }
                    .into(),
                ),
                ("requests", Json::Array(vec![])),
                ("responses", Json::Array(vec![])),
                ("attachments", Json::Array(vec![])),
                ("replyTo", Json::Null),
                ("authorKind", if question { "agent" } else { "user" }.into()),
                (
                    "purpose",
                    if question { "question" } else { "update" }.into(),
                ),
                ("agentDelivery", "queue".into()),
                ("contactUser", false.into()),
            ]);
            let actor = Json::object([
                ("principalId", "caller".into()),
                ("serviceNodeId", "board-node".into()),
                ("generation", Json::Number(1.0)),
                ("source", if question { "worker" } else { "user" }.into()),
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
        let locale = Locale;
        let executor = Executor::default();
        let projection = CategoryProjection::new(&operations.store, &principal, &locale, &executor);
        executor.drive(projection.categories()).await.unwrap();
        if round > 0 {
            projection.observe(
                s(f(&operation.value, "request"), "taskId"),
                4.0,
                &later_task,
            );
        }
        let node = JsString::from("board-node");
        let notices = TaskCommentNotices;
        let observer = TaskWriteObserver {
            projection: &projection,
            gates: &gates,
            notices: &notices,
            client: client.clone(),
            service_node_id: &node,
        };
        let observed_operation = operation.clone();
        let failed = Cell::new(round == 0 && !question);
        let callback = |pin: Json,
                        task: Json|
         -> Pin<Box<dyn Future<Output = IvyResult<()>> + '_>> {
            let observer = &observer;
            let operation = &observed_operation;
            let failed = &failed;
            Box::pin(async move {
                observer.written(operation, &pin, &task).await?;
                if f(&task, "publication").is_null() && failed.replace(false) {
                    return Err(
                        IvyError::new("outcome_unknown", "Lost final comment callback.")
                            .with_outcome(Outcome::Unknown),
                    );
                }
                Ok(())
            })
        };
        let checks = Checks {
            first: round == 0,
            store: &operations.store,
        };
        let validation = ActionValidation {
            store: &operations.store,
            checks: &checks,
        };
        let unused = Unused;
        let settings = Json::object([
            ("principalId", "board".into()),
            ("rootObjectId", "root".into()),
            (
                "scheduler",
                Json::object([
                    ("enabled", true.into()),
                    ("intervalMs", Json::Number(1000.0)),
                    ("pageSize", Json::Number(50.0)),
                ]),
            ),
            ("phoneTarget", Json::Null),
        ]);
        let actions = TaskActions {
            store: &operations.store,
            settings: &settings,
            catalogs: &catalogs,
            policies: &validation,
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
            read_scope: &read_scopes,
        };
        let cached = CachedExecution {
            operations: &operations,
            principal_id: &principal,
            services: &components,
        };
        let execution_executor = Executor::default();
        let execution = ExecutionQueue::new(&cached, &execution_executor);
        let runtime = AdmissionRuntime {
            client: client.clone(),
            contracts: &journal.contracts,
            catalogs: &catalogs,
        };
        let owner = EngineOwner {
            service_node_id: node.clone(),
            generation: 1.0,
        };
        let engine =
            EngineAdmission::new(&operations, &settings, &owner, &runtime, &execution).unwrap();
        let invocation_executor = Executor::default();
        let invocations = EngineInvocations::new(&invocation_executor);
        let caller = JsString::from("caller");
        let check_cancelled = || Ok(());
        let context = ReadContext {
            caller_principal_id: &caller,
            operation_id: s(&operation.value, "operationId"),
            generation: 1.0,
            check_cancelled: &check_cancelled,
        };
        // The accepted Operation and all replayed effects live in the real
        // SQLite journal. Every public retry traverses admission and both
        // retained queues, including after the final notice reply was lost.
        let reply = invocations
            .invoke(&engine, f(&operation.value, "request"), &context)
            .unwrap();
        let result = invocation_executor
            .drive(execution_executor.drive(reply))
            .await;
        invocation_executor.drive(invocations.stop()).await;
        execution_executor.drive(execution.settled()).await;
        assert_eq!(client.owner_checks.load(Ordering::SeqCst), round + 1);
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
        assert_eq!(
            f(&task.value, "workflowState"),
            &Json::from(if question { "waiting" } else { "todo" })
        );
        if question {
            assert_eq!(f(f(&task.value, "waiting"), "reason"), &Json::from("user"));
        } else {
            assert_eq!(f(&task.value, "waiting"), &Json::Null);
        }
        assert_eq!(
            f(&task.value, "workRevision"),
            &Json::Number(if question { 0.0 } else { 1.0 })
        );
        assert_eq!(
            f(&task.value, "agentCommentCount"),
            &Json::Number(if question { 1.0 } else { 0.0 })
        );
        assert_eq!(f(&task.value, "comments").as_array().unwrap().len(), 1);
        assert_eq!(
            f(&task.value, "commentDeliveries")
                .as_array()
                .unwrap()
                .len(),
            if question { 0 } else { 1 }
        );
        assert_eq!(f(&task.value, "lastHistory"), f(&writes[1], "outcome"));
        assert_eq!(count(&connection, "active_gates"), 0);
        let categories = executor.drive(projection.categories()).await.unwrap();
        assert_eq!(
            f(&categories, "categories"),
            &Json::Array(vec![
                if round == 0 {
                    "Original category"
                } else {
                    "Concurrent category"
                }
                .into()
            ])
        );
        assert_eq!(
            client.notice_calls.lock().unwrap().len(),
            if question {
                if round == 0 { 1 } else { 2 }
            } else {
                0
            }
        );
        assert_eq!(
            client.notices.lock().unwrap().len(),
            if question { 1 } else { 0 }
        );
        for notice in client.notice_calls.lock().unwrap().iter() {
            assert_eq!(
                f(f(notice, "payload"), "title"),
                &Json::from("Ivy · Original title")
            );
        }
        if round == 0 {
            assert_eq!(result.unwrap_err().outcome, Outcome::Unknown);
            assert_eq!(f(&saved.value, "phase"), &Json::from("needs_attention"));
            assert_eq!(f(&task.pin, "revision"), &Json::Number(3.0));
            assert_eq!(count(&connection, "cleanup_roots"), 0);
            let mut changed = task.value;
            let mut fields = f(&changed, "fields").clone();
            fields.insert("title", "Concurrent title".into());
            fields.insert("category", "Concurrent category".into());
            changed.insert("fields", fields);
            // A different writer commits after the final effect but before
            // recovery. Returning the old action receipt must not rewrite it.
            let changed_pin = journal
                .write(
                    "task-board/task",
                    &changed,
                    "other-writer",
                    &Json::object([
                        ("objectId", f(&task.pin, "objectId").clone()),
                        ("expectedRevision", Json::Number(3.0)),
                    ]),
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
        executor.drive(projection.settled()).await;
    }
}
