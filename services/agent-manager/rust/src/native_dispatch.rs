//! Agent-specific native dispatch and observation hooks, separate from launch.
use crate::{
    native_clock::NATIVE_CLOCK_PRINCIPAL,
    native_journal::Identity,
    native_operations::{NativeOperations, NativeRun},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::codex_rpc::{
    Events, HookFuture, NativeNotification, NativeRpc, RequestOptions,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Notification/input delivery stays with the service. These callbacks run in
/// wire order; schedule work that needs this RPC actor instead of awaiting it.
#[derive(Clone)]
pub struct Observations {
    pub on_input: Arc<dyn Fn(IvyJson) -> HookFuture + Send + Sync>,
    pub on_notification: Arc<dyn Fn(NativeNotification) -> HookFuture + Send + Sync>,
    pub on_close: Arc<dyn Fn(String) -> HookFuture + Send + Sync>,
}
impl Default for Observations {
    fn default() -> Self {
        Self {
            on_input: Arc::new(|_| Box::pin(async { Ok(()) })),
            on_notification: Arc::new(|_| Box::pin(async { Ok(()) })),
            on_close: Arc::new(|_| Box::pin(async { Ok(()) })),
        }
    }
}
pub fn journal_events(
    service_node_id: JsString,
    epoch: JsString,
    operations: &NativeOperations,
    observations: Observations,
) -> Events {
    let memory = operations.journal();
    Events {
        on_request: Arc::new({
            let memory = memory.clone();
            let epoch = epoch.clone();
            let callback = observations.on_input;
            move |request| {
                let memory = memory.clone();
                let epoch = epoch.clone();
                let service_node_id = service_node_id.clone();
                let callback = callback.clone();
                Box::pin(async move {
                    let identity = IvyJson::object([
                        ("serviceNodeId", IvyJson::String(service_node_id)),
                        ("epoch", IvyJson::String(epoch)),
                        ("requestId", request.id),
                    ]);
                    let input = memory.lock().unwrap().observe_input(
                        &identity,
                        &request.method,
                        &request.params,
                    )?;
                    callback(input).await
                })
            }
        }),
        on_notification: Arc::new({
            let memory = memory.clone();
            let epoch = epoch.clone();
            let callback = observations.on_notification;
            move |notification| {
                let memory = memory.clone();
                let epoch = epoch.clone();
                let callback = callback.clone();
                Box::pin(async move {
                    // The original publishes/cache-observes the event before
                    // resolving a pending answer from serverRequest/resolved.
                    callback(notification.clone()).await?;
                    if notification.method == JsString::from("serverRequest/resolved") {
                        if notification.params == IvyJson::Null {
                            return Err(IvyError::new(
                                "native_observation_commit_failed",
                                "Native observation commit failed.",
                            ));
                        }
                        memory.lock().unwrap().resolve_input_json(
                            &epoch,
                            notification
                                .params
                                .get("requestId")
                                .unwrap_or(&IvyJson::Null),
                            notification
                                .params
                                .get("threadId")
                                .unwrap_or(&IvyJson::Null),
                        )?;
                    }
                    Ok(())
                })
            }
        }),
        on_close: Arc::new({
            let callback = observations.on_close;
            move |code| {
                let memory = memory.clone();
                let epoch = epoch.clone();
                let callback = callback.clone();
                Box::pin(async move {
                    memory
                        .lock()
                        .unwrap()
                        .lose_epoch(&epoch, &JsString::from(code.clone()))?;
                    callback(code).await
                })
            }
        }),
    }
}

#[derive(Clone)]
pub struct NativeDispatch {
    rpc: NativeRpc,
    epoch: JsString,
    operations: NativeOperations,
    closing: Arc<AtomicBool>,
}
impl NativeDispatch {
    pub fn new(
        rpc: NativeRpc,
        epoch: JsString,
        operations: NativeOperations,
        closing: Arc<AtomicBool>,
    ) -> Self {
        Self {
            rpc,
            epoch,
            operations,
            closing,
        }
    }
    pub fn available(&self) -> IvyResult<()> {
        if !self.closing.load(Ordering::SeqCst)
            && self.rpc.connected()
            && self.operations.journal().lock().unwrap().epoch() == Some(&self.epoch)
        {
            Ok(())
        } else {
            Err(IvyError::new(
                "native_unavailable",
                "The current native owner is unavailable.",
            ))
        }
    }
    pub fn invoke(
        &self,
        identity: Identity,
        method: JsString,
        params: IvyJson,
    ) -> IvyResult<NativeRun> {
        let owner = self.clone();
        let action_id = identity.clone();
        let action_method = method.clone();
        let action_params = params.clone();
        self.operations.run(
            identity,
            method,
            params,
            move || async move {
                let memory = owner.operations.journal();
                let result = async {
                    owner.available()?;
                    let before_send = {
                        let memory = memory.clone();
                        let identity = action_id.clone();
                        let epoch = owner.epoch.clone();
                        Arc::new(move |id| {
                            let memory = memory.clone();
                            let identity = identity.clone();
                            let epoch = epoch.clone();
                            Box::pin(async move {
                                memory.lock().unwrap().dispatch(&identity, &epoch, &id)?;
                                Ok(())
                            }) as HookFuture
                        })
                    };
                    let before_resolve = {
                        let memory = memory.clone();
                        let identity = action_id.clone();
                        let epoch = owner.epoch.clone();
                        Arc::new(move |id, reply| {
                            let memory = memory.clone();
                            let identity = identity.clone();
                            let epoch = epoch.clone();
                            Box::pin(async move {
                                memory
                                    .lock()
                                    .unwrap()
                                    .finish(&identity, &epoch, &id, &reply)?;
                                Ok(())
                            }) as HookFuture
                        })
                    };
                    owner
                        .rpc
                        .request(
                            action_method,
                            action_params,
                            RequestOptions {
                                request_id: Some(owner.operations.request_id(&action_id)?.into()),
                                before_send: Some(before_send),
                                before_resolve: Some(before_resolve),
                                ..Default::default()
                            },
                        )
                        .await?;
                    Ok::<_, IvyError>(())
                }
                .await;
                if let Err(error) = result {
                    reject_accepted(&owner.operations, &action_id, &error)?;
                }
                Ok(memory
                    .lock()
                    .unwrap()
                    .get(&action_id)?
                    .unwrap_or(IvyJson::Null))
            },
            false,
        )
    }
    /// Outer operation/version/definition guards belong to the management handler.
    pub fn answer(
        &self,
        identity: Identity,
        input_identity: IvyJson,
        reply: IvyJson,
    ) -> IvyResult<NativeRun> {
        let params = IvyJson::object([
            ("identity", input_identity.clone()),
            ("reply", reply.clone()),
        ]);
        let owner = self.clone();
        let action_id = identity.clone();
        self.operations.run(
            identity,
            "agent.answer".into(),
            params,
            move || async move {
                let memory = owner.operations.journal();
                let result = async {
                    owner.available()?;
                    let input =
                        memory
                            .lock()
                            .unwrap()
                            .input(&input_identity)?
                            .ok_or_else(|| {
                                IvyError::new(
                                    "not_found",
                                    "The exact native input was not observed by this owner.",
                                )
                            })?;
                    let method = input
                        .get("method")
                        .and_then(IvyJson::as_js_string)
                        .ok_or_else(IvyError::internal)?
                        .clone();
                    if method == JsString::from("currentTime/read") {
                        return Err(IvyError::new(
                            "native_clock_owned",
                            "Native host clock requests are answered by their owning AgentManager.",
                        ));
                    }
                    let hook = {
                        let memory = memory.clone();
                        let identity = action_id.clone();
                        let input = input_identity.clone();
                        let reply = reply.clone();
                        Arc::new(move |_| {
                            let memory = memory.clone();
                            let identity = identity.clone();
                            let input = input.clone();
                            let reply = reply.clone();
                            Box::pin(async move {
                                memory
                                    .lock()
                                    .unwrap()
                                    .dispatch_answer(&identity, &input, &reply)?;
                                Ok(())
                            }) as HookFuture
                        })
                    };
                    let request_id = input_identity
                        .get("requestId")
                        .ok_or_else(IvyError::internal)?
                        .clone();
                    owner.rpc.answer(method, request_id, reply, hook).await
                }
                .await;
                if let Err(error) = result {
                    reject_accepted(&owner.operations, &action_id, &error)?;
                    return Err(error);
                }
                Ok(memory
                    .lock()
                    .unwrap()
                    .get(&action_id)?
                    .unwrap_or(IvyJson::Null))
            },
            false,
        )
    }
}
fn reject_accepted(
    operations: &NativeOperations,
    identity: &Identity,
    error: &IvyError,
) -> IvyResult<()> {
    let memory = operations.journal();
    let mut memory = memory.lock().unwrap();
    if memory
        .get(identity)?
        .is_some_and(|value| value.get("phase") == Some(&IvyJson::from("accepted")))
    {
        memory.reject_before_dispatch(identity, &error.code_js())?;
    }
    Ok(())
}
/// Shared by management entry points; the local clock identity is never a caller.
pub fn caller_identity(caller: JsString, operation_id: Option<JsString>) -> IvyResult<Identity> {
    let operation_id = operation_id.filter(|id| !id.0.is_empty()).ok_or_else(|| {
        IvyError::new(
            "operation_id_required",
            "Native requests require a stable caller operationId.",
        )
    })?;
    if caller == JsString::from(NATIVE_CLOCK_PRINCIPAL) {
        return Err(IvyError::new(
            "native_reserved_principal",
            "The native host clock journal identity is reserved for local protocol responses.",
        ));
    }
    Ok(Identity {
        caller_principal_id: caller,
        operation_id,
    })
}
