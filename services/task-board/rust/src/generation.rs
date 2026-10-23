//! One connection generation's public calls, readiness, native wakeups and ticks
//! on the existing journal owner. The host owns configuration, health and launch.
use crate::{
    engine_service::ServiceRuntime,
    native_orchestrator::{NativeOrchestrator, OrchestrationEngine, OrchestrationServices},
    operations::Store,
    reconciler::{ReconcileEngine, ReconcileExecutor, ReconcileStore, Reconciler},
    service_bridge::{LocalHandler, ServiceBridge, ServiceOwner, service_bridge},
};
use ivy_contracts::{IvyJson as Json, IvyResult, JsString};
use ivy_sdk::{
    clock::{Clock, SystemClock},
    service::{InvocationContext, ServiceFuture, ServiceState, ToolHandler},
};
use std::{
    cell::RefCell,
    future::{Future, pending, poll_fn},
    pin::Pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Poll, Waker},
    time::Duration,
};
use tokio::sync::mpsc;

#[allow(async_fn_in_trait)]
pub trait GenerationRuntime {
    fn recovered(&self) -> bool;
    fn diagnostics(&self) -> Vec<Json>;
    fn task_changed(&self);
    fn notification(&self, value: &Json) -> bool;
    async fn verify_owner(&self) -> IvyResult<()>;
    async fn tick(&self);
    fn stop_admissions(&self);
    async fn stop(&self);
}
pub trait NativeNotifications {
    fn notification(&self, value: &Json) -> bool;
}
impl<S: Store, E: OrchestrationEngine, V: OrchestrationServices> NativeNotifications
    for NativeOrchestrator<'_, S, E, V>
{
    fn notification(&self, value: &Json) -> bool {
        NativeOrchestrator::notification(self, value)
    }
}
pub struct ReconcilerRuntime<'r, 'a, 'x, S, E, X, N> {
    pub reconciler: &'r Reconciler<'a, 'x, S, E, X>,
    pub native: &'r N,
}
impl<
    'a,
    S: ReconcileStore + 'a,
    E: ReconcileEngine + 'a,
    X: ReconcileExecutor<'a>,
    N: NativeNotifications,
> GenerationRuntime for ReconcilerRuntime<'_, 'a, '_, S, E, X, N>
{
    fn recovered(&self) -> bool {
        self.reconciler.recovered()
    }
    fn diagnostics(&self) -> Vec<Json> {
        self.reconciler.diagnostics()
    }
    fn task_changed(&self) {
        self.reconciler.task_changed();
    }
    fn notification(&self, value: &Json) -> bool {
        let native = self.native.notification(value);
        self.reconciler.notification(value) || native
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        self.reconciler.verify_owner().await
    }
    async fn tick(&self) {
        self.reconciler.tick().await;
    }
    fn stop_admissions(&self) {
        self.reconciler.stop_admissions();
    }
    async fn stop(&self) {
        self.reconciler.stop().await;
    }
}
#[allow(async_fn_in_trait)]
pub trait GenerationClock {
    fn now_millis(&self) -> f64;
    async fn sleep(&self, delay_ms: f64);
}
impl GenerationClock for SystemClock {
    fn now_millis(&self) -> f64 {
        self.utc_millis() as f64
    }
    async fn sleep(&self, delay_ms: f64) {
        tokio::time::sleep(Duration::from_millis(delay_ms.max(0.0) as u64)).await;
    }
}
enum Command {
    Ready(bool),
    Kick,
    Notification(Json),
    Retire,
}
#[derive(Clone)]
pub struct GenerationControl {
    generation: u64,
    bridge: ServiceBridge,
    sender: mpsc::UnboundedSender<Command>,
    stopping: Arc<AtomicBool>,
}
pub struct Generation {
    bridge: ServiceBridge,
    owner: ServiceOwner,
    receiver: mpsc::UnboundedReceiver<Command>,
    interval_ms: f64,
    stopping: Arc<AtomicBool>,
}
pub fn generation(generation: u64, interval_ms: f64) -> (GenerationControl, Generation) {
    let (bridge, owner) = service_bridge(generation);
    let (sender, receiver) = mpsc::unbounded_channel();
    let stopping = Arc::new(AtomicBool::new(false));
    (
        GenerationControl {
            generation,
            bridge: bridge.clone(),
            sender,
            stopping: stopping.clone(),
        },
        Generation {
            bridge,
            owner,
            receiver,
            interval_ms,
            stopping,
        },
    )
}
impl GenerationControl {
    pub fn handler(&self, tool: &str) -> ToolHandler {
        self.bridge.handler(tool)
    }
    pub fn readiness(&self, context: InvocationContext) -> ServiceFuture<Json> {
        self.bridge.invoke("@readiness", Json::Null, context)
    }
    pub fn kick(&self) {
        if !self.stopping.load(Ordering::Acquire) {
            let _ = self.sender.send(Command::Kick);
        }
    }
    pub fn notification(&self, value: &Json) {
        if !self.stopping.load(Ordering::Acquire) {
            let _ = self.sender.send(Command::Notification(value.clone()));
        }
    }
    pub fn state(&self, value: &ServiceState) {
        if self.stopping.load(Ordering::Acquire) {
            return;
        }
        if value.generation.is_some_and(|g| g != self.generation) {
            return;
        }
        if ["offline", "stopped", "connecting"].contains(&value.status) {
            drop(self.retire());
        } else {
            let _ = self.sender.send(Command::Ready(value.status == "ready"));
        }
    }
    pub fn retire(&self) -> ServiceFuture<()> {
        self.stopping.store(true, Ordering::Release);
        let retired = self.bridge.retire();
        let _ = self.sender.send(Command::Retire);
        retired
    }
}
pub struct GenerationServiceRuntime<'a, R, C> {
    pub runtime: &'a R,
    pub control: &'a GenerationControl,
    pub now: &'a C,
}
impl<R: GenerationRuntime, C: Fn() -> JsString> ServiceRuntime
    for GenerationServiceRuntime<'_, R, C>
{
    fn recovered(&self) -> bool {
        self.runtime.recovered()
    }
    fn observed_at(&self) -> JsString {
        (self.now)()
    }
    fn task_changed(&self) {
        self.runtime.task_changed();
    }
    fn kick(&self) {
        self.control.kick();
    }
}
struct Handler<'a, H, R> {
    public: &'a H,
    runtime: &'a R,
}
impl<H: LocalHandler, R: GenerationRuntime> LocalHandler for Handler<'_, H, R> {
    async fn handle(&self, tool: &str, args: Json, context: InvocationContext) -> IvyResult<Json> {
        if tool != "@readiness" {
            return self.public.handle(tool, args, context).await;
        }
        self.runtime.verify_owner().await?;
        Ok(Json::object([
            ("ready", true.into()),
            ("diagnostics", Json::Array(self.runtime.diagnostics())),
        ]))
    }
}
/// The source's wall-clock scheduling decisions; timer futures retain their
/// original monotonic deadline when another command leaves timer_at unchanged.
pub struct TickSchedule {
    interval_ms: f64,
    ready: bool,
    started: bool,
    stopped: bool,
    active: bool,
    wake_pending: bool,
    last_started_at: f64,
    timer_at: Option<f64>,
    waiters: Vec<Waker>,
}
impl TickSchedule {
    pub fn new(interval_ms: f64) -> Self {
        Self {
            interval_ms,
            ready: false,
            started: false,
            stopped: false,
            active: false,
            wake_pending: false,
            last_started_at: 0.0,
            timer_at: None,
            waiters: vec![],
        }
    }
    pub fn timer_at(&self) -> Option<f64> {
        self.timer_at
    }
    fn schedule(&mut self, now: f64, delay: f64) {
        let at = now + delay;
        if self.timer_at.is_none_or(|current| current > at) {
            self.timer_at = Some(at);
        }
    }
    pub fn ready(&mut self, ready: bool, now: f64) -> bool {
        self.ready = ready;
        if ready && !self.started {
            self.started = true;
            self.kick(now)
        } else {
            false
        }
    }
    pub fn kick(&mut self, now: f64) -> bool {
        if self.stopped || !self.ready {
            return false;
        }
        if self.active {
            self.wake_pending = true;
            return false;
        }
        let delay = (self.interval_ms.min(250.0) - (now - self.last_started_at)).max(0.0);
        if delay > 0.0 {
            self.schedule(now, delay);
            return false;
        }
        self.wake_pending = false;
        self.timer_at = None;
        self.active = true;
        self.last_started_at = now;
        true
    }
    pub fn timer(&mut self, now: f64) -> bool {
        self.timer_at = None;
        self.kick(now)
    }
    pub fn finished(&mut self, now: f64, recovered: bool) {
        self.active = false;
        for waker in self.waiters.drain(..) {
            waker.wake();
        }
        if !self.stopped {
            self.schedule(
                now,
                if !recovered {
                    50.0
                } else if self.wake_pending {
                    0.0
                } else {
                    self.interval_ms
                },
            );
        }
    }
    pub fn stop(&mut self) {
        self.stopped = true;
        self.timer_at = None;
    }
    async fn settled(state: &RefCell<Self>) {
        poll_fn(|cx| {
            let mut state = state.borrow_mut();
            if !state.active {
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
}
type Work<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;
async fn optional(work: &mut Option<Work<'_>>) {
    match work {
        Some(work) => work.await,
        None => pending().await,
    }
}
impl Generation {
    /// Drive the active tick concurrently with accepted calls. Retirement stops
    /// fresh tick/reconciler admissions, then awaits both the retained tick and
    /// admitted reconciler work before the required execution/projection drain.
    pub async fn run<H: LocalHandler, R: GenerationRuntime, C: GenerationClock>(
        mut self,
        handler: &H,
        runtime: &R,
        clock: &C,
        drain: impl Future<Output = IvyResult<()>>,
    ) -> IvyResult<()> {
        let ticks = Rc::new(RefCell::new(TickSchedule::new(self.interval_ms)));
        let selected = Handler {
            public: handler,
            runtime,
        };
        let state = ticks.clone();
        let mut owner = Box::pin(self.owner.run(&selected, async {
            state.borrow_mut().stop();
            runtime.stop_admissions();
            let (_, ()) = tokio::join!(runtime.stop(), TickSchedule::settled(&state));
            drain.await
        }));
        let mut tick: Option<Work<'_>> = None;
        let mut timer: Option<Work<'_>> = None;
        let mut timer_at = None;
        let mut commands_open = true;
        loop {
            if self.stopping.load(Ordering::Acquire) {
                ticks.borrow_mut().stop();
                runtime.stop_admissions();
                commands_open = false;
            }
            let at = ticks.borrow().timer_at;
            if at != timer_at {
                timer = at.map(|at| {
                    Box::pin(clock.sleep((at - clock.now_millis()).max(0.0))) as Work<'_>
                });
                timer_at = at;
            }
            let mut start = false;
            tokio::select! {
                result = &mut owner => return result,
                command = self.receiver.recv(), if commands_open => {
                    start = match command {
                        Some(Command::Ready(ready)) => ticks.borrow_mut().ready(ready, clock.now_millis()),
                        Some(Command::Kick) => ticks.borrow_mut().kick(clock.now_millis()),
                        Some(Command::Notification(value)) => runtime.notification(&value) && ticks.borrow_mut().kick(clock.now_millis()),
                        Some(Command::Retire) | None => {
                            drop(self.bridge.retire());
                            commands_open = false;
                            ticks.borrow_mut().stop();
                            runtime.stop_admissions();
                            false
                        }
                    };
                },
                _ = optional(&mut tick) => {
                    tick = None;
                    ticks.borrow_mut().finished(clock.now_millis(), runtime.recovered());
                },
                _ = optional(&mut timer) => {
                    timer = None; timer_at = None;
                    start = ticks.borrow_mut().timer(clock.now_millis());
                }
            }
            if start {
                tick = Some(Box::pin(runtime.tick()));
            }
        }
    }
}
