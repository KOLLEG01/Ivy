//! Task checks over exact retained records and the admitted native connection.
//! Current schema/conversation policy and final runtime composition remain owned.
use crate::{
    allocation::plan_semantics,
    configuration::{
        check_configuration, configuration_view, effective_execution, execution_with_defaults,
    },
    coordination::{Dependencies, GraphError, blocker_satisfied, validate_blocker},
    engine_validation::check_aggregate,
    journal_store::Contracts,
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    native_lifecycle::LifecycleStore,
    native_plan::read_plan,
    native_recovery::fresh_context_recovery,
    operations::Store,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, hash::digest, json::canonical,
    limits::JSON_OBJECT_CONTENT_BYTES, whatwg_url::parse_url,
};
use ivy_sdk::{
    native_evidence::native_instant,
    project_inventory::host_project,
    rpc::{RequestOptions, RpcClient},
    tools::ToolClients,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[allow(async_fn_in_trait)]
pub trait CheckStore: LifecycleStore {
    fn client(&self) -> Arc<dyn RpcClient>;
    fn local_artifact(&self, pin: &Json) -> IvyResult<Option<Json>>;
    async fn check_configuration(
        &self,
        value: &Json,
        service: Option<&JsString>,
        allow_unavailable: bool,
    ) -> IvyResult<()>;
}
impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    Q: LifecyclePages,
> CheckStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    fn client(&self) -> Arc<dyn RpcClient> {
        self.journal.client.clone()
    }
    fn local_artifact(&self, pin: &Json) -> IvyResult<Option<Json>> {
        Ok(self.journal.journal.artifact(pin)?.map(|a| {
            Json::object([
                ("contentHash", a.content_hash.into()),
                ("mediaType", a.media_type.into()),
            ])
        }))
    }
    async fn check_configuration(
        &self,
        value: &Json,
        service: Option<&JsString>,
        allow: bool,
    ) -> IvyResult<()> {
        check_configuration(self.journal, value, service, allow).await
    }
}
/// Source-compatible checks for explicit owner composition. This is not a
/// default current policy implementation for creation, handoff or scheduling.
pub struct TaskChecks<'a, S> {
    pub store: &'a S,
    pub catalogs: &'a CheckedNativeContracts,
    pub tools: &'a ToolClients,
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn arr(v: &Json) -> IvyResult<&[Json]> {
    v.as_array().ok_or_else(IvyError::internal)
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
fn eq(v: &Json, k: &str, text: &str) -> bool {
    v.get(k) == Some(&text.into())
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(a: Option<&Json>, b: Option<&Json>) -> IvyResult<bool> {
    Ok(match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            canonical(a, JSON_OBJECT_CONTENT_BYTES)? == canonical(b, JSON_OBJECT_CONTENT_BYTES)?
        }
        _ => false,
    })
}
pub fn date(value: Option<&Json>) -> IvyResult<()> {
    if value == Some(&Json::Null) {
        return Ok(());
    }
    require(
        value
            .and_then(Json::as_js_string)
            .is_some_and(|v| native_instant(v, None).is_ok()),
        "task_board_invalid_date",
        "Workflow dates must be real, canonical UTC timestamps.",
    )
}
fn normalized(v: &JsString) -> bool {
    !v.0.is_empty()
        && v.trim_start() == *v
        && JsString(v.0.iter().rev().copied().collect())
            .trim_start()
            .0
            .len()
            == v.0.len()
}
fn graph_error(error: GraphError<IvyError>) -> IvyError {
    match error {
        GraphError::Read(e) => e,
        e => {
            let (code, message) = e.policy_error().unwrap();
            IvyError::new(code, message)
        }
    }
}

impl<S: CheckStore> TaskChecks<'_, S> {
    pub async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()> {
        check_aggregate(self.store, task, id).await
    }
    pub async fn artifact(&self, artifact: &Json) -> IvyResult<()> {
        if let Some(local) = self.store.local_artifact(f(artifact, "object"))? {
            return require(
                f(&local, "contentHash") == f(artifact, "contentHash")
                    && f(&local, "mediaType") == f(artifact, "mediaType"),
                "task_board_evidence_mismatch",
                "A local workflow artifact must match its exact revision, hash and media type.",
            );
        }
        let value = self
            .store
            .client()
            .request(
                "objects.read",
                f(artifact, "object").clone(),
                RequestOptions::default(),
            )
            .await?;
        let content = value.required("content")?;
        let bytes = if eq(content, "encoding", "json") {
            canonical(content.required("value")?, JSON_OBJECT_CONTENT_BYTES)?.into_bytes()
        } else if eq(content, "encoding", "base64") {
            // Object content is admitted canonical base64 by Objects.decode.
            STANDARD
                .decode(s(content, "value")?.to_utf8()?)
                .map_err(|_| IvyError::internal())?
        } else {
            String::from_utf16_lossy(&s(content, "value")?.0).into_bytes()
        };
        require(
            !truthy(f(f(&value, "object"), "effectivelyArchived"))
                && f(f(&value, "revision"), "revision") == f(f(artifact, "object"), "revision")
                && f(f(&value, "revision"), "contentHash") == f(artifact, "contentHash")
                && f(f(&value, "revision"), "mediaType") == f(artifact, "mediaType"),
            "task_board_evidence_mismatch",
            "An artifact must pin an available exact revision, hash and media type.",
        )?;
        require(
            f(f(&value, "revision"), "byteLength").as_f64() == Some(bytes.len() as f64)
                && f(artifact, "contentHash").as_js_string() == Some(&digest(&bytes).into()),
            "task_board_evidence_mismatch",
            "The returned artifact bytes must match the retained hash and length.",
        )
    }
    pub async fn content(&self, content: &Json) -> IvyResult<()> {
        for artifact in arr(f(content, "artifacts"))?.iter().chain(
            arr(f(content, "checks"))?
                .iter()
                .map(|c| arr(f(c, "evidence")))
                .collect::<IvyResult<Vec<_>>>()?
                .into_iter()
                .flatten(),
        ) {
            self.artifact(artifact).await?;
        }
        for reference in arr(f(content, "codeReferences"))? {
            let url = parse_url(s(reference, "url")?).map_err(|_| {
                IvyError::new(
                    "task_board_invalid_reference",
                    "A code reference URL must have valid HTTP(S) syntax.",
                )
            })?;
            require(
                ["http:", "https:"].contains(&url.protocol.as_str())
                    && !url.hostname.is_empty()
                    && url.username.is_empty()
                    && url.password.is_empty(),
                "task_board_invalid_reference",
                "Code references must be HTTP(S) URLs without embedded credentials.",
            )?;
        }
        Ok(())
    }
    pub async fn dependencies(
        &self,
        fields: &Json,
        own: Option<&JsString>,
        satisfied: bool,
    ) -> IvyResult<()> {
        let mut done = HashSet::new();
        let mut visiting = HashSet::new();
        let mut tasks = HashMap::new();
        let mut pending = arr(f(fields, "dependencies"))?
            .iter()
            .map(|id| {
                Ok((
                    id.as_js_string().ok_or_else(IvyError::internal)?.clone(),
                    false,
                ))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        if satisfied {
            for id in arr(f(fields, "dependencies"))? {
                let task = self
                    .store
                    .lifecycle_read("task-board/task", id, true, true)
                    .await?
                    .document;
                tasks.insert(
                    id.as_js_string().ok_or_else(IvyError::internal)?.clone(),
                    task.clone(),
                );
                require(
                    eq(&task.value, "workflowState", "done")
                        && !truthy(f(&task.value, "publication")),
                    "task_board_dependency_waiting",
                    "Every direct dependency must have an accepted result before starting.",
                )?;
            }
        }
        while let Some((id, exit)) = pending.pop() {
            if exit {
                visiting.remove(&id);
                done.insert(id);
                continue;
            }
            require(
                Some(&id) != own && !visiting.contains(&id),
                "task_board_dependency_cycle",
                "The dependency graph would contain a cycle.",
            )?;
            if done.contains(&id) {
                continue;
            }
            require(
                done.len() + visiting.len() < 10000,
                "task_board_dependency_limit",
                "Dependency verification exceeds the 10,000-task complete traversal limit.",
            )?;
            let task = if let Some(task) = tasks.get(&id) {
                task.clone()
            } else {
                let task = self
                    .store
                    .lifecycle_read("task-board/task", &Json::String(id.clone()), true, true)
                    .await?
                    .document;
                tasks.insert(id.clone(), task.clone());
                task
            };
            require(
                !truthy(f(&task.value, "publication")),
                "task_board_publication_pending",
                "A dependency still has an unfinished publication.",
            )?;
            visiting.insert(id.clone());
            pending.push((id, true));
            for dependency in arr(f(f(&task.value, "fields"), "dependencies"))? {
                pending.push((
                    dependency
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?
                        .clone(),
                    false,
                ));
            }
            if truthy(f(&task.value, "blocker")) {
                pending.push((s(f(&task.value, "blocker"), "taskId")?.clone(), false));
            }
        }
        Ok(())
    }
    pub async fn fields(&self, fields: &Json, own: Option<&JsString>) -> IvyResult<()> {
        date(fields.get("nextReviewAt"))?;
        date(fields.get("dueAt"))?;
        self.dependencies(fields, own, false).await?;
        require(
            f(fields, "category").is_null()
                || f(fields, "category").as_js_string().is_some_and(normalized),
            "invalid_arguments",
            "Task category must be normalized nonempty text.",
        )?;
        let workspace = f(fields, "workspaceRequirement");
        if eq(workspace, "kind", "repository_path") {
            let url = parse_url(s(workspace, "repositoryUrl")?)?;
            require(
                url.username.is_empty() && url.password.is_empty(),
                "invalid_arguments",
                "Repository URLs cannot contain embedded credentials.",
            )?;
        }
        if eq(workspace, "kind", "existing_project") {
            let requirement = if !f(fields, "executionRequirement").is_null() {
                f(fields, "executionRequirement").clone()
            } else {
                f(
                    f(
                        f(&configuration_view(self.store).await?, "configuration"),
                        "defaults",
                    ),
                    "executionRequirement",
                )
                .clone()
            };
            let host = if eq(&requirement, "kind", "host") {
                Some(s(&requirement, "hostId")?)
            } else {
                None
            };
            if host_project(
                self.store.client().as_ref(),
                host,
                s(workspace, "projectId")?,
                RequestOptions::default(),
            )
            .await?
            .is_none()
            {
                let mut message = JsString::from("The selected project is not registered");
                if let Some(host) = host.filter(|v| !v.0.is_empty()) {
                    message.0.extend(" on ".encode_utf16());
                    message.0.extend(&host.0);
                }
                message
                    .0
                    .extend(". Choose a project from the execution host's catalog.".encode_utf16());
                return Err(IvyError::new_js(
                    "task_board_project_unavailable".into(),
                    message,
                ));
            }
        }
        Ok(())
    }
    pub async fn initial_state(
        &self,
        request: &Json,
        fields: &Json,
        actor: &Json,
        at: &JsString,
    ) -> IvyResult<Json> {
        let state = if f(request, "workflowState").is_null() {
            Json::from("todo")
        } else {
            f(request, "workflowState").clone()
        };
        let is = |v: &str| state == Json::from(v);
        require(
            !truthy(f(request, "waiting")) || is("waiting"),
            "invalid_arguments",
            "Waiting details are only valid for an initial waiting state.",
        )?;
        require(
            !is("in_progress") || eq(fields, "control", "user"),
            "task_board_user_controlled",
            "Agent work needs a real scheduler claim; create it in Todo to request execution.",
        )?;
        require(
            !is("review") || eq(fields, "control", "user"),
            "task_board_native_evidence_required",
            "Agent review requires its own observed native completion; only manual work can be created in Review.",
        )?;
        require(
            !truthy(f(request, "initialResult")) || is("review") || is("done"),
            "invalid_arguments",
            "Initial final material is only valid in Review or Done.",
        )?;
        if truthy(f(request, "initialResult")) {
            require(
                eq(fields, "control", "user") && eq(actor, "source", "user"),
                "task_board_native_evidence_required",
                "Only user-controlled manual work can supply an initial final Result.",
            )?;
            self.content(f(request, "initialResult")).await?;
        }
        require(
            !is("review") || truthy(f(request, "initialResult")),
            "task_board_result_required",
            "Manual Review needs an initial final Result.",
        )?;
        if !is("waiting") {
            return Ok(Json::object([
                ("workflowState", state),
                ("waiting", Json::Null),
            ]));
        }
        let waiting = f(request, "waiting");
        require(
            truthy(waiting),
            "invalid_arguments",
            "An initial waiting state needs a reason and details.",
        )?;
        require(
            !eq(waiting, "reason", "dependency")
                || truthy(f(waiting, "blocker"))
                || !arr(f(fields, "dependencies"))?.is_empty(),
            "invalid_arguments",
            "A dependency wait needs a linked blocker or task dependency.",
        )?;
        require(
            !eq(waiting, "reason", "capability")
                || f(fields, "requiredCapabilities")
                    .as_array()
                    .is_some_and(|v| !v.is_empty()),
            "invalid_arguments",
            "A capability wait needs required capabilities.",
        )?;
        let mut deferral = Json::object([
            ("reason", f(waiting, "reason").clone()),
            ("detail", f(waiting, "detail").clone()),
            ("nextReviewAt", f(fields, "nextReviewAt").clone()),
        ]);
        if truthy(f(waiting, "blocker")) {
            deferral.insert("blocker", f(waiting, "blocker").clone());
        }
        self.deferral(&deferral, None, at).await?;
        let mut result = Json::object([
            ("workflowState", state),
            (
                "waiting",
                Json::object([
                    ("reason", f(waiting, "reason").clone()),
                    ("detail", f(waiting, "detail").clone()),
                    ("since", Json::String(at.clone())),
                ]),
            ),
        ]);
        if truthy(f(waiting, "blocker")) {
            result.insert("blocker", f(waiting, "blocker").clone());
        }
        Ok(result)
    }
    pub async fn deferral(
        &self,
        request: &Json,
        own: Option<&JsString>,
        at: &JsString,
    ) -> IvyResult<()> {
        if truthy(f(request, "blocker")) {
            require(
                eq(request, "reason", "dependency") || eq(request, "reason", "workspace"),
                "invalid_arguments",
                "Task blockers require a dependency or workspace reason.",
            )?;
            validate_blocker(
                s(f(request, "blocker"), "taskId")?.clone(),
                own,
                |id| async move {
                    let task = self
                        .store
                        .lifecycle_read("task-board/task", &Json::String(id), true, true)
                        .await?
                        .document
                        .value;
                    Ok::<_, IvyError>(Dependencies {
                        dependencies: arr(f(f(&task, "fields"), "dependencies"))?
                            .iter()
                            .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
                            .collect::<IvyResult<_>>()?,
                        blocker: if truthy(f(&task, "blocker")) {
                            Some(s(f(&task, "blocker"), "taskId")?.clone())
                        } else {
                            None
                        },
                    })
                },
            )
            .await
            .map_err(graph_error)?;
        }
        date(request.get("nextReviewAt"))?;
        require(
            !eq(request, "reason", "time")
                || f(request, "nextReviewAt")
                    .as_js_string()
                    .is_some_and(|v| !v.0.is_empty() && v.0.as_slice() > at.0.as_slice()),
            "task_board_invalid_date",
            "A time deferral needs a future next-review date.",
        )
    }
    pub async fn check_blocker(&self, blocker: Option<&Json>) -> IvyResult<()> {
        let Some(blocker) = blocker.filter(|v| truthy(v)) else {
            return Ok(());
        };
        let other = self
            .store
            .lifecycle_read("task-board/task", f(blocker, "taskId"), true, true)
            .await?
            .document
            .value;
        let phase = if truthy(f(&other, "lastRun")) {
            Some(
                self.store
                    .lifecycle_read("task-board/run", f(&other, "lastRun"), true, true)
                    .await?
                    .document
                    .value,
            )
        } else {
            None
        };
        if !blocker_satisfied(
            &s(blocker, "until")?.to_utf8()?,
            &s(&other, "workflowState")?.to_utf8()?,
            truthy(f(&other, "claim")),
            truthy(f(&other, "publication")),
            phase
                .as_ref()
                .map(|v| s(v, "phase").and_then(JsString::to_utf8))
                .transpose()?
                .as_deref(),
        ) {
            let mut message = JsString::from("Waiting for Task ");
            message.0.extend(&s(&other, "taskKey")?.0);
            message.0.extend(" to be ".encode_utf16());
            message.0.extend(&s(blocker, "until")?.0);
            message.0.push(46);
            return Err(IvyError::new_js(
                "task_board_dependency_waiting".into(),
                message,
            ));
        }
        Ok(())
    }
    pub async fn execution_options(
        &self,
        fields: &Json,
        task: Option<&Json>,
        allow_unavailable: bool,
    ) -> IvyResult<()> {
        let options = f(fields, "nativeOptions");
        if !eq(fields, "control", "agent")
            && !truthy(f(options, "model"))
            && !truthy(f(options, "reasoningEffort"))
            && (!truthy(f(options, "serviceTier")) || eq(options, "serviceTier", "standard"))
        {
            return Ok(());
        }
        let defaults = if let Some(task) = task {
            let mut task = task.clone();
            task.insert("fields", fields.clone());
            effective_execution(self.store, &task).await?
        } else {
            execution_with_defaults(
                fields,
                f(
                    f(&configuration_view(self.store).await?, "configuration"),
                    "defaults",
                ),
            )
        };
        let service = if let Some(task) = task {
            if !f(f(task, "primaryResourceRef"), "serviceNodeId").is_null() {
                f(f(task, "primaryResourceRef"), "serviceNodeId")
                    .as_js_string()
                    .cloned()
            } else {
                fresh_context_recovery(self.store, task)
                    .await?
                    .as_ref()
                    .and_then(|r| f(f(r, "target"), "serviceNodeId").as_js_string().cloned())
            }
        } else {
            None
        };
        self.store
            .check_configuration(
                &Json::object([("schemaVersion", Json::Number(1.0)), ("defaults", defaults)]),
                service.as_ref(),
                allow_unavailable,
            )
            .await
    }
    pub async fn execution(
        &self,
        task: &Json,
        intent: &Json,
        id: &JsString,
        now: &JsString,
        target: Option<&Json>,
        workspace: Option<&Json>,
        coordination: bool,
    ) -> IvyResult<Json> {
        require(
            target.is_some_and(truthy)
                && workspace.is_some_and(truthy)
                && target.and_then(|t| t.get("hostId")) == workspace.and_then(|w| w.get("hostId"))
                && target.and_then(|t| t.get("serviceNodeId"))
                    == workspace.and_then(|w| w.get("serviceNodeId")),
            "task_board_execution_plan_required",
            "Start must retain one resolved native owner and workspace.",
        )?;
        let target = target.unwrap();
        let workspace = workspace.unwrap();
        if let Some(recovery) = fresh_context_recovery(self.store, task).await? {
            require(
                same(Some(target), recovery.get("target"))?
                    && f(workspace, "canonicalCwd") == f(f(&recovery, "workspace"), "canonicalCwd")
                    && f(workspace, "intendedPath") == f(f(&recovery, "workspace"), "intendedPath"),
                "task_board_workspace_mismatch",
                "Fresh recovery must retain the original owner and workspace.",
            )?;
        }
        if !coordination {
            require(
                !truthy(f(f(task, "fields"), "nextReviewAt"))
                    || s(f(task, "fields"), "nextReviewAt")?.0.as_slice() <= now.0.as_slice(),
                "task_board_time_waiting",
                "The task is deferred until its next review time.",
            )?;
            self.dependencies(f(task, "fields"), Some(id), true).await?;
            self.check_blocker(task.get("blocker")).await?;
        }
        let plan = read_plan(self.store, self.catalogs, intent).await?;
        plan_semantics(&plan)?;
        if truthy(f(&plan, "location")) {
            require(
                f(f(&plan, "location"), "serviceNodeId") == f(target, "serviceNodeId")
                    && f(f(&plan, "location"), "hostId") == f(target, "hostId")
                    && f(f(&plan, "location"), "cwd") == f(workspace, "canonicalCwd"),
                "task_board_project_mismatch",
                "The saved project location belongs to another target host.",
            )?;
        }
        let node = self
            .store
            .client()
            .request(
                "serviceNodes.get",
                Json::object([("serviceNodeId", f(target, "serviceNodeId").clone())]),
                RequestOptions::default(),
            )
            .await?;
        require(
            f(&node, "hostId") == f(target, "hostId")
                && eq(&node, "serviceName", "agent-manager")
                && truthy(f(&node, "ready"))
                && truthy(f(&node, "connected"))
                && f(&node, "nativeVersion") == f(&plan, "nativeVersion"),
            "task_board_target_unavailable",
            "The selected native owner is not ready at the exact plan version.",
        )?;
        for (method, key) in [
            ("thread/start", "threadStart"),
            ("thread/resume", "threadResume"),
            ("turn/start", "turnStart"),
            ("turn/interrupt", "turnInterrupt"),
        ] {
            let name = JsString::from(format!("codex.{method}"));
            let tool = self
                .tools
                .native_service_tools(s(&node, "serviceNodeId")?.clone())?
                .binding(&name, None)
                .await?;
            let tool = tool.value();
            require(
                f(tool, "definitionHash") == f(f(&plan, "definitions"), key)
                    && f(f(tool, "definition"), "interfaceVersion") == f(&plan, "nativeVersion")
                    && eq(f(tool, "definition"), "nativeMethod", method),
                "tool_definition_changed",
                "The selected native definition differs from the saved plan. Save a new explicit plan before starting.",
            )?;
        }
        require(
            truthy(f(&plan, "location"))
                && f(f(&plan, "location"), "cwd") == f(workspace, "canonicalCwd"),
            "task_board_project_mismatch",
            "The plan must start in the resolved working directory.",
        )?;
        if truthy(f(task, "primaryResourceRef")) {
            require(
                f(f(task, "primaryResourceRef"), "serviceNodeId") == f(target, "serviceNodeId")
                    && eq(f(task, "primaryResourceRef"), "namespace", "codex")
                    && eq(f(task, "primaryResourceRef"), "kind", "thread"),
                "task_board_primary_mismatch",
                "The primary context must remain on its original selected native owner.",
            )?;
        }
        Ok(plan)
    }
}
