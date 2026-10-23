use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    allocation::AllocationStore,
    checks::date,
    engine_actions::{ActionRestore, ActionStore},
    engine_validation::{ActionValidationChecks, ActionValidationStore},
    local_journal::{LocalJournal, initialize},
    native_lifecycle::{LifecycleDocument, LifecycleStore},
    operations::{Document, Store},
    wake::{WakeFailure, WakeStore, should_wake, validate_dependencies},
};
use std::cell::{Cell, RefCell};

pub const AT: &str = "2026-10-23T00:00:30.000Z";
pub trait JsonEdit {
    fn required_mut(&mut self, key: &str) -> Option<&mut Json>;
    fn as_array_mut(&mut self) -> Option<&mut Vec<Json>>;
}
impl JsonEdit for Json {
    fn required_mut(&mut self, key: &str) -> Option<&mut Json> {
        match self {
            Json::Object(fields) => fields
                .iter_mut()
                .find(|(name, _)| name == &JsString::from(key))
                .map(|(_, value)| value),
            _ => None,
        }
    }
    fn as_array_mut(&mut self) -> Option<&mut Vec<Json>> {
        match self {
            Json::Array(values) => Some(values),
            _ => None,
        }
    }
}
pub fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
pub fn s<'a>(v: &'a Json, key: &str) -> &'a JsString {
    v.required(key).unwrap().as_js_string().unwrap()
}
pub fn pin(id: &str, revision: f64) -> Json {
    Json::object([
        ("objectId", id.into()),
        ("revision", Json::Number(revision)),
    ])
}
pub fn actor(source: &str) -> Json {
    Json::object([
        ("source", source.into()),
        ("principalId", "caller".into()),
        ("serviceNodeId", "board-node".into()),
        ("generation", Json::Number(1.0)),
    ])
}
pub fn task() -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskKey", "TASK-0001".into()),
        (
            "fields",
            Json::object([
                ("title", "Assignment".into()),
                ("description", "Current action policy".into()),
                ("acceptanceCriteria", Json::Array(vec![])),
                ("control", "agent".into()),
                ("requiredCapabilities", Json::Array(vec![])),
                ("executionRequirement", Json::Null),
                ("workspaceRequirement", Json::Null),
                ("allowParallel", false.into()),
                ("priority", Json::Number(2.0)),
                ("category", Json::Null),
                ("dueAt", Json::Null),
                ("userContact", "ticket".into()),
            ]),
        ),
        ("workflowState", "todo".into()),
        ("wakeAt", Json::Null),
        ("dependencies", Json::Array(vec![])),
        (
            "observedDependencies",
            Json::object([] as [(&str, Json); 0]),
        ),
        ("runtimeWait", Json::Null),
        ("claim", Json::Null),
        ("publication", Json::Null),
        ("primaryResourceRef", Json::Null),
        ("attemptCount", Json::Number(0.0)),
        ("workRevision", Json::Number(0.0)),
        ("agentCommentCount", Json::Number(0.0)),
        ("lastRun", Json::Null),
        ("lastHistory", Json::Null),
        ("latestResult", Json::Null),
        ("acceptedReview", Json::Null),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        ("attachments", Json::Array(vec![])),
        ("createdAt", AT.into()),
        ("updatedAt", AT.into()),
    ])
}
pub fn request(action: &str) -> Json {
    Json::object([
        ("action", action.into()),
        ("operationId", format!("{action}-operation").into()),
        ("taskId", "task".into()),
        ("expectedRevision", Json::Number(1.0)),
    ])
}
pub fn comment_request() -> Json {
    let mut request = request("comment");
    for (key, value) in [
        ("commentId", "input".into()),
        ("body", "Please examine this input.".into()),
        ("requests", Json::Array(vec![])),
        ("responses", Json::Array(vec![])),
        ("attachments", Json::Array(vec![])),
        ("replyTo", Json::Null),
        ("authorKind", "agent".into()),
        ("purpose", "update".into()),
        ("contactUser", false.into()),
    ] {
        request.insert(key, value);
    }
    request
}
pub fn claim(task: &mut Json) {
    let run = pin("run", 1.0);
    task.insert(
        "claim",
        Json::object([("operationId", "original".into()), ("run", run.clone())]),
    );
    task.insert("lastRun", run);
    task.insert("workflowState", "in_progress".into());
}
pub fn question(id: &str, recipient: Json) -> Json {
    let mut c = comment_request();
    c.remove("action");
    c.remove("taskId");
    c.remove("expectedRevision");
    c.insert("commentId", id.into());
    c.insert("purpose", "question".into());
    c.insert("recipient", recipient);
    c.insert("sequence", Json::Number(1.0));
    c.insert("author", actor("worker"));
    c
}
pub struct World {
    pub db: Connection,
    pub reads: RefCell<Vec<Json>>,
    pub restores: Cell<usize>,
    pub checks: Cell<usize>,
}
impl World {
    pub fn new(value: Json) -> Self {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let world = Self {
            db,
            reads: RefCell::default(),
            restores: Cell::new(0),
            checks: Cell::new(0),
        };
        world.create("task-board/task", "task", &value);
        world
    }
    pub fn local(&self) -> LocalJournal<'_> {
        LocalJournal {
            connection: &self.db,
            root_object_id: Some("root".into()),
        }
    }
    pub fn create(&self, key: &str, id: &str, value: &Json) {
        self.local()
            .create(
                key,
                &id.into(),
                Some(&"root".into()),
                value,
                &AT.into(),
                &id.into(),
            )
            .unwrap();
    }
    pub fn saved(&self) -> Document {
        self.local()
            .read("task-board/task", &"task".into(), true)
            .unwrap()
    }
    pub fn replace(&self, value: &Json) {
        let doc = self.saved();
        self.local()
            .update(
                "task-board/task",
                &"task".into(),
                f(&doc.pin, "revision").as_f64().unwrap(),
                value,
                &AT.into(),
            )
            .unwrap();
    }
    pub fn read_task(&self, address: &Json, primary: bool) -> IvyResult<LifecycleDocument> {
        self.reads.borrow_mut().push(address.clone());
        let doc = self.local().read("task-board/task", address, primary)?;
        Ok(LifecycleDocument {
            metadata: self.local().metadata(s(&doc.pin, "objectId"), "3.1.0")?,
            document: doc,
        })
    }
}
impl Store for &World {
    fn root_object_id(&self) -> Option<JsString> {
        Some("root".into())
    }
    fn contract_version(&self, key: &str) -> IvyResult<String> {
        Ok(match key {
            "task-board/task" => "3.1.0",
            "task-board/run" => "3.0.0",
            _ => "1.0.0",
        }
        .into())
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        Ok(false)
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>> {
        self.local().key(id)
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
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
        self.local().read(key, address, primary)
    }
    async fn write(&self, key: &str, value: &Json, _: &str, destination: &Json) -> IvyResult<Json> {
        if let Some(create) = destination.get("create") {
            self.local().create(
                key,
                s(create, "name"),
                create.get("parentId").and_then(Json::as_js_string),
                value,
                &AT.into(),
                &uuid::Uuid::new_v4().to_string().into(),
            )
        } else {
            self.local().update(
                key,
                s(destination, "objectId"),
                f(destination, "expectedRevision").as_f64().unwrap(),
                value,
                &AT.into(),
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
impl LifecycleStore for &World {
    async fn lifecycle_read(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        _: bool,
    ) -> IvyResult<LifecycleDocument> {
        assert_eq!(key, "task-board/task");
        self.read_task(address, primary)
    }
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        Ok(Json::object([
            ("objects", Json::Array(vec![])),
            ("nextCursor", Json::Null),
        ]))
    }
    async fn archive(&self, _: &Json) -> IvyResult<Json> {
        panic!("No archive in action policy tests")
    }
    async fn stat(&self, _: &JsString) -> IvyResult<Json> {
        panic!("No stat in action policy tests")
    }
    fn release_workspace(&self, _: &JsString) -> IvyResult<()> {
        Ok(())
    }
}
impl WakeStore for &World {
    async fn watched_task(&self, id: &JsString) -> IvyResult<Json> {
        let doc = self.read_task(&Json::String(id.clone()), true)?;
        Ok(Json::object([
            ("value", doc.document.value),
            ("metadata", doc.metadata),
        ]))
    }
    fn deleted(&self, error: &IvyError) -> bool {
        WakeFailure::deleted(error)
    }
}
impl AllocationStore for &World {
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        Ok(Json::object([
            ("objects", Json::Array(vec![])),
            ("nextCursor", Json::Null),
        ]))
    }
    async fn workspace_owner(
        &self,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> IvyResult<Option<JsString>> {
        Ok(None)
    }
    async fn named_plan(&self, _: &JsString) -> IvyResult<Option<Document>> {
        panic!("Plan admission is a separate owner")
    }
}
impl ActionValidationStore for &World {
    async fn reserve_available_workspace(
        &self,
        _: &JsString,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> IvyResult<Option<JsString>> {
        Ok(None)
    }
}
impl ActionStore for &World {
    async fn write_attachment(
        &self,
        _: &JsString,
        _: &JsString,
        _: &JsString,
        _: &[u8],
    ) -> IvyResult<Json> {
        panic!("No binary upload in action tests")
    }
}
// These tests exercise action policy and retained publication, not the separate
// contract registry, native plan discovery or RPC owner. Those bindings are explicit.
impl ActionValidationChecks for &World {
    async fn aggregate(&self, _: &Json, _: &JsString) -> IvyResult<()> {
        Ok(())
    }
    async fn fields(&self, _: &Json, _: &JsString) -> IvyResult<()> {
        self.checks.set(self.checks.get() + 1);
        Ok(())
    }
    async fn execution_options(&self, _: &Json, _: Option<&Json>) -> IvyResult<()> {
        self.checks.set(self.checks.get() + 1);
        Ok(())
    }
    async fn execution(
        &self,
        task: &Json,
        _: &Json,
        _: &JsString,
        at: &JsString,
        _: &Json,
        _: &Json,
    ) -> IvyResult<Json> {
        if !should_wake(self, task, at).await? {
            return Err(IvyError::new("task_board_input_required", "No new work"));
        }
        Ok(Json::object([(
            "turnStart",
            Json::object([(
                "input",
                Json::Array(vec![Json::object([
                    ("type", "text".into()),
                    ("text", "Input".into()),
                ])]),
            )]),
        )]))
    }
    async fn deferral(&self, request: &Json, id: &JsString, _: &JsString) -> IvyResult<()> {
        date(Some(f(request, "wakeAt")))?;
        date(Some(f(f(request, "runtimeWait"), "retryAt")))?;
        if let Some(ids) = request.get("dependencies").filter(|v| !v.is_null()) {
            validate_dependencies(self, Some(id), ids.as_array().unwrap()).await?;
        }
        Ok(())
    }
    async fn content(&self, _: &Json) -> IvyResult<()> {
        panic!("Result content is checked separately")
    }
    async fn artifact(&self, _: &Json) -> IvyResult<()> {
        panic!("Artifact identity is checked separately")
    }
}
impl ActionRestore for &World {
    async fn restore_for_execution(&self, _: &Document, _: &Document) -> IvyResult<()> {
        self.restores.set(self.restores.get() + 1);
        Ok(())
    }
}
pub fn same(left: &Json, right: &Json) {
    assert_eq!(hash_json(left).unwrap(), hash_json(right).unwrap());
}
