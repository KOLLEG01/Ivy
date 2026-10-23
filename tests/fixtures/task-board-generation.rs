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
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-generation-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
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
struct Native(Cell<usize>);
impl NativeNotifications for Native {
    fn notification(&self, value: &Json) -> bool {
        self.0.set(self.0.get() + 1);
        value
            .get("matched")
            .and_then(Json::as_bool)
            .unwrap_or(false)
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
    // Readiness is a retained local request even before autonomous tick start.
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
    control.notification(&Json::object([("matched", false.into())]));
    control.notification(&Json::object([("matched", true.into())]));
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
        let reply = control.readiness(context());
        let drained = Cell::new(false);
        if owner_failure {
            drop(control.retire());
        }
        drop(control);
        tasks
            .drive(driver.run(&public, &runtime, &clock, async {
                drained.set(true);
                Ok(())
            }))
            .await
            .unwrap();
        assert!(drained.get());
        let observed = reply.await;
        if owner_failure {
            let error = observed.unwrap_err();
            assert_eq!(error.code, "service_unavailable");
            assert_eq!(error.outcome, Outcome::Unknown);
        } else {
            assert_eq!(observed.unwrap().get("ready"), Some(&Json::Bool(true)));
        }
        assert_eq!(
            handler(Json::Null, context()).await.unwrap_err().code,
            "service_not_ready"
        );
    }
}
