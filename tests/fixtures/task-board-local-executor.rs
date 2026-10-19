use ivy_contracts::{IvyError, IvyJson as Json, Outcome};
use ivy_task_board::{
    engine_invocations::EngineInvocations, local_executor::LocalExecutor,
    reconciler::ReconcileExecutor,
};
use std::{
    cell::{Cell, RefCell},
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Default)]
struct Gate {
    open: Cell<bool>,
    changed: Notify,
}
impl Gate {
    async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            if self.open.get() {
                return;
            }
            changed.await;
        }
    }
    fn release(&self) {
        self.open.set(true);
        self.changed.notify_waiters();
    }
}
#[derive(Default)]
struct WakeFlag(AtomicBool);
impl Wake for WakeFlag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}
fn poll<F: Future>(future: Pin<&mut F>, wake: &Arc<WakeFlag>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(&Waker::from(wake.clone())))
}
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("Owner jobs did not drain")
}

#[tokio::test(flavor = "current_thread")]
async fn owner_jobs_wake_an_idle_driver_and_drain_concurrent_jobs_before_returning_an_error() {
    let events = RefCell::new(vec![]);
    let slow = Gate::default();
    let fast = Gate::default();
    let root = Gate::default();
    let jobs = LocalExecutor::default();
    let wake = Arc::new(WakeFlag::default());
    let failure = IvyError::new("fixture_refusal", "Owner result.")
        .with_details(Json::object([("retained", true.into())]))
        .with_outcome(Outcome::Unknown);
    let mut driver = Box::pin(jobs.drive(async {
        root.wait().await;
        events.borrow_mut().push("root");
        Err::<(), _>(failure.clone())
    }));
    assert!(poll(driver.as_mut(), &wake).is_pending());
    wake.0.store(false, Ordering::Release);
    jobs.spawn(Box::pin(async {
        events.borrow_mut().push("slow");
        slow.wait().await;
        events.borrow_mut().push("slow.done");
    }));
    jobs.spawn(Box::pin(async {
        events.borrow_mut().push("fast");
        fast.wait().await;
        events.borrow_mut().push("fast.done");
    }));
    assert!(
        wake.0.load(Ordering::Acquire),
        "Submission must wake the idle owner"
    );
    assert!(poll(driver.as_mut(), &wake).is_pending());
    assert!(events.borrow().contains(&"slow") && events.borrow().contains(&"fast"));
    root.release();
    fast.release();
    assert!(poll(driver.as_mut(), &wake).is_pending());
    assert!(events.borrow().contains(&"root"));
    assert!(events.borrow().contains(&"fast.done"));
    assert!(!events.borrow().contains(&"slow.done"));
    // Work admitted while the completed root is still draining also belongs to
    // this owner. A ready result cannot release the journal around that work.
    jobs.spawn(Box::pin(async {
        events.borrow_mut().push("tail");
    }));
    slow.release();
    assert_eq!(bounded(driver).await.unwrap_err(), failure);
    assert!(events.borrow().contains(&"slow.done") && events.borrow().contains(&"tail"));
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_driver_and_invocation_observers_keep_accepted_jobs_for_the_next_driver() {
    let gate = Gate::default();
    let writes = Cell::new(0);
    let entered = Cell::new(false);
    let jobs = LocalExecutor::default();
    let invocations = EngineInvocations::new(&jobs);
    let reply = invocations
        .run(async {
            entered.set(true);
            gate.wait().await;
            writes.set(writes.get() + 1);
            Ok(Json::from("saved"))
        })
        .unwrap();
    drop(reply);
    let wake = Arc::new(WakeFlag::default());
    let mut first = Box::pin(jobs.drive(invocations.stop()));
    assert!(poll(first.as_mut(), &wake).is_pending());
    assert!(entered.get());
    drop(first);
    assert_eq!(writes.get(), 0);
    assert!(invocations.run(async { Ok(Json::Null) }).is_err());
    gate.release();
    bounded(jobs.drive(invocations.settled())).await;
    assert_eq!(writes.get(), 1);
    // Reuse after a completed driver neither re-polls the original root nor
    // repeats a completed accepted invocation.
    assert_eq!(bounded(jobs.drive(async { 17 })).await, 17);
    assert_eq!(writes.get(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn interrupted_owner_executor_releases_the_admitted_invocation_with_unknown_outcome() {
    let entered = Cell::new(false);
    let reply = {
        let jobs = LocalExecutor::default();
        let invocations = EngineInvocations::new(&jobs);
        let reply = invocations
            .run(async {
                entered.set(true);
                pending::<()>().await;
                Ok(Json::Null)
            })
            .unwrap();
        let mut driver = Box::pin(jobs.drive(pending::<()>()));
        assert!(poll(driver.as_mut(), &Arc::new(WakeFlag::default())).is_pending());
        assert!(entered.get());
        drop(driver);
        reply
    };
    let error = bounded(reply).await.unwrap_err();
    assert_eq!(error.code_js(), "internal_error".into());
    assert_eq!(error.message_js(), "Service invocation failed.".into());
    assert_eq!(error.outcome, Outcome::Unknown);
}
