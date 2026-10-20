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
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
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
fn condition(state: &str, detail: impl Into<JsString>) -> Json {
    Json::object([
        ("state", state.into()),
        ("detail", Json::String(detail.into())),
    ])
}
/// Shared read projection over the exact Task; publication and claim take priority.
pub fn execution_condition(task: &Json) -> IvyResult<Json> {
    let fields = f(task, "fields");
    let claim = f(task, "claim");
    let waiting = f(task, "waiting");
    if truthy(f(task, "publication")) {
        return Ok(condition(
            "publishing",
            "Publishing the current Task update.",
        ));
    }
    if truthy(claim) {
        if eq(claim, "phase", "publishing_result") {
            return Ok(condition(
                "publishing",
                "Native execution ended; publishing the ticket handoff.",
            ));
        }
        let host = s(f(claim, "target"), "hostId")?.clone();
        if eq(waiting, "reason", "host") {
            return Ok(condition(
                "recovering",
                concat([
                    "Reconnecting to ".into(),
                    host,
                    "; TaskBoard will retry automatically.".into(),
                ]),
            ));
        }
        if eq(claim, "phase", "outcome_unknown") || eq(waiting, "reason", "external_outcome") {
            return Ok(condition(
                "recovering",
                concat([
                    "Reconciling the original native execution on ".into(),
                    host,
                    "; TaskBoard will retry automatically.".into(),
                ]),
            ));
        }
        if ["claimed", "run_created", "starting"]
            .iter()
            .any(|p| eq(claim, "phase", p))
        {
            return Ok(condition(
                "starting",
                concat(["Starting on ".into(), host, ".".into()]),
            ));
        }
        if eq(claim, "phase", "waiting_input") {
            return Ok(condition("needs_user", "The native Task needs user input."));
        }
        return Ok(condition(
            "active",
            concat(["Working on ".into(), host, ".".into()]),
        ));
    }
    if ["backlog", "done", "cancelled"]
        .iter()
        .any(|v| eq(task, "workflowState", v))
    {
        return Ok(condition(
            "idle",
            if eq(task, "workflowState", "backlog") {
                "Backlog Tasks are not scheduled."
            } else {
                "No work is scheduled."
            },
        ));
    }
    if eq(task, "workflowState", "review") {
        return Ok(condition("needs_user", "Final result is ready for review."));
    }
    if truthy(waiting) {
        let state = if eq(waiting, "reason", "time") {
            "deferred"
        } else if eq(waiting, "reason", "dependency") {
            "blocked_dependency"
        } else if eq(waiting, "reason", "external_outcome") {
            "outcome_unknown"
        } else if eq(waiting, "reason", "user") {
            "needs_user"
        } else {
            "blocked_environment"
        };
        return Ok(condition(state, s(waiting, "detail")?.clone()));
    }
    if eq(fields, "control", "user") {
        return Ok(if eq(task, "workflowState", "in_progress") {
            condition("active", "Manual work is in progress.")
        } else {
            condition("needs_user", "Ready for you to start.")
        });
    }
    let capabilities = f(fields, "requiredCapabilities").as_array().unwrap_or(&[]);
    let mut needs = JsString::default();
    if !capabilities.is_empty() {
        needs = " with ".into();
        for (i, capability) in capabilities.iter().enumerate() {
            if i != 0 {
                needs.0.extend(", ".encode_utf16());
            }
            needs
                .0
                .extend_from_slice(&capability.as_js_string().ok_or_else(IvyError::internal)?.0);
        }
    }
    let requirement = f(fields, "executionRequirement");
    Ok(
        if !truthy(requirement) || eq(requirement, "kind", "automatic") {
            if needs.0.is_empty() {
                condition("queued", "Waiting for the next available execution PC.")
            } else {
                condition(
                    "queued",
                    concat(["Waiting for a ready host".into(), needs, ".".into()]),
                )
            }
        } else {
            condition(
                "queued",
                concat([
                    "Waiting for ".into(),
                    s(requirement, "hostId")?.clone(),
                    needs,
                    ".".into(),
                ]),
            )
        },
    )
}
/// Match workspaceIdentity, which deliberately does not normalize dot segments.
pub async fn workspace_identity<S: Store>(store: &S, task: &Json) -> IvyResult<Option<JsString>> {
    let mut workspace = f(f(task, "claim"), "workspace").clone();
    if workspace.is_null() && truthy(f(task, "lastRun")) {
        workspace = f(
            &store
                .read("task-board/run", f(task, "lastRun"), true)
                .await?
                .value,
            "workspace",
        )
        .clone();
    }
    if !truthy(&workspace) {
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
        path = path.to_lowercase();
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
    pub async fn operation(&self, id: &JsString, context: &ReadContext<'_>) -> IvyResult<Json> {
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
        self.operations
            .find(context.caller_principal_id, id)
            .await?
            .map(|d| d.value)
            .ok_or_else(|| {
                IvyError::new(
                    "not_found",
                    "This caller has no retained TaskBoard operation with that identity.",
                )
            })
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
            items.len() == 1 && !truthy(f(&page, "nextCursor")),
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
        let identity = if truthy(related) {
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
            if truthy(f(query, key)) {
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
        if truthy(f(query, "parentTaskId")) {
            options.insert(
                "contractVersions",
                Json::Array(vec![store.contract_version("task-board/task")?.into()]),
            );
        }
        if truthy(f(query, "cursor")) {
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
            if truthy(related) && !peers.contains(s(item, "objectId")?) {
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
