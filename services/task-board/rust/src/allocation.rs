//! Select a current eligible native host and retain its immutable execution plan.
//! Selection does not create a Run or authorize native execution.
use crate::{
    configuration::{effective_execution, supports_execution_options},
    native_intent::TaskPrompts,
    native_recovery::fresh_context_recovery,
    operations::{Document, Store, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    native_evidence::NativeOwnerIdentity,
    native_owner::NativeOwner,
    native_plan::{NativePlanKind, save_native_plan, validate_native_plan_draft},
    native_project::native_thread_project,
    rpc::{RequestOptions, RpcClient},
    tools::{BoundToolClient, InterfaceRequirement, ToolCallOptions, ToolClients},
};
use std::{collections::HashMap, future::Future, sync::Arc, task::Poll};

#[cfg(test)]
#[path = "../../../../tests/fixtures/task-board-allocation-continuation.rs"]
mod continuation_tests;

#[allow(async_fn_in_trait)]
pub trait AllocationStore: Store {
    async fn page(&self, key: &str, options: &Json) -> IvyResult<Json>;
    async fn workspace_owner(
        &self,
        host: &JsString,
        path: &JsString,
        task: &JsString,
    ) -> IvyResult<Option<JsString>>;
    async fn named_plan(&self, name: &JsString) -> IvyResult<Option<Document>>;
}
/// Native owner bindings. Eligible service identifiers have ordinal order.
pub trait AllocationRuntime: TaskPrompts {
    fn client(&self) -> Arc<dyn RpcClient>;
    fn tools(&self) -> &Arc<ToolClients>;
    fn principal_id(&self) -> &JsString;
    fn service_node_id(&self) -> &JsString;
}
pub struct Allocator<'a, S, E> {
    pub store: &'a S,
    pub engine: &'a E,
    pub catalogs: &'a CheckedNativeContracts,
}
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn eq(v: &Json, k: &str, t: &str) -> bool {
    v.get(k).is_some_and(|value| value.is_str(t))
}
fn same(left: &Json, right: &Json) -> IvyResult<bool> {
    if left.is_null() || right.is_null() {
        return Ok(left == right);
    }
    Ok(canonical(left, MANAGEMENT_FRAME_BYTES)? == canonical(right, MANAGEMENT_FRAME_BYTES)?)
}
fn arr(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
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
fn join(v: &[JsString], separator: &str) -> JsString {
    let mut out = Vec::new();
    for (i, s) in v.iter().enumerate() {
        if i > 0 {
            out.extend(separator.encode_utf16())
        }
        out.extend(&s.0)
    }
    JsString(out)
}
fn text(v: &Json) -> JsString {
    v.as_js_string().cloned().unwrap_or_default()
}
fn nullish<'a>(v: &'a Json, fallback: &'a Json) -> &'a Json {
    if v.is_null() { fallback } else { v }
}
pub fn reserves_workspace(r: &Json) -> bool {
    eq(r, "kind", "existing_project") && (field(r, "useWorktree").as_bool() == Some(true))
}
pub fn shared_project_key(r: &Json, w: &Json) -> Option<JsString> {
    if eq(r, "kind", "existing_project") && (field(r, "useWorktree").as_bool() != Some(true)) {
        Some(concat(["project:".into(), text(field(r, "projectId"))]))
    } else if eq(r, "kind", "directory_path") {
        Some(concat([
            "directory:".into(),
            text(field(w, "canonicalCwd")),
        ]))
    } else {
        None
    }
}
pub fn plan_semantics(plan: &Json) -> IvyResult<()> {
    require(
        field(field(plan, "threadStart"), "ephemeral") != &true.into(),
        "task_board_ephemeral_context",
        "A durable workflow cannot use an ephemeral native context.",
    )?;
    for key in ["threadStart", "threadResume", "turnStart"] {
        let p = plan.required(key)?;
        require(
            field(p, "permissions").is_null()
                || (field(p, "sandbox").is_null() && field(p, "sandboxPolicy").is_null()),
            "task_board_permission_conflict",
            "A native plan cannot mix permissions with legacy sandbox fields.",
        )?;
    }
    Ok(())
}
pub async fn shared_project_owner<S: AllocationStore>(
    store: &S,
    task: &Json,
    task_id: &JsString,
    target: &Json,
    workspace: &Json,
) -> IvyResult<Option<JsString>> {
    let Some(key) = shared_project_key(
        field(field(task, "fields"), "workspaceRequirement"),
        workspace,
    ) else {
        return Ok(None);
    };
    if let Some(owner) = store
        .workspace_owner(s(target, "hostId")?, &key, task_id)
        .await?
    {
        return Ok(Some(owner));
    }
    let mut cursor = None;
    loop {
        let mut options = Json::object([
            ("limit", Json::Number(100.0)),
            (
                "where",
                Json::object([
                    ("op", "in".into()),
                    ("field", "data:/workflowState".into()),
                    (
                        "value",
                        Json::Array(vec!["in_progress".into(), "waiting".into()]),
                    ),
                ]),
            ),
        ]);
        if let Some(c) = cursor {
            options.insert("cursor", Json::String(c));
        }
        let page = store.page("task-board/task", &options).await?;
        for item in arr(field(&page, "items")) {
            let id = s(item, "objectId")?;
            if id == task_id {
                continue;
            }
            let peer = store
                .read("task-board/task", &Json::String(id.clone()), true)
                .await?
                .value;
            if (field(field(task, "fields"), "allowParallel").as_bool() == Some(true))
                && (field(field(&peer, "fields"), "allowParallel").as_bool() == Some(true))
            {
                continue;
            }
            if eq(&peer, "workflowState", "waiting")
                && (eq(field(&peer, "runtimeWait"), "reason", "workspace")
                    || field(&peer, "lastRun").is_null())
            {
                continue;
            }
            let claim = field(&peer, "claim");
            let run = if claim.is_null() && !field(&peer, "lastRun").is_null() {
                Some(
                    store
                        .read("task-board/run", peer.required("lastRun")?, true)
                        .await?
                        .value,
                )
            } else {
                None
            };
            if claim.is_null()
                && (run.is_none()
                    || ["completed", "failed", "cancelled"]
                        .iter()
                        .any(|p| eq(run.as_ref().unwrap(), "phase", p)))
            {
                continue;
            }
            let source = if !claim.is_null() {
                claim
            } else {
                run.as_ref().unwrap()
            };
            let selected = field(source, "target");
            let w = field(source, "workspace");
            if selected.get("hostId") != target.get("hostId") || w.is_null() {
                continue;
            }
            if shared_project_key(field(field(&peer, "fields"), "workspaceRequirement"), w)
                == Some(key.clone())
            {
                return Ok(Some(id.clone()));
            }
        }
        cursor = field(&page, "nextCursor")
            .as_js_string()
            .filter(|s| !s.0.is_empty())
            .cloned();
        if cursor.is_none() {
            break;
        }
    }
    Ok(None)
}
struct Candidate {
    node: Json,
    catalog: Json,
    workspace: Json,
}
fn validate_continuation(task: &Document, run: &Json) -> IvyResult<()> {
    require(
        run.get("taskId") == task.pin.get("objectId")
            && same(
                field(run, "primaryResourceRef"),
                field(&task.value, "primaryResourceRef"),
            )?
            && ["completed", "failed", "cancelled"]
                .iter()
                .any(|phase| eq(run, "phase", phase))
            && !["active", "outcome_unknown"]
                .iter()
                .any(|state| eq(field(run, "externalOutcome"), "state", state)),
        "task_board_execution_unresolved",
        "Conversation continuation requires its own terminal native Run and saved context.",
    )
}
fn continuation_workspace(
    task: &Json,
    previous: &Json,
    observed: &Json,
    resolved_cwd: Option<&Json>,
) -> IvyResult<Json> {
    let result = observed.required("reply")?.get("result").ok_or_else(|| {
        IvyError::new(
            "task_board_native_read_failed",
            "The saved conversation could not be observed.",
        )
    })?;
    let thread = result.required("thread")?;
    require(
        thread.get("id") == field(task, "primaryResourceRef").get("nativeId")
            && thread.get("ephemeral") == Some(&Json::Bool(false))
            && thread.get("cwd").and_then(Json::as_js_string).is_some()
            && resolved_cwd.or_else(|| thread.get("cwd")) == previous.get("canonicalCwd")
            && (field(previous, "project").is_null()
                || thread.get("projectId") == field(previous, "project").get("nativeId")),
        "task_board_workspace_mismatch",
        "The native conversation no longer identifies its retained working directory and project.",
    )?;
    let mut workspace = previous.clone();
    if let Some(project) = thread.get("projectId").and_then(Json::as_js_string) {
        workspace.insert("nativeProjectId", Json::String(project.clone()));
    }
    Ok(workspace)
}
impl<S: AllocationStore, E: AllocationRuntime> Allocator<'_, S, E> {
    async fn owner(
        &self,
        task: &Document,
        node: &Json,
        workspace: &Json,
    ) -> IvyResult<Option<JsString>> {
        if reserves_workspace(field(field(&task.value, "fields"), "workspaceRequirement")) {
            self.store
                .workspace_owner(
                    s(node, "hostId")?,
                    s(workspace, "intendedPath")?,
                    s(&task.pin, "objectId")?,
                )
                .await
        } else {
            shared_project_owner(
                self.store,
                &task.value,
                s(&task.pin, "objectId")?,
                node,
                workspace,
            )
            .await
        }
    }
    pub async fn allocate(
        &self,
        task: &Document,
        operation_id: &JsString,
        feedback: Option<&JsString>,
        retain_workspace: bool,
    ) -> IvyResult<Json> {
        let recovery = fresh_context_recovery(self.store, &task.value).await?;
        let continuation = if retain_workspace
            && !field(&task.value, "primaryResourceRef").is_null()
            && !field(&task.value, "lastRun").is_null()
        {
            let run = self
                .store
                .read(
                    "task-board/run",
                    field(field(&task.value, "lastRun"), "objectId"),
                    true,
                )
                .await?;
            validate_continuation(task, &run.value)?;
            Some(run)
        } else {
            None
        };
        let effective = effective_execution(self.store, &task.value).await?;
        let requirement = if eq(
            field(&effective, "executionRequirement"),
            "kind",
            "automatic",
        ) {
            &Json::Null
        } else {
            field(&effective, "executionRequirement")
        };
        let mut skipped = Vec::<JsString>::new();
        let listed = self
            .engine
            .client()
            .request(
                "serviceNodes.list",
                Json::object([
                    ("serviceName", "agent-manager".into()),
                    ("limit", Json::Number(200.0)),
                ]),
                RequestOptions::default(),
            )
            .await?;
        let mut nodes = vec![];
        for node in arr(listed.required("items")?) {
            if eq(requirement, "kind", "host") && node.get("hostId") != requirement.get("hostId") {
                continue;
            }
            let prefix = concat([s(node, "hostId")?.clone(), ": ".into()]);
            if let Some(recovery) = &recovery {
                let target = recovery.required("target")?;
                if node.get("hostId") != target.get("hostId")
                    || node.get("serviceNodeId") != target.get("serviceNodeId")
                {
                    skipped.push(concat([
                        prefix,
                        "recovery must use AgentManager ".into(),
                        s(target, "serviceNodeId")?.clone(),
                    ]));
                    continue;
                }
            }
            let primary = field(&task.value, "primaryResourceRef");
            if !primary.is_null() && node.get("serviceNodeId") != primary.get("serviceNodeId") {
                skipped.push(concat([
                    prefix,
                    "the task conversation belongs to AgentManager ".into(),
                    s(primary, "serviceNodeId")?.clone(),
                ]));
                continue;
            }
            if ["connected", "synced", "ready"]
                .iter()
                .any(|k| (field(node, k).as_bool() != Some(true)))
            {
                skipped.push(concat([
                    prefix,
                    "AgentManager is ".into(),
                    if (field(node, "connected").as_bool() != Some(true)) {
                        "disconnected"
                    } else if (field(node, "synced").as_bool() != Some(true)) {
                        "not synchronized"
                    } else {
                        "not ready"
                    }
                    .into(),
                ]));
                continue;
            }
            nodes.push(node.clone());
        }
        let required = arr(field(field(&task.value, "fields"), "requiredCapabilities"));
        let mut candidates = vec![];
        let mut workspace_failure = None;
        let mut unsupported = false;
        let mut capability_gap: Option<(JsString, Vec<JsString>)> = None;
        for node in nodes {
            let result: IvyResult<Option<Candidate>> = async {
                let tools = BoundToolClient::new(
                    self.engine.client(),
                    s(&node, "serviceNodeId")?.clone(),
                    &[InterfaceRequirement {
                        namespace: "agent".into(),
                        interface_version: "1.0.0".into(),
                    }],
                    None,
                )?;
                let status = tools
                    .read(
                        &"agent.status".into(),
                        Json::Object(vec![]),
                        ToolCallOptions::default(),
                    )
                    .await?;
                if !eq(&status, "state", "ready")
                    || status.get("hostId") != node.get("hostId")
                    || status.get("serviceNodeId") != node.get("serviceNodeId")
                {
                    skipped.push(concat([
                        s(&node, "hostId")?.clone(),
                        ": native agent is ".into(),
                        text(field(&status, "state")),
                    ]));
                    return Ok(None);
                }
                let profile = tools
                    .read(
                        &"agent.capabilities".into(),
                        Json::Object(vec![]),
                        ToolCallOptions::default(),
                    )
                    .await?;
                if eq(requirement, "kind", "host")
                    && node.get("hostId") != requirement.get("hostId")
                {
                    return Ok(None);
                }
                let missing: Vec<JsString> = required
                    .iter()
                    .filter(|key| {
                        !arr(field(&profile, "capabilities"))
                            .iter()
                            .any(|v| v.get("key") == Some(*key))
                    })
                    .map(text)
                    .collect();
                if !missing.is_empty() {
                    if capability_gap.is_none() {
                        capability_gap = Some((s(&node, "hostId")?.clone(), missing));
                    }
                    return Ok(None);
                }
                let workspace = if let Some(previous) = &continuation {
                    let target = Json::object([
                        ("hostId", node.required("hostId")?.clone()),
                        ("serviceNodeId", node.required("serviceNodeId")?.clone()),
                    ]);
                    require(
                        same(field(&previous.value, "target"), &target)?,
                        "task_board_workspace_mismatch",
                        "Conversation continuation must retain its original native host.",
                    )?;
                    let owner = NativeOwner::new(
                        self.engine.tools().clone(),
                        self.engine.principal_id().clone(),
                        NativeOwnerIdentity {
                            service_node_id: s(&node, "serviceNodeId")?.clone(),
                            host_id: Some(s(&status, "hostId")?.clone()),
                            native_version: s(&status, "nativeVersion")?.clone(),
                            native_executable_hash: s(&status, "nativeExecutableHash")?.clone(),
                            catalog_hash: s(&status, "catalogHash")?.clone(),
                        },
                        RequestOptions::default(),
                    );
                    let observed = owner
                        .read(
                            &"thread/read".into(),
                            Json::object([
                                (
                                    "threadId",
                                    field(&task.value, "primaryResourceRef")
                                        .required("nativeId")?
                                        .clone(),
                                ),
                                ("includeTurns", false.into()),
                            ]),
                            None,
                            true,
                        )
                        .await?;
                    let previous_workspace = previous.value.required("workspace")?;
                    let cwd = observed
                        .get("reply")
                        .and_then(|reply| reply.get("result"))
                        .and_then(|result| result.get("thread"))
                        .and_then(|thread| thread.get("cwd"));
                    // Resolve differing spellings on the native owner, whose
                    // filesystem can recognize aliases such as Windows'
                    // verbatim prefix. Never interpret a remote host's paths
                    // using this service's local platform or change its saved
                    // workspace to the observed spelling.
                    let resolved = if cwd.is_some_and(|cwd| {
                        cwd.as_js_string().is_some()
                            && Some(cwd) != previous_workspace.get("canonicalCwd")
                    }) {
                        Some(
                            tools
                                .call(
                                    &"agent.resolveWorkspace".into(),
                                    Json::object([
                                        ("taskKey", task.value.required("taskKey")?.clone()),
                                        (
                                            "requirement",
                                            Json::object([
                                                ("kind", "directory_path".into()),
                                                ("path", cwd.unwrap().clone()),
                                            ]),
                                        ),
                                        ("prepare", false.into()),
                                    ]),
                                    None,
                                    ToolCallOptions::default(),
                                )
                                .await?,
                        )
                    } else {
                        None
                    };
                    continuation_workspace(
                        &task.value,
                        previous_workspace,
                        &observed,
                        resolved
                            .as_ref()
                            .and_then(|workspace| workspace.get("canonicalCwd")),
                    )?
                } else {
                    tools
                        .call(
                            &"agent.resolveWorkspace".into(),
                            Json::object([
                                ("taskKey", task.value.required("taskKey")?.clone()),
                                (
                                    "requirement",
                                    task.value
                                        .required("fields")?
                                        .required("workspaceRequirement")?
                                        .clone(),
                                ),
                                ("prepare", false.into()),
                            ]),
                            None,
                            ToolCallOptions::default(),
                        )
                        .await?
                };
                if let Some(previous) = &recovery {
                    let old = previous.required("workspace")?;
                    require(
                        workspace.get("canonicalCwd") == old.get("canonicalCwd")
                            && workspace.get("intendedPath") == old.get("intendedPath"),
                        "task_board_workspace_mismatch",
                        "Fresh recovery must retain the original workspace.",
                    )?;
                }
                let catalog = tools
                    .read(
                        &"agent.catalog".into(),
                        Json::Object(vec![]),
                        ToolCallOptions::default(),
                    )
                    .await?;
                if !supports_execution_options(
                    self.engine.client(),
                    &node,
                    effective.required("nativeOptions")?,
                )
                .await?
                {
                    unsupported = true;
                    return Ok(None);
                }
                Ok(Some(Candidate {
                    node: node.clone(),
                    catalog,
                    workspace,
                }))
            }
            .await;
            match result {
                Ok(Some(candidate)) => candidates.push(candidate),
                Ok(None) => {}
                Err(error) => {
                    if [
                        "not_found",
                        "target_conflict",
                        "task_board_project_unavailable",
                        "task_board_project_mismatch",
                        "task_board_workspace_mismatch",
                    ]
                    .contains(&error.code.as_str())
                    {
                        if workspace_failure.is_none() {
                            workspace_failure = Some(error)
                        }
                    } else {
                        skipped.push(concat([
                            s(&node, "hostId")?.clone(),
                            ": ".into(),
                            error.message_js(),
                        ]));
                    }
                }
            }
        }
        if candidates.is_empty() {
            if let Some(error) = workspace_failure {
                return Err(error);
            }
            let (code, detail) = if unsupported {
                (
                    "task_board_model_unavailable",
                    "No eligible ready host supports the selected model, reasoning and speed."
                        .into(),
                )
            } else if let Some((host, missing)) = capability_gap {
                (
                    "task_board_capability_unavailable",
                    concat([
                        host,
                        " does not advertise ".into(),
                        join(&missing, ", "),
                        ".".into(),
                    ]),
                )
            } else {
                let detail = if eq(requirement, "kind", "host") {
                    concat([
                        "No ready AgentManager is available on ".into(),
                        s(requirement, "hostId")?.clone(),
                        ".".into(),
                    ])
                } else if !required.is_empty() {
                    concat([
                        "No ready AgentManager advertises ".into(),
                        join(&required.iter().map(text).collect::<Vec<_>>(), ", "),
                        ".".into(),
                    ])
                } else {
                    "No ready AgentManager is currently available.".into()
                };
                (
                    "task_board_target_unavailable",
                    if skipped.is_empty() {
                        detail
                    } else {
                        concat([
                            detail,
                            " ".into(),
                            join(&skipped[..skipped.len().min(3)], "; "),
                            ".".into(),
                        ])
                    },
                )
            };
            return Err(IvyError::new_js(code.into(), detail));
        }
        let mut runs = HashMap::<JsString, usize>::new();
        let mut cursor = None;
        loop {
            let mut options = Json::object([
                ("limit", Json::Number(50.0)),
                (
                    "where",
                    Json::object([
                        ("op", "in".into()),
                        ("field", "data:/phase".into()),
                        (
                            "value",
                            Json::Array(
                                [
                                    "starting",
                                    "running",
                                    "waiting_input",
                                    "publishing_result",
                                    "cancel_requested",
                                    "outcome_unknown",
                                ]
                                .into_iter()
                                .map(Json::from)
                                .collect(),
                            ),
                        ),
                    ]),
                ),
            ]);
            if let Some(c) = cursor {
                options.insert("cursor", Json::String(c));
            }
            let page = self.store.page("task-board/run", &options).await?;
            for item in arr(field(&page, "items")) {
                let run = self
                    .store
                    .read(
                        "task-board/run",
                        &Json::object([
                            ("objectId", item.required("objectId")?.clone()),
                            ("revision", item.required("revision")?.clone()),
                        ]),
                        true,
                    )
                    .await?;
                *runs
                    .entry(s(run.value.required("target")?, "serviceNodeId")?.clone())
                    .or_default() += 1;
            }
            cursor = field(&page, "nextCursor")
                .as_js_string()
                .filter(|s| !s.0.is_empty())
                .cloned();
            if cursor.is_none() {
                break;
            }
        }
        let requirement = task
            .value
            .required("fields")?
            .required("workspaceRequirement")?;
        candidates.sort_by(|a, b| {
            let project = |c: &Candidate| {
                eq(requirement, "kind", "existing_project")
                    && !field(&c.workspace, "project").is_null()
            };
            project(b)
                .cmp(&project(a))
                .then_with(|| {
                    runs.get(&text(field(&a.node, "serviceNodeId")))
                        .copied()
                        .unwrap_or(0)
                        .cmp(
                            &runs
                                .get(&text(field(&b.node, "serviceNodeId")))
                                .copied()
                                .unwrap_or(0),
                        )
                })
                .then_with(|| {
                    text(field(&a.node, "serviceNodeId"))
                        .cmp(&text(field(&b.node, "serviceNodeId")))
                })
        });
        let mut selected = None;
        for c in candidates {
            if continuation.is_some() {
                selected = Some(c);
                break;
            }
            match self.owner(task, &c.node, &c.workspace).await? {
                None => {
                    selected = Some(c);
                    break;
                }
                Some(owner) => {
                    workspace_failure = Some(
                        IvyError::new_js(
                            "task_board_workspace_reserved".into(),
                            concat([
                                "Workspace is already reserved by Task ".into(),
                                owner.clone(),
                                ".".into(),
                            ]),
                        )
                        .with_details(Json::object([("blockerTaskId", Json::String(owner))])),
                    )
                }
            }
        }
        let selected = match selected {
            Some(c) => c,
            None => return Err(workspace_failure.unwrap()),
        };
        let tools = BoundToolClient::new(
            self.engine.client(),
            s(&selected.node, "serviceNodeId")?.clone(),
            &[
                InterfaceRequirement {
                    namespace: "agent".into(),
                    interface_version: "1.0.0".into(),
                },
                InterfaceRequirement {
                    namespace: "codex".into(),
                    interface_version: s(&selected.catalog, "version")?.to_utf8()?,
                },
            ],
            None,
        )?;
        let workspace = if continuation.is_some() {
            selected.workspace.clone()
        } else {
            tools
                .call(
                    &"agent.resolveWorkspace".into(),
                    Json::object([
                        ("taskKey", task.value.required("taskKey")?.clone()),
                        ("requirement", requirement.clone()),
                        ("prepare", true.into()),
                    ]),
                    Some(operation_id.clone()),
                    ToolCallOptions::default(),
                )
                .await?
        };
        if continuation.is_none() {
            if let Some(owner) = self.owner(task, &selected.node, &workspace).await? {
                return Err(IvyError::new_js(
                    "task_board_workspace_reserved".into(),
                    concat([
                        "Workspace is already reserved by Task ".into(),
                        owner.clone(),
                        ".".into(),
                    ]),
                )
                .with_details(Json::object([("blockerTaskId", Json::String(owner))])));
            }
        }
        if let Some(previous) = &recovery {
            let old = previous.required("workspace")?;
            require(
                workspace.get("canonicalCwd") == old.get("canonicalCwd")
                    && workspace.get("intendedPath") == old.get("intendedPath"),
                "task_board_workspace_mismatch",
                "Fresh recovery must retain the original workspace after preparation.",
            )?;
        }
        let names: Vec<JsString> = [
            "codex.thread/start",
            "codex.thread/resume",
            "codex.turn/start",
            "codex.turn/interrupt",
        ]
        .into_iter()
        .map(JsString::from)
        .collect();
        let mut futures = names
            .iter()
            .map(|n| Box::pin(tools.binding(n, None)))
            .collect::<Vec<_>>();
        let mut replies = [None, None, None, None];
        let bindings = std::future::poll_fn(|cx| {
            for (index, future) in futures.iter_mut().enumerate() {
                if replies[index].is_none() {
                    if let Poll::Ready(reply) = future.as_mut().poll(cx) {
                        replies[index] = Some(reply);
                    }
                }
            }
            for reply in &mut replies {
                if reply.as_ref().is_some_and(Result::is_err) {
                    return Poll::Ready(Err(reply.take().unwrap().err().unwrap()));
                }
            }
            if replies.iter().all(Option::is_some) {
                Poll::Ready(Ok(replies
                    .iter_mut()
                    .map(|r| r.take().unwrap().unwrap())
                    .collect::<Vec<_>>()))
            } else {
                Poll::Pending
            }
        })
        .await?;
        let prompt = match feedback {
            Some(feedback) => feedback.clone(),
            None => self
                .engine
                .task_prompt(&task.value, self.engine.service_node_id(), None)?,
        };
        let options = effective.required("nativeOptions")?;
        let tier = field(options, "serviceTier");
        let tier = if tier == &Json::from("fast") || tier == &Json::from("flex") {
            tier.clone()
        } else {
            Json::Null
        };
        let mut start = Json::object([
            ("cwd", workspace.required("canonicalCwd")?.clone()),
            ("serviceTier", tier.clone()),
        ]);
        if field(&workspace, "nativeProjectId")
            .as_js_string()
            .is_some_and(|text| !text.0.is_empty())
        {
            start.insert("projectId", workspace.required("nativeProjectId")?.clone());
        } else if continuation.is_some() {
            start.insert("projectId", "".into());
        }
        let mut turn = Json::object([
            (
                "input",
                Json::Array(vec![Json::object([
                    ("type", "text".into()),
                    ("text", Json::String(prompt)),
                    ("text_elements", Json::Array(vec![])),
                ])]),
            ),
            ("serviceTier", tier),
        ]);
        if field(options, "model")
            .as_js_string()
            .is_some_and(|text| !text.0.is_empty())
        {
            start.insert("model", options.required("model")?.clone());
            turn.insert("model", options.required("model")?.clone());
        }
        if field(options, "reasoningEffort")
            .as_js_string()
            .is_some_and(|text| !text.0.is_empty())
        {
            turn.insert("effort", options.required("reasoningEffort")?.clone());
        }
        let mut draft = Json::object([
            (
                "nativeVersion",
                selected.catalog.required("version")?.clone(),
            ),
            (
                "catalogSourceHash",
                selected.catalog.required("sourceHash")?.clone(),
            ),
            (
                "location",
                Json::object([
                    (
                        "serviceNodeId",
                        selected.node.required("serviceNodeId")?.clone(),
                    ),
                    ("hostId", selected.node.required("hostId")?.clone()),
                    (
                        "kind",
                        if eq(requirement, "kind", "existing_project")
                            && (field(&workspace, "useWorktree").as_bool() != Some(true))
                            || eq(requirement, "kind", "directory_path")
                        {
                            "existing"
                        } else if ["repository_path", "new_project_path"]
                            .iter()
                            .any(|v| eq(requirement, "kind", v))
                        {
                            "normal"
                        } else {
                            "internal"
                        }
                        .into(),
                    ),
                    ("cwd", workspace.required("canonicalCwd")?.clone()),
                    (
                        "projectId",
                        nullish(
                            field(&workspace, "nativeProjectId"),
                            nullish(
                                field(field(&workspace, "project"), "nativeId"),
                                &Json::String(concat([
                                    "task-board-".into(),
                                    s(&task.value, "taskKey")?.clone(),
                                ])),
                            ),
                        )
                        .clone(),
                    ),
                ]),
            ),
            (
                "definitions",
                Json::object(
                    ["threadStart", "threadResume", "turnStart", "turnInterrupt"]
                        .into_iter()
                        .zip(&bindings)
                        .map(|(k, b)| (k, b.value().required("definitionHash").unwrap().clone())),
                ),
            ),
            ("threadStart", start),
            (
                "threadResume",
                Json::object([("cwd", workspace.required("canonicalCwd")?.clone())]),
            ),
            ("turnStart", turn),
        ]);
        let start = native_thread_project(
            self.engine.tools(),
            s(&selected.node, "serviceNodeId")?,
            draft.required("threadStart")?,
            RequestOptions::default(),
        )
        .await?;
        if let Some(id) = start
            .get("projectId")
            .and_then(Json::as_js_string)
            .filter(|s| !s.0.is_empty())
        {
            let mut location = draft.required("location")?.clone();
            location.insert("projectId", Json::String(id.clone()));
            draft.insert("location", location);
        }
        draft.insert("threadStart", start);
        let contract = self.catalogs.checked_native_contract(
            s(&draft, "nativeVersion")?,
            Some(&Json::object([(
                "sourceHash",
                draft.required("catalogSourceHash")?.clone(),
            )])),
        )?;
        validate_native_plan_draft(&contract, &draft, NativePlanKind::TaskBoard)?;
        plan_semantics(&draft)?;
        let plan = save_native_plan(
            &contract,
            &draft,
            NativePlanKind::TaskBoard,
            self.store.root_object_id().as_ref(),
            &concat([operation_id.clone(), ":parameters".into()]),
        )?;
        let name = concat([
            "Allocated plan ".into(),
            s(&task.value, "taskKey")?.clone(),
            " revision ".into(),
            task.pin
                .required("revision")?
                .as_f64()
                .ok_or_else(IvyError::internal)?
                .to_string()
                .into(),
            " operation ".into(),
            operation_id.clone(),
        ]);
        let existing = self.store.named_plan(&name).await?;
        let intent = match existing {
            Some(d) => d.pin,
            None => {
                self.store
                    .write(
                        "task-board/native-plan",
                        &plan,
                        &mutation(operation_id, "plan")?,
                        &Json::object([(
                            "create",
                            Json::object([
                                (
                                    "parentId",
                                    self.store
                                        .root_object_id()
                                        .map(Json::String)
                                        .unwrap_or(Json::Null),
                                ),
                                ("name", Json::String(name)),
                            ]),
                        )]),
                    )
                    .await?
            }
        };
        let mut workspace = workspace;
        workspace.remove("nativeProjectId");
        Ok(Json::object([
            (
                "target",
                Json::object([
                    ("hostId", selected.node.required("hostId")?.clone()),
                    (
                        "serviceNodeId",
                        selected.node.required("serviceNodeId")?.clone(),
                    ),
                ]),
            ),
            ("workspace", workspace),
            ("intent", intent),
        ]))
    }
}
