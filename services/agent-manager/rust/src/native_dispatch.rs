//! Agent-specific native dispatch and observation hooks, separate from launch.
use crate::{
    native_clock::NATIVE_CLOCK_PRINCIPAL,
    native_journal::Identity,
    native_operations::{NativeOperations, NativeRun},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    codex_admission::Priority,
    codex_rpc::{Events, HookFuture, NativeNotification, NativeRpc, RequestOptions},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
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
                let callback = callback.clone();
                Box::pin(async move {
                    let input = {
                        let mut journal = memory.lock().unwrap();
                        let identity = journal.input_identity(&epoch, &request.id);
                        journal.observe_input(&identity, &request.method, &request.params)?
                    };
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
                    if let Some(thread) = notification
                        .params
                        .get("threadId")
                        .and_then(IvyJson::as_js_string)
                    {
                        if notification.method.eq_str("thread/status/changed")
                            || notification.method.eq_str("turn/started")
                        {
                            memory.lock().unwrap().observe_thread_status(
                                &epoch,
                                thread,
                                if notification.method.eq_str("thread/status/changed") {
                                    notification.params.get("status")
                                } else {
                                    None
                                },
                            )?;
                        }
                    }
                    let resolved =
                        notification
                            .method
                            .eq_str("serverRequest/resolved")
                            .then(|| {
                                (
                                    notification.params.is_null(),
                                    notification
                                        .params
                                        .get("requestId")
                                        .cloned()
                                        .unwrap_or(IvyJson::Null),
                                    notification
                                        .params
                                        .get("threadId")
                                        .cloned()
                                        .unwrap_or(IvyJson::Null),
                                )
                            });
                    // The original publishes/cache-observes the event before
                    // resolving a pending answer from serverRequest/resolved.
                    callback(notification).await?;
                    if let Some((null, request, thread)) = resolved {
                        if null {
                            return Err(IvyError::new(
                                "native_observation_commit_failed",
                                "Native observation commit failed.",
                            ));
                        }
                        memory
                            .lock()
                            .unwrap()
                            .resolve_input_json(&epoch, &request, &thread)?;
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
    preparation: Arc<std::sync::Mutex<ThreadPreparation>>,
    thread_locks: Arc<Mutex<BTreeMap<JsString, Weak<tokio::sync::RwLock<()>>>>>,
}
enum ThreadAccess {
    Work {
        _guard: tokio::sync::OwnedRwLockReadGuard<()>,
    },
    Release {
        _guard: tokio::sync::OwnedRwLockWriteGuard<()>,
    },
}
pub type ThreadPreparation = Arc<dyn Fn() -> HookFuture + Send + Sync>;
/// Keep only a bounded identity in observer closures, never the request body.
/// Null records that the caller supplied an identity we cannot safely attribute.
pub fn thread_observation_params(params: &IvyJson) -> IvyJson {
    IvyJson::object(params.get("threadId").map(|thread| {
        (
            "threadId",
            thread
                .as_js_string()
                .filter(|id| id.0.len() <= 1024)
                .cloned()
                .map(IvyJson::String)
                .unwrap_or(IvyJson::Null),
        )
    }))
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
            thread_locks: Arc::new(Mutex::new(BTreeMap::new())),
            preparation: Arc::new(std::sync::Mutex::new(Arc::new(|| {
                Box::pin(async { Ok(()) })
            }))),
        }
    }
    pub fn set_thread_preparation(&self, preparation: ThreadPreparation) {
        *self.preparation.lock().unwrap() = preparation;
    }
    // Only accepted mutation work reaches this lock. Weak entries retain no
    // completed thread state; calls for different threads remain independent.
    async fn thread_lock(&self, method: &JsString, params: &IvyJson) -> Option<ThreadAccess> {
        if ivy_contracts::native_contract::is_read_only_native_method(method) {
            return None;
        }
        let thread = params
            .get("threadId")?
            .as_js_string()
            .filter(|id| id.0.len() <= 1024)?;
        let lock = {
            let mut locks = self.thread_locks.lock().unwrap();
            locks.retain(|_, lock| lock.strong_count() != 0);
            match locks.get(thread).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(tokio::sync::RwLock::new(()));
                    locks.insert(thread.clone(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        Some(
            if method.eq_str("agent.release") || method.eq_str("thread/unsubscribe") {
                ThreadAccess::Release {
                    _guard: lock.write_owned().await,
                }
            } else {
                ThreadAccess::Work {
                    _guard: lock.read_owned().await,
                }
            },
        )
    }
    fn check_release_waiters(&self, thread: &JsString, identity: &Identity) -> IvyResult<()> {
        let queued = self
            .thread_locks
            .lock()
            .unwrap()
            .get(thread)
            .is_some_and(|lock| lock.strong_count() > 1);
        if queued || self.operations.thread_has_pending_work(thread, identity)? {
            return Err(IvyError::new(
                "native_thread_busy",
                "Accepted or queued native work still needs this thread.",
            ));
        }
        Ok(())
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
        self.operations.run(
            identity,
            method,
            params,
            move |action_params| async move {
                let memory = owner.operations.journal();
                let result = async {
                    owner.available()?;
                    let guard = owner.thread_lock(&action_method, &action_params).await;
                    owner.available()?;
                    if ["thread/start", "thread/resume", "thread/fork"]
                        .iter()
                        .any(|m| action_method.eq_str(m))
                    {
                        let prepare = owner.preparation.lock().unwrap().clone();
                        prepare().await?;
                        owner.available()?;
                    }
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
                            let _guard = &guard;
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
                    // Cleanup must use the control capacity already reserved by
                    // native admission, command retention and both transports.
                    let priority = if action_method.eq_str("turn/interrupt")
                        || action_method.eq_str("thread/unsubscribe")
                    {
                        Priority::Interaction
                    } else {
                        Priority::Background
                    };
                    owner
                        .rpc
                        .request(
                            action_method,
                            action_params,
                            RequestOptions {
                                request_id: Some(owner.operations.request_id(&action_id)?.into()),
                                priority,
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
    /// Transient interactions share the native connection and must contribute
    /// caller ownership even when their observer times out before a late reply.
    pub async fn interact(
        &self,
        identity: Identity,
        request: crate::interactions::Request,
    ) -> IvyResult<IvyJson> {
        self.available()?;
        let guard = self.thread_lock(&request.method, &request.params).await;
        self.available()?;
        let observe = !ivy_contracts::native_contract::is_read_only_native_method(&request.method)
            && (request.params.get("threadId").is_some()
                || ["thread/start", "thread/resume", "thread/fork"]
                    .iter()
                    .any(|method| request.method.eq_str(method)));
        let params = thread_observation_params(&request.params);
        let before_send = {
            let memory = self.operations.journal();
            let epoch = self.epoch.clone();
            let identity = identity.clone();
            let method = request.method.clone();
            let params = params.clone();
            Arc::new(move |_| {
                let memory = memory.clone();
                let epoch = epoch.clone();
                let identity = identity.clone();
                let method = method.clone();
                let params = params.clone();
                Box::pin(async move {
                    memory
                        .lock()
                        .unwrap()
                        .observe_thread_dispatch(&epoch, &identity, &method, &params)
                }) as HookFuture
            })
        };
        let observe_reply = {
            let memory = self.operations.journal();
            let epoch = self.epoch.clone();
            let method = request.method.clone();
            Arc::new(move |reply: &IvyJson| {
                let _guard = &guard;
                memory
                    .lock()
                    .unwrap()
                    .observe_thread_reply(&epoch, &identity, &method, &params, reply)
            })
        };
        let timeout_ms =
            crate::native_management::native_read_timeout_ms(&request.method, &request.params);
        self.rpc
            .request(
                request.method,
                request.params,
                RequestOptions {
                    timeout_ms,
                    request_id: Some(request.request_id.into()),
                    detach_on_timeout: request.detach_on_timeout,
                    priority: if request.interaction_priority {
                        Priority::Interaction
                    } else {
                        Priority::Background
                    },
                    before_send: observe.then_some(before_send),
                    observe_reply: observe.then_some(observe_reply),
                    ..Default::default()
                },
            )
            .await
    }
    /// Caller-scoped release uses the same durable operation owner as native
    /// mutations. A local release has no manufactured native reply/request ID.
    pub fn release(&self, identity: Identity, params: IvyJson) -> IvyResult<NativeRun> {
        let owner = self.clone();
        let action_id = identity.clone();
        self.operations.run(
            identity,
            "agent.release".into(),
            params,
            move |params| async move {
                let memory = owner.operations.journal();
                let result = async {
                    owner.available()?;
                    let guard = owner.thread_lock(&"agent.release".into(), &params).await;
                    owner.available()?;
                    let thread = params
                        .required("threadId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?
                        .clone();
                    let epoch = params
                        .required("expectedEpoch")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?
                        .clone();
                    let expected = params
                        .required("expectedOperationId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?
                        .clone();
                    owner.check_release_waiters(&thread, &action_id)?;
                    memory
                        .lock()
                        .unwrap()
                        .check_thread_release(&thread, &action_id, &epoch, &expected)?;
                    let observe_reply = {
                        let memory = memory.clone();
                        let epoch = epoch.clone();
                        let thread = thread.clone();
                        Arc::new(move |reply: &IvyJson| {
                            let value = reply.get("result").and_then(|v| v.get("thread"));
                            let status = value
                                .filter(|v| {
                                    v.get("id").and_then(IvyJson::as_js_string) == Some(&thread)
                                })
                                .and_then(|v| v.get("status"));
                            memory
                                .lock()
                                .unwrap()
                                .observe_thread_status(&epoch, &thread, status)
                        })
                    };
                    // This bounded native read does not attach or copy the history.
                    // Unknown/active status is never a release authorization.
                    let reply = owner
                        .rpc
                        .request(
                            "thread/read".into(),
                            IvyJson::object([
                                ("threadId", thread.clone().into()),
                                ("includeTurns", false.into()),
                            ]),
                            RequestOptions {
                                timeout_ms: 15_000,
                                detach_on_timeout: true,
                                priority: Priority::Interaction,
                                observe_reply: Some(observe_reply),
                                ..Default::default()
                            },
                        )
                        .await?;
                    if reply
                        .get("result")
                        .and_then(|v| v.get("thread"))
                        .and_then(|v| v.get("status"))
                        .and_then(|v| v.get("type"))
                        .is_none_or(|v| !v.is_str("idle"))
                    {
                        return Err(IvyError::new(
                            "native_thread_busy",
                            "Native thread is not confirmed idle; its context remains attached.",
                        ));
                    }
                    {
                        owner.check_release_waiters(&thread, &action_id)?;
                        let mut journal = memory.lock().unwrap();
                        journal.check_thread_idle(&thread)?;
                        if journal.check_thread_release(&thread, &action_id, &epoch, &expected)? {
                            return journal
                                .finish_logical_release(&action_id, &thread, &epoch, &expected);
                        }
                    }
                    let before_send = {
                        let memory = memory.clone();
                        let identity = action_id.clone();
                        let epoch = epoch.clone();
                        let owner = owner.clone();
                        Arc::new(move |id| {
                            let owner = owner.clone();
                            let memory = memory.clone();
                            let identity = identity.clone();
                            let epoch = epoch.clone();
                            let thread = thread.clone();
                            let expected = expected.clone();
                            Box::pin(async move {
                                owner.check_release_waiters(&thread, &identity)?;
                                let mut journal = memory.lock().unwrap();
                                journal.check_thread_idle(&thread)?;
                                if journal
                                    .check_thread_release(&thread, &identity, &epoch, &expected)?
                                {
                                    return Err(IvyError::new(
                                        "native_subscription_changed",
                                        "Another caller now needs the native subscription.",
                                    ));
                                }
                                journal.dispatch(&identity, &epoch, &id)?;
                                Ok(())
                            }) as HookFuture
                        })
                    };
                    let before_resolve = {
                        let memory = memory.clone();
                        let identity = action_id.clone();
                        let epoch = epoch.clone();
                        Arc::new(move |id, reply| {
                            let _guard = &guard;
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
                            "thread/unsubscribe".into(),
                            IvyJson::object([("threadId", params.required("threadId")?.clone())]),
                            RequestOptions {
                                request_id: Some(owner.operations.request_id(&action_id)?.into()),
                                priority: Priority::Interaction,
                                before_send: Some(before_send),
                                before_resolve: Some(before_resolve),
                                ..Default::default()
                            },
                        )
                        .await?;
                    memory
                        .lock()
                        .unwrap()
                        .get(&action_id)?
                        .ok_or_else(IvyError::internal)
                }
                .await;
                if let Err(error) = result {
                    reject_accepted(&owner.operations, &action_id, &error)?;
                }
                memory
                    .lock()
                    .unwrap()
                    .get(&action_id)?
                    .ok_or_else(IvyError::internal)
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
        let params = IvyJson::object([("identity", input_identity), ("reply", reply)]);
        let owner = self.clone();
        let action_id = identity.clone();
        self.operations.run(
            identity,
            "agent.answer".into(),
            params,
            move |mut params| async move {
                let memory = owner.operations.journal();
                let result = async {
                    owner.available()?;
                    let input_identity =
                        params.remove("identity").ok_or_else(IvyError::internal)?;
                    let reply = params.remove("reply").ok_or_else(IvyError::internal)?;
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
