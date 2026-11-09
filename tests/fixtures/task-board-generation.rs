#[path = "reference-data.rs"]
mod reference_data;
use super::*;
use ivy_sdk::{
    rpc::RequestOptions,
    service::{InvocationContext, ServiceState},
};
use ivy_task_board::{
    engine_service::ServiceRuntime,
    generation::{
        GenerationClock, GenerationRuntime, GenerationServiceRuntime, NativeNotifications,
        ReconcilerRuntime, TickSchedule, generation,
    },
    local_executor::LocalExecutor,
    service_bridge::LocalHandler,
};
use std::future::poll_fn;

#[test]
fn generation_tick_schedule_matches_original_main_loop() {
    let output = reference_data::read(include_bytes!("task-board-generation-cases.json.gz"));
    let rows = parse(
        std::str::from_utf8(&output).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let mut actions = 0;
    for row in rows.as_array().unwrap() {
        let mut ticks = TickSchedule::new(num(row, "intervalMs"));
        let mut now = 1000000.0;
        let mut starts = vec![];
        let mut snapshots = vec![];
        for action in f(row, "actions").as_array().unwrap() {
            actions += 1;
            let a = action.as_array().unwrap();
            let start = match a[0].as_str().unwrap().as_str() {
                "ready" => ticks.ready(a[1].as_bool().unwrap(), now),
                "kick" => ticks.kick(now),
                "finish" => {
                    ticks.finished(now, a[1].as_bool().unwrap());
                    false
                }
                "advance" => {
                    now += a[1].as_f64().unwrap();
                    false
                }
                "timer" => ticks.timer_at().is_some() && ticks.timer(now),
                other => panic!("Unknown schedule action: {other}"),
            };
            if start {
                starts.push(Json::Number(now));
            }
            snapshots.push(Json::object([
                ("starts", Json::Array(starts.clone())),
                (
                    "timerAt",
                    ticks.timer_at().map(Json::Number).unwrap_or(Json::Null),
                ),
            ]));
        }
        assert_eq!(
            hash_json(&Json::Array(snapshots)).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}",
            s(row, "name")
        );
    }
    assert_eq!(rows.as_array().unwrap().len(), 21);
    assert!(actions > 150);
}
struct ManualClock {
    now: Cell<f64>,
    sleeps: RefCell<Vec<(f64, Rc<Gate>)>>,
}
impl ManualClock {
    fn new() -> Self {
        Self {
            now: Cell::new(1000000.0),
            sleeps: RefCell::default(),
        }
    }
    fn advance(&self, millis: f64) {
        self.now.set(self.now.get() + millis);
        self.sleeps.borrow_mut().retain(|(at, gate)| {
            if *at <= self.now.get() {
                gate.release();
                false
            } else {
                true
            }
        });
    }
}
impl GenerationClock for ManualClock {
    fn now_millis(&self) -> f64 {
        self.now.get()
    }
    async fn sleep(&self, delay_ms: f64) {
        let gate = Rc::new(Gate::default());
        self.sleeps
            .borrow_mut()
            .push((self.now.get() + delay_ms, gate.clone()));
        gate.wait().await;
    }
}
#[derive(Default)]
struct Native(Cell<usize>, Cell<bool>);
impl NativeNotifications for Native {
    fn diagnostics(&self) -> IvyResult<Vec<Json>> {
        if self.1.get() {
            Err(IvyError::new("invalid_arguments", "Invalid time value"))
        } else {
            Ok(vec![])
        }
    }
    fn notification(&self, value: &Json) -> bool {
        self.0.set(self.0.get() + 1);
        value
            .get("params")
            .and_then(|p| p.get("payload"))
            .and_then(|event| event.get("method"))
            == Some(&"turn/completed".into())
    }
}
fn native_hint(method: &str) -> Json {
    Json::object([(
        "params",
        Json::object([
            ("serviceNodeId", "fixture-agent".into()),
            (
                "payload",
                Json::object([
                    ("serviceNodeId", "fixture-agent".into()),
                    ("epoch", "fixture-epoch".into()),
                    ("method", method.into()),
                    (
                        "params",
                        Json::object([
                            ("threadId", "fixture-thread".into()),
                            ("turnId", "fixture-turn".into()),
                        ]),
                    ),
                ]),
            ),
        ]),
    )])
}

#[derive(Default)]
struct HintRuntime(RefCell<Vec<Json>>, Cell<usize>);
impl GenerationRuntime for HintRuntime {
    fn recovered(&self) -> bool {
        true
    }
    fn diagnostics(&self) -> IvyResult<Vec<Json>> {
        Ok(vec![])
    }
    fn task_changed(&self) {}
    fn notification(&self, value: &Json) -> bool {
        self.0.borrow_mut().push(value.clone());
        true
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        Ok(())
    }
    async fn tick(&self) {
        self.1.set(self.1.get() + 1);
    }
    fn stop_admissions(&self) {}
    async fn stop(&self) {}
}

fn hint_body(method: &str, thread: &str, turn: Json, extra: Json) -> Json {
    Json::object([(
        "params",
        Json::object([
            ("serviceNodeId", "fixture-agent".into()),
            (
                "payload",
                Json::object([
                    ("serviceNodeId", "fixture-agent".into()),
                    ("epoch", "fixture-epoch".into()),
                    ("method", method.into()),
                    (
                        "params",
                        Json::object([
                            ("threadId", thread.into()),
                            ("turnId", turn),
                            ("text", extra),
                        ]),
                    ),
                ]),
            ),
        ]),
    )])
}

#[tokio::test(flavor = "current_thread")]
async fn fresh_readiness_resumes_ticks_after_a_degraded_timer_has_expired() {
    let (control, driver) = generation(7, 1000.0);
    let runtime = HintRuntime::default();
    let gate = Gate::default();
    gate.release();
    let public = Public {
        gate: &gate,
        entered: Cell::new(0),
    };
    let clock = ManualClock::new();
    control.state(&ready());
    let mut work = Box::pin(driver.run(&public, &runtime, &clock, async { Ok(()) }));
    pump_pending(work.as_mut()).await;
    assert_eq!(runtime.1.get(), 1);
    let mut degraded = ready();
    degraded.status = "degraded";
    control.state(&degraded);
    pump_pending(work.as_mut()).await;
    clock.advance(2000.0);
    pump_pending(work.as_mut()).await;
    assert_eq!(runtime.1.get(), 1);
    control.state(&ready());
    pump_pending(work.as_mut()).await;
    assert_eq!(runtime.1.get(), 2);
    let retired = control.retire();
    work.await.unwrap();
    retired.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn generation_coalesces_control_and_compact_hints_without_copying_native_bodies() {
    let (control, driver) = generation(7, 1000.0);
    let mut text = Vec::with_capacity(8 * 1024 * 1024);
    text.push(65);
    let huge = hint_body(
        "item/output/delta",
        "thread",
        "turn".into(),
        Json::String(JsString(text)),
    );
    for _ in 0..10_000 {
        control.notification(&huge);
        control.kick();
        control.state(&ready());
    }
    assert_eq!(control.notification_usage().0, 1);
    assert!(control.notification_usage().1 < 8 * 1024);
    // A non-null invalid turn cannot turn into a thread-wide or absent ID.
    control.notification(&hint_body(
        "turn/completed",
        "thread",
        false.into(),
        Json::Null,
    ));
    assert_eq!(control.notification_usage().0, 2);
    let runtime = HintRuntime::default();
    let gate = Gate::default();
    gate.release();
    let public = Public {
        gate: &gate,
        entered: Cell::new(0),
    };
    let clock = ManualClock::new();
    let mut work = Box::pin(driver.run(&public, &runtime, &clock, async { Ok(()) }));
    pump_pending(work.as_mut()).await;
    assert_eq!(control.notification_usage(), (0, 0));
    let values = runtime.0.borrow();
    assert_eq!(values.len(), 2);
    assert!(values.iter().all(|value| value.retained_bytes() < 8 * 1024));
    let completed = values
        .iter()
        .find(|value| {
            value
                .required("params")
                .unwrap()
                .required("payload")
                .unwrap()
                .get("method")
                == Some(&"turn/completed".into())
        })
        .unwrap();
    let body = completed
        .required("params")
        .unwrap()
        .required("payload")
        .unwrap()
        .required("params")
        .unwrap();
    assert_eq!(body.get("turnId"), Some(&Json::Bool(false)));
    assert!(body.get("text").is_none());
    drop(values);
    let retired = control.retire();
    work.await.unwrap();
    retired.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn notification_overload_retires_with_bounded_memory_and_drains_original_calls() {
    for wide in [false, true] {
        let (control, driver) = generation(7, 1000.0);
        let call = control.handler("accepted")(Json::Null, context());
        let mut max_count = 0;
        let mut max_bytes = 0;
        let mut accepted = 0;
        for index in 0..200 {
            let thread = format!(
                "{}-{index}",
                if wide {
                    "x".repeat(1500)
                } else {
                    String::new()
                }
            );
            control.notification(&hint_body(
                "turn/completed",
                &thread,
                "turn".into(),
                Json::Null,
            ));
            let (count, bytes) = control.notification_usage();
            max_count = max_count.max(count);
            max_bytes = max_bytes.max(bytes);
            if count == 0 {
                break;
            }
            accepted += 1;
        }
        assert!(max_count <= 128 && max_bytes <= 256 * 1024);
        if wide {
            assert!(accepted < 128);
        } else {
            assert_eq!(accepted, 128);
        }
        let refused = control.handler("late")(Json::Null, context())
            .await
            .unwrap_err();
        assert_eq!(refused.code, "service_not_ready");
        let gate = Gate::default();
        let public = Public {
            gate: &gate,
            entered: Cell::new(0),
        };
        let runtime = HintRuntime::default();
        let clock = ManualClock::new();
        let drained = Cell::new(false);
        let mut work = Box::pin(driver.run(&public, &runtime, &clock, async {
            drained.set(true);
            Ok(())
        }));
        pump_pending(work.as_mut()).await;
        assert_eq!(public.entered.get(), 1);
        assert!(!drained.get());
        drop(call);
        gate.release();
        tokio::time::timeout(std::time::Duration::from_secs(2), work)
            .await
            .unwrap()
            .unwrap();
        assert!(drained.get());
        assert!(runtime.0.borrow().is_empty());
    }
}
struct Public<'a> {
    gate: &'a Gate,
    entered: Cell<usize>,
}
impl LocalHandler for Public<'_> {
    async fn handle(&self, _: &str, args: Json, context: InvocationContext) -> IvyResult<Json> {
        assert_eq!(context.generation, 7);
        self.entered.set(self.entered.get() + 1);
        self.gate.wait().await;
        Ok(args)
    }
}
fn context() -> InvocationContext {
    InvocationContext::detached(
        "caller".into(),
        Some("operation".into()),
        7,
        RequestOptions::default().cancellation,
        ivy_sdk::IvyError::new("service_unavailable", "Service connection is stopping.")
            .with_outcome(ivy_sdk::Outcome::Unknown),
    )
}
fn ready() -> ServiceState {
    ServiceState {
        status: "ready",
        generation: Some(7),
        code: None,
        message: None,
        phase: None,
    }
}
async fn pump_pending<F: Future>(work: Pin<&mut F>) {
    let mut work = work;
    poll_fn(|cx| {
        for _ in 0..12 {
            assert!(work.as_mut().poll(cx).is_pending());
        }
        Poll::Ready(())
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn generation_retires_pending_page_public_call_and_reconciler_before_final_drain() {
    let input = Json::object([
        (
            "counts",
            Json::Array([1.0, 0.0, 1.0, 0.0].into_iter().map(Json::Number).collect()),
        ),
        ("pausePage", true.into()),
    ]);
    let world = World::new(&input);
    let tasks = LocalExecutor::default();
    let reconciler = Reconciler::new(&world, &world, &tasks);
    let native = Native::default();
    let runtime = ReconcilerRuntime {
        maintenance: None,
        reconciler: &reconciler,
        native: &native,
    };
    let clock = ManualClock::new();
    let (control, driver) = generation(7, 1000.0);
    fn send_sync<T: Send + Sync>(_: &T) {}
    send_sync(&control);
    send_sync(&control.handler("public"));
    let call_gate = Gate::default();
    let public = Public {
        gate: &call_gate,
        entered: Cell::new(0),
    };
    let drain = Gate::default();
    let drain_entered = Cell::new(false);
    let closed = Cell::new(false);
    let mut work = Box::pin(tasks.drive(driver.run(&public, &runtime, &clock, async {
        drain_entered.set(true);
        drain.wait().await;
        closed.set(true);
        Ok(())
    })));
    // Readiness observes the owner without entering the public mailbox.
    native.1.set(true);
    pump_pending(work.as_mut()).await;
    let refused_readiness = control.readiness(context());
    assert_eq!(
        refused_readiness.await.unwrap_err().code,
        "invalid_arguments"
    );
    native.1.set(false);
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_millis(2100)) => {},
        result = work.as_mut() => panic!("Generation ended before readiness refresh: {result:?}"),
    }
    let readiness = control.readiness(context());
    pump_pending(work.as_mut()).await;
    assert_eq!(
        readiness.await.unwrap(),
        Json::object([("ready", true.into()), ("diagnostics", Json::Array(vec![]))])
    );
    control.state(&ready());
    pump_pending(work.as_mut()).await;
    assert_eq!(world.holds.borrow().len(), 1);
    world.release(None);
    // Settle the released owner's callback before advancing the independent
    // manual clock. The production queue follows registered future wakeups.
    pump_pending(work.as_mut()).await;
    // Complete startup Operation scan, then empty Run scan, using the real
    // Reconciler and owner job queue. Runtime adapter sees the exact readiness.
    for _ in 0..2 {
        clock.advance(250.0);
        control.kick();
        pump_pending(work.as_mut()).await;
    }
    assert!(runtime.recovered());
    let now = || "2026-01-02T00:00:00.000Z".into();
    let service = GenerationServiceRuntime {
        runtime: &runtime,
        control: &control,
        now: &now,
    };
    assert!(service.recovered());
    assert_eq!(
        service.observed_at(),
        JsString::from("2026-01-02T00:00:00.000Z")
    );
    service.task_changed();
    // Revisit ordinary Operations, leave its admitted recovery job pending,
    // then advance through Runs to the suspended Task page read.
    for _ in 0..3 {
        clock.advance(250.0);
        service.kick();
        pump_pending(work.as_mut()).await;
    }
    assert!(world.page_paused.get());
    assert_eq!(world.holds.borrow().len(), 1);
    let reply = control.handler("public")(Json::object([("accepted", true.into())]), context());
    control.notification(&native_hint("turn/started"));
    control.notification(&native_hint("turn/completed"));
    pump_pending(work.as_mut()).await;
    assert_eq!(public.entered.get(), 1);
    assert_eq!(native.0.get(), 2);
    assert_eq!(
        world.used.borrow()[2],
        1,
        "A native wake cannot overlap the pending tick."
    );
    let mut stale = ready();
    stale.generation = Some(8);
    stale.status = "offline";
    control.state(&stale);
    let remote = control.clone();
    let mut retired = std::thread::spawn(move || remote.retire()).join().unwrap();
    drop(control.retire());
    assert_eq!(
        control.handler("late")(Json::Null, context())
            .await
            .unwrap_err()
            .code,
        "service_not_ready"
    );
    pump_pending(work.as_mut()).await;
    assert!(!drain_entered.get());
    assert!(!closed.get());
    poll_fn(|cx| {
        assert!(retired.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    world.page_gate.release();
    pump_pending(work.as_mut()).await;
    assert!(
        !drain_entered.get(),
        "An active public call still retains the owner."
    );
    call_gate.release();
    pump_pending(work.as_mut()).await;
    assert_eq!(
        reply.await.unwrap(),
        Json::object([("accepted", true.into())])
    );
    assert!(
        !drain_entered.get(),
        "The admitted recovery job must finish too."
    );
    world.release(None);
    pump_pending(work.as_mut()).await;
    assert!(drain_entered.get());
    assert!(!closed.get());
    assert!(
        world
            .trace
            .borrow()
            .iter()
            .all(|e| e.as_array().unwrap()[0] != Json::from("schedule")),
        "Retirement must prevent a new claim after the retained page returns."
    );
    drain.release();
    work.as_mut().await.unwrap();
    retired.await.unwrap();
    assert!(closed.get());
    drop(work);
    tasks.drive(async {}).await;
}
#[tokio::test(flavor = "current_thread")]
async fn generation_control_loss_and_readiness_failure_drain_without_success_fabrication() {
    for owner_failure in [false, true] {
        let mut input = Json::object([("counts", Json::Array(vec![Json::Number(0.0); 4]))]);
        if owner_failure {
            input.insert(
                "fault",
                Json::object([
                    ("where", "owner".into()),
                    ("code", "service_unavailable".into()),
                    ("execution", "unknown".into()),
                    ("always", true.into()),
                ]),
            );
        }
        let world = World::new(&input);
        let tasks = LocalExecutor::default();
        let reconciler = Reconciler::new(&world, &world, &tasks);
        let native = Native::default();
        let runtime = ReconcilerRuntime {
            maintenance: None,
            reconciler: &reconciler,
            native: &native,
        };
        let clock = ManualClock::new();
        let (control, driver) = generation(7, 1000.0);
        let gate = Gate::default();
        gate.release();
        let public = Public {
            gate: &gate,
            entered: Cell::new(0),
        };
        let handler = control.handler("public");
        let drained = Cell::new(false);
        let mut work = Box::pin(tasks.drive(driver.run(&public, &runtime, &clock, async {
            drained.set(true);
            Ok(())
        })));
        pump_pending(work.as_mut()).await;
        let observed = control.readiness(context()).await;
        if owner_failure {
            let error = observed.unwrap_err();
            assert_eq!(error.code, "service_unavailable");
            assert_eq!(error.outcome, Outcome::Unknown);
        } else {
            assert_eq!(observed.unwrap().get("ready"), Some(&Json::Bool(true)));
        }
        drop(control);
        work.await.unwrap();
        assert!(drained.get());
        assert_eq!(
            handler(Json::Null, context()).await.unwrap_err().code,
            "service_not_ready"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_capacity_keeps_retirement_available_and_drains_every_accepted_call() {
    use ivy_task_board::service_bridge::{MAX_PENDING_CALLS, service_bridge};
    let (bridge, owner) = service_bridge(7);
    let gate = Gate::default();
    gate.release();
    let public = Public {
        gate: &gate,
        entered: Cell::new(0),
    };
    let replies: Vec<_> = (0..MAX_PENDING_CALLS)
        .map(|_| bridge.invoke("public", Json::Null, context()))
        .collect();
    let refused = bridge
        .invoke("public", Json::Null, context())
        .await
        .unwrap_err();
    assert_eq!(refused.code, "limit_exceeded");
    assert_eq!(refused.outcome, Outcome::NotExecuted);
    assert_eq!(bridge.retained_usage().0, MAX_PENDING_CALLS);
    let retired = bridge.retire();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        owner.run(&public, async { Ok(()) }),
    )
    .await
    .unwrap()
    .unwrap();
    retired.await.unwrap();
    assert_eq!(public.entered.get(), MAX_PENDING_CALLS);
    // Completed but unobserved replies still own their reservations.
    assert_eq!(bridge.retained_usage().0, MAX_PENDING_CALLS);
    for reply in replies {
        assert_eq!(reply.await.unwrap(), Json::Null);
    }
    assert_eq!(bridge.retained_usage(), (0, 0));
    assert_eq!(
        bridge
            .invoke("late", Json::Null, context())
            .await
            .unwrap_err()
            .code,
        "service_not_ready"
    );
}

fn retained_text(bytes: usize) -> Json {
    let mut text = Vec::with_capacity(bytes / std::mem::size_of::<u16>());
    text.push(b'x' as u16);
    Json::String(JsString(text))
}

#[tokio::test(flavor = "current_thread")]
async fn public_and_internal_calls_share_count_and_argument_capacity_before_admission() {
    use ivy_task_board::{
        engine_invocations::EngineInvocations,
        journal_store::ReadScopes,
        service_bridge::{MAX_RETAINED_BYTES, service_bridge},
        work_budget::MAX_WORK_ITEMS,
    };
    for bytes_full in [false, true] {
        let (bridge, owner) = service_bridge(7);
        let budget = bridge.work_budget();
        let scopes = ReadScopes::with_budget(budget.clone());
        let executor = LocalExecutor::default();
        let invocations = EngineInvocations::with_budget(&executor, budget.clone());
        let (internal, internal_owner) = invocations.internal_calls(&scopes);
        let mut pending = Vec::new();
        if !bytes_full {
            for _ in 0..MAX_WORK_ITEMS - 1 {
                pending.push(internal.native_update(&Json::Null).unwrap());
            }
        }
        let public = bridge.invoke(
            "public",
            if bytes_full {
                retained_text(MAX_RETAINED_BYTES / 2)
            } else {
                Json::Null
            },
            context(),
        );
        let request = if bytes_full {
            retained_text(MAX_RETAINED_BYTES / 2)
        } else {
            Json::Null
        };
        let failure = internal.native_comment(&request).err().unwrap();
        assert_eq!(failure.code, "limit_exceeded");
        assert_eq!(failure.outcome, Outcome::NotExecuted);
        assert_eq!(
            budget.retained_usage().0,
            if bytes_full { 1 } else { MAX_WORK_ITEMS }
        );
        drop(pending);
        // Losing observers cannot release argument memory still in either queue.
        assert_eq!(
            budget.retained_usage().0,
            if bytes_full { 1 } else { MAX_WORK_ITEMS }
        );
        drop(internal_owner);
        assert_eq!(budget.retained_usage().0, 1);
        drop(public);
        assert_eq!(budget.retained_usage().0, 1);
        drop(owner);
        assert_eq!(budget.retained_usage(), (0, 0));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn internal_reply_pressure_reports_unknown_after_work_and_preserves_public_charge() {
    use ivy_task_board::{
        engine_invocations::EngineInvocations,
        service_bridge::{MAX_RETAINED_BYTES, service_bridge},
    };
    let (bridge, owner) = service_bridge(7);
    let budget = bridge.work_budget();
    let public = bridge.invoke("public", retained_text(MAX_RETAINED_BYTES / 2), context());
    let committed = Cell::new(false);
    let executor = LocalExecutor::default();
    let invocations = EngineInvocations::with_budget(&executor, budget.clone());
    let reply = invocations
        .run(async {
            committed.set(true);
            Ok(retained_text(MAX_RETAINED_BYTES / 2))
        })
        .unwrap();
    executor.drive(invocations.stop()).await;
    assert!(committed.get());
    let failure = reply.await.unwrap_err();
    assert_eq!(failure.code, "limit_exceeded");
    assert_eq!(failure.outcome, Outcome::Unknown);
    assert_eq!(budget.retained_usage().0, 1);
    drop(public);
    assert!(budget.retained_usage().1 >= MAX_RETAINED_BYTES / 2);
    drop(owner);
    assert_eq!(budget.retained_usage(), (0, 0));
}

#[derive(Default)]
struct PendingPublic {
    gate: tokio::sync::Notify,
    entered: Cell<usize>,
}
impl LocalHandler for PendingPublic {
    async fn handle(&self, _: &str, args: Json, _: InvocationContext) -> IvyResult<Json> {
        self.entered.set(self.entered.get() + 1);
        self.gate.notified().await;
        Ok(args)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_admission_counts_backing_capacity_and_keeps_abandoned_accepted_work() {
    use ivy_task_board::service_bridge::{MAX_RETAINED_BYTES, service_bridge};
    let (bridge, owner) = service_bridge(7);
    let public = PendingPublic::default();
    let mut replies = vec![];
    for _ in 0..3 {
        replies.push(bridge.invoke("public", retained_text(MAX_RETAINED_BYTES / 4), context()));
    }
    let refused = bridge
        .invoke("public", retained_text(MAX_RETAINED_BYTES / 4), context())
        .await
        .unwrap_err();
    assert_eq!(refused.code, "limit_exceeded");
    assert_eq!(refused.outcome, Outcome::NotExecuted);
    assert!(bridge.retained_usage().1 >= 3 * MAX_RETAINED_BYTES / 4);
    drop(replies);
    assert_eq!(bridge.retained_usage().0, 3);
    let mut work = Box::pin(owner.run(&public, async { Ok(()) }));
    pump_pending(work.as_mut()).await;
    assert_eq!(public.entered.get(), 3);
    assert_eq!(bridge.retained_usage().0, 3);
    let mut retired = bridge.retire();
    pump_pending(work.as_mut()).await;
    poll_fn(|cx| {
        assert!(retired.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    public.gate.notify_waiters();
    tokio::time::timeout(std::time::Duration::from_secs(2), work)
        .await
        .unwrap()
        .unwrap();
    retired.await.unwrap();
    assert_eq!(public.entered.get(), 3);
    assert_eq!(bridge.retained_usage(), (0, 0));
}

struct LargeReply {
    bytes: usize,
    committed: Cell<usize>,
}
impl LocalHandler for LargeReply {
    async fn handle(&self, _: &str, _: Json, _: InvocationContext) -> IvyResult<Json> {
        self.committed.set(self.committed.get() + 1);
        Ok(retained_text(self.bytes))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_reply_remains_charged_until_observed_and_oversize_preserves_unknown_outcome() {
    use ivy_task_board::service_bridge::{MAX_RETAINED_BYTES, service_bridge};
    for oversize in [false, true] {
        let (bridge, owner) = service_bridge(7);
        let handler = LargeReply {
            bytes: if oversize {
                MAX_RETAINED_BYTES
            } else {
                MAX_RETAINED_BYTES / 4
            },
            committed: Cell::new(0),
        };
        let reply = bridge.invoke("public", Json::Null, context());
        let retired = bridge.retire();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            owner.run(&handler, async { Ok(()) }),
        )
        .await
        .unwrap()
        .unwrap();
        retired.await.unwrap();
        assert_eq!(handler.committed.get(), 1);
        assert_eq!(bridge.retained_usage().0, 1);
        if oversize {
            let failure = reply.await.unwrap_err();
            assert_eq!(failure.code, "limit_exceeded");
            assert_eq!(failure.outcome, Outcome::Unknown);
        } else {
            assert!(bridge.retained_usage().1 >= MAX_RETAINED_BYTES / 4);
            assert_eq!(reply.await.unwrap().as_js_string().unwrap().0.len(), 1);
        }
        assert_eq!(bridge.retained_usage(), (0, 0));
    }
}
