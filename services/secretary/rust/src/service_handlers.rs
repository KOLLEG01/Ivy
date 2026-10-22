//! The sixteen registered Secretary callbacks. The service generation owns the
//! engine, supplies its connection cancellation check, and retains admitted work.
use crate::{engine::SecretaryEngine, follow_up::SecretaryFollowUp};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, checked_native_contract::CheckedNativeContracts,
    hash::hash_json,
};
use ivy_sdk::{
    clock::date_to_iso,
    service::{InvocationContext, ToolHandler},
    tools::ToolClients,
};
use std::{collections::HashMap, future::Future, sync::Arc};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicMethod {
    Capture,
    Assess,
    Attach,
    Checkpoint,
    Operation,
    OperationRead,
    Status,
    Binding,
    ListAssignments,
    GetAssignment,
    SaveConfiguration,
    SaveAssignment,
    CreateAssignment,
    UpdateAssignment,
    FollowUp,
    FollowUpRead,
}
impl PublicMethod {
    pub const ALL: [Self; 16] = [
        Self::Capture,
        Self::Assess,
        Self::Attach,
        Self::Checkpoint,
        Self::Operation,
        Self::OperationRead,
        Self::Status,
        Self::Binding,
        Self::ListAssignments,
        Self::GetAssignment,
        Self::SaveConfiguration,
        Self::SaveAssignment,
        Self::CreateAssignment,
        Self::UpdateAssignment,
        Self::FollowUp,
        Self::FollowUpRead,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Assess => "assess",
            Self::Attach => "attach",
            Self::Checkpoint => "checkpoint",
            Self::Operation => "operation",
            Self::OperationRead => "operationRead",
            Self::Status => "status",
            Self::Binding => "binding",
            Self::ListAssignments => "listAssignments",
            Self::GetAssignment => "getAssignment",
            Self::SaveConfiguration => "saveConfiguration",
            Self::SaveAssignment => "saveAssignment",
            Self::CreateAssignment => "createAssignment",
            Self::UpdateAssignment => "updateAssignment",
            Self::FollowUp => "followUp",
            Self::FollowUpRead => "followUpRead",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineMethod {
    Binding,
    ListAssignments,
    GetAssignment,
    SaveConfiguration,
    SaveAssignment,
    Operation,
    OperationRead,
    Status,
    Action,
    FollowUp,
    FollowUpRead,
}
impl EngineMethod {
    pub fn name(self) -> &'static str {
        match self {
            Self::Binding => "binding",
            Self::ListAssignments => "listAssignments",
            Self::GetAssignment => "getAssignment",
            Self::SaveConfiguration => "saveConfiguration",
            Self::SaveAssignment => "saveAssignment",
            Self::Operation => "operation",
            Self::OperationRead => "operationRead",
            Self::Status => "status",
            Self::Action => "action",
            Self::FollowUp => "followUp",
            Self::FollowUpRead => "followUpRead",
        }
    }
}
pub struct HandlerCall {
    pub method: EngineMethod,
    /// The concrete engine arguments, including the authenticated caller.
    pub arguments: Vec<IvyJson>,
    pub wake: bool,
}
impl HandlerCall {
    pub async fn complete(
        &self,
        work: impl Future<Output = IvyResult<IvyJson>>,
        wake: impl FnOnce(),
    ) -> IvyResult<IvyJson> {
        let result = work.await?;
        if self.wake {
            wake();
        }
        Ok(result)
    }
}
fn not_ready(method: PublicMethod) -> IvyError {
    IvyError::new(
        "service_not_ready",
        match method {
            PublicMethod::FollowUp => "Secretary needs its reconciled item context.",
            PublicMethod::FollowUpRead => "Secretary is reconnecting.",
            _ => "Secretary needs its reconciled current generation.",
        },
    )
}
/// The SDK callback remains Send. The generation owns and polls the domain work
/// on its journal thread, even if the requesting future abandons its receiver.
pub struct Invocation {
    pub method: PublicMethod,
    pub arguments: IvyJson,
    pub context: InvocationContext,
    pub reply: oneshot::Sender<IvyResult<IvyJson>>,
}
pub fn registered_handlers(
    sender: mpsc::UnboundedSender<Invocation>,
) -> HashMap<JsString, ToolHandler> {
    PublicMethod::ALL
        .into_iter()
        .map(|method| {
            let sender = sender.clone();
            let handler: ToolHandler = Arc::new(move |arguments, context| {
                let sender = sender.clone();
                Box::pin(async move {
                    let (reply, result) = oneshot::channel();
                    sender
                        .send(Invocation {
                            method,
                            arguments,
                            context,
                            reply,
                        })
                        .map_err(|_| not_ready(method))?;
                    result.await.map_err(|_| not_ready(method))?
                })
            });
            (format!("secretary.{}", method.name()).into(), handler)
        })
        .collect()
}
fn need(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("invalid_arguments", message))
    }
}
fn string(value: Option<&IvyJson>) -> IvyResult<IvyJson> {
    value
        .and_then(IvyJson::as_js_string)
        .cloned()
        .map(IvyJson::String)
        .ok_or_else(|| IvyError::new("invalid_arguments", "The request field must be a string."))
}
/// Admit only against the selected generation. The connection owns the abort
/// reason; this callback does not replace it with a domain cancellation error.
pub fn admit(
    method: PublicMethod,
    args: &IvyJson,
    context: &InvocationContext,
    generation: Option<u64>,
    check_signal: impl FnOnce() -> IvyResult<()>,
) -> IvyResult<HandlerCall> {
    if generation != Some(context.generation) {
        return Err(not_ready(method));
    }
    if !matches!(method, PublicMethod::FollowUp | PublicMethod::FollowUpRead) {
        check_signal()?;
    }
    need(
        args.as_object().is_some(),
        "Tool arguments must be an object.",
    )?;
    let routed = || {
        context
            .operation_id
            .as_ref()
            .map(|id| IvyJson::String(id.clone()))
            == args.get("operationId").cloned()
    };
    let caller = IvyJson::String(context.caller_principal_id.clone());
    let call = |method, arguments, wake| {
        Ok(HandlerCall {
            method,
            arguments,
            wake,
        })
    };
    match method {
        PublicMethod::Binding => call(EngineMethod::Binding, vec![caller], false),
        PublicMethod::ListAssignments => call(
            EngineMethod::ListAssignments,
            vec![caller, args.clone()],
            false,
        ),
        PublicMethod::GetAssignment => call(
            EngineMethod::GetAssignment,
            vec![caller, args.clone()],
            false,
        ),
        PublicMethod::SaveConfiguration => {
            need(
                routed(),
                "The configuration and routed operation identities must agree.",
            )?;
            call(
                EngineMethod::SaveConfiguration,
                vec![caller, args.clone()],
                true,
            )
        }
        PublicMethod::SaveAssignment
        | PublicMethod::CreateAssignment
        | PublicMethod::UpdateAssignment => {
            need(
                routed(),
                "The assignment and routed operation identities must agree.",
            )?;
            let allowed = method == PublicMethod::SaveAssignment
                || if method == PublicMethod::CreateAssignment {
                    args.get("assignment") == Some(&IvyJson::Null)
                } else {
                    args.get("assignment") != Some(&IvyJson::Null)
                };
            need(
                allowed,
                if method == PublicMethod::CreateAssignment {
                    "Creating an assignment requires assignment=null."
                } else {
                    "Updating an assignment requires its current revision pin."
                },
            )?;
            call(
                EngineMethod::SaveAssignment,
                vec![caller, args.clone()],
                true,
            )
        }
        PublicMethod::Operation | PublicMethod::OperationRead => {
            let scope = args.get("expectedScope").cloned().unwrap_or(IvyJson::Null);
            let id = string(args.get("operationId"))?;
            call(
                if method == PublicMethod::Operation {
                    EngineMethod::Operation
                } else {
                    EngineMethod::OperationRead
                },
                vec![caller, scope, id],
                false,
            )
        }
        PublicMethod::Status => call(
            EngineMethod::Status,
            vec![
                caller,
                args.get("expectedScope").cloned().unwrap_or(IvyJson::Null),
            ],
            false,
        ),
        PublicMethod::FollowUp => {
            need(
                routed(),
                "The follow-up and routed operation identities must agree.",
            )?;
            call(EngineMethod::FollowUp, vec![args.clone(), caller], true)
        }
        PublicMethod::FollowUpRead => call(
            EngineMethod::FollowUpRead,
            vec![string(args.get("operationId"))?, caller],
            false,
        ),
        _ => {
            need(
                args.get("action") == Some(&IvyJson::from(method.name())) && routed(),
                "The domain and routed action identities must agree.",
            )?;
            call(EngineMethod::Action, vec![caller, args.clone()], true)
        }
    }
}

pub struct SecretaryHandlers<'a, 'engine, 'journal> {
    pub engine: &'a SecretaryEngine<'a, 'engine, 'journal>,
    pub generation: u64,
    pub service_node_id: &'a JsString,
    pub host_id: &'a JsString,
    pub tools: Arc<ToolClients>,
    pub retained: &'a CheckedNativeContracts,
}
impl SecretaryHandlers<'_, '_, '_> {
    pub async fn execute(&self, call: &HandlerCall) -> IvyResult<IvyJson> {
        let a = &call.arguments;
        match call.method {
            EngineMethod::Binding => {
                let identity = self.engine.evidence.settings.required("identity")?;
                let scope = identity.required("scope")?;
                self.engine
                    .evidence
                    .authorize(&a[0], scope, "status", None)?;
                self.engine.evidence.verify_owner().await?;
                let mut binding = IvyJson::object([
                    (
                        "serviceNodeId",
                        IvyJson::String(self.service_node_id.clone()),
                    ),
                    ("hostId", IvyJson::String(self.host_id.clone())),
                    ("available", true.into()),
                    ("expectedScope", scope.clone()),
                    ("generation", IvyJson::Number(self.generation as f64)),
                ]);
                binding.insert("bindingHash", hash_json(&binding)?.into());
                binding.insert(
                    "observedAt",
                    IvyJson::String(
                        date_to_iso(self.engine.operation_clock.utc_millis() as f64)
                            .map_err(|_| IvyError::internal())?,
                    ),
                );
                Ok(binding)
            }
            EngineMethod::ListAssignments => self.engine.list_assignments(&a[0], &a[1]).await,
            EngineMethod::GetAssignment => self.engine.get_assignment(&a[0], &a[1]).await,
            EngineMethod::SaveConfiguration => self.engine.save_configuration(&a[0], &a[1]).await,
            EngineMethod::SaveAssignment => self.engine.save_assignment(&a[0], &a[1]).await,
            EngineMethod::Operation => self.engine.evidence.operation(&a[0], &a[1], &a[2]).await,
            EngineMethod::OperationRead => self.engine.operation_read(&a[0], &a[1], &a[2]).await,
            EngineMethod::Status => self.engine.status(&a[0], &a[1]).await,
            EngineMethod::Action => self.engine.action(&a[0], &a[1]).await,
            EngineMethod::FollowUp | EngineMethod::FollowUpRead => {
                let worker = SecretaryFollowUp::new(self.engine, self.tools.clone(), self.retained);
                if call.method == EngineMethod::FollowUp {
                    worker
                        .run(&a[0], a[1].as_js_string().ok_or_else(IvyError::internal)?)
                        .await
                } else {
                    worker
                        .read(
                            a[0].as_js_string().ok_or_else(IvyError::internal)?,
                            a[1].as_js_string().ok_or_else(IvyError::internal)?,
                        )
                        .await
                }
            }
        }
    }
}
/// Waking follows successful completion only; an abandoned caller must not
/// prevent the generation from polling this admitted future to completion.
pub async fn invoke(
    selected: Option<&SecretaryHandlers<'_, '_, '_>>,
    method: PublicMethod,
    args: &IvyJson,
    context: &InvocationContext,
    check_signal: impl FnOnce() -> IvyResult<()>,
    wake: impl FnOnce(),
) -> IvyResult<IvyJson> {
    let call = admit(
        method,
        args,
        context,
        selected.map(|h| h.generation),
        check_signal,
    )?;
    let selected = selected.ok_or_else(IvyError::internal)?;
    call.complete(selected.execute(&call), wake).await
}
