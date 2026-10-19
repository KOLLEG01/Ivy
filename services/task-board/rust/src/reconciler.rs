//! Fair bounded recovery scans on the journal owner's local async executor.
//! Admitted tasks survive a tick and are drained before the store is closed.
use crate::{
    local_gates::PendingGate,
    native_inspector::InspectionStore,
    native_orchestrator::{NativeOrchestrator, OrchestrationEngine, OrchestrationServices},
    native_signals::NativeSignals,
    operations::Store,
    scheduler::{
        Scheduler, SchedulerAllocation, SchedulerEngine, SchedulerOperations, SchedulerPolicies,
        SchedulerRuntime, SchedulerStore,
    },
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome};
use ivy_sdk::rpc::RpcClient;
use std::{
    cell::RefCell,
    collections::HashMap,
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    task::{Poll, Waker},
};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Lane {
    Operations,
    Runs,
    Tasks,
    Deliveries,
}
const LANES: [Lane; 4] = [Lane::Operations, Lane::Runs, Lane::Tasks, Lane::Deliveries];
impl Lane {
    pub fn key(self) -> &'static str {
        match self {
            Self::Operations => "task-board/operation",
            Self::Runs => "task-board/run",
            Self::Tasks => "task-board/task",
            Self::Deliveries => "task-board/delivery",
        }
    }
}
#[allow(async_fn_in_trait)]
pub trait ReconcileStore: SchedulerStore {
    fn pending_coordination_page(
        &self,
        after: Option<&JsString>,
        limit: i64,
    ) -> IvyResult<Vec<PendingGate>>;
    async fn collect_local(&self) -> IvyResult<()>;
}
/// The binding starts work on the same local owner executor, retains it after
/// tick returns, and never cancels it before settled/stop has drained it.
pub trait ReconcileExecutor<'a> {
    fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + 'a>>);
}
#[allow(async_fn_in_trait)]
pub trait ReconcileEngine {
    fn service_node_id(&self) -> &JsString;
    fn page_size(&self) -> f64;
    fn now_millis(&self) -> f64;
    fn now(&self) -> JsString;
    async fn verify_owner(&self) -> IvyResult<()>;
    async fn recover_operation(&self, id: &JsString) -> IvyResult<()>;
    async fn sync_native_comments(&self, id: &JsString) -> IvyResult<()>;
    async fn drain_native(&self, id: &JsString) -> IvyResult<bool>;
    async fn native_signals(&self, id: &JsString) -> IvyResult<()>;
    async fn schedule(&self, id: &JsString) -> IvyResult<()>;
    async fn delivery(&self, id: &JsString) -> IvyResult<()>;
}
#[allow(async_fn_in_trait)]
pub trait ReconcilePolicies: SchedulerPolicies {
    fn page_size(&self) -> f64;
    fn now_millis(&self) -> f64;
    async fn sync_native_comments(&self, id: &JsString) -> IvyResult<()>;
    async fn delivery(&self, id: &JsString) -> IvyResult<()>;
}
#[allow(async_fn_in_trait)]
pub trait ReconcileNative {
    async fn drain(&self, id: &JsString) -> IvyResult<bool>;
}
impl<S: Store, E: OrchestrationEngine, V: OrchestrationServices> ReconcileNative
    for NativeOrchestrator<'_, S, E, V>
{
    async fn drain(&self, id: &JsString) -> IvyResult<bool> {
        NativeOrchestrator::drain(self, id).await
    }
}
#[allow(async_fn_in_trait)]
pub trait ReconcileSignals {
    async fn step(&self, id: &JsString) -> IvyResult<()>;
}
impl<S: InspectionStore> ReconcileSignals for NativeSignals<'_, S> {
    async fn step(&self, id: &JsString) -> IvyResult<()> {
        NativeSignals::step(self, id).await.map(|_| ())
    }
}
#[allow(async_fn_in_trait)]
pub trait ReconcileSchedule {
    async fn task(&self, id: &JsString) -> IvyResult<()>;
}
impl<S: SchedulerStore, E: SchedulerEngine, A: SchedulerAllocation, O: SchedulerOperations>
    ReconcileSchedule for Scheduler<'_, S, E, A, O>
{
    async fn task(&self, id: &JsString) -> IvyResult<()> {
        Scheduler::task(self, id).await.map(|_| ())
    }
}
/// Concrete composition uses the delivered scheduler/native/signals programs;
/// current dispatch, mirroring and delivery remain the owning engine's policies.
pub struct ReconcileComponents<'a, R, P, N, T, G> {
    pub runtime: &'a SchedulerRuntime<R, P>,
    pub native: &'a N,
    pub scheduler: &'a T,
    pub signals: &'a G,
}
impl<
    R: RpcClient + 'static,
    P: ReconcilePolicies,
    N: ReconcileNative,
    T: ReconcileSchedule,
    G: ReconcileSignals,
> ReconcileEngine for ReconcileComponents<'_, R, P, N, T, G>
{
    fn service_node_id(&self) -> &JsString {
        &self.runtime.service_node_id
    }
    fn page_size(&self) -> f64 {
        self.runtime.policies.page_size()
    }
    fn now_millis(&self) -> f64 {
        self.runtime.policies.now_millis()
    }
    fn now(&self) -> JsString {
        self.runtime.policies.now()
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        SchedulerEngine::verify_owner(self.runtime).await
    }
    async fn recover_operation(&self, id: &JsString) -> IvyResult<()> {
        self.runtime
            .policies
            .recover_operation(id)
            .await
            .map(|_| ())
    }
    async fn sync_native_comments(&self, id: &JsString) -> IvyResult<()> {
        self.runtime.policies.sync_native_comments(id).await
    }
    async fn drain_native(&self, id: &JsString) -> IvyResult<bool> {
        self.native.drain(id).await
    }
    async fn native_signals(&self, id: &JsString) -> IvyResult<()> {
        self.signals.step(id).await
    }
    async fn schedule(&self, id: &JsString) -> IvyResult<()> {
        self.scheduler.task(id).await
    }
    async fn delivery(&self, id: &JsString) -> IvyResult<()> {
        self.runtime.policies.delivery(id).await
    }
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn prefix(p: &str, id: &JsString) -> JsString {
    JsString(p.encode_utf16().chain(id.0.iter().copied()).collect())
}
fn query(l: Lane, limit: f64, c: Option<&JsString>) -> Json {
    let where_ = if l == Lane::Tasks {
        Json::object([
            ("op", "and".into()),
            (
                "args",
                Json::Array(vec![
                    Json::object([
                        ("op", "eq".into()),
                        ("field", "data:/fields/control".into()),
                        ("value", "agent".into()),
                    ]),
                    Json::object([
                        ("op", "in".into()),
                        ("field", "data:/workflowState".into()),
                        (
                            "value",
                            Json::Array(["todo", "waiting", "review"].map(Json::from).to_vec()),
                        ),
                    ]),
                ]),
            ),
        ])
    } else {
        Json::object([
            ("op", "in".into()),
            (
                "field",
                if l == Lane::Deliveries {
                    "data:/state"
                } else {
                    "data:/phase"
                }
                .into(),
            ),
            (
                "value",
                Json::Array(
                    match l {
                        Lane::Operations => {
                            vec!["accepted", "preparing", "publishing", "needs_attention"]
                        }
                        Lane::Runs => vec![
                            "starting",
                            "running",
                            "waiting_input",
                            "publishing_result",
                            "cancel_requested",
                            "outcome_unknown",
                        ],
                        _ => vec!["pending", "dispatching", "outcome_unknown"],
                    }
                    .into_iter()
                    .map(Json::from)
                    .collect(),
                ),
            ),
        ])
    };
    let mut order = vec![];
    if l == Lane::Tasks {
        order.push(Json::object([
            ("field", "data:/fields/priority".into()),
            ("direction", "desc".into()),
        ]));
    }
    order.push(Json::object([
        ("field", "object.id".into()),
        ("direction", "asc".into()),
    ]));
    let mut q = Json::object([
        ("where", where_),
        ("limit", Json::Number(limit)),
        ("orderBy", Json::Array(order)),
    ]);
    if let Some(c) = c.filter(|s| !s.0.is_empty()) {
        q.insert("cursor", Json::String(c.clone()));
    }
    q
}
#[derive(Default)]
struct Completion {
    done: bool,
    waiters: Vec<Waker>,
}
struct PendingPage {
    ids: Vec<JsString>,
    next: Option<JsString>,
    task_changes: u64,
}
struct State {
    recovering: Option<Lane>,
    recovery_scanned: bool,
    lane: usize,
    task_changes: u64,
    busy: bool,
    last_cleanup: f64,
    gate_cursor: Option<JsString>,
    stopped: bool,
    active: Vec<(JsString, Rc<RefCell<Completion>>)>,
    pages: HashMap<Lane, Rc<RefCell<PendingPage>>>,
    cursors: HashMap<Lane, Option<JsString>>,
    failures: Vec<(JsString, Json)>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            recovering: Some(Lane::Operations),
            recovery_scanned: false,
            lane: 0,
            task_changes: 0,
            busy: false,
            last_cleanup: 0.0,
            gate_cursor: None,
            stopped: false,
            active: vec![],
            pages: HashMap::new(),
            cursors: HashMap::new(),
            failures: vec![],
        }
    }
}
struct Shared<'a, S, E> {
    store: &'a S,
    engine: &'a E,
    state: Rc<RefCell<State>>,
}
struct Finished {
    state: Rc<RefCell<State>>,
    key: JsString,
    completion: Rc<RefCell<Completion>>,
}
impl Drop for Finished {
    fn drop(&mut self) {
        self.state
            .borrow_mut()
            .active
            .retain(|(k, c)| k != &self.key || !Rc::ptr_eq(c, &self.completion));
        let mut c = self.completion.borrow_mut();
        c.done = true;
        for w in c.waiters.drain(..) {
            w.wake();
        }
    }
}
struct Busy(Rc<RefCell<State>>);
impl Drop for Busy {
    fn drop(&mut self) {
        self.0.borrow_mut().busy = false;
    }
}
pub struct Reconciler<'a, 'x, S, E, X> {
    shared: Rc<Shared<'a, S, E>>,
    executor: &'x X,
}
impl<S: ReconcileStore, E: ReconcileEngine> Shared<'_, S, E> {
    fn record(&self, key: &JsString, error: Option<IvyError>) {
        let mut st = self.state.borrow_mut();
        let index = st.failures.iter().position(|(k, _)| k == key);
        let Some(error) = error else {
            if let Some(i) = index {
                st.failures.remove(i);
            }
            return;
        };
        let now = self.engine.now();
        let first = index
            .map(|i| f(&st.failures[i].1, "firstObservedAt").clone())
            .unwrap_or(Json::String(now.clone()));
        let mut message = error.message_js();
        message.0.truncate(2048);
        let d = Json::object([
            ("code", Json::String(error.code_js())),
            (
                "resource",
                Json::object([("objectId", Json::String(key.clone()))]),
            ),
            (
                "source",
                Json::String(self.engine.service_node_id().clone()),
            ),
            ("severity", "error".into()),
            ("status", "current".into()),
            ("firstObservedAt", first),
            ("lastObservedAt", Json::String(now)),
            ("message", Json::String(message)),
        ]);
        if let Some(i) = index {
            st.failures[i].1 = d;
        } else {
            if st.failures.len() >= 32 {
                st.failures.remove(0);
            }
            st.failures.push((key.clone(), d));
        }
    }
    async fn publication_failure(&self, id: &JsString, error: IvyError, key: Option<&JsString>) {
        if error.outcome == Outcome::NotExecuted {
            if let Ok(original) = self
                .store
                .read("task-board/operation", &Json::String(id.clone()), true)
                .await
            {
                let e = f(&original.value, "error");
                if f(&original.value, "phase") == &Json::from("failed")
                    && f(e, "execution") == &Json::from("not_executed")
                    && f(e, "code") == &Json::String(error.code_js())
                    && f(e, "message") == &Json::String(error.message_js())
                {
                    self.record(id, None);
                    self.record(&prefix("publication:", id), None);
                    return;
                }
            }
        }
        self.record(key.unwrap_or(&prefix("publication:", id)), Some(error));
    }
    async fn task_key(&self, l: Lane, id: &JsString) -> IvyResult<JsString> {
        if l == Lane::Tasks {
            return Ok(id.clone());
        }
        let d = self
            .store
            .read(l.key(), &Json::String(id.clone()), true)
            .await?;
        if l == Lane::Operations {
            Ok(d.value
                .required("request")?
                .get("taskId")
                .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
                .transpose()?
                .unwrap_or_else(|| "shared".into()))
        } else {
            Ok(s(&d.value, "taskId")?.clone())
        }
    }
    async fn process(&self, l: Lane, id: &JsString) -> IvyResult<()> {
        let result: IvyResult<()> = async {
            match l {
                Lane::Operations => {
                    self.engine.recover_operation(id).await?;
                    self.record(&prefix("publication:", id), None);
                }
                Lane::Deliveries => {
                    self.engine.delivery(id).await?;
                }
                Lane::Runs => {
                    let error = self.engine.sync_native_comments(id).await.err();
                    self.record(&prefix("inputs:", id), error);
                    if !self.engine.drain_native(id).await? {
                        return Ok(());
                    }
                    let error = self.engine.native_signals(id).await.err();
                    self.record(&prefix("signals:", id), error);
                }
                Lane::Tasks => {
                    if self.state.borrow().recovering.is_some() {
                        return Err(IvyError::new(
                            "task_board_recovery_pending",
                            "Complete startup recovery before new automatic claims.",
                        ));
                    }
                    self.engine.schedule(id).await?;
                }
            }
            self.record(id, None);
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if ["revision_conflict", "task_board_publication_busy"].contains(&error.code.as_str()) {
                return Ok(());
            }
            if l == Lane::Operations {
                self.publication_failure(id, error, Some(id)).await;
            } else {
                self.record(id, Some(error));
            }
        }
        Ok(())
    }
}
impl<'a, 'x, S: ReconcileStore + 'a, E: ReconcileEngine + 'a, X: ReconcileExecutor<'a>>
    Reconciler<'a, 'x, S, E, X>
{
    pub fn new(store: &'a S, engine: &'a E, executor: &'x X) -> Self {
        Self {
            shared: Rc::new(Shared {
                store,
                engine,
                state: Rc::new(RefCell::new(State::default())),
            }),
            executor,
        }
    }
    pub fn recovered(&self) -> bool {
        self.shared.state.borrow().recovering.is_none()
    }
    pub async fn verify_owner(&self) -> IvyResult<()> {
        self.shared.engine.verify_owner().await
    }
    pub fn task_changed(&self) {
        let mut st = self.shared.state.borrow_mut();
        st.task_changes = st.task_changes.wrapping_add(1);
        st.cursors.remove(&Lane::Tasks);
        st.pages.remove(&Lane::Tasks);
    }
    pub fn diagnostics(&self) -> Vec<Json> {
        self.shared
            .state
            .borrow()
            .failures
            .iter()
            .map(|(_, v)| v.clone())
            .collect()
    }
    pub async fn settled(&self) {
        let pending = self
            .shared
            .state
            .borrow()
            .active
            .iter()
            .map(|(_, c)| c.clone())
            .collect::<Vec<_>>();
        poll_fn(|cx| {
            let mut done = true;
            for c in &pending {
                let mut c = c.borrow_mut();
                if !c.done {
                    done = false;
                    if !c.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                        c.waiters.push(cx.waker().clone());
                    }
                }
            }
            if done { Poll::Ready(()) } else { Poll::Pending }
        })
        .await;
    }
    pub fn stop_admissions(&self) {
        self.shared.state.borrow_mut().stopped = true;
    }
    pub async fn stop(&self) {
        self.stop_admissions();
        self.settled().await;
    }
    fn start(&self, key: JsString, l: Lane, id: JsString, gate: bool) {
        let completion = Rc::new(RefCell::new(Completion::default()));
        let sh = self.shared.clone();
        sh.state
            .borrow_mut()
            .active
            .push((key.clone(), completion.clone()));
        let guard = Finished {
            state: sh.state.clone(),
            key: key.clone(),
            completion,
        };
        self.executor.spawn(Box::pin(async move {
            let _guard = guard;
            let result = if gate {
                match sh.engine.recover_operation(&id).await {
                    Ok(()) => {
                        sh.record(&id, None);
                        sh.record(&prefix("publication:", &id), None);
                        Ok(())
                    }
                    Err(e) => {
                        sh.publication_failure(&id, e, None).await;
                        Ok(())
                    }
                }
            } else {
                sh.process(l, &id).await
            };
            if let Err(e) = result {
                sh.record(&key, Some(e));
            }
        }));
    }
    async fn recover_gates(&self) -> IvyResult<()> {
        let sh = &self.shared;
        let cursor = sh.state.borrow().gate_cursor.clone();
        let mut gates = sh.store.pending_coordination_page(cursor.as_ref(), 10)?;
        if gates.is_empty() && cursor.is_some() {
            sh.state.borrow_mut().gate_cursor = None;
            gates = sh.store.pending_coordination_page(None, 10)?;
        }
        for gate in gates {
            if sh.state.borrow().stopped {
                return Ok(());
            }
            let id = &gate.operation_id;
            match sh.task_key(Lane::Operations, id).await {
                Ok(key) => {
                    if sh.state.borrow().stopped {
                        return Ok(());
                    }
                    let active = sh.state.borrow().active.iter().any(|(k, _)| k == &key);
                    if !active {
                        if sh.state.borrow().active.len() >= 10 {
                            return Ok(());
                        }
                        self.start(key, Lane::Operations, id.clone(), true);
                    }
                    sh.state.borrow_mut().gate_cursor = Some(gate.gate_id);
                }
                Err(e) => sh.publication_failure(id, e, None).await,
            }
        }
        Ok(())
    }
    async fn scan(&self, l: Lane) -> IvyResult<bool> {
        let sh = &self.shared;
        let (changes, cursor, saved) = {
            let st = sh.state.borrow();
            (
                st.task_changes,
                st.cursors.get(&l).cloned().flatten(),
                st.pages.get(&l).cloned(),
            )
        };
        let pending = match saved {
            Some(p) => p,
            None => {
                let page = match sh
                    .store
                    .page(l.key(), &query(l, sh.engine.page_size(), cursor.as_ref()))
                    .await
                {
                    Err(e) if e.code == "invalid_cursor" && cursor.is_some() => {
                        sh.store
                            .page(l.key(), &query(l, sh.engine.page_size(), None))
                            .await?
                    }
                    v => v?,
                };
                let next = f(&page, "nextCursor")
                    .as_js_string()
                    .filter(|s| !s.0.is_empty())
                    .cloned();
                if next.is_some() && next == cursor {
                    return Err(IvyError::new(
                        "task_board_cursor_regression",
                        "A nonempty scan must advance its query cursor.",
                    ));
                }
                let ids = page
                    .required("items")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .map(|p| s(p, "objectId").cloned())
                    .collect::<IvyResult<Vec<_>>>()?;
                let p = Rc::new(RefCell::new(PendingPage {
                    ids,
                    next,
                    task_changes: changes,
                }));
                sh.state.borrow_mut().pages.insert(l, p.clone());
                p
            }
        };
        let count = pending.borrow().ids.len();
        for _ in 0..count {
            if sh.state.borrow().stopped {
                break;
            }
            let id = pending.borrow().ids[0].clone();
            let key = sh.task_key(l, &id).await.unwrap_or_else(|_| id.clone());
            if sh.state.borrow().stopped {
                return Ok(false);
            }
            let active = sh.state.borrow().active.iter().any(|(k, _)| k == &key);
            if !active {
                if sh.state.borrow().active.len() >= 10 {
                    return Ok(false);
                }
                self.start(key, l, id.clone(), false);
            }
            pending.borrow_mut().ids.remove(0);
            if active && sh.state.borrow().recovering == Some(l) {
                pending.borrow_mut().ids.push(id);
            }
        }
        let mut st = sh.state.borrow_mut();
        let p = pending.borrow();
        if st.stopped || !p.ids.is_empty() {
            return Ok(false);
        }
        if st.pages.get(&l).is_some_and(|v| Rc::ptr_eq(v, &pending)) {
            st.pages.remove(&l);
        }
        if l != Lane::Tasks || p.task_changes == st.task_changes {
            st.cursors.insert(l, p.next.clone());
        }
        Ok(p.next.is_none())
    }
    pub async fn tick(&self) {
        {
            let mut st = self.shared.state.borrow_mut();
            if st.busy || st.stopped {
                return;
            }
            st.busy = true;
        }
        let _busy = Busy(self.shared.state.clone());
        let sh = &self.shared;
        let result: IvyResult<()> = async {
            sh.engine.verify_owner().await?;
            if sh.engine.now_millis() - sh.state.borrow().last_cleanup >= 60000.0 {
                sh.state.borrow_mut().last_cleanup = sh.engine.now_millis();
                sh.record(&"cleanup".into(), sh.store.collect_local().await.err());
                let storage = match sh.store.free_bytes() {
                    Ok(Some(n)) if n < 1073741824.0 => Some(IvyError::new(
                        "task_board_storage_low",
                        "Less than 1 GiB remains on the TaskBoard journal filesystem.",
                    )),
                    Ok(_) => None,
                    Err(e) => Some(e),
                };
                sh.record(&"storage".into(), storage);
            }
            sh.record(&"publication".into(), None);
            let recovering = sh.state.borrow().recovering;
            if let Some(l) = recovering {
                if !sh.state.borrow().recovery_scanned {
                    let scanned = self.scan(l).await?;
                    sh.state.borrow_mut().recovery_scanned = scanned;
                }
                self.recover_gates().await?;
                let mut st = sh.state.borrow_mut();
                if st.recovery_scanned && st.active.is_empty() {
                    st.recovering = if l == Lane::Operations {
                        Some(Lane::Runs)
                    } else {
                        None
                    };
                    st.recovery_scanned = false;
                }
            } else {
                let lane = sh.state.borrow().lane;
                let mut scan = Box::pin(async { self.scan(LANES[lane]).await.map(|_| ()) });
                let mut gates = Box::pin(self.recover_gates());
                let mut a = None;
                let mut b = None;
                let (a, b) = poll_fn(|cx| {
                    if a.is_none() {
                        if let Poll::Ready(v) = scan.as_mut().poll(cx) {
                            a = Some(v);
                        }
                    }
                    if b.is_none() {
                        if let Poll::Ready(v) = gates.as_mut().poll(cx) {
                            b = Some(v);
                        }
                    }
                    if a.is_some() && b.is_some() {
                        Poll::Ready((a.take().unwrap(), b.take().unwrap()))
                    } else {
                        Poll::Pending
                    }
                })
                .await;
                // Like Promise.allSettled, both paths finish before the first rejection is used.
                a?;
                b?;
                sh.state.borrow_mut().lane = (lane + 1) % LANES.len();
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            sh.record(&"publication".into(), Some(e));
        }
    }
}
