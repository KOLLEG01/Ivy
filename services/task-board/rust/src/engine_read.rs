//! Engine reads and revision-checked per-caller comment markers.
//! Admission uses the owner's current contract bundle, not a frozen schema.
use crate::{
    inspection_journal::verify_owner,
    journal_store::Contracts,
    native_lifecycle::LifecycleStore,
    operations::{Document, Operations, Store, mutation},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
use ivy_sdk::rpc::RpcClient;
use std::collections::HashSet;

#[path = "comment_reads.rs"]
pub mod comment_reads;
#[path = "condition.rs"]
pub mod condition_policy;
#[path = "conversation.rs"]
pub mod conversation;
#[path = "read_failure.rs"]
pub mod read_failure;
#[path = "read_policy.rs"]
pub mod read_policy;

#[allow(async_fn_in_trait)]
pub trait EngineReadRuntime {
    fn validate(&self, definition: &str, value: &Json) -> IvyResult<()>;
    fn generation(&self) -> f64;
    async fn verify_owner(&self) -> IvyResult<()>;
}
pub struct ReadRuntime<'a, P> {
    pub client: &'a dyn RpcClient,
    pub contracts: &'a P,
    pub principal_id: &'a JsString,
    pub service_node_id: &'a JsString,
    pub generation: f64,
}
impl<P: Contracts> EngineReadRuntime for ReadRuntime<'_, P> {
    fn validate(&self, name: &str, value: &Json) -> IvyResult<()> {
        self.contracts.validate_definition(name, value)
    }
    fn generation(&self) -> f64 {
        self.generation
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        verify_owner(self.client, self.principal_id, self.service_node_id).await
    }
}
pub struct ReadContext<'a> {
    pub caller_principal_id: &'a JsString,
    pub operation_id: &'a JsString,
    pub generation: f64,
    pub check_cancelled: &'a dyn Fn() -> IvyResult<()>,
}
pub struct EngineReads<'a, S, C, R> {
    pub operations: &'a Operations<S, C>,
    pub runtime: &'a R,
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn present(v: &Json) -> bool {
    !v.is_null()
}
fn eq(v: &Json, k: &str, want: &str) -> bool {
    v.get(k) == Some(&Json::from(want))
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn concat(parts: impl IntoIterator<Item = JsString>) -> JsString {
    JsString(parts.into_iter().flat_map(|s| s.0).collect())
}
/// Current condition for a fully materialized Task. The production bounded
/// reader additionally supplies retained failed-handoff evidence.
pub fn execution_condition(task: &Json) -> IvyResult<Json> {
    let task = ivy_sdk::upgrade_task_record(task)?;
    let questions = ivy_sdk::open_task_questions(&task, None)?;
    let wake = crate::wake::wake_condition(&task, &questions);
    condition_policy::task_condition_snapshot(&task, Some(&wake), None)
}
/// Match workspaceIdentity, which deliberately does not normalize dot segments.
pub async fn workspace_identity<S: Store>(store: &S, task: &Json) -> IvyResult<Option<JsString>> {
    let mut workspace = f(f(task, "claim"), "workspace").clone();
    if workspace.is_null() && present(f(task, "lastRun")) {
        workspace = f(
            &store
                .read("task-board/run", f(task, "lastRun"), true)
                .await?
                .value,
            "workspace",
        )
        .clone();
    }
    if !present(&workspace) {
        return Ok(None);
    }
    let mut path = s(&workspace, "canonicalCwd")?.clone();
    for u in &mut path.0 {
        if *u == 92 {
            *u = 47;
        }
    }
    if path.0.last() == Some(&47) {
        path.0.pop();
    }
    let units = &path.0;
    let drive = units.len() >= 3
        && ((65..=90).contains(&units[0]) || (97..=122).contains(&units[0]))
        && units[1..3] == [58, 47];
    if drive || units.starts_with(&[47, 47]) {
        path = ivy_sdk::lowercase_utf16(&path.0);
    }
    Ok(Some(concat([
        s(&workspace, "hostId")?.clone(),
        ":".into(),
        path,
    ])))
}
/// Native continuation observes only the exact revisions returned by the
/// owner's Task page. Locale sorting and path canonicalization are not added.
pub async fn peer_state<S: LifecycleStore>(
    store: &S,
    task: &Json,
    task_id: &JsString,
) -> IvyResult<JsString> {
    let Some(identity) = workspace_identity(store, task).await? else {
        return hash_json(&Json::Array(vec![])).map(Into::into);
    };
    let mut states = Vec::new();
    let mut cursor: Option<JsString> = None;
    loop {
        let mut query = Json::object([
            ("limit", Json::Number(100.0)),
            (
                "where",
                Json::object([
                    ("op", "in".into()),
                    ("field", "data:/workflowState".into()),
                    (
                        "value",
                        Json::Array(
                            ["todo", "in_progress", "waiting", "review"]
                                .into_iter()
                                .map(Into::into)
                                .collect(),
                        ),
                    ),
                ]),
            ),
        ]);
        if let Some(cursor) = cursor {
            query.insert("cursor", Json::String(cursor));
        }
        let page = store.page("task-board/task", &query).await?;
        for item in page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let id = s(item, "objectId")?;
            if id == task_id {
                continue;
            }
            let peer = store
                .read(
                    "task-board/task",
                    &Json::object([
                        ("objectId", Json::String(id.clone())),
                        ("revision", item.required("revision")?.clone()),
                    ]),
                    true,
                )
                .await?;
            if workspace_identity(store, &peer.value).await?.as_ref() == Some(&identity) {
                states.push(concat([
                    id.clone(),
                    ":".into(),
                    s(&peer.value, "workflowState")?.clone(),
                ]));
            }
        }
        cursor = page
            .get("nextCursor")
            .and_then(Json::as_js_string)
            .filter(|s| !s.0.is_empty())
            .cloned();
        if cursor.is_none() {
            break;
        }
    }
    // Array.prototype.sort compares UTF16 code units and retains duplicates.
    states.sort();
    hash_json(&Json::Array(states.into_iter().map(Json::String).collect())).map(Into::into)
}
pub fn read_state_name(principal: &JsString, task: &JsString) -> IvyResult<String> {
    Ok(format!(
        "Comment read state {}",
        &hash_json(&Json::object([
            ("principalId", Json::String(principal.clone())),
            ("taskId", Json::String(task.clone()))
        ]))?[7..]
    ))
}
fn check_state(value: &Json, principal: &JsString, task: &JsString) -> IvyResult<()> {
    require(
        f(value, "principalId").as_js_string() == Some(principal)
            && f(value, "taskId").as_js_string() == Some(task),
        "task_board_identity_conflict",
        "Comment read state belongs to another user or Task.",
    )
}
impl<S: LifecycleStore, C: Fn() -> String, R: EngineReadRuntime> EngineReads<'_, S, C, R> {
    pub async fn operation(
        &self,
        id: &JsString,
        context: &ReadContext<'_>,
        receipt_only: bool,
    ) -> IvyResult<Json> {
        self.runtime.verify_owner().await?;
        (context.check_cancelled)()?;
        self.runtime.validate(
            "OperationQuery",
            &Json::object([("operationId", Json::String(id.clone()))]),
        )?;
        require(
            context.generation == self.runtime.generation(),
            "task_board_identity_conflict",
            "The Operation query must use this live connection generation.",
        )?;
        if receipt_only {
            if let Some(receipt) = self
                .operations
                .store
                .retired_operation_receipt(context.caller_principal_id, id)?
            {
                self.runtime.validate("OperationReceipt", &receipt)?;
                require(
                    receipt.get("callerPrincipalId")
                        == Some(&Json::String(context.caller_principal_id.clone()))
                        && receipt.get("operationId") == Some(&Json::String(id.clone())),
                    "task_board_identity_conflict",
                    "Operation receipt does not match its original caller.",
                )?;
                return Ok(receipt);
            }
        }
        let operation = self
            .operations
            .find(context.caller_principal_id, id)
            .await?
            .map(|d| d.value)
            .ok_or_else(|| {
                IvyError::new(
                    "not_found",
                    "This caller has no retained TaskBoard operation with that identity.",
                )
            })?;
        if receipt_only {
            let receipt =
                crate::journal_store::persistent_reads::operation_receipt::operation_receipt::<
                    IvyError,
                >(&operation)?;
            self.runtime.validate("OperationReceipt", &receipt)?;
            Ok(receipt)
        } else {
            Ok(operation)
        }
    }
    async fn task_document(&self, query: &Json) -> IvyResult<Document> {
        let address = f(query, "task");
        if address.as_js_string().is_some() {
            return self
                .operations
                .store
                .read("task-board/task", address, true)
                .await;
        }
        let page = self
            .operations
            .store
            .page(
                "task-board/task",
                &Json::object([
                    (
                        "where",
                        Json::object([
                            ("op", "eq".into()),
                            ("field", "data:/taskKey".into()),
                            ("value", address.required("taskKey")?.clone()),
                        ]),
                    ),
                    ("limit", Json::Number(2.0)),
                ]),
            )
            .await?;
        let items = f(&page, "items")
            .as_array()
            .ok_or_else(IvyError::internal)?;
        require(
            items.len() == 1 && !present(f(&page, "nextCursor")),
            "not_found",
            "Task key was not found in this workspace.",
        )?;
        self.operations
            .store
            .read("task-board/task", items[0].required("objectId")?, true)
            .await
    }
    pub async fn task(&self, query: &Json, principal: &JsString) -> IvyResult<Json> {
        let document = self.task_document(query).await?;
        let condition = execution_condition(&document.value)?;
        let id = s(&document.pin, "objectId")?;
        let state = self
            .operations
            .store
            .named(
                "task-board/comment-read-state",
                &read_state_name(principal, id)?,
                None,
            )
            .await?;
        if let Some(state) = &state {
            check_state(&state.value, principal, id)?;
        }
        let read = state
            .as_ref()
            .and_then(|s| f(&s.value, "readAgentCommentCount").as_f64())
            .unwrap_or(0.0);
        let unread = (f(&document.value, "agentCommentCount")
            .as_f64()
            .ok_or_else(IvyError::internal)?
            - read)
            .max(0.0);
        Ok(Json::object([
            ("object", document.pin),
            ("task", document.value),
            ("executionCondition", condition),
            ("unreadAgentComments", Json::Number(unread)),
        ]))
    }
    pub async fn list(&self, query: &Json) -> IvyResult<Json> {
        self.runtime.validate("TaskListQuery", query)?;
        let store = &self.operations.store;
        let related = f(query, "relatedTo");
        let identity = if present(related) {
            workspace_identity(
                store,
                &store.read("task-board/task", related, true).await?.value,
            )
            .await?
        } else {
            None
        };
        let mut filters = vec![];
        for (key, field) in [
            ("status", "data:/workflowState"),
            ("category", "data:/fields/category"),
            ("parentTaskId", "data:/parentTaskId"),
        ] {
            if present(f(query, key)) {
                filters.push(Json::object([
                    ("op", "eq".into()),
                    ("field", field.into()),
                    ("value", f(query, key).clone()),
                ]));
            }
        }
        let mut options = Json::object([
            (
                "limit",
                query
                    .get("limit")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(Json::Number(50.0)),
            ),
            (
                "select",
                Json::Array(
                    [
                        "data:/taskKey",
                        "data:/fields/title",
                        "data:/workflowState",
                        "data:/fields/category",
                    ]
                    .into_iter()
                    .map(Json::from)
                    .collect(),
                ),
            ),
        ]);
        if present(f(query, "parentTaskId")) {
            options.insert(
                "contractVersions",
                Json::Array(vec![store.contract_version("task-board/task")?.into()]),
            );
        }
        if present(f(query, "cursor")) {
            options.insert("cursor", f(query, "cursor").clone());
        }
        if !filters.is_empty() {
            options.insert(
                "where",
                if filters.len() == 1 {
                    filters.remove(0)
                } else {
                    Json::object([("op", "and".into()), ("args", Json::Array(filters))])
                },
            );
        }
        let page = store.page("task-board/task", &options).await?;
        let items = f(&page, "items")
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let mut peers = HashSet::new();
        if let Some(identity) = identity {
            for item in items {
                if item.get("objectId") != Some(related) {
                    let pin = Json::object([
                        ("objectId", item.required("objectId")?.clone()),
                        ("revision", item.required("revision")?.clone()),
                    ]);
                    if workspace_identity(
                        store,
                        &store.read("task-board/task", &pin, true).await?.value,
                    )
                    .await?
                        == Some(identity.clone())
                    {
                        peers.insert(s(item, "objectId")?.clone());
                    }
                }
            }
        }
        let mut summaries = vec![];
        for item in items {
            if present(related) && !peers.contains(s(item, "objectId")?) {
                continue;
            }
            let values = f(item, "values");
            require(
                ["data:/taskKey", "data:/fields/title", "data:/workflowState"]
                    .iter()
                    .all(|k| f(values, k).as_js_string().is_some())
                    && values
                        .get("data:/fields/category")
                        .is_some_and(|v| v.is_null() || v.as_js_string().is_some()),
                "task_board_contract_mismatch",
                "Task summary has invalid fields.",
            )?;
            summaries.push(Json::object([
                ("taskId", item.required("objectId")?.clone()),
                ("revision", item.required("revision")?.clone()),
                ("taskKey", f(values, "data:/taskKey").clone()),
                ("title", f(values, "data:/fields/title").clone()),
                ("workflowState", f(values, "data:/workflowState").clone()),
                ("category", f(values, "data:/fields/category").clone()),
            ]));
        }
        Ok(Json::object([
            ("items", Json::Array(summaries)),
            ("nextCursor", page.required("nextCursor")?.clone()),
        ]))
    }
    pub async fn mark_comments_read(
        &self,
        request: &Json,
        context: &ReadContext<'_>,
    ) -> IvyResult<Json> {
        self.runtime.validate("MarkCommentsReadRequest", request)?;
        self.runtime.verify_owner().await?;
        (context.check_cancelled)()?;
        require(
            context.generation == self.runtime.generation()
                && f(request, "operationId").as_js_string() == Some(context.operation_id),
            "task_board_identity_conflict",
            "The read marker must retain the live caller operation identity.",
        )?;
        let store = &self.operations.store;
        let task = store
            .lifecycle_read(
                "task-board/task",
                &Json::object([
                    ("objectId", request.required("taskId")?.clone()),
                    (
                        "revision",
                        request.required("expectedTaskRevision")?.clone(),
                    ),
                ]),
                true,
                false,
            )
            .await?;
        require(
            task.metadata.get("currentRevision") == request.get("expectedTaskRevision"),
            "revision_conflict",
            "The Task changed before its comments were marked read.",
        )?;
        let id = s(request, "taskId")?;
        let name = read_state_name(context.caller_principal_id, id)?;
        let now = (self.operations.now)();
        let count = f(&task.document.value, "agentCommentCount")
            .as_f64()
            .ok_or_else(IvyError::internal)?;
        for _ in 0..12 {
            (context.check_cancelled)()?;
            let current = store
                .named("task-board/comment-read-state", &name, None)
                .await?;
            if let Some(current) = &current {
                check_state(&current.value, context.caller_principal_id, id)?;
            }
            if let Some(current) = &current {
                if f(&current.value, "readAgentCommentCount")
                    .as_f64()
                    .is_some_and(|n| n >= count)
                {
                    return Ok(Json::object([
                        ("task", task.document.pin),
                        ("readState", current.pin.clone()),
                        ("unreadAgentComments", Json::Number(0.0)),
                    ]));
                }
            }
            let value = Json::object([
                ("schemaVersion", Json::Number(1.0)),
                (
                    "principalId",
                    Json::String(context.caller_principal_id.clone()),
                ),
                ("taskId", Json::String(id.clone())),
                ("readAgentCommentCount", Json::Number(count)),
                ("updatedAt", now.clone().into()),
            ]);
            let previous = current
                .as_ref()
                .map(|d| d.pin.clone())
                .unwrap_or(Json::Null);
            let step = format!(
                "mark-comments-read:{}",
                hash_json(&Json::object([
                    ("request", request.clone()),
                    ("previous", previous),
                    ("value", value.clone())
                ]))?
            );
            let destination = if let Some(current) = current {
                Json::object([
                    ("objectId", current.pin.required("objectId")?.clone()),
                    (
                        "expectedRevision",
                        current.pin.required("revision")?.clone(),
                    ),
                ])
            } else {
                Json::object([(
                    "create",
                    Json::object([
                        (
                            "parentId",
                            store
                                .root_object_id()
                                .map(Json::String)
                                .unwrap_or(Json::Null),
                        ),
                        ("name", name.clone().into()),
                    ]),
                )])
            };
            match store
                .write(
                    "task-board/comment-read-state",
                    &value,
                    &mutation(&JsString::from(name.as_str()), &step)?,
                    &destination,
                )
                .await
            {
                Ok(pin) => {
                    return Ok(Json::object([
                        ("task", task.document.pin),
                        ("readState", pin),
                        ("unreadAgentComments", Json::Number(0.0)),
                    ]));
                }
                Err(e) if matches!(e.code.as_str(), "revision_conflict" | "mutation_conflict") => {
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Err(IvyError::new(
            "task_board_contention",
            "The comment read marker changed repeatedly. Retry from the current Task.",
        ))
    }
}
