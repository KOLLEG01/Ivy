//! Borrowed jobs on the existing journal owner. Each dependency layer owns its
//! queue; no task is moved to another thread or a separate runtime.
use crate::reconciler::ReconcileExecutor;
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    cell::{Cell, RefCell},
    future::{Future, poll_fn},
    pin::Pin,
    task::{Context, Poll, Waker},
};

type Work<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

#[derive(Default)]
pub struct LocalExecutor<'a> {
    queued: RefCell<Vec<Work<'a>>>,
    running: RefCell<FuturesUnordered<Work<'a>>>,
    waiter: RefCell<Option<Waker>>,
    driving: Cell<bool>,
}
impl<'a> ReconcileExecutor<'a> for LocalExecutor<'a> {
    fn spawn(&self, work: Work<'a>) {
        self.queued.borrow_mut().push(work);
        if let Some(waiter) = self.waiter.borrow().as_ref() {
            waiter.wake_by_ref();
        }
    }
}
struct Driving<'a>(&'a Cell<bool>);
impl Drop for Driving<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}
impl LocalExecutor<'_> {
    fn poll_jobs(&self, cx: &mut Context<'_>) -> bool {
        let queued = std::mem::take(&mut *self.queued.borrow_mut());
        let mut running = self.running.borrow_mut();
        for job in queued {
            running.push(job);
        }
        while let Poll::Ready(Some(())) = running.poll_next_unpin(cx) {}
        running.is_empty() && self.queued.borrow().is_empty()
    }
    /// Drive one local root and retain its result until every admitted job has
    /// settled. Dropping this driver leaves its queued/running jobs in the
    /// executor; a later driver can resume them. Dropping the owner executor
    /// itself interrupts them, allowing their domain guards to report unknown.
    pub async fn drive<F: Future>(&self, root: F) -> F::Output {
        assert!(
            !self.driving.replace(true),
            "A local executor already has a driver."
        );
        let _driving = Driving(&self.driving);
        let mut root = Box::pin(root);
        let mut result = None;
        poll_fn(|cx| {
            *self.waiter.borrow_mut() = Some(cx.waker().clone());
            if result.is_none() {
                if let Poll::Ready(value) = root.as_mut().poll(cx) {
                    result = Some(value);
                }
            }
            let idle = self.poll_jobs(cx);
            if idle && result.is_some() {
                *self.waiter.borrow_mut() = None;
                Poll::Ready(result.take().unwrap())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}
