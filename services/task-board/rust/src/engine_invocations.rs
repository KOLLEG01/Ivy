//! Complete public invocations retained on the journal's local owner.
//! Retirement stops entry before draining admission, publication and replies.
use crate::{
    engine_admission::{AdmissionExecution, EngineAdmission},
    engine_read::ReadContext,
    journal_store::{CapturedReadScope, Contracts, ReadScopes},
    operations::Store,
    reconciler::ReconcileExecutor,
    work_budget::{MAX_WORK_ITEMS, Reservation, WorkBudget},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, Outcome};
use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::sync::{Notify, mpsc, oneshot};

const CALL_OVERHEAD: usize = 4096;
#[derive(Default)]
struct State {
    stopped: Cell<bool>,
    active: Cell<usize>,
    drained: Notify,
}
/// A reply observer. Dropping it never drops the owner's accepted future.
pub struct InvocationRequest {
    received: oneshot::Receiver<IvyResult<Json>>,
    _reservation: Arc<Reservation>,
}
impl Future for InvocationRequest {
    type Output = IvyResult<Json>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().received)
            .poll(cx)
            .map(|result| result.unwrap_or_else(|_| Err(interrupted())))
    }
}
struct Accepted {
    state: Rc<State>,
    reply: Option<oneshot::Sender<IvyResult<Json>>>,
    reservation: Arc<Reservation>,
}
fn accept(
    state: &Rc<State>,
    budget: &WorkBudget,
    bytes: usize,
) -> IvyResult<(InvocationRequest, Accepted)> {
    check_open(state)?;
    let reservation = budget.call(CALL_OVERHEAD.saturating_add(bytes))?;
    let (reply, received) = oneshot::channel();
    state.active.set(state.active.get() + 1);
    Ok((
        InvocationRequest {
            received,
            _reservation: reservation.clone(),
        },
        Accepted {
            state: state.clone(),
            reply: Some(reply),
            reservation,
        },
    ))
}
fn check_open(state: &State) -> IvyResult<()> {
    if state.stopped.get() {
        Err(IvyError::new(
            "service_not_ready",
            "TaskBoard has no reconciled current connection generation.",
        ))
    } else {
        Ok(())
    }
}
fn interrupted() -> IvyError {
    IvyError::new("internal_error", "Service invocation failed.").with_outcome(Outcome::Unknown)
}
async fn settled(state: &State) {
    loop {
        let changed = state.drained.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if state.active.get() == 0 {
            return;
        }
        changed.await;
    }
}
async fn complete(mut accepted: Accepted, work: impl Future<Output = IvyResult<Json>>) {
    let result = work.await;
    if let Some(reply) = accepted.reply.take() {
        if !reply.is_closed() {
            let result = accepted.reservation.reply(result, CALL_OVERHEAD);
            let _ = reply.send(result);
        }
    }
}
impl Drop for Accepted {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(Err(interrupted()));
        }
        self.state.active.set(self.state.active.get() - 1);
        if self.state.active.get() == 0 {
            self.state.drained.notify_waiters();
        }
    }
}
pub struct EngineInvocations<'a, 'x, X> {
    executor: &'x X,
    state: Rc<State>,
    budget: WorkBudget,
    lifetime: std::marker::PhantomData<&'a ()>,
}
impl<'a, 'x, X: ReconcileExecutor<'a>> EngineInvocations<'a, 'x, X> {
    pub fn new(executor: &'x X) -> Self {
        Self::with_budget(executor, WorkBudget::default())
    }
    pub fn with_budget(executor: &'x X, budget: WorkBudget) -> Self {
        Self {
            executor,
            state: Rc::new(State::default()),
            budget,
            lifetime: std::marker::PhantomData,
        }
    }
    /// Retain the entire handler, including any pre-Operation RPC or write.
    /// Do not coalesce here: Operations must compare each original request.
    pub fn run(
        &self,
        work: impl Future<Output = IvyResult<Json>> + 'a,
    ) -> IvyResult<InvocationRequest> {
        self.run_with(0, || work)
    }
    fn run_with<F: Future<Output = IvyResult<Json>> + 'a>(
        &self,
        bytes: usize,
        build: impl FnOnce() -> F,
    ) -> IvyResult<InvocationRequest> {
        let (observer, accepted) = accept(
            &self.state,
            &self.budget,
            bytes.saturating_add(std::mem::size_of::<F>()),
        )?;
        self.executor.spawn(Box::pin(complete(accepted, build())));
        Ok(observer)
    }
    /// Own the input and identity, so the caller's stack is not the lifetime of
    /// admission. The source checks cancellation once, before its first await.
    pub fn invoke<S: Store, C: Fn() -> String, P: Contracts, E: AdmissionExecution>(
        &self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
        input: &Json,
        context: &ReadContext<'_>,
    ) -> IvyResult<InvocationRequest> {
        let bytes = input
            .retained_bytes()
            .saturating_add(context.caller_principal_id.0.capacity().saturating_mul(2))
            .saturating_add(context.operation_id.0.capacity().saturating_mul(2));
        self.run_with(bytes, || {
            let input = input.clone();
            let cancelled = (context.check_cancelled)();
            let caller = context.caller_principal_id.clone();
            let operation = context.operation_id.clone();
            let generation = context.generation;
            async move {
                let check_cancelled = || cancelled.clone();
                engine
                    .invoke(
                        &input,
                        &ReadContext {
                            caller_principal_id: &caller,
                            operation_id: &operation,
                            generation,
                            check_cancelled: &check_cancelled,
                        },
                    )
                    .await
            }
        })
    }
    /// Internal native/scheduler/recovery calls retain pre-Operation admission
    /// just like public invocations. Retiring the generation stops new entry;
    /// dropping an observer cannot cancel an accepted owner check or write.
    pub fn native_update<S: Store, C: Fn() -> String, P: Contracts, E: AdmissionExecution>(
        &self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
        request: &Json,
    ) -> IvyResult<InvocationRequest> {
        self.run_with(request.retained_bytes(), || {
            let request = request.clone();
            async move { engine.native_update(&request).await }
        })
    }
    pub fn native_comment<S: Store, C: Fn() -> String, P: Contracts, E: AdmissionExecution>(
        &self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
        request: &Json,
    ) -> IvyResult<InvocationRequest> {
        self.run_with(request.retained_bytes(), || {
            let request = request.clone();
            async move { engine.native_comment(&request).await }
        })
    }
    pub fn schedule<S: Store, C: Fn() -> String, P: Contracts, E: AdmissionExecution>(
        &self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
        request: &Json,
    ) -> IvyResult<InvocationRequest> {
        self.run_with(request.retained_bytes(), || {
            let request = request.clone();
            async move { engine.schedule(&request).await }
        })
    }
    pub fn recover_operation<S: Store, C: Fn() -> String, P: Contracts, E: AdmissionExecution>(
        &self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
        object_id: &ivy_contracts::JsString,
    ) -> IvyResult<InvocationRequest> {
        self.run_with(object_id.0.capacity().saturating_mul(2), || {
            let object_id = object_id.clone();
            async move { engine.recover_operation(&object_id).await }
        })
    }
    pub fn recover_publication<
        S: crate::publication_coordination::CoordinationStore,
        C: Fn() -> String,
        P: Contracts,
        E: AdmissionExecution,
    >(
        &self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
    ) -> IvyResult<InvocationRequest> {
        self.run(async move { engine.recover_publication().await })
    }
    /// Call synchronously when retiring the generation, before waiting for any
    /// other owner work. Already accepted handlers can still enter execution.
    pub fn stop_admissions(&self) {
        self.state.stopped.set(true);
    }
    pub async fn settled(&self) {
        settled(&self.state).await
    }
    pub fn stop(&self) -> impl Future<Output = ()> + '_ {
        self.stop_admissions();
        self.settled()
    }
    /// Construct native/scheduler callbacks before their execution graph. The
    /// receiver binds the actual engine after construction on this same owner.
    /// Both paths reserve the same invocation gate at synchronous submission.
    pub fn internal_calls<'s>(
        &self,
        scopes: &'s ReadScopes,
    ) -> (InternalCalls<'s>, InternalOwner<'s, 'x, X>) {
        let (sender, receiver) = mpsc::channel(MAX_WORK_ITEMS);
        (
            InternalCalls {
                state: self.state.clone(),
                budget: self.budget.clone(),
                sender,
                scopes,
            },
            InternalOwner {
                state: self.state.clone(),
                receiver,
                executor: self.executor,
                scopes,
            },
        )
    }
}

enum InternalAction {
    NativeUpdate(Json),
    NativeComment(Json),
    Schedule(Json),
    RecoverOperation(ivy_contracts::JsString),
    RecoverPublication,
}
struct InternalCall {
    action: InternalAction,
    context: CapturedReadScope,
    accepted: Accepted,
}
/// A local callback handle containing no reference to the cyclic engine graph.
#[derive(Clone)]
pub struct InternalCalls<'s> {
    state: Rc<State>,
    budget: WorkBudget,
    sender: mpsc::Sender<InternalCall>,
    scopes: &'s ReadScopes,
}
impl InternalCalls<'_> {
    fn submit(
        &self,
        bytes: usize,
        build: impl FnOnce() -> InternalAction,
    ) -> IvyResult<InvocationRequest> {
        let (observer, accepted) = accept(&self.state, &self.budget, bytes)?;
        // A lost receiver drops Accepted, retaining the existing unknown reply.
        let _ = self.sender.try_send(InternalCall {
            action: build(),
            context: self.scopes.capture(),
            accepted,
        });
        Ok(observer)
    }
    pub fn native_update(&self, request: &Json) -> IvyResult<InvocationRequest> {
        self.submit(request.retained_bytes(), || {
            InternalAction::NativeUpdate(request.clone())
        })
    }
    pub fn native_comment(&self, request: &Json) -> IvyResult<InvocationRequest> {
        self.submit(request.retained_bytes(), || {
            InternalAction::NativeComment(request.clone())
        })
    }
    pub fn schedule(&self, request: &Json) -> IvyResult<InvocationRequest> {
        self.submit(request.retained_bytes(), || {
            InternalAction::Schedule(request.clone())
        })
    }
    pub fn recover_operation(
        &self,
        object_id: &ivy_contracts::JsString,
    ) -> IvyResult<InvocationRequest> {
        self.submit(object_id.0.capacity().saturating_mul(2), || {
            InternalAction::RecoverOperation(object_id.clone())
        })
    }
    pub fn recover_publication(&self) -> IvyResult<InvocationRequest> {
        self.submit(0, || InternalAction::RecoverPublication)
    }
}
/// Drive alongside the generation root using the existing owner executor.
/// Releasing this receiver closes all admission; queued calls report unknown.
pub struct InternalOwner<'s, 'x, X> {
    state: Rc<State>,
    receiver: mpsc::Receiver<InternalCall>,
    executor: &'x X,
    scopes: &'s ReadScopes,
}
impl<X> Drop for InternalOwner<'_, '_, X> {
    fn drop(&mut self) {
        self.state.stopped.set(true);
    }
}
impl<'s, X> InternalOwner<'s, '_, X> {
    /// Already accepted calls dispatch even after retirement closes the shared
    /// gate. Drain them and all nested execution before releasing the graph.
    /// The generation root still owns public/tick/reconciler retirement.
    pub async fn run<
        'a,
        S: crate::publication_coordination::CoordinationStore + 'a,
        C: Fn() -> String + 'a,
        P: Contracts + 'a,
        E: AdmissionExecution + 'a,
        F: Future,
    >(
        mut self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
        root: F,
    ) -> F::Output
    where
        's: 'a,
        X: ReconcileExecutor<'a>,
    {
        let mut root = Box::pin(root);
        let mut open = true;
        let result = loop {
            tokio::select! {
                result = &mut root => break result,
                call = self.receiver.recv(), if open => match call {
                    Some(call) => self.dispatch(engine, call),
                    None => open = false,
                }
            }
        };
        self.state.stopped.set(true);
        self.receiver.close();
        while let Some(call) = self.receiver.recv().await {
            self.dispatch(engine, call);
        }
        settled(&self.state).await;
        result
    }
    fn dispatch<
        'a,
        S: crate::publication_coordination::CoordinationStore + 'a,
        C: Fn() -> String + 'a,
        P: Contracts + 'a,
        E: AdmissionExecution + 'a,
    >(
        &self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
        call: InternalCall,
    ) where
        's: 'a,
        X: ReconcileExecutor<'a>,
    {
        self.executor.spawn(Box::pin(self.scopes.resume(
            call.context,
            complete(call.accepted, async move {
                match call.action {
                    InternalAction::NativeUpdate(v) => engine.native_update(&v).await,
                    InternalAction::NativeComment(v) => engine.native_comment(&v).await,
                    InternalAction::Schedule(v) => engine.schedule(&v).await,
                    InternalAction::RecoverOperation(id) => engine.recover_operation(&id).await,
                    InternalAction::RecoverPublication => engine.recover_publication().await,
                }
            }),
        )));
    }
}
