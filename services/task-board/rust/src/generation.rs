//! One connection generation's public calls, readiness, native wakeups and ticks
//! on the existing journal owner. The host owns configuration, health and launch.
use crate::{
    engine_service::ServiceRuntime,
    native_orchestrator::{NativeOrchestrator, OrchestrationEngine, OrchestrationServices},
    operations::Store,
    reconciler::{ReconcileEngine, ReconcileExecutor, ReconcileStore, Reconciler},
    service_bridge::{LocalHandler, ServiceBridge, ServiceOwner, service_bridge},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::{
    clock::{Clock, SystemClock, date_to_iso},
    service::{InvocationContext, ServiceFuture, ServiceState, ToolHandler},
};
use std::{
    cell::RefCell,
    future::{Future, pending, poll_fn},
    pin::Pin,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Poll, Waker},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

#[allow(async_fn_in_trait)]
pub trait GenerationRuntime {
    fn recovered(&self) -> bool;
    fn diagnostics(&self) -> IvyResult<Vec<Json>>;
    fn task_changed(&self);
    fn notification(&self, value: &Json) -> bool;
    async fn verify_owner(&self) -> IvyResult<()>;
    async fn tick(&self);
    fn stop_admissions(&self);
    async fn stop(&self);
}
pub trait NativeNotifications {
    fn diagnostics(&self) -> IvyResult<Vec<Json>>;
    fn notification(&self, value: &Json) -> bool;
}
impl<S: Store, E: OrchestrationEngine, V: OrchestrationServices> NativeNotifications
    for NativeOrchestrator<'_, S, E, V>
{
    fn diagnostics(&self) -> IvyResult<Vec<Json>> {
        NativeOrchestrator::diagnostics(self)
    }
    fn notification(&self, value: &Json) -> bool {
        NativeOrchestrator::notification(self, value)
    }
}
pub struct ReconcilerRuntime<'r, 'a, 'x, S, E, X, N> {
    pub reconciler: &'r Reconciler<'a, 'x, S, E, X>,
    pub native: &'r N,
    pub maintenance: Option<&'r dyn crate::maintenance::MaintenanceTick>,
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
    fn diagnostics(&self) -> IvyResult<Vec<Json>> {
        let mut diagnostics = self.reconciler.diagnostics();
        diagnostics.extend(self.native.diagnostics()?);
        if let Some(maintenance) = self.maintenance {
            diagnostics.extend(maintenance.diagnostics());
        }
        Ok(diagnostics)
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
        if let Some(maintenance) = self.maintenance {
            maintenance.tick().await;
        }
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
const MAX_NOTIFICATION_COUNT: usize = 128;
const MAX_NOTIFICATION_BYTES: usize = 256 * 1024;
// Reserve the bounded Vec's unused slots as well as occupied hint capacity.
const MAX_NOTIFICATION_PAYLOAD_BYTES: usize =
    MAX_NOTIFICATION_BYTES - MAX_NOTIFICATION_COUNT * std::mem::size_of::<Json>();
const MAX_HINT_BYTES: usize = 8 * 1024;
const READINESS_INTERVAL: Duration = Duration::from_secs(2);
const READINESS_MAX_AGE: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Inputs {
    ready: Option<bool>,
    kick: bool,
    notifications: Vec<Json>,
    bytes: usize,
    overflowed: bool,
}
struct Readiness {
    observed: Instant,
    utc_millis: f64,
    result: IvyResult<Json>,
}
impl Readiness {
    fn observe(result: IvyResult<Vec<Json>>) -> Self {
        let utc_millis = SystemClock.utc_millis() as f64;
        Self { observed: Instant::now(), utc_millis, result: result.and_then(|diagnostics| {
            // Heartbeats allow at most 100 diagnostics. Keep a bounded snapshot
            // before copying it across the I/O boundary, including spare slots.
            let mut kept = Vec::with_capacity(diagnostics.len().min(100));
            let mut bytes = 8192usize;
            let mut omitted = 0;
            for diagnostic in diagnostics {
                let charge = diagnostic.retained_bytes();
                if kept.len() < 99 && charge <= 128 * 1024 - bytes {
                    bytes += charge;
                    kept.push(diagnostic);
                } else {
                    omitted += 1;
                }
            }
            if omitted != 0 {
                kept.push(control_diagnostic("task_board_diagnostics_truncated",
                    &format!("{omitted} additional workflow diagnostics are omitted from this heartbeat."),
                    utc_millis, utc_millis)?);
            }
            Ok(Json::object([("ready", true.into()), ("diagnostics", Json::Array(kept))]))
        }) }
    }
}
fn control_diagnostic(code: &str, message: &str, first: f64, last: f64) -> IvyResult<Json> {
    Ok(Json::object([
        ("code", code.into()),
        ("resource", Json::Null),
        ("source", "task-board".into()),
        ("severity", "warning".into()),
        ("status", "current".into()),
        ("firstObservedAt", date_to_iso(first)?.into()),
        ("lastObservedAt", date_to_iso(last)?.into()),
        ("message", message.into()),
    ]))
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn scalar(value: &Json) -> &Json {
    static INVALID: Json = Json::Bool(false);
    match value {
        Json::Null | Json::String(_) => value,
        _ => &INVALID,
    }
}

/// Keep only fields used by native adoption, progress and reconciliation.
/// Invalid non-null turn IDs stay invalid; they must never become absent IDs.
/// A fixed small scratch projection is checked before any string is copied.
fn notification_hint(value: &Json) -> Result<Option<Json>, ()> {
    static INVALID: Json = Json::Bool(false);
    let params = field(value, "params");
    let event = field(params, "payload");
    let body = field(event, "params");
    let method = field(event, "method");
    let method = if method
        .as_js_string()
        .is_some_and(|v| v.0.starts_with(&[105, 116, 101, 109, 47]))
    {
        "item/"
    } else if method == &Json::from("turn/started") {
        "turn/started"
    } else if method == &Json::from("turn/completed") {
        "turn/completed"
    } else if method == &Json::from("thread/status/changed") {
        "thread/status/changed"
    } else if method == &Json::from("serverRequest/resolved") {
        "serverRequest/resolved"
    } else {
        return Ok(None);
    };
    // Neither consumer can match a non-string node or thread identity.
    if field(params, "serviceNodeId").as_js_string().is_none()
        || field(body, "threadId").as_js_string().is_none()
    {
        return Ok(None);
    }
    let fields = [
        scalar(field(params, "serviceNodeId")),
        scalar(field(event, "serviceNodeId")),
        scalar(field(event, "epoch")),
        scalar(field(body, "threadId")),
        scalar(field(body, "turnId")),
        scalar(field(field(body, "turn"), "id")),
    ];
    let backend = event.get("backend").map(|backend| {
        if backend.as_object().is_some_and(|v| v.len() == 3)
            && ["backendId", "provider", "accountId"]
                .iter()
                .all(|key| backend.get(key).and_then(Json::as_js_string).is_some())
        {
            backend
        } else {
            &INVALID
        }
    });
    let mut bytes = 4096usize;
    for value in fields.into_iter().chain(
        backend
            .into_iter()
            .flat_map(|value| ["backendId", "provider", "accountId"].map(|key| field(value, key))),
    ) {
        if let Some(value) = value.as_js_string() {
            bytes = bytes
                .checked_add(value.0.len().checked_mul(2).ok_or(())?)
                .ok_or(())?;
            if bytes > MAX_HINT_BYTES {
                return Err(());
            }
        }
    }
    let mut event = Json::object([
        ("method", method.into()),
        ("serviceNodeId", fields[1].clone()),
        ("epoch", fields[2].clone()),
        (
            "params",
            Json::object([
                ("threadId", fields[3].clone()),
                ("turnId", fields[4].clone()),
                ("turn", Json::object([("id", fields[5].clone())])),
            ]),
        ),
    ]);
    if let Some(backend) = backend {
        event.insert("backend", backend.clone());
    }
    let hint = Json::object([
        ("method", "notifications.provider".into()),
        (
            "params",
            Json::object([
                ("namespace", "agent".into()),
                ("name", "notification".into()),
                ("serviceNodeId", fields[0].clone()),
                ("payload", event),
            ]),
        ),
    ]);
    if hint.retained_bytes() > MAX_HINT_BYTES {
        return Err(());
    }
    Ok(Some(hint))
}
#[derive(Clone)]
pub struct GenerationControl {
    generation: u64,
    bridge: ServiceBridge,
    sender: mpsc::Sender<()>,
    inputs: Arc<Mutex<Inputs>>,
    readiness: Arc<Mutex<Option<Readiness>>>,
    stopping: Arc<AtomicBool>,
}
pub struct Generation {
    bridge: ServiceBridge,
    owner: ServiceOwner,
    receiver: mpsc::Receiver<()>,
    inputs: Arc<Mutex<Inputs>>,
    readiness: Arc<Mutex<Option<Readiness>>>,
    interval_ms: f64,
    stopping: Arc<AtomicBool>,
}
pub fn generation(generation: u64, interval_ms: f64) -> (GenerationControl, Generation) {
    let (bridge, owner) = service_bridge(generation);
    let (sender, receiver) = mpsc::channel(1);
    let inputs = Arc::new(Mutex::new(Inputs::default()));
    let readiness = Arc::new(Mutex::new(None));
    let stopping = Arc::new(AtomicBool::new(false));
    (
        GenerationControl {
            generation,
            bridge: bridge.clone(),
            sender,
            inputs: inputs.clone(),
            readiness: readiness.clone(),
            stopping: stopping.clone(),
        },
        Generation {
            bridge,
            owner,
            receiver,
            inputs,
            readiness,
            interval_ms,
            stopping,
        },
    )
}
impl GenerationControl {
    pub fn work_budget(&self) -> crate::work_budget::WorkBudget {
        self.bridge.work_budget()
    }
    pub fn handler(&self, tool: &str) -> ToolHandler {
        self.bridge.handler(tool)
    }
    pub fn readiness(&self, context: InvocationContext) -> ServiceFuture<Json> {
        let stopping = self.stopping.clone();
        let readiness = self.readiness.clone();
        let same_generation = context.generation == self.generation;
        Box::pin(async move {
            if stopping.load(Ordering::Acquire) || !same_generation {
                Err(IvyError::new(
                    "service_not_ready",
                    "Workflow state is not attached to this generation.",
                ))
            } else {
                let readiness = readiness.lock().unwrap();
                match readiness.as_ref() {
                    Some(value) if value.observed.elapsed() <= READINESS_MAX_AGE => {
                        value.result.clone()
                    }
                    Some(value) => control_diagnostic(
                        "task_board_journal_busy",
                        "The journal owner has not refreshed its readiness observation.",
                        value.utc_millis + READINESS_MAX_AGE.as_millis() as f64,
                        SystemClock.utc_millis() as f64,
                    )
                    .map(|diagnostic| {
                        Json::object([
                            ("ready", false.into()),
                            ("diagnostics", Json::Array(vec![diagnostic])),
                        ])
                    }),
                    None => Err(IvyError::new(
                        "service_not_ready",
                        "Workflow owner verification has not completed.",
                    )),
                }
            }
        })
    }
    pub(crate) fn owner_readiness(&self, result: IvyResult<Vec<Json>>) {
        *self.readiness.lock().unwrap() = Some(Readiness::observe(result));
    }
    pub fn kick(&self) {
        if !self.stopping.load(Ordering::Acquire) {
            self.inputs.lock().unwrap().kick = true;
            let _ = self.sender.try_send(());
        }
    }
    pub fn notification(&self, value: &Json) {
        if self.stopping.load(Ordering::Acquire) {
            return;
        }
        let mut inputs = self.inputs.lock().unwrap();
        if self.stopping.load(Ordering::Acquire) {
            return;
        }
        let hint = match notification_hint(value) {
            Ok(None) => return,
            Ok(Some(hint)) if inputs.notifications.contains(&hint) => return,
            Ok(Some(hint))
                if inputs.notifications.len() < MAX_NOTIFICATION_COUNT
                    && hint.retained_bytes() <= MAX_NOTIFICATION_PAYLOAD_BYTES - inputs.bytes =>
            {
                hint
            }
            _ => {
                inputs.overflowed = true;
                drop(inputs);
                // Hints are not durable work. Retire and reconcile the original
                // durable operations on a fresh connection rather than silently
                // losing progress/adoption evidence or retaining arbitrary bodies.
                drop(self.retire());
                return;
            }
        };
        inputs.bytes += hint.retained_bytes();
        inputs.notifications.push(hint);
        let _ = self.sender.try_send(());
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
            self.inputs.lock().unwrap().ready = Some(value.status == "ready");
            let _ = self.sender.try_send(());
        }
    }
    pub fn retire(&self) -> ServiceFuture<()> {
        self.stopping.store(true, Ordering::Release);
        let retired = self.bridge.retire();
        let mut inputs = self.inputs.lock().unwrap();
        inputs.notifications.clear();
        inputs.bytes = 0;
        let _ = self.sender.try_send(());
        retired
    }
    pub(crate) fn notification_overflowed(&self) -> bool {
        self.inputs.lock().unwrap().overflowed
    }
    pub fn notification_usage(&self) -> (usize, usize) {
        let inputs = self.inputs.lock().unwrap();
        (inputs.notifications.len(), inputs.bytes)
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
        let state = ticks.clone();
        let mut owner = Box::pin(self.owner.run(handler, async {
            state.borrow_mut().stop();
            runtime.stop_admissions();
            let (_, ()) = tokio::join!(runtime.stop(), TickSchedule::settled(&state));
            drain.await
        }));
        let mut tick: Option<Work<'_>> = None;
        let mut timer: Option<Work<'_>> = None;
        let mut timer_at = None;
        let mut commands_open = true;
        let mut readiness = Box::pin(async {
            let initialized = self.readiness.lock().unwrap().is_some();
            if initialized {
                tokio::time::sleep(READINESS_INTERVAL).await;
            }
            loop {
                let result = runtime
                    .verify_owner()
                    .await
                    .and_then(|()| runtime.diagnostics());
                *self.readiness.lock().unwrap() = Some(Readiness::observe(result));
                tokio::time::sleep(READINESS_INTERVAL).await;
            }
        });
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
            let pending_inputs = {
                let inputs = self.inputs.lock().unwrap();
                inputs.ready.is_some() || inputs.kick || !inputs.notifications.is_empty()
            };
            tokio::select! {
                result = &mut owner => return result,
                _ = &mut readiness, if commands_open => unreachable!(),
                command = async { if pending_inputs { Some(()) } else { self.receiver.recv().await } }, if commands_open => {
                    start = match command {
                        Some(()) if !self.stopping.load(Ordering::Acquire) => {
                            let (ready, kick, notification) = {
                                let mut inputs = self.inputs.lock().unwrap();
                                let notification = inputs.notifications.pop();
                                if let Some(value) = &notification { inputs.bytes -= value.retained_bytes(); }
                                (inputs.ready.take(), std::mem::take(&mut inputs.kick), notification)
                            };
                            let mut start = ready.is_some_and(|ready| {
                                let resumed = ready && !ticks.borrow().ready;
                                let started = ticks.borrow_mut().ready(ready, clock.now_millis());
                                started || (resumed && ticks.borrow_mut().kick(clock.now_millis()))
                            });
                            let notified = notification.as_ref().is_some_and(|value| runtime.notification(value));
                            if kick || notified {
                                start |= ticks.borrow_mut().kick(clock.now_millis());
                            }
                            start
                        },
                        Some(()) | None => {
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

#[cfg(test)]
#[path = "../../../../tests/fixtures/task-board-generation-readiness.rs"]
mod tests;
