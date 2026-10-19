use super::*;
use ivy_task_board::{
    engine_execution::{ExecutionProgram, ExecutionQueue},
    reconciler::ReconcileExecutor,
};
use std::{
    cell::{Cell, RefCell},
    future::{Future, poll_fn},
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

#[derive(Default)]
pub(super) struct Executor<'a> {
    work: RefCell<VecDeque<Pin<Box<dyn Future<Output = ()> + 'a>>>>,
    waiter: RefCell<Option<Waker>>,
}
impl<'a> ReconcileExecutor<'a> for Executor<'a> {
    fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + 'a>>) {
        self.work.borrow_mut().push_back(work);
        if let Some(waiter) = self.waiter.borrow().as_ref() {
            waiter.wake_by_ref();
        }
    }
}
impl Executor<'_> {
    fn step(&self, cx: &mut Context<'_>) {
        *self.waiter.borrow_mut() = Some(cx.waker().clone());
        let mut pending = VecDeque::new();
        loop {
            let mut work = std::mem::take(&mut *self.work.borrow_mut());
            if work.is_empty() {
                break;
            }
            while let Some(mut work) = work.pop_front() {
                if work.as_mut().poll(cx).is_pending() {
                    pending.push_back(work);
                }
            }
        }
        self.work.borrow_mut().extend(pending);
    }
    pub(super) async fn drive<F: Future>(&self, work: F) -> F::Output {
        let mut work = Box::pin(work);
        poll_fn(|cx| {
            self.step(cx);
            work.as_mut().poll(cx)
        })
        .await
    }
}
#[derive(Default)]
struct Gate {
    open: AtomicBool,
    entered: AtomicUsize,
    waiter: Mutex<Option<Waker>>,
}
impl Gate {
    async fn wait(&self) {
        self.entered.fetch_add(1, Ordering::SeqCst);
        poll_fn(|cx| {
            if self.open.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                *self.waiter.lock().unwrap() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
    }
    fn release(&self) {
        self.open.store(true, Ordering::SeqCst);
        if let Some(waiter) = self.waiter.lock().unwrap().take() {
            waiter.wake();
        }
    }
}
struct PendingClient {
    client: Client,
    first: AtomicBool,
    gate: Arc<Gate>,
}
impl RpcClient for PendingClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        args: Json,
        opts: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if self.first.swap(false, Ordering::SeqCst) {
                self.gate.wait().await;
            }
            self.client.request(method, args, opts).await
        })
    }
}
struct Program<'a> {
    bindings: &'a Bindings,
    gate: &'a Gate,
    calls: Cell<usize>,
}
impl ExecutionProgram for Program<'_> {
    async fn run(&self, operation: Document) -> IvyResult<Json> {
        self.calls.set(self.calls.get() + 1);
        self.gate.wait().await;
        self.bindings.execute(operation).await
    }
}
#[tokio::test(flavor = "current_thread")]
async fn abandoned_admission_survives_pending_owner_then_nested_execution_and_retirement() {
    // Reuse both complete original invocation traces. Operations and both queues
    // are real; RPC/record I/O and the final execution result are finite ports.
    let row = data()
        .as_array()
        .unwrap()
        .iter()
        .find(|r| s(r, "name") == &JsString::from("ordinary user then retained retry"))
        .unwrap();
    let replay = Arc::new(Replay {
        steps: Mutex::new(
            f(row, "boundaries")
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .collect(),
        ),
        name: s(row, "name").clone(),
    });
    let owner_gate = Arc::new(Gate::default());
    let client = Arc::new(PendingClient {
        client: Client(replay.clone()),
        first: AtomicBool::new(true),
        gate: owner_gate.clone(),
    });
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let bindings = Bindings(replay.clone());
    let runtime = AdmissionRuntime {
        client,
        contracts: &bindings,
        catalogs: &catalogs,
    };
    let owner = EngineOwner {
        service_node_id: "task-node".into(),
        generation: 7.0,
    };
    let operations = Operations {
        store: World {
            replay: replay.clone(),
            root: Some("root".into()),
        },
        now: || "2026-01-02T03:04:05.006Z".to_owned(),
    };
    let execution_gate = Gate::default();
    let program = Program {
        bindings: &bindings,
        gate: &execution_gate,
        calls: Cell::new(0),
    };
    let execution_executor = Executor::default();
    let execution = ExecutionQueue::new(&program, &execution_executor);
    let engine = EngineAdmission::new(
        &operations,
        f(row, "settings"),
        &owner,
        &runtime,
        &execution,
    )
    .unwrap();
    let executor = Executor::default();
    let invocations = EngineInvocations::new(&executor);
    let cancelled = Cell::new(false);
    let calls = Cell::new(0);
    let mut observer = {
        let caller = JsString::from("caller");
        let operation_id = JsString::from("op");
        let check_cancelled = || {
            calls.set(calls.get() + 1);
            replay.call("cancel", [])?;
            assert!(!cancelled.get());
            Ok(())
        };
        let context = ReadContext {
            caller_principal_id: &caller,
            operation_id: &operation_id,
            generation: 7.0,
            check_cancelled: &check_cancelled,
        };
        let mut input = f(&f(row, "steps").as_array().unwrap()[0], "request").clone();
        let observer = invocations.invoke(&engine, &input, &context).unwrap();
        // Later caller changes cannot replace the retained original input.
        input.insert("reason", "mutated after acceptance".into());
        Box::pin(observer)
    };
    assert_eq!(
        calls.get(),
        1,
        "Cancellation is checked at entry, not at a later owner poll."
    );
    let mut drain = Box::pin(invocations.settled());
    poll_fn(|cx| {
        executor.step(cx);
        execution_executor.step(cx);
        assert_eq!(owner_gate.entered.load(Ordering::SeqCst), 1);
        assert_eq!(program.calls.get(), 0);
        assert!(observer.as_mut().poll(cx).is_pending());
        assert!(drain.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    // The first owner RPC is pending. The handler's caller and context are gone.
    drop(observer);
    cancelled.set(true);
    owner_gate.release();
    poll_fn(|cx| {
        executor.step(cx);
        execution_executor.step(cx);
        assert_eq!(program.calls.get(), 1);
        assert_eq!(execution_gate.entered.load(Ordering::SeqCst), 1);
        assert!(drain.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    execution_gate.release();
    executor.drive(execution_executor.drive(&mut drain)).await;
    executor.drive(execution.settled()).await;
    assert_eq!(
        calls.get(),
        1,
        "Cancellation after first await is advisory in the original."
    );
    // A genuine retry still performs all original admission/identity checks.
    let caller = JsString::from("caller");
    let operation_id = JsString::from("op");
    let check_cancelled = || replay.call("cancel", []).map(|_| ());
    let context = ReadContext {
        caller_principal_id: &caller,
        operation_id: &operation_id,
        generation: 7.0,
        check_cancelled: &check_cancelled,
    };
    let observer = invocations
        .invoke(
            &engine,
            f(&f(row, "steps").as_array().unwrap()[1], "request"),
            &context,
        )
        .unwrap();
    invocations.stop_admissions();
    assert_eq!(
        invocations
            .invoke(&engine, &Json::Null, &context)
            .err()
            .unwrap()
            .code_js(),
        JsString::from("service_not_ready")
    );
    let mut stop = Box::pin(invocations.stop());
    poll_fn(|cx| {
        assert!(stop.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let result = executor
        .drive(execution_executor.drive(observer))
        .await
        .unwrap();
    assert_eq!(
        result,
        f(
            &f(f(row, "expected"), "outcomes").as_array().unwrap()[1],
            "ok"
        )
        .clone()
    );
    executor.drive(stop).await;
    assert_eq!(program.calls.get(), 2);
    assert!(replay.steps.lock().unwrap().is_empty());
    assert!(executor.work.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn retirement_keeps_parallel_results_errors_and_unwinding_work_separate() {
    let gate = Gate::default();
    let executor = Executor::default();
    let invocations = EngineInvocations::new(&executor);
    let first = invocations
        .run(async {
            gate.wait().await;
            Ok("first".into())
        })
        .unwrap();
    let error = invocations
        .run(async {
            Err(
                IvyError::new("deadline_exceeded", "Original uncertain reply.")
                    .with_outcome(Outcome::Unknown),
            )
        })
        .unwrap();
    let panicked = invocations
        .run(async { panic!("owner task panic") })
        .unwrap();
    let mut stop = Box::pin(invocations.stop());
    assert!(
        invocations.run(async { Ok(Json::Null) }).is_err(),
        "Stop closes entry before its drain future is polled."
    );
    // The same unwind boundary used by a local owner: dropping its failed job
    // releases accounting, but reports an uncertain result to a live observer.
    let mut work = executor.work.borrow_mut().pop_back().unwrap();
    poll_fn(|cx| {
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.as_mut().poll(cx)))
                .is_err()
        );
        Poll::Ready(())
    })
    .await;
    drop(work);
    let failure = panicked.await.unwrap_err();
    assert_eq!(failure.code_js(), JsString::from("internal_error"));
    assert_eq!(failure.outcome, Outcome::Unknown);
    poll_fn(|cx| {
        executor.step(cx);
        assert!(stop.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let original = error.await.unwrap_err();
    assert_eq!(original.code_js(), JsString::from("deadline_exceeded"));
    assert_eq!(
        original.message_js(),
        JsString::from("Original uncertain reply.")
    );
    assert_eq!(original.outcome, Outcome::Unknown);
    gate.release();
    assert_eq!(executor.drive(first).await.unwrap(), Json::from("first"));
    executor.drive(stop).await;
}
