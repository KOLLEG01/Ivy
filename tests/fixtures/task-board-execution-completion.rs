use crate::{
    engine_execution::{
        ReadScopeExecutor,
        completion::{LocalExecutionQueue, LocalExecutionState},
    },
    engine_read::read_failure::ReadCause,
    journal_store::{
        CachedRecord, ReadScopes,
        mutation_ports::{LocalMutationProgram, WriteFailure},
        persistent_reads::json_values::{self as v, Value},
    },
    local_executor::LocalExecutor,
    reconciler::ReconcileExecutor,
};
use ivy_contracts::{IvyError, Outcome};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

#[derive(Default)]
struct Gate {
    open: Cell<bool>,
    waiters: RefCell<Vec<Waker>>,
}
impl Gate {
    async fn wait(&self) {
        poll_fn(|cx| {
            if self.open.get() {
                Poll::Ready(())
            } else {
                self.waiters.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
    fn release(&self) {
        self.open.set(true);
        for waiter in self.waiters.take() {
            waiter.wake();
        }
    }
}
struct Opaque<'a> {
    borrowed: &'a u8,
    non_send: Rc<()>,
    drops: Rc<Cell<usize>>,
}
impl Drop for Opaque<'_> {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}
struct Program<'a> {
    gate: &'a Gate,
    calls: Cell<usize>,
    seen: RefCell<Vec<Value>>,
    error: Option<WriteFailure<Opaque<'a>>>,
}
impl<'a> LocalMutationProgram<Opaque<'a>> for Program<'a> {
    async fn run_graph(&self, operation: Value) -> Result<Value, WriteFailure<Opaque<'a>>> {
        self.calls.set(self.calls.get() + 1);
        self.seen.borrow_mut().push(operation.clone());
        self.gate.wait().await;
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(operation),
        }
    }
}
#[derive(Default)]
struct Executor<'a>(RefCell<VecDeque<Pin<Box<dyn Future<Output = ()> + 'a>>>>);
impl<'a> ReconcileExecutor<'a> for Executor<'a> {
    fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + 'a>>) {
        self.0.borrow_mut().push_back(work);
    }
}
impl Executor<'_> {
    async fn start_all(&self) {
        poll_fn(|cx| {
            let mut jobs = std::mem::take(&mut *self.0.borrow_mut());
            while let Some(mut work) = jobs.pop_front() {
                if work.as_mut().poll(cx).is_pending() {
                    self.0.borrow_mut().push_back(work);
                }
            }
            Poll::Ready(())
        })
        .await
    }
    async fn drive<F: Future>(&self, work: F) -> F::Output {
        let mut work = Box::pin(work);
        poll_fn(|cx| {
            let mut jobs = std::mem::take(&mut *self.0.borrow_mut());
            while let Some(mut job) = jobs.pop_front() {
                if job.as_mut().poll(cx).is_pending() {
                    self.0.borrow_mut().push_back(job);
                }
            }
            work.as_mut().poll(cx)
        })
        .await
    }
}
struct WakeCount(AtomicUsize);
impl std::task::Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
fn value(label: &str) -> Value {
    v::object([("label".into(), Value::String(label.into()))])
}

#[tokio::test(flavor = "current_thread")]
async fn completions_coalesce_json_requests_and_readmit_after_settlement() {
    let gate = Gate::default();
    let program = Program {
        gate: &gate,
        calls: Cell::new(0),
        seen: RefCell::default(),
        error: None,
    };
    let state = LocalExecutionState::default();
    let executor = Executor::default();
    let queue = LocalExecutionQueue::new(&program, &state, &executor);
    let original = value("before");
    let first = queue.execute_graph(&"one".into(), original.clone());
    let duplicate = queue.execute_graph(&"one".into(), value("ignored replacement"));
    let other_value = value("other");
    let other = queue.execute_graph(&"two".into(), other_value.clone());
    assert_eq!(executor.0.borrow().len(), 2);
    assert_eq!(program.calls.get(), 0);
    drop(first);
    executor.start_all().await;
    assert_eq!(program.calls.get(), 2);
    assert!(program.seen.borrow()[0].eq(&original));
    gate.release();
    let (result, other_result) = executor
        .drive(async { (duplicate.await.unwrap(), other.await.unwrap()) })
        .await;
    assert!(result.eq(&original));
    assert!(
        v::field(&result, "label")
            .unwrap()
            .eq(&Value::String("before".into()))
    );
    assert!(other_result.eq(&other_value));
    queue.settled().await;
    let later_value = value("later");
    let later = executor
        .drive(queue.execute_graph(&"one".into(), later_value.clone()))
        .await
        .unwrap();
    assert!(later.eq(&later_value));
    assert_eq!(program.calls.get(), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn completions_preserve_unconstrained_borrowed_and_ordinary_causes_for_all_observers() {
    for kind in ["source", "ivy"] {
        let borrowed = 13;
        let drops = Rc::new(Cell::new(0));
        let gate = Gate::default();
        let error = match kind {
            "source" => WriteFailure::source(Opaque {
                borrowed: &borrowed,
                non_send: Rc::new(()),
                drops: drops.clone(),
            }),
            _ => IvyError::new("revision_conflict", "retained typed cause")
                .with_outcome(Outcome::Unknown)
                .into(),
        };
        let program = Program {
            gate: &gate,
            calls: Cell::new(0),
            seen: RefCell::default(),
            error: Some(error.clone()),
        };
        let state = LocalExecutionState::default();
        let executor = Executor::default();
        let queue = LocalExecutionQueue::new(&program, &state, &executor);
        drop(queue.execute_graph(&"id".into(), value("abandoned")));
        let first = queue.execute_graph(&"id".into(), value("ignored"));
        let second = queue.execute_graph(&"id".into(), value("ignored again"));
        executor.start_all().await;
        assert_eq!(program.calls.get(), 1);
        gate.release();
        let failures = executor
            .drive(async { [first.await.unwrap_err(), second.await.unwrap_err()] })
            .await;
        queue.settled().await;
        for failure in &failures {
            assert!(failure.ptr_eq(&error));
            if let ReadCause::Source(source) = failure.cause() {
                assert!(std::ptr::eq(source.borrowed, &borrowed));
                assert_eq!(Rc::strong_count(&source.non_send), 1);
            }
        }
        assert_eq!(drops.get(), 0);
        drop(failures);
        drop(queue);
        drop(executor);
        drop(state);
        drop(program);
        drop(error);
        assert_eq!(drops.get(), usize::from(kind == "source"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn interruption_wakes_replies_and_drains_before_releasing_the_admitted_id() {
    for first_poll in [false, true] {
        let gate = Gate::default();
        let program = Program {
            gate: &gate,
            calls: Cell::new(0),
            seen: RefCell::default(),
            error: None,
        };
        let state = LocalExecutionState::default();
        let executor = Executor::default();
        let queue = LocalExecutionQueue::new(&program, &state, &executor);
        drop(queue.execute_graph(&"id".into(), value("unobserved")));
        let mut reply = Box::pin(queue.execute_graph(&"id".into(), value("ignored")));
        let mut drain = Box::pin(queue.settled());
        let counter = Arc::new(WakeCount(AtomicUsize::new(0)));
        let waker = Waker::from(counter.clone());
        let mut context = Context::from_waker(&waker);
        assert!(reply.as_mut().poll(&mut context).is_pending());
        assert!(drain.as_mut().poll(&mut context).is_pending());
        let mut job = executor.0.borrow_mut().pop_front().unwrap();
        assert!(executor.0.borrow().is_empty());
        if first_poll {
            assert!(job.as_mut().poll(&mut context).is_pending());
        }
        let before = counter.0.load(Ordering::SeqCst);
        drop(job);
        assert_eq!(counter.0.load(Ordering::SeqCst) - before, 2);
        let Poll::Ready(Err(error)) = reply.as_mut().poll(&mut context) else {
            panic!("orphaned completion")
        };
        let ReadCause::Ivy(error) = error.cause() else {
            panic!("owner interruption is typed")
        };
        assert_eq!(error.code, "internal_error");
        assert_eq!(error.message_js(), "Service invocation failed.".into());
        assert_eq!(error.outcome, Outcome::Unknown);
        assert!(drain.as_mut().poll(&mut context).is_ready());
        assert_eq!(program.calls.get(), usize::from(first_poll));
        gate.release();
        let recovered_value = value("recovered");
        let recovered = executor
            .drive(queue.execute_graph(&"id".into(), recovered_value.clone()))
            .await
            .unwrap();
        assert!(recovered.eq(&recovered_value));
        assert_eq!(program.calls.get(), usize::from(first_poll) + 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn execution_inherits_each_action_scope_and_drains_on_the_existing_local_owner() {
    struct Scoped<'a> {
        scopes: &'a ReadScopes,
        gate: &'a Gate,
        maps: RefCell<Vec<crate::journal_store::CacheEntries>>,
    }
    impl LocalMutationProgram<()> for Scoped<'_> {
        async fn run_graph(&self, operation: Value) -> Result<Value, WriteFailure<()>> {
            let map = self.scopes.cache().entries().unwrap();
            self.maps.borrow_mut().push(map.clone());
            self.gate.wait().await;
            assert!(Rc::ptr_eq(&map, &self.scopes.cache().entries().unwrap()));
            Ok(operation)
        }
    }
    let scopes = ReadScopes::default();
    let gate = Gate::default();
    let program = Scoped {
        scopes: &scopes,
        gate: &gate,
        maps: RefCell::default(),
    };
    let state = LocalExecutionState::default();
    let executor = LocalExecutor::default();
    let scoped = ReadScopeExecutor {
        scopes: &scopes,
        executor: &executor,
    };
    let queue = LocalExecutionQueue::new(&program, &state, &scoped);
    let original = value("first");
    let first = scopes
        .with_read_cache(async {
            scopes.cache().entries().unwrap().borrow_mut().push((
                "first".into(),
                CachedRecord {
                    object_id: original.clone(),
                    document: None,
                    materialized: Some(original.clone()),
                    retained_bytes: 0,
                },
            ));
            queue.execute_graph(&"one".into(), original.clone())
        })
        .await;
    let second = scopes
        .with_read_cache(async { queue.execute_graph(&"two".into(), value("second")) })
        .await;
    assert!(scopes.cache().entries().is_none());
    let mut driver =
        Box::pin(executor.drive(async { (first.await.unwrap(), second.await.unwrap()) }));
    poll_fn(|cx| {
        assert!(driver.as_mut().poll(cx).is_pending());
        assert_eq!(program.maps.borrow().len(), 2);
        assert!(!Rc::ptr_eq(
            &program.maps.borrow()[0],
            &program.maps.borrow()[1]
        ));
        assert_eq!(program.maps.borrow()[0].borrow().len(), 1);
        assert!(program.maps.borrow()[1].borrow().is_empty());
        assert!(scopes.cache().entries().is_none());
        Poll::Ready(())
    })
    .await;
    gate.release();
    let results = driver.await;
    assert!(results.0.eq(&original));
    queue.settled().await;
    assert!(scopes.cache().entries().is_none());
}
