//! One completion/coalescing policy for decoded and owner-local programs.
#![cfg_attr(not(test), allow(dead_code))]
use crate::{
    journal_store::{
        mutation_ports::{LocalMutationProgram, WriteFailure},
        persistent_reads::json_values::Value,
    },
    reconciler::ReconcileExecutor,
};
use ivy_contracts::{IvyError, JsString, Outcome};
use std::{
    cell::RefCell,
    collections::HashMap,
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

struct Completion<V, E> {
    result: Option<Result<V, E>>,
    waiters: Vec<Waker>,
}
impl<V, E> Default for Completion<V, E> {
    fn default() -> Self {
        Self {
            result: None,
            waiters: Vec::new(),
        }
    }
}
pub(super) struct State<V, E> {
    active: HashMap<JsString, Rc<RefCell<Completion<V, E>>>>,
    waiters: Vec<Waker>,
}
impl<V, E> Default for State<V, E> {
    fn default() -> Self {
        Self {
            active: HashMap::new(),
            waiters: Vec::new(),
        }
    }
}
pub(super) struct ActiveExecution<V, E: From<IvyError>> {
    id: JsString,
    state: Rc<RefCell<State<V, E>>>,
    completion: Rc<RefCell<Completion<V, E>>>,
}
impl<V, E: From<IvyError>> ActiveExecution<V, E> {
    pub(super) fn complete(self, result: Result<V, E>) {
        self.completion.borrow_mut().result = Some(result);
    }
}
impl<V, E: From<IvyError>> Drop for ActiveExecution<V, E> {
    fn drop(&mut self) {
        let replies = {
            let mut completion = self.completion.borrow_mut();
            completion.result.get_or_insert_with(|| {
                Err(
                    IvyError::new("internal_error", "Service invocation failed.")
                        .with_outcome(Outcome::Unknown)
                        .into(),
                )
            });
            std::mem::take(&mut completion.waiters)
        };
        let drained = {
            let mut state = self.state.borrow_mut();
            if state
                .active
                .get(&self.id)
                .is_some_and(|c| Rc::ptr_eq(c, &self.completion))
            {
                state.active.remove(&self.id);
            }
            if state.active.is_empty() {
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
pub(crate) struct Request<V, E> {
    completion: Rc<RefCell<Completion<V, E>>>,
}
impl<V: Clone, E: Clone> Future for Request<V, E> {
    type Output = Result<V, E>;
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
pub(super) fn admit<V, E: From<IvyError>>(
    state: &Rc<RefCell<State<V, E>>>,
    id: JsString,
) -> (Request<V, E>, Option<ActiveExecution<V, E>>) {
    if let Some(completion) = state.borrow().active.get(&id) {
        return (
            Request {
                completion: completion.clone(),
            },
            None,
        );
    }
    let completion = Rc::new(RefCell::new(Completion::default()));
    state
        .borrow_mut()
        .active
        .insert(id.clone(), completion.clone());
    // Admission owns this guard before any producer poll, including when its
    // observer is never polled and the executor is interrupted immediately.
    let active = ActiveExecution {
        id,
        state: state.clone(),
        completion: completion.clone(),
    };
    (Request { completion }, Some(active))
}
pub(super) async fn settled<V, E>(state: &RefCell<State<V, E>>) {
    poll_fn(|cx| {
        let mut state = state.borrow_mut();
        if state.active.is_empty() {
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

/// Keys have already been admitted by the operation owner. Input/output roots
/// and failures remain authoritative through every coalesced observer.
pub(crate) struct LocalExecutionState<E>(Rc<RefCell<State<Value, WriteFailure<E>>>>);
impl<E> Default for LocalExecutionState<E> {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(State::default())))
    }
}
// State precedes borrowed job queues in the generation's drop order. A queue
// borrows that owner, without borrowing its executor for a producer lifetime.
pub(crate) struct LocalExecutionQueue<'a, 'x, P, X, E> {
    program: &'a P,
    executor: &'x X,
    state: &'a LocalExecutionState<E>,
}
impl<'a, 'x, P: LocalMutationProgram<E> + 'a, X: ReconcileExecutor<'a>, E>
    LocalExecutionQueue<'a, 'x, P, X, E>
{
    pub(crate) fn new(program: &'a P, state: &'a LocalExecutionState<E>, executor: &'x X) -> Self {
        Self {
            program,
            executor,
            state,
        }
    }
    pub(crate) fn execute_graph(
        &self,
        id: &JsString,
        operation: Value,
    ) -> Request<Value, WriteFailure<E>> {
        let (request, active) = admit(&self.state.0, id.clone());
        if let Some(active) = active {
            let program = self.program;
            self.executor.spawn(Box::pin(async move {
                active.complete(program.run_graph(operation).await);
            }));
        }
        request
    }
    pub(crate) async fn settled(&self) {
        settled(&self.state.0).await;
    }
}
