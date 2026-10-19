//! Complete public invocations retained on the journal's local owner.
//! Retirement stops entry before draining admission, publication and replies.
use crate::{
    engine_admission::{AdmissionExecution, EngineAdmission},
    engine_read::ReadContext,
    journal_store::{CapturedReadScope, Contracts, ReadScopes},
    operations::Store,
    reconciler::ReconcileExecutor,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, Outcome};
use std::{
    cell::RefCell,
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};
use tokio::sync::mpsc;

#[derive(Default)]
struct Completion {
    result: Option<IvyResult<Json>>,
    waiters: Vec<Waker>,
}
#[derive(Default)]
struct State {
    stopped: bool,
    active: usize,
    waiters: Vec<Waker>,
}
/// A reply observer. Dropping it never drops the owner's accepted future.
pub struct InvocationRequest {
    completion: Rc<RefCell<Completion>>,
}
impl Future for InvocationRequest {
    type Output = IvyResult<Json>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut completion = self.completion.borrow_mut();
        if let Some(result) = &completion.result {
            Poll::Ready(result.clone())
        } else {
            if !completion.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                completion.waiters.push(cx.waker().clone());
            }
            Poll::Pending
        }
    }
}
struct Accepted {
    state: Rc<RefCell<State>>,
    completion: Rc<RefCell<Completion>>,
}
fn accept(state: &Rc<RefCell<State>>) -> IvyResult<(InvocationRequest, Accepted)> {
    check_open(state)?;
    let completion = Rc::new(RefCell::new(Completion::default()));
    state.borrow_mut().active += 1;
    Ok((
        InvocationRequest {
            completion: completion.clone(),
        },
        Accepted {
            state: state.clone(),
            completion,
        },
    ))
}
fn check_open(state: &RefCell<State>) -> IvyResult<()> {
    if state.borrow().stopped {
        Err(IvyError::new(
            "service_not_ready",
            "TaskBoard has no reconciled current connection generation.",
        ))
    } else {
        Ok(())
    }
}
async fn settled(state: &RefCell<State>) {
    poll_fn(|cx| {
        let mut state = state.borrow_mut();
        if state.active == 0 {
            Poll::Ready(())
        } else {
            if !state.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                state.waiters.push(cx.waker().clone());
            }
            Poll::Pending
        }
    })
    .await
}
async fn complete(accepted: Accepted, work: impl Future<Output = IvyResult<Json>>) {
    let result = work.await;
    accepted.completion.borrow_mut().result = Some(result);
    drop(accepted);
}
impl Drop for Accepted {
    fn drop(&mut self) {
        let replies = {
            let mut completion = self.completion.borrow_mut();
            // Match the SDK's failed-handler outcome if the owner unwinds.
            completion.result.get_or_insert_with(|| {
                Err(
                    IvyError::new("internal_error", "Service invocation failed.")
                        .with_outcome(Outcome::Unknown),
                )
            });
            std::mem::take(&mut completion.waiters)
        };
        let drained = {
            let mut state = self.state.borrow_mut();
            state.active -= 1;
            if state.active == 0 {
                std::mem::take(&mut state.waiters)
            } else {
                vec![]
            }
        };
        for waiter in replies.into_iter().chain(drained) {
            waiter.wake();
        }
    }
}
pub struct EngineInvocations<'a, 'x, X> {
    executor: &'x X,
    state: Rc<RefCell<State>>,
    lifetime: std::marker::PhantomData<&'a ()>,
}
impl<'a, 'x, X: ReconcileExecutor<'a>> EngineInvocations<'a, 'x, X> {
    pub fn new(executor: &'x X) -> Self {
        Self {
            executor,
            state: Rc::new(RefCell::new(State::default())),
            lifetime: std::marker::PhantomData,
        }
    }
    fn check_open(&self) -> IvyResult<()> {
        check_open(&self.state)
    }
    /// Retain the entire handler, including any pre-Operation RPC or write.
    /// Do not coalesce here: Operations must compare each original request.
    pub fn run(
        &self,
        work: impl Future<Output = IvyResult<Json>> + 'a,
    ) -> IvyResult<InvocationRequest> {
        let (observer, accepted) = accept(&self.state)?;
        self.executor.spawn(Box::pin(complete(accepted, work)));
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
        self.check_open()?;
        let input = input.clone();
        let cancelled = (context.check_cancelled)();
        let caller = context.caller_principal_id.clone();
        let operation = context.operation_id.clone();
        let generation = context.generation;
        self.run(async move {
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
        self.check_open()?;
        let request = request.clone();
        self.run(async move { engine.native_update(&request).await })
    }
    pub fn native_comment<S: Store, C: Fn() -> String, P: Contracts, E: AdmissionExecution>(
        &self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
        request: &Json,
    ) -> IvyResult<InvocationRequest> {
        self.check_open()?;
        let request = request.clone();
        self.run(async move { engine.native_comment(&request).await })
    }
    pub fn schedule<S: Store, C: Fn() -> String, P: Contracts, E: AdmissionExecution>(
        &self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
        request: &Json,
    ) -> IvyResult<InvocationRequest> {
        self.check_open()?;
        let request = request.clone();
        self.run(async move { engine.schedule(&request).await })
    }
    pub fn recover_operation<S: Store, C: Fn() -> String, P: Contracts, E: AdmissionExecution>(
        &self,
        engine: &'a EngineAdmission<'a, S, C, P, E>,
        object_id: &ivy_contracts::JsString,
    ) -> IvyResult<InvocationRequest> {
        self.check_open()?;
        let object_id = object_id.clone();
        self.run(async move { engine.recover_operation(&object_id).await })
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
        self.state.borrow_mut().stopped = true;
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
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            InternalCalls {
                state: self.state.clone(),
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
    state: Rc<RefCell<State>>,
    sender: mpsc::UnboundedSender<InternalCall>,
    scopes: &'s ReadScopes,
}
impl InternalCalls<'_> {
    fn submit(&self, action: InternalAction) -> IvyResult<InvocationRequest> {
        let (observer, accepted) = accept(&self.state)?;
        // A lost receiver drops Accepted, retaining the existing unknown reply.
        let _ = self.sender.send(InternalCall {
            action,
            context: self.scopes.capture(),
            accepted,
        });
        Ok(observer)
    }
    pub fn native_update(&self, request: &Json) -> IvyResult<InvocationRequest> {
        self.submit(InternalAction::NativeUpdate(request.clone()))
    }
    pub fn native_comment(&self, request: &Json) -> IvyResult<InvocationRequest> {
        self.submit(InternalAction::NativeComment(request.clone()))
    }
    pub fn schedule(&self, request: &Json) -> IvyResult<InvocationRequest> {
        self.submit(InternalAction::Schedule(request.clone()))
    }
    pub fn recover_operation(
        &self,
        object_id: &ivy_contracts::JsString,
    ) -> IvyResult<InvocationRequest> {
        self.submit(InternalAction::RecoverOperation(object_id.clone()))
    }
    pub fn recover_publication(&self) -> IvyResult<InvocationRequest> {
        self.submit(InternalAction::RecoverPublication)
    }
}
/// Drive alongside the generation root using the existing owner executor.
/// Releasing this receiver closes all admission; queued calls report unknown.
pub struct InternalOwner<'s, 'x, X> {
    state: Rc<RefCell<State>>,
    receiver: mpsc::UnboundedReceiver<InternalCall>,
    executor: &'x X,
    scopes: &'s ReadScopes,
}
impl<X> Drop for InternalOwner<'_, '_, X> {
    fn drop(&mut self) {
        self.state.borrow_mut().stopped = true;
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
        self.state.borrow_mut().stopped = true;
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
