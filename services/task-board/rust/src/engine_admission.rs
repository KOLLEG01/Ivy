//! Public action admission before retained execution. The owner supplies its
//! current Settings contract and exact native catalogs on the same connection.
use crate::{
    allocation::plan_semantics,
    configuration::configuration_view,
    engine_execution::{ExecutionProgram, ExecutionQueue},
    engine_read::ReadContext,
    inspection_journal::verify_owner,
    journal_store::Contracts,
    native_plan::catalog,
    operations::{Document, Operations, Store},
    publication_coordination::{CoordinationStore, PublicationCoordination},
    reconciler::ReconcileExecutor,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, hash::hash_json,
};
use ivy_sdk::{
    native_plan::{NativePlanKind, save_native_plan, validate_native_plan_draft},
    rpc::{RequestOptions, RpcClient},
    tools::{BoundToolClient, InterfaceRequirement, ToolCallOptions},
};
use std::sync::Arc;

pub struct EngineOwner {
    pub service_node_id: JsString,
    pub generation: f64,
}
pub struct AdmissionRuntime<'a, P> {
    pub client: Arc<dyn RpcClient>,
    pub contracts: &'a P,
    pub catalogs: &'a CheckedNativeContracts,
}
#[allow(async_fn_in_trait)]
pub trait AdmissionExecution {
    async fn execute(&self, operation: Document) -> IvyResult<Json>;
}
impl<'a, P: ExecutionProgram + 'a, X: ReconcileExecutor<'a>> AdmissionExecution
    for ExecutionQueue<'a, '_, P, X>
{
    async fn execute(&self, operation: Document) -> IvyResult<Json> {
        ExecutionQueue::execute(self, operation)?.await
    }
}
pub struct EngineAdmission<'a, S, C, P, E> {
    operations: &'a Operations<S, C>,
    settings: &'a Json,
    owner: &'a EngineOwner,
    runtime: &'a AdmissionRuntime<'a, P>,
    execution: &'a E,
}
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn s<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn eq(value: &Json, key: &str, wanted: &str) -> bool {
    value.get(key) == Some(&Json::from(wanted))
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
impl<'a, S: Store, C: Fn() -> String, P: Contracts, E: AdmissionExecution>
    EngineAdmission<'a, S, C, P, E>
{
    pub fn new(
        operations: &'a Operations<S, C>,
        settings: &'a Json,
        owner: &'a EngineOwner,
        runtime: &'a AdmissionRuntime<'a, P>,
        execution: &'a E,
    ) -> IvyResult<Self> {
        runtime
            .contracts
            .validate_definition("Settings", settings)?;
        require(
            f(settings, "rootObjectId")
                == &operations
                    .store
                    .root_object_id()
                    .map(Json::String)
                    .unwrap_or(Json::Null),
            "task_board_scope_mismatch",
            "The engine and settings must select the same workflow root.",
        )?;
        Ok(Self {
            operations,
            settings,
            owner,
            runtime,
            execution,
        })
    }
    pub async fn verify_owner(&self) -> IvyResult<()> {
        verify_owner(
            self.runtime.client.as_ref(),
            s(self.settings, "principalId")?,
            &self.owner.service_node_id,
        )
        .await
    }
    fn actor(&self, principal_id: &JsString, source: &str) -> Json {
        Json::object([
            ("principalId", Json::String(principal_id.clone())),
            (
                "serviceNodeId",
                Json::String(self.owner.service_node_id.clone()),
            ),
            ("generation", Json::Number(self.owner.generation)),
            ("source", source.into()),
        ])
    }
    pub async fn invoke(&self, input: &Json, context: &ReadContext<'_>) -> IvyResult<Json> {
        let mut request = input.clone();
        (context.check_cancelled)()?;
        self.verify_owner().await?;
        require(
            context.generation == self.owner.generation
                && f(&request, "operationId").as_js_string() == Some(context.operation_id),
            "task_board_identity_conflict",
            "Outer operation and live connection identities must match the TaskBoard request.",
        )?;
        let workspace = f(&request, "expectedWorkspace");
        if truthy(workspace) {
            require(
                f(workspace, "principalId") == f(self.settings, "principalId")
                    && f(workspace, "rootObjectId")
                        == &self
                            .operations
                            .store
                            .root_object_id()
                            .map(Json::String)
                            .unwrap_or(Json::Null)
                    && f(workspace, "callerPrincipalId").as_js_string()
                        == Some(context.caller_principal_id),
                "task_board_workspace_changed",
                "The original caller or workflow owner changed. This request was not admitted.",
            )?;
        }
        let worker = (eq(&request, "action", "comment") && eq(&request, "authorKind", "agent"))
            || (eq(&request, "action", "create") && truthy(f(&request, "parentTaskId")));
        let actor = self.actor(
            context.caller_principal_id,
            if worker { "worker" } else { "user" },
        );
        if eq(&request, "action", "savePlan") {
            let draft = request.required("plan")?;
            let contract = catalog(self.runtime.catalogs, draft)?;
            validate_native_plan_draft(&contract, draft, NativePlanKind::TaskBoard)?;
            plan_semantics(draft)?;
            let location = f(draft, "location");
            if truthy(location) {
                let node = self
                    .runtime
                    .client
                    .request(
                        "serviceNodes.get",
                        Json::object([("serviceNodeId", f(location, "serviceNodeId").clone())]),
                        RequestOptions::default(),
                    )
                    .await?;
                require(
                    f(&node, "hostId") == f(location, "hostId")
                        && eq(&node, "serviceName", "agent-manager"),
                    "task_board_target_mismatch",
                    "Plan location must identify its actual target AgentManager.",
                )?;
                let observed = BoundToolClient::new(
                    self.runtime.client.clone(),
                    s(location, "serviceNodeId")?.clone(),
                    &[InterfaceRequirement {
                        namespace: "agent".into(),
                        interface_version: "1.0.0".into(),
                    }],
                    None,
                )?
                .call(
                    &"agent.resolveProject".into(),
                    Json::object([
                        (
                            "selection",
                            Json::object([
                                ("kind", "existing".into()),
                                ("cwd", f(location, "cwd").clone()),
                            ]),
                        ),
                        ("expectedProjectId", f(location, "projectId").clone()),
                    ]),
                    None,
                    ToolCallOptions::default(),
                )
                .await?;
                require(
                    f(&observed, "cwd") == f(location, "cwd"),
                    "task_board_project_mismatch",
                    "The plan must retain the target-validated canonical working directory.",
                )?;
            }
            let plan = save_native_plan(
                &contract,
                draft,
                NativePlanKind::TaskBoard,
                self.operations.store.root_object_id().as_ref(),
                &hash_json(&Json::object([
                    (
                        "callerPrincipalId",
                        Json::String(context.caller_principal_id.clone()),
                    ),
                    ("operationId", f(&request, "operationId").clone()),
                ]))?
                .into(),
            )?;
            let draft_hash = hash_json(&request)?;
            request.insert("plan", plan);
            request.insert("draftHash", draft_hash.into());
        }
        self.execution
            .execute(self.operations.begin(&request, &actor).await?)
            .await
    }
    pub async fn configuration(&self) -> IvyResult<Json> {
        self.verify_owner().await?;
        configuration_view(&self.operations.store).await
    }
    pub async fn schedule(&self, request: &Json) -> IvyResult<Json> {
        self.verify_owner().await?;
        require(
            truthy(f(f(self.settings, "scheduler"), "enabled")),
            "task_board_scheduler_disabled",
            "Autonomous scheduling is disabled.",
        )?;
        self.begin_internal(request, "scheduler").await
    }
    pub async fn native_update(&self, request: &Json) -> IvyResult<Json> {
        self.verify_owner().await?;
        self.begin_internal(request, "native").await
    }
    pub async fn native_comment(&self, request: &Json) -> IvyResult<Json> {
        self.verify_owner().await?;
        self.begin_internal(request, "native").await
    }
    async fn begin_internal(&self, request: &Json, source: &str) -> IvyResult<Json> {
        let actor = self.actor(s(self.settings, "principalId")?, source);
        self.execution
            .execute(self.operations.begin(request, &actor).await?)
            .await
    }
    pub async fn recover_operation(&self, object_id: &JsString) -> IvyResult<Json> {
        self.verify_owner().await?;
        self.execution
            .execute(
                self.operations
                    .store
                    .read(
                        "task-board/operation",
                        &Json::String(object_id.clone()),
                        true,
                    )
                    .await?,
            )
            .await
    }
}
impl<S: CoordinationStore, C: Fn() -> String, P: Contracts, E: AdmissionExecution>
    EngineAdmission<'_, S, C, P, E>
{
    pub async fn recover_publication(&self) -> IvyResult<Json> {
        self.verify_owner().await?;
        let coordination = PublicationCoordination {
            store: &self.operations.store,
            principal_id: s(self.settings, "principalId")?,
            now: &self.operations.now,
        };
        let Some(pending) = coordination.pending()? else {
            return Ok(Json::Null);
        };
        let operation = self
            .operations
            .store
            .read("task-board/operation", f(&pending, "objectId"), true)
            .await?;
        if eq(&operation.value, "phase", "failed") {
            coordination
                .release(s(&operation.pin, "objectId")?, false)
                .await?;
            return Ok(Json::Null);
        }
        self.execution.execute(operation).await
    }
}
