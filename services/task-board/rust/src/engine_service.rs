//! Public tool dispatch on the journal owner. Contract and runtime policies are
//! supplied by that owner; neither a schema nor a conversation policy is inferred.
use crate::{
    category_projection::{CategoryLocale, CategoryProjection, CategoryStore},
    engine_admission::{AdmissionExecution, EngineAdmission},
    engine_invocations::EngineInvocations,
    engine_read::{
        EngineReadRuntime, EngineReads, ReadContext,
        read_failure::{ReadFailure, provider_read_error},
    },
    journal_store::Contracts,
    native_lifecycle::LifecycleStore,
    reconciler::ReconcileExecutor,
    service_bridge::LocalHandler,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::service::InvocationContext;

pub trait ServiceRuntime {
    fn recovered(&self) -> bool;
    fn observed_at(&self) -> JsString;
    fn task_changed(&self);
    fn kick(&self);
}
#[allow(async_fn_in_trait)]
pub trait ServiceCategories {
    async fn categories(&self) -> IvyResult<Json>;
}
pub type LocalReadFuture<'a, E> =
    futures_util::future::LocalBoxFuture<'a, Result<Json, ReadFailure<E>>>;
/// Bind the complete owner-local read (including owner checks) at its provider
/// catch. Other tools keep the existing engine handler and invocation drain.
pub struct ReadService<'a, H, E> {
    pub read: &'a dyn Fn(Json, InvocationContext) -> LocalReadFuture<'a, E>,
    pub other: &'a H,
}
impl<H: LocalHandler, E> LocalHandler for ReadService<'_, H, E> {
    async fn handle(&self, tool: &str, args: Json, context: InvocationContext) -> IvyResult<Json> {
        if tool == "read" {
            (self.read)(args, context)
                .await
                .map_err(provider_read_error)
        } else {
            self.other.handle(tool, args, context).await
        }
    }
}
impl<'a, S: CategoryStore + 'a, L: CategoryLocale + 'a, X: ReconcileExecutor<'a>> ServiceCategories
    for CategoryProjection<'a, '_, S, L, X>
{
    async fn categories(&self) -> IvyResult<Json> {
        CategoryProjection::categories(self).await
    }
}
pub struct EngineService<'a, S, C, P, E, R, T, K> {
    pub admission: &'a EngineAdmission<'a, S, C, P, E>,
    pub reads: EngineReads<'a, S, C, R>,
    pub runtime: &'a T,
    pub categories: &'a K,
}
/// Also retain the complete call in the engine's existing invocation drain when
/// this facade is shared with internal callers or generation reconciliation.
pub struct RetainedService<'a, 'i, 'x, H, X> {
    pub handler: &'a H,
    pub invocations: &'i EngineInvocations<'a, 'x, X>,
}
impl<'a, H: LocalHandler + 'a, X: ReconcileExecutor<'a>> LocalHandler
    for RetainedService<'a, '_, '_, H, X>
{
    async fn handle(&self, tool: &str, args: Json, context: InvocationContext) -> IvyResult<Json> {
        let handler = self.handler;
        let tool = tool.to_owned();
        self.invocations
            .run(async move { handler.handle(&tool, args, context).await })?
            .await
    }
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
impl<
    S: LifecycleStore,
    C: Fn() -> String,
    P: Contracts,
    E: AdmissionExecution,
    R: EngineReadRuntime,
    T: ServiceRuntime,
    K: ServiceCategories,
> LocalHandler for EngineService<'_, S, C, P, E, R, T, K>
{
    async fn handle(&self, tool: &str, args: Json, context: InvocationContext) -> IvyResult<Json> {
        let owner = self.admission.owner();
        require(
            context.generation as f64 == owner.generation,
            "service_not_ready",
            "TaskBoard has no reconciled current connection generation.",
        )?;
        // AbortSignal.throwIfAborted rejects with a non-Ivy exception. Its public
        // conversion is IvyError.internal(), including the unknown outcome.
        let check_cancelled = || {
            if context.signal.is_cancelled() {
                Err(IvyError::internal())
            } else {
                Ok(())
            }
        };
        let empty = JsString::default();
        let read_context = ReadContext {
            caller_principal_id: &context.caller_principal_id,
            operation_id: context.operation_id.as_ref().unwrap_or(&empty),
            generation: context.generation as f64,
            check_cancelled: &check_cancelled,
        };
        match tool {
            "workspace" => {
                self.admission.verify_owner().await?;
                check_cancelled()?;
                let settings = self.admission.settings();
                Ok(Json::object([
                    ("serviceNodeId", Json::String(owner.service_node_id.clone())),
                    ("generation", Json::Number(owner.generation)),
                    ("principalId", f(settings, "principalId").clone()),
                    ("rootObjectId", f(settings, "rootObjectId").clone()),
                    (
                        "callerPrincipalId",
                        Json::String(context.caller_principal_id),
                    ),
                    ("role", "user".into()),
                    ("recovered", self.runtime.recovered().into()),
                    ("observedAt", Json::String(self.runtime.observed_at())),
                    ("scheduler", f(settings, "scheduler").clone()),
                    ("phoneTarget", f(settings, "phoneTarget").clone()),
                ]))
            }
            "operation" | "operationReceipt" => {
                let id = args
                    .required("operationId")?
                    .as_js_string()
                    .ok_or_else(|| {
                        IvyError::new("invalid_arguments", "operationId must be a string.")
                    })?;
                self.reads.operation(id, &read_context, true).await
            }
            "configuration" => self.admission.configuration().await,
            "read" => {
                self.admission.verify_owner().await?;
                self.reads.task(&args, &context.caller_principal_id).await
            }
            "list" => {
                self.admission.verify_owner().await?;
                self.reads.list(&args).await
            }
            "markCommentsRead" => self.reads.mark_comments_read(&args, &read_context).await,
            "categories" => {
                self.admission.verify_owner().await?;
                self.categories.categories().await
            }
            "archive" => {
                let outcome = self.admission.archive(&args, &read_context).await?;
                self.runtime.task_changed();
                Ok(outcome)
            }
            _ => {
                require(
                    self.runtime.recovered()
                        || ![Json::from("start"), Json::from("continue")]
                            .contains(f(&args, "action")),
                    "task_board_recovery_pending",
                    "Existing Operations and Runs are being reconciled before accepting fresh execution claims.",
                )?;
                let outcome = self.admission.invoke(&args, &read_context).await?;
                self.runtime.task_changed();
                self.runtime.kick();
                Ok(outcome)
            }
        }
    }
}
