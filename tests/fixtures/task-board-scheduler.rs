use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{
    allocation::{AllocationRuntime, AllocationStore},
    coordination::recovery_retry_delay_ms,
    local_journal::{self, LocalJournal},
    native_intent::TaskPrompts,
    operations::{Document, Operations, Store},
    scheduler::{
        Scheduler, SchedulerAllocation, SchedulerEngine, SchedulerPolicies, SchedulerStore,
    },
    wake::{WakeFailure, WakeStore, dependency_snapshot},
};
use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

const AT: &str = "2026-10-23T00:00:30.000Z";
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s(v: &Json, k: &str) -> JsString {
    v.required(k).unwrap().as_js_string().unwrap().clone()
}
fn pin(id: &str, revision: f64) -> Json {
    Json::object([
        ("objectId", id.into()),
        ("revision", Json::Number(revision)),
    ])
}
fn task() -> Json {
    Json::object([
        ("fields", Json::object([("control", "agent".into())])),
        ("workflowState", "todo".into()),
        ("wakeAt", Json::Null),
        ("runtimeWait", Json::Null),
        ("completionIssue", Json::Null),
        ("dependencies", Json::Array(vec![])),
        (
            "observedDependencies",
            Json::object([] as [(&str, Json); 0]),
        ),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        ("claim", Json::Null),
        ("publication", Json::Null),
        ("updatedAt", "2026-10-23T00:00:00.000Z".into()),
    ])
}
struct NoRpc;
impl RpcClient for NoRpc {
    fn request<'a>(&'a self, _: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async { panic!("Scheduler policy does not make native calls before allocation.") })
    }
}
struct World {
    db: Connection,
    now: RefCell<String>,
    calls: RefCell<Vec<Json>>,
    tools: Arc<ToolClients>,
    principal: JsString,
    node: JsString,
    enabled: Cell<bool>,
    missing: Cell<bool>,
    archived: Cell<bool>,
    rate: RefCell<Option<JsString>>,
    allocation_error: RefCell<Option<IvyError>>,
    dispatch_error: Cell<bool>,
    free: Cell<Option<f64>>,
    header_error: RefCell<Option<IvyError>>,
}
impl World {
    fn new(task: Json) -> Self {
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        let world = Self {
            db,
            now: RefCell::new(AT.into()),
            calls: RefCell::default(),
            tools: Arc::new(ToolClients::new(Arc::new(NoRpc))),
            principal: "caller".into(),
            node: "node".into(),
            enabled: Cell::new(true),
            missing: Cell::new(false),
            archived: Cell::new(false),
            rate: RefCell::default(),
            allocation_error: RefCell::default(),
            dispatch_error: Cell::new(false),
            free: Cell::new(None),
            header_error: RefCell::default(),
        };
        world
            .local()
            .create(
                "task-board/task",
                &"Task".into(),
                Some(&"root".into()),
                &task,
                &AT.into(),
                &"task".into(),
            )
            .unwrap();
        world
    }
    fn local(&self) -> LocalJournal<'_> {
        LocalJournal {
            connection: &self.db,
            root_object_id: Some("root".into()),
        }
    }
    fn event(&self, kind: &str, value: Json) {
        self.calls
            .borrow_mut()
            .push(Json::object([("kind", kind.into()), ("value", value)]));
    }
    fn count(&self, kind: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|c| f(c, "kind") == &Json::from(kind))
            .count()
    }
    fn values(&self, kind: &str) -> Vec<Json> {
        self.calls
            .borrow()
            .iter()
            .filter(|c| f(c, "kind") == &Json::from(kind))
            .map(|c| f(c, "value").clone())
            .collect()
    }
    fn read_task(&self) -> Document {
        self.local()
            .read("task-board/task", &"task".into(), true)
            .unwrap()
    }
    fn set_task(&self, value: Json) {
        let d = self.read_task();
        self.local()
            .update(
                "task-board/task",
                &"task".into(),
                f(&d.pin, "revision").as_f64().unwrap(),
                &value,
                &self.now().into(),
            )
            .unwrap();
    }
    fn operation(&self, request: &Json) -> Document {
        self.local()
            .named(
                "task-board/operation",
                &format!(
                    "TaskBoard operation {}",
                    ivy_task_board::operations::identity_hash(
                        &self.principal,
                        &s(request, "operationId")
                    )
                    .unwrap()
                )
                .into(),
                Some(&"root".into()),
            )
            .unwrap()
            .unwrap()
    }
    fn update_operation(&self, operation: &Document, value: Json) {
        self.local()
            .update(
                "task-board/operation",
                &s(&operation.pin, "objectId"),
                f(&operation.pin, "revision").as_f64().unwrap(),
                &value,
                &self.now().into(),
            )
            .unwrap();
    }
    async fn run(&self) -> IvyResult<Json> {
        let store = self;
        let operations = Operations {
            store,
            now: || self.now.borrow().clone(),
        };
        Scheduler {
            store: &store,
            engine: self,
            allocator: self,
            operations: &operations,
        }
        .task(&"task".into())
        .await
    }
}
impl Store for &World {
    fn root_object_id(&self) -> Option<JsString> {
        Some("root".into())
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        Ok(false)
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>> {
        self.local().key(id)
    }
    fn complete_local_workflow(&self, id: &JsString, _: &JsString) -> IvyResult<()> {
        self.event("complete", Json::String(id.clone()));
        Ok(())
    }
    async fn named(
        &self,
        key: &str,
        name: &str,
        parent: Option<&JsString>,
    ) -> IvyResult<Option<Document>> {
        self.local()
            .named(key, &name.into(), parent.or(Some(&"root".into())))
    }
    async fn read(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        self.event(
            "read",
            Json::object([("key", key.into()), ("address", address.clone())]),
        );
        self.local().read(key, address, primary)
    }
    async fn write(&self, key: &str, value: &Json, _: &str, destination: &Json) -> IvyResult<Json> {
        self.event(
            "write",
            Json::object([("key", key.into()), ("value", value.clone())]),
        );
        if let Some(create) = destination.get("create") {
            self.local().create(
                key,
                &s(create, "name"),
                create
                    .get("parentId")
                    .and_then(Json::as_js_string)
                    .or(Some(&"root".into())),
                value,
                &self.now(),
                &uuid::Uuid::new_v4().to_string().into(),
            )
        } else {
            self.local().update(
                key,
                &s(destination, "objectId"),
                f(destination, "expectedRevision").as_f64().unwrap(),
                value,
                &self.now(),
            )
        }
    }
    async fn prepared_mutation_id(&self, id: &str) -> IvyResult<JsString> {
        Ok(id.into())
    }
    async fn write_prepared(
        &self,
        key: &str,
        value: &Json,
        id: &JsString,
        destination: &Json,
    ) -> IvyResult<Json> {
        self.write(key, value, &id.to_utf8()?, destination).await
    }
}
impl AllocationStore for &World {
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Injected allocation owns discovery")
    }
    async fn workspace_owner(
        &self,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> IvyResult<Option<JsString>> {
        panic!("Injected allocation owns workspace admission")
    }
    async fn named_plan(&self, _: &JsString) -> IvyResult<Option<Document>> {
        panic!("Injected allocation owns retained plans")
    }
}
impl WakeStore for &World {
    async fn watched_task(&self, id: &JsString) -> IvyResult<Json> {
        self.event("header", Json::String(id.clone()));
        if let Some(error) = self.header_error.borrow().as_ref() {
            return Err(error.clone());
        }
        let doc = self
            .local()
            .read("task-board/task", &Json::String(id.clone()), true)?;
        Ok(Json::object([
            ("value", doc.value),
            ("metadata", self.local().metadata(id, "3.1.0")?),
        ]))
    }
    fn deleted(&self, error: &IvyError) -> bool {
        WakeFailure::deleted(error)
    }
}
impl SchedulerStore for &World {
    fn free_bytes(&self) -> IvyResult<Option<f64>> {
        self.event("free", Json::Null);
        Ok(self.free.get())
    }
    fn pending_operation(&self, id: &JsString) -> IvyResult<Option<JsString>> {
        self.local().pending_operation(id, "scheduler", None)
    }
}
impl TaskPrompts for World {
    fn task_prompt(&self, _: &Json, _: &JsString, mode: Option<&Json>) -> IvyResult<JsString> {
        assert!(mode.is_none());
        self.event("prompt", Json::Null);
        Ok("Current prompt".into())
    }
    fn task_update_prompt(&self, _: &Json, _: &JsString, _: bool) -> IvyResult<JsString> {
        panic!("No update prompt")
    }
}
impl AllocationRuntime for World {
    fn client(&self) -> Arc<dyn RpcClient> {
        Arc::new(NoRpc)
    }
    fn tools(&self) -> &Arc<ToolClients> {
        &self.tools
    }
    fn principal_id(&self) -> &JsString {
        &self.principal
    }
    fn service_node_id(&self) -> &JsString {
        &self.node
    }
}
impl SchedulerPolicies for World {
    async fn read_task(&self, id: &JsString) -> IvyResult<Document> {
        Store::read(&self, "task-board/task", &Json::String(id.clone()), true).await
    }
    fn enabled(&self) -> bool {
        self.enabled.get()
    }
    fn interval_ms(&self) -> f64 {
        1000.0
    }
    fn now(&self) -> JsString {
        self.now.borrow().as_str().into()
    }
    async fn aggregate(&self, _: &Json, id: &JsString) -> IvyResult<()> {
        self.event("aggregate", Json::String(id.clone()));
        Ok(())
    }
    async fn forward_delegated_review(&self, _: &Document) -> IvyResult<()> {
        self.event("review", Json::Null);
        Ok(())
    }
    async fn unstarted_missing_context(&self, _: &Document) -> IvyResult<bool> {
        Ok(self.missing.get())
    }
    async fn unstarted_archived_context(&self, _: &Document) -> IvyResult<bool> {
        Ok(self.archived.get())
    }
    async fn failed_rate_limit_retry_at(&self, _: &Document) -> IvyResult<Option<JsString>> {
        Ok(self.rate.borrow().clone())
    }
    async fn schedule(&self, request: &Json) -> IvyResult<Json> {
        self.event("dispatch", request.clone());
        let operations = Operations {
            store: self,
            now: || self.now.borrow().clone(),
        };
        operations
            .begin(
                request,
                &Json::object([
                    ("principalId", Json::String(self.principal.clone())),
                    ("source", "scheduler".into()),
                ]),
            )
            .await?;
        if self.dispatch_error.get() {
            return Err(
                IvyError::new("native_connection_lost", "Reply lost after acceptance")
                    .with_outcome(Outcome::Unknown),
            );
        }
        Ok(request.clone())
    }
    async fn recover_operation(&self, id: &JsString) -> IvyResult<Json> {
        self.event("recover", Json::String(id.clone()));
        Ok(Json::object([("recovered", Json::String(id.clone()))]))
    }
}
impl SchedulerEngine for World {
    async fn verify_owner(&self) -> IvyResult<()> {
        self.event("owner", Json::Null);
        Ok(())
    }
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString> {
        Ok(hash_json(scope)?.into())
    }
}
impl SchedulerAllocation for World {
    async fn allocate(
        &self,
        _: &Document,
        _: &JsString,
        feedback: Option<&JsString>,
        retain: bool,
    ) -> IvyResult<Json> {
        assert_eq!(feedback, Some(&JsString::from("Current prompt")));
        self.event("allocate", retain.into());
        if let Some(error) = self.allocation_error.borrow().as_ref() {
            return Err(error.clone());
        }
        Ok(Json::object([
            (
                "target",
                Json::object([
                    ("hostId", "host".into()),
                    ("serviceNodeId", "native".into()),
                ]),
            ),
            (
                "workspace",
                Json::object([("canonicalCwd", "/saved".into())]),
            ),
            ("intent", pin("plan", 1.0)),
        ]))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_wake_policy_schedules_due_work_without_foreign_done_or_question_gates() {
    let mut value = task();
    value.insert("workflowState", "waiting".into());
    value.insert("wakeAt", AT.into());
    value.insert("dependencies", Json::Array(vec!["unfinished".into()]));
    value.insert(
        "comments",
        Json::Array(vec![Json::object([
            ("commentId", "question".into()),
            ("purpose", "question".into()),
            ("authorKind", "agent".into()),
            ("recipient", Json::object([("kind", "user".into())])),
            ("body", "Independent pending question".into()),
        ])]),
    );
    let world = World::new(value);
    let request = world.run().await.unwrap();
    assert_eq!(f(&request, "action"), &Json::from("start"));
    assert_eq!(world.count("header"), 0);
    assert_eq!(world.values("allocate"), vec![Json::Bool(false)]);
    assert_eq!(f(&request, "commentIds"), &Json::Array(vec![]));
    for field in ["coordinationOnly", "blocker", "nextReviewAt"] {
        assert!(request.get(field).is_none());
    }
    let mut changed = task();
    changed.insert("workflowState", "done".into());
    changed.insert("executionChangeRequested", true.into());
    let settings = World::new(changed);
    assert_eq!(
        f(&settings.run().await.unwrap(), "action"),
        &Json::from("start")
    );
    assert_eq!(settings.values("allocate"), vec![Json::Bool(true)]);
    let calls = world.calls.borrow();
    assert!(
        calls
            .iter()
            .position(|c| f(c, "kind") == &Json::from("complete"))
            .unwrap()
            < calls
                .iter()
                .position(|c| f(c, "kind") == &Json::from("dispatch"))
                .unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn queued_inputs_are_bounded_and_retain_context_even_for_manual_terminal_tasks() {
    let mut value = task();
    value.insert("workflowState", "done".into());
    value.insert("fields", Json::object([("control", "user".into())]));
    value.insert(
        "primaryResourceRef",
        Json::object([("nativeId", "saved".into())]),
    );
    let mut comments = vec![Json::object([
        ("commentId", "own".into()),
        ("authorKind", "agent".into()),
    ])];
    let mut deliveries = vec![Json::object([
        ("commentId", "own".into()),
        ("state", "queued".into()),
    ])];
    for i in 0..270 {
        let id = Json::from(format!("peer{i}"));
        comments.push(Json::object([
            ("commentId", id.clone()),
            ("authorKind", "agent".into()),
            ("sourceTaskId", "peer".into()),
        ]));
        deliveries.push(Json::object([
            ("commentId", id),
            ("state", "queued".into()),
        ]));
    }
    value.insert("comments", Json::Array(comments));
    value.insert("commentDeliveries", Json::Array(deliveries));
    let world = World::new(value);
    let request = world.run().await.unwrap();
    assert_eq!(f(&request, "action"), &Json::from("continue"));
    assert_eq!(f(&request, "feedback"), &Json::from("Current prompt"));
    let ids = f(&request, "commentIds").as_array().unwrap();
    assert_eq!(ids.len(), 256);
    assert_eq!(ids[0], Json::from("peer0"));
    assert_eq!(ids[255], Json::from("peer255"));
    assert_eq!(world.values("allocate"), vec![Json::Bool(true)]);
    let mut retry = world.read_task().value;
    retry.insert(
        "runtimeWait",
        Json::object([("retryAt", "2026-10-23T00:01:00.000Z".into())]),
    );
    world.set_task(retry);
    // No original intent can be skipped by editing its Task; recover it first.
    assert!(world.run().await.unwrap().get("recovered").is_some());
    assert_eq!(world.count("allocate"), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn allocation_failure_creates_runtime_retry_without_a_task_blocker_or_wake_rewrite() {
    for (code, reason, millis) in [
        ("native_capacity", "host", 5000),
        ("task_board_workspace_conflict", "workspace", 30000),
        ("task_board_capability_unavailable", "capability", 30000),
        ("revision_conflict", "host", 250),
    ] {
        let mut value = task();
        value.insert("wakeAt", "2026-10-23T00:01:00.000Z".into());
        value.insert(
            "runtimeWait",
            Json::object([
                ("since", "2026-10-22T22:00:00.000Z".into()),
                ("retryAt", AT.into()),
            ]),
        );
        let world = World::new(value.clone());
        *world.allocation_error.borrow_mut() = Some(IvyError::new(code, &"x".repeat(2100)));
        let request = world.run().await.unwrap();
        assert_eq!(f(&request, "action"), &Json::from("defer"));
        let wait = f(&request, "runtimeWait");
        assert_eq!(f(wait, "reason"), &Json::from(reason));
        assert_eq!(s(wait, "detail").0.len(), 2048);
        assert_eq!(f(wait, "since"), &Json::from("2026-10-22T22:00:00.000Z"));
        assert_eq!(
            ivy_sdk::native_evidence::native_instant(&s(wait, "retryAt"), None).unwrap()
                - ivy_sdk::native_evidence::native_instant(&AT.into(), None).unwrap(),
            millis
        );
        assert_eq!(world.read_task().content_hash, hash_json(&value).unwrap());
        assert_eq!(world.read_task().pin, pin("task", 1.0));
        for field in ["blocker", "wakeAt", "nextReviewAt", "reason", "detail"] {
            assert!(request.get(field).is_none());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn watched_material_changes_and_original_read_failures_reach_scheduler() {
    let mut value = task();
    value.insert("workflowState", "review".into());
    value.insert("dependencies", Json::Array(vec!["peer".into()]));
    let world = World::new(value);
    let peer = Json::object([
        ("workRevision", Json::Number(1.0)),
        ("acceptedReview", Json::Null),
        ("workflowState", "waiting".into()),
    ]);
    world
        .local()
        .create(
            "task-board/task",
            &"Peer".into(),
            Some(&"root".into()),
            &peer,
            &AT.into(),
            &"peer".into(),
        )
        .unwrap();
    let mut value = world.read_task().value;
    value.insert(
        "observedDependencies",
        dependency_snapshot(&&world, &value).await.unwrap(),
    );
    world.set_task(value);
    assert!(world.run().await.unwrap().is_null());
    assert_eq!(world.count("allocate"), 0);
    let error = IvyError::new("deadline_exceeded", "Original watched-header outcome")
        .with_outcome(Outcome::Unknown)
        .with_details(Json::object([("token", "original".into())]));
    *world.header_error.borrow_mut() = Some(error.clone());
    let actual = world.run().await.unwrap_err();
    assert_eq!(actual, error);
    *world.header_error.borrow_mut() = None;
    let mut changed = peer;
    changed.insert("workRevision", Json::Number(2.0));
    world
        .local()
        .update("task-board/task", &"peer".into(), 1.0, &changed, &AT.into())
        .unwrap();
    assert_eq!(
        f(&world.run().await.unwrap(), "action"),
        &Json::from("start")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pending_original_intent_wins_over_new_task_revision_and_future_retry() {
    let world = World::new(task());
    world.dispatch_error.set(true);
    assert_eq!(
        world.run().await.unwrap_err().outcome.literal(),
        Some(Outcome::Unknown)
    );
    let request = world.values("dispatch").remove(0);
    let original = world.operation(&request);
    let mut value = world.read_task().value;
    value.insert("workflowState", "done".into());
    value.insert(
        "runtimeWait",
        Json::object([("retryAt", "2026-11-01T00:00:00.000Z".into())]),
    );
    world.set_task(value);
    assert_eq!(
        f(&world.run().await.unwrap(), "recovered"),
        original.pin.get("objectId").unwrap()
    );
    assert_eq!(world.count("allocate"), 1);
    assert_eq!(world.count("review"), 1);
    let originals: i64 = world
        .db
        .query_row(
            "SELECT COUNT(*) FROM documents WHERE key='task-board/scheduler-attempt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(originals, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_claim_recovers_completion_and_never_authorizes_another_run() {
    for phase in ["accepted", "succeeded"] {
        let world = World::new(task());
        let request = world.run().await.unwrap();
        let original = world.operation(&request);
        let mut operation = original.value.clone();
        operation.insert("actor", Json::object([("source", "native".into())]));
        operation.insert("phase", phase.into());
        world.update_operation(&original, operation);
        let mut value = world.read_task().value;
        value.insert("claim", Json::object([("run", pin("run", 1.0))]));
        world.set_task(value);
        world
            .local()
            .create(
                "task-board/run",
                &"Run".into(),
                Some(&"root".into()),
                &Json::object([
                    ("phase", "completed".into()),
                    ("result", pin("result", 1.0)),
                ]),
                &AT.into(),
                &"run".into(),
            )
            .unwrap();
        world
            .local()
            .create(
                "task-board/result",
                &"Result".into(),
                Some(&"root".into()),
                &Json::object([("operationId", f(&request, "operationId").clone())]),
                &AT.into(),
                &"result".into(),
            )
            .unwrap();
        if phase == "accepted" {
            assert!(world.run().await.unwrap().is_null());
            assert_eq!(world.count("recover"), 1);
        } else {
            assert_eq!(
                world.run().await.unwrap_err().code,
                "task_board_claim_inconsistent"
            );
        }
        assert_eq!(world.count("aggregate"), 1);
        assert_eq!(world.count("allocate"), 1);
        assert!(
            world
                .values("read")
                .iter()
                .any(|read| f(read, "key") == &Json::from("task-board/run")
                    && f(read, "address") == &Json::from("run"))
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn only_conclusive_unapplied_failure_allows_a_delayed_exact_successor() {
    for (code, after, before) in [
        (
            "revision_conflict",
            "2026-10-23T00:00:31.000Z",
            "2026-10-23T00:00:30.999Z",
        ),
        (
            "native_connection_lost",
            "2026-10-23T00:00:35.000Z",
            "2026-10-23T00:00:34.999Z",
        ),
        (
            "other",
            "2026-10-23T00:01:00.000Z",
            "2026-10-23T00:00:59.999Z",
        ),
    ] {
        let world = World::new(task());
        let request = world.run().await.unwrap();
        let original = world.operation(&request);
        let mut failure = original.value.clone();
        failure.insert("phase", "failed".into());
        failure.insert(
            "error",
            Json::object([("code", code.into()), ("execution", "unknown".into())]),
        );
        world.update_operation(&original, failure.clone());
        *world.now.borrow_mut() = after.into();
        assert_eq!(
            world.run().await.unwrap_err().code,
            "task_board_outcome_unresolved"
        );
        let current = world
            .local()
            .read("task-board/operation", f(&original.pin, "objectId"), true)
            .unwrap();
        failure.insert(
            "error",
            Json::object([("code", code.into()), ("execution", "not_executed".into())]),
        );
        failure.insert(
            "writes",
            Json::Array(vec![Json::object([("outcome", pin("applied", 1.0))])]),
        );
        world.update_operation(&current, failure.clone());
        assert_eq!(
            world.run().await.unwrap_err().code,
            "task_board_outcome_unresolved"
        );
        let current = world
            .local()
            .read("task-board/operation", f(&original.pin, "objectId"), true)
            .unwrap();
        failure.insert("writes", Json::Array(vec![]));
        world.update_operation(&current, failure);
        *world.now.borrow_mut() = before.into();
        assert!(world.run().await.unwrap().is_null());
        assert_eq!(world.count("allocate"), 1);
        *world.now.borrow_mut() = after.into();
        let successor = world.run().await.unwrap();
        assert_ne!(f(&successor, "operationId"), f(&request, "operationId"));
        assert_eq!(world.count("allocate"), 2);
        let attempt = world
            .local()
            .named(
                "task-board/scheduler-attempt",
                &"Scheduled task task revision 1".into(),
                Some(&"task".into()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(f(&attempt.value, "sequence"), &Json::Number(2.0));
        assert_eq!(
            f(&attempt.value, "previous"),
            &pin(&s(&attempt.pin, "objectId").to_utf8().unwrap(), 1.0)
        );
        let predecessor = world
            .local()
            .read(
                "task-board/operation",
                f(&attempt.value, "previousOperation"),
                true,
            )
            .unwrap();
        assert_eq!(
            predecessor.value.get("requestHash"),
            Some(&Json::from(hash_json(&request).unwrap()))
        );
    }
    assert_eq!(
        recovery_retry_delay_ms(Some("task_board_native_order")),
        1000
    );
}

#[tokio::test(flavor = "current_thread")]
async fn future_wait_rate_limit_and_recovery_delay_do_not_create_new_attempts() {
    for mode in ["runtime", "rate", "recovery", "storage"] {
        let mut value = task();
        if mode == "runtime" {
            value.insert(
                "runtimeWait",
                Json::object([("retryAt", "2026-10-23T01:00:00.000Z".into())]),
            );
        }
        if mode == "recovery" {
            value.insert("lastRun", pin("run", 1.0));
        }
        let world = World::new(value.clone());
        match mode {
            "rate" => *world.rate.borrow_mut() = Some("2026-10-23T01:00:00.000Z".into()),
            "recovery" => {
                world.missing.set(true);
                world
                    .local()
                    .create(
                        "task-board/run",
                        &"Run".into(),
                        Some(&"root".into()),
                        &Json::object([("finishedAt", "2026-10-23T00:00:29.000Z".into())]),
                        &AT.into(),
                        &"run".into(),
                    )
                    .unwrap();
            }
            "storage" => world.free.set(Some(1.0)),
            _ => {}
        }
        if mode == "storage" {
            assert_eq!(
                world.run().await.unwrap_err().code,
                "task_board_storage_low"
            );
        } else {
            assert!(world.run().await.unwrap().is_null());
        }
        assert_eq!(world.count("allocate"), 0);
        assert_eq!(world.count("dispatch"), 0);
        assert_eq!(world.read_task().content_hash, hash_json(&value).unwrap());
        assert_eq!(world.read_task().pin, pin("task", 1.0));
        if mode == "recovery" {
            *world.now.borrow_mut() = "2026-10-23T00:01:00.000Z".into();
            assert_eq!(
                f(&world.run().await.unwrap(), "action"),
                &Json::from("recoverThread")
            );
            assert_eq!(world.count("allocate"), 0);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_intent_identity_is_verified_before_replaying_a_completed_operation() {
    for (key, value) in [
        ("principalId", "other".into()),
        ("task", pin("other-task", 1.0)),
        ("requestHash", "changed".into()),
        ("sequence", Json::Number(2.0)),
    ] {
        let world = World::new(task());
        let request = world.run().await.unwrap();
        let original = world.operation(&request);
        let mut completed = original.value.clone();
        completed.insert("phase", "succeeded".into());
        world.update_operation(&original, completed);
        let saved = world
            .local()
            .named(
                "task-board/scheduler-attempt",
                &"Scheduled task task revision 1".into(),
                Some(&"task".into()),
            )
            .unwrap()
            .unwrap();
        let mut corrupted = saved.value;
        corrupted.insert(key, value);
        world
            .local()
            .update(
                "task-board/scheduler-attempt",
                &s(&saved.pin, "objectId"),
                1.0,
                &corrupted,
                &AT.into(),
            )
            .unwrap();
        assert_eq!(
            world.run().await.unwrap_err().code,
            "task_board_identity_conflict"
        );
        assert_eq!(world.count("allocate"), 1);
        assert_eq!(world.count("dispatch"), 1);
    }
}

#[test]
fn pending_operation_query_uses_current_revisions_source_run_and_oldest_identity() {
    let world = World::new(task());
    let insert = |id: &str, revision: i64, source: &str, phase: &str, created: &str, run: &str| {
        let value = Json::object([
            (
                "request",
                Json::object([("taskId", "task".into()), ("run", pin(run, 1.0))]),
            ),
            ("actor", Json::object([("source", source.into())])),
            ("phase", phase.into()),
        ]);
        world
            .db
            .execute(
                "INSERT OR IGNORE INTO documents VALUES(?,'task-board/operation',?,'root',?,?,?)",
                params![id, id, revision, created, created],
            )
            .unwrap();
        world
            .db
            .execute(
                "INSERT INTO revisions VALUES(?,?,?,?)",
                params![
                    id,
                    revision,
                    ivy_contracts::json::canonical(&value, 1_000_000).unwrap(),
                    created
                ],
            )
            .unwrap();
    };
    insert("old", 2, "scheduler", "accepted", "2026-01-01", "run");
    insert("old", 1, "scheduler", "publishing", "2025-01-01", "old-run");
    insert(
        "new",
        1,
        "scheduler",
        "needs_attention",
        "2026-02-01",
        "run",
    );
    insert(
        "native",
        1,
        "native",
        "preparing",
        "2025-02-01",
        "native-run",
    );
    insert("finished", 1, "scheduler", "failed", "2024-01-01", "run");
    assert_eq!(
        world
            .local()
            .pending_operation(&"task".into(), "scheduler", None)
            .unwrap(),
        Some("old".into())
    );
    assert_eq!(
        world
            .local()
            .pending_operation(&"task".into(), "native", Some(&"native-run".into()))
            .unwrap(),
        Some("native".into())
    );
    assert_eq!(
        world
            .local()
            .pending_operation(&"task".into(), "scheduler", Some(&"old-run".into()))
            .unwrap(),
        None
    );
    let mut statement=world.db.prepare("EXPLAIN QUERY PLAN SELECT id FROM revisions INDEXED BY pending_task_operations WHERE json_extract(value_json,'$.request.taskId')=? AND json_extract(value_json,'$.actor.source')=? AND json_extract(value_json,'$.phase') IN ('accepted','preparing','publishing','needs_attention')").unwrap();
    let plan = statement
        .query_map(params!["task", "scheduler"], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("pending_task_operations"))
    );
}
