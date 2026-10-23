//! Task checks over exact retained records and the admitted native connection.
//! Current schema/conversation policy and final runtime composition remain owned.
use crate::{
    allocation::plan_semantics,
    configuration::{
        check_configuration, configuration_view, effective_execution, execution_with_defaults,
    },
    engine_validation::check_aggregate,
    journal_store::Contracts,
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    native_lifecycle::LifecycleStore,
    native_plan::read_plan,
    native_recovery::fresh_context_recovery,
    wake::{WakeStore, should_wake, validate_dependencies},
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
use std::sync::Arc;

#[allow(async_fn_in_trait)]
pub trait CheckStore: LifecycleStore + WakeStore {
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
/// Current field, wake and execution admission on the retained store owners.
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
fn eq(v: &Json, k: &str, text: &str) -> bool {
    v.get(k) == Some(&text.into())
}

async fn initial_manual_state(
    request: &Json,
    fields: &Json,
    actor: &Json,
    content: impl std::future::Future<Output = IvyResult<()>>,
) -> IvyResult<Json> {
    let state = request
        .get("workflowState")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| "todo".into());
    let is = |value: &str| state == Json::from(value);
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
        f(request, "initialResult").is_null() || is("review") || is("done"),
        "invalid_arguments",
        "Initial final material is only valid in Review or Done.",
    )?;
    if !f(request, "initialResult").is_null() {
        require(
            eq(fields, "control", "user") && eq(actor, "source", "user"),
            "task_board_native_evidence_required",
            "Only user-controlled manual work can supply an initial final Result.",
        )?;
        content.await?;
    }
    require(
        !is("review") || !f(request, "initialResult").is_null(),
        "task_board_result_required",
        "Manual Review needs an initial final Result.",
    )?;
    Ok(state)
}

/// Current creation records the initial watch snapshot without gating work on
/// foreign workflow state or inventing a next-step graph.
pub(crate) async fn current_initial_state<E: From<IvyError>, S: crate::wake::WakeStore<E>>(
    store: &S,
    request: &Json,
    fields: &Json,
    actor: &Json,
    content: impl std::future::Future<Output = IvyResult<()>>,
) -> Result<Json, E> {
    let state = initial_manual_state(request, fields, actor, content).await?;
    let wake = request.get("wakeAt").cloned().unwrap_or(Json::Null);
    date(Some(&wake))?;
    let ids = request
        .get("dependencies")
        .cloned()
        .unwrap_or_else(|| Json::Array(vec![]));
    crate::wake::validate_dependencies(store, None, arr(&ids)?).await?;
    let watched = Json::object([("dependencies", ids.clone())]);
    let observed = crate::wake::dependency_snapshot(store, &watched).await?;
    Ok(Json::object([
        ("workflowState", state),
        ("wakeAt", wake),
        ("dependencies", ids),
        ("observedDependencies", observed),
        ("runtimeWait", Json::Null),
    ]))
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
    !v.0.is_empty() && v.trim() == *v
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
            f(f(&value, "object"), "effectivelyArchived").as_bool() != Some(true)
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
        self.content_references(content)
    }
    pub(crate) async fn current_content(&self, content: &Json) -> IvyResult<()> {
        for artifact in arr(f(content, "artifacts"))? {
            self.artifact(artifact).await?;
        }
        self.content_references(content)
    }
    fn content_references(&self, content: &Json) -> IvyResult<()> {
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
    pub async fn fields(&self, fields: &Json, _own: Option<&JsString>) -> IvyResult<()> {
        self.current_fields(fields).await
    }
    pub(crate) async fn current_fields(&self, fields: &Json) -> IvyResult<()> {
        date(fields.get("dueAt"))?;
        self.field_workspace(fields).await
    }
    async fn field_workspace(&self, fields: &Json) -> IvyResult<()> {
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
        _at: &JsString,
    ) -> IvyResult<Json> {
        current_initial_state(
            self.store,
            request,
            fields,
            actor,
            self.current_content(f(request, "initialResult")),
        )
        .await
    }
    pub async fn deferral(
        &self,
        request: &Json,
        own: Option<&JsString>,
        _at: &JsString,
    ) -> IvyResult<()> {
        date(Some(f(request, "wakeAt")))?;
        if let Some(ids) = request.get("dependencies").filter(|value| !value.is_null()) {
            validate_dependencies(self.store, own, arr(ids)?).await?;
        }
        date(Some(f(f(request, "runtimeWait"), "retryAt")))?;
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
            && f(options, "model").is_null()
            && f(options, "reasoningEffort").is_null()
            && (f(options, "serviceTier").is_null() || eq(options, "serviceTier", "standard"))
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
        _id: &JsString,
        now: &JsString,
        target: Option<&Json>,
        workspace: Option<&Json>,
    ) -> IvyResult<Json> {
        require(
            target.is_some_and(|value| !value.is_null())
                && workspace.is_some_and(|value| !value.is_null())
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
        require(
            should_wake(self.store, task, now).await?,
            "task_board_input_required",
            "No work or new input is pending.",
        )?;
        let plan = read_plan(self.store, self.catalogs, intent).await?;
        plan_semantics(&plan)?;
        if !f(&plan, "location").is_null() {
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
                && f(&node, "ready").as_bool() == Some(true)
                && f(&node, "connected").as_bool() == Some(true)
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
            !f(&plan, "location").is_null()
                && f(f(&plan, "location"), "cwd") == f(workspace, "canonicalCwd"),
            "task_board_project_mismatch",
            "The plan must start in the resolved working directory.",
        )?;
        if !f(task, "primaryResourceRef").is_null() {
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

#[cfg(test)]
#[path = "../../../../tests/fixtures/task-board-current-initial-state.rs"]
mod current_initial_state_tests;
