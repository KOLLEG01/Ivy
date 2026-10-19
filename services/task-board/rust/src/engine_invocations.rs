//! Complete public invocations retained on the journal's local owner.
//! Retirement stops entry before draining admission, publication and replies.
use crate::{
    engine_admission::{AdmissionExecution, EngineAdmission},
    engine_read::ReadContext,
    journal_store::Contracts,
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
        if self.state.borrow().stopped {
            Err(IvyError::new(
                "service_not_ready",
                "TaskBoard has no reconciled current connection generation.",
            ))
        } else {
            Ok(())
        }
    }
    /// Retain the entire handler, including any pre-Operation RPC or write.
    /// Do not coalesce here: Operations must compare each original request.
    pub fn run(
        &self,
        work: impl Future<Output = IvyResult<Json>> + 'a,
    ) -> IvyResult<InvocationRequest> {
        self.check_open()?;
        let completion = Rc::new(RefCell::new(Completion::default()));
        self.state.borrow_mut().active += 1;
        let accepted = Accepted {
            state: self.state.clone(),
            completion: completion.clone(),
        };
        self.executor.spawn(Box::pin(async move {
            let result = work.await;
            accepted.completion.borrow_mut().result = Some(result);
            drop(accepted);
        }));
        Ok(InvocationRequest { completion })
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
    /// Call synchronously when retiring the generation, before waiting for any
    /// other owner work. Already accepted handlers can still enter execution.
    pub fn stop_admissions(&self) {
        self.state.borrow_mut().stopped = true;
    }
    pub async fn settled(&self) {
        poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
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
    pub fn stop(&self) -> impl Future<Output = ()> + '_ {
        self.stop_admissions();
        self.settled()
    }
}
