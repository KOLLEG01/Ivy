use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    allocation::AllocationStore,
    local_gates::PendingGate,
    operations::{Document, Store},
    reconciler::{ReconcileEngine, ReconcileExecutor, ReconcileStore, Reconciler},
    scheduler::SchedulerStore,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    path::Path,
    pin::Pin,
    process::Command,
    rc::Rc,
    sync::{Arc, OnceLock},
    task::{Context, Poll, Wake, Waker},
};
#[path = "task-board-reconciler-journal.rs"]
mod journal;
#[path = "task-board-generation.rs"]
mod generation;
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn flag(v: &Json, k: &str) -> bool {
    f(v, k) == &true.into()
}
fn num(v: &Json, k: &str) -> f64 {
    f(v, k).as_f64().unwrap_or(0.0)
}
fn id(n: usize) -> JsString {
    format!("00000000-0000-4000-8000-{n:012}").into()
}
fn data() -> &'static Json {
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        let o = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-reconciler-oracle.mjs",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        parse(
            std::str::from_utf8(&o.stdout).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
#[derive(Default)]
struct Gate {
    open: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}
impl Gate {
    fn release(&self) {
        self.open.set(true);
        if let Some(w) = self.waker.borrow_mut().take() {
            w.wake();
        }
    }
    async fn wait(&self) {
        std::future::poll_fn(|cx| {
            if self.open.get() {
                Poll::Ready(())
            } else {
                *self.waker.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
    }
}
type Work<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;
#[derive(Default)]
struct Tasks<'a>(RefCell<Vec<Work<'a>>>);
impl<'a> ReconcileExecutor<'a> for Tasks<'a> {
    fn spawn(&self, f: Work<'a>) {
        self.0.borrow_mut().push(f);
    }
}
impl Tasks<'_> {
    fn pump(&self, cx: &mut Context<'_>) {
        let tasks = std::mem::take(&mut *self.0.borrow_mut());
        let mut pending = vec![];
        for mut task in tasks {
            if task.as_mut().poll(cx).is_pending() {
                pending.push(task);
            }
        }
        pending.append(&mut *self.0.borrow_mut());
        *self.0.borrow_mut() = pending;
    }
}
struct Noop;
impl Wake for Noop {
    fn wake(self: Arc<Self>) {}
}
struct World<'a> {
    input: &'a Json,
    records: HashMap<JsString, Json>,
    ids: [Vec<JsString>; 4],
    trace: RefCell<Vec<Json>>,
    holds: RefCell<Vec<(JsString, Rc<Gate>)>>,
    used: RefCell<[usize; 4]>,
    fault_used: Cell<bool>,
    backoff: Cell<bool>,
    page_gate: Rc<Gate>,
    page_paused: Cell<bool>,
    millis: Cell<f64>,
    at: RefCell<JsString>,
    service: JsString,
}
impl<'a> World<'a> {
    fn new(input: &'a Json) -> Self {
        let counts = f(input, "counts").as_array().unwrap();
        let ids = std::array::from_fn(|l| {
            (0..counts[l].as_f64().unwrap() as usize)
                .map(|n| id([200, 300, 100, 400][l] + n))
                .collect::<Vec<_>>()
        });
        let task = |n| id(100 + if flag(input, "sameTask") { 0 } else { n });
        let mut records = HashMap::new();
        for l in 0..4 {
            for (n, id) in ids[l].iter().enumerate() {
                records.insert(
                    id.clone(),
                    if l == 0 {
                        Json::object([
                            (
                                "request",
                                if flag(input, "unscoped") {
                                    Json::object(Vec::<(&str, Json)>::new())
                                } else {
                                    Json::object([("taskId", Json::String(task(n)))])
                                },
                            ),
                            (
                                "phase",
                                if flag(input, "terminal") {
                                    "failed"
                                } else {
                                    "accepted"
                                }
                                .into(),
                            ),
                            (
                                "error",
                                if flag(input, "terminal") {
                                    Json::object([
                                        ("execution", "not_executed".into()),
                                        (
                                            "code",
                                            input
                                                .get("terminalCode")
                                                .unwrap_or(f(f(input, "fault"), "code"))
                                                .clone(),
                                        ),
                                        (
                                            "message",
                                            f(input, "fault")
                                                .get("message")
                                                .cloned()
                                                .unwrap_or_else(|| "Fixture failure.".into()),
                                        ),
                                    ])
                                } else {
                                    Json::Null
                                },
                            ),
                        ])
                    } else {
                        Json::object([("taskId", Json::String(task(n)))])
                    },
                );
            }
        }
        for n in 0..num(input, "gates") as usize {
            records.entry(id(200 + n)).or_insert_with(|| {
                Json::object([
                    ("request", Json::object([("taskId", Json::String(task(n)))])),
                    ("phase", "accepted".into()),
                    ("error", Json::Null),
                ])
            });
        }
        Self {
            input,
            records,
            ids,
            trace: RefCell::default(),
            holds: RefCell::default(),
            used: RefCell::new([0; 4]),
            fault_used: Cell::new(false),
            backoff: Cell::new(false),
            page_gate: Rc::default(),
            page_paused: Cell::new(false),
            millis: Cell::new(0.0),
            at: RefCell::new("2026-01-02T00:00:00.000Z".into()),
            service: "task-board-node".into(),
        }
    }
    fn event(&self, args: impl IntoIterator<Item = Json>) {
        self.trace
            .borrow_mut()
            .push(Json::Array(args.into_iter().collect()));
    }
    fn fail(&self, where_: &str) -> IvyResult<()> {
        let fault = f(self.input, "fault");
        if f(fault, "where") == &Json::from(where_)
            && (!self.fault_used.get() || flag(fault, "always"))
        {
            self.fault_used.set(true);
            if where_ == "drain" {
                self.backoff.set(true);
            }
            Err(IvyError::new_js(
                s(fault, "code").clone(),
                fault
                    .get("message")
                    .and_then(Json::as_js_string)
                    .cloned()
                    .unwrap_or_else(|| "Fixture failure.".into()),
            )
            .with_outcome(if f(fault, "execution") == &Json::from("unknown") {
                Outcome::Unknown
            } else {
                Outcome::NotExecuted
            }))
        } else {
            Ok(())
        }
    }
    async fn work(&self, kind: &str, key: &JsString) -> IvyResult<()> {
        self.event([kind.into(), Json::String(key.clone())]);
        let v = self.records.get(key);
        let task = v
            .and_then(|v| {
                v.get("taskId")
                    .or_else(|| v.get("request").and_then(|v| v.get("taskId")))
            })
            .and_then(Json::as_js_string)
            .unwrap_or(key)
            .clone();
        let gate = Rc::new(Gate::default());
        self.holds.borrow_mut().push((task, gate.clone()));
        gate.wait().await;
        self.fail(kind)
    }
    fn release(&self, key: Option<&JsString>) {
        let mut holds = self.holds.borrow_mut();
        let mut keep = vec![];
        for (k, g) in holds.drain(..) {
            if key.is_none_or(|id| id == &k) {
                g.release();
            } else {
                keep.push((k, g));
            }
        }
        *holds = keep;
    }
}
impl Store for World<'_> {
    fn root_object_id(&self) -> Option<JsString> {
        Some("root".into())
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        panic!("No contracts")
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        panic!("No retired operations")
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        panic!("No key query")
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("No writes")
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        panic!("No named reads")
    }
    async fn read(&self, k: &str, a: &Json, _: bool) -> IvyResult<Document> {
        self.event(["read".into(), k.into(), a.clone()]);
        self.fail("read")?;
        let value = self.records.get(a.as_js_string().unwrap()).unwrap().clone();
        Ok(Document {
            pin: Json::object([("objectId", a.clone()), ("revision", Json::Number(1.0))]),
            content_hash: hash_json(&value)?,
            value,
            parent_id: Some("root".into()),
        })
    }
    async fn write(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("No writes")
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("No writes")
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        panic!("No writes")
    }
}
impl AllocationStore for World<'_> {
    async fn named_plan(&self, _: &JsString) -> IvyResult<Option<Document>> {
        panic!("No allocation")
    }
    async fn workspace_owner(
        &self,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> IvyResult<Option<JsString>> {
        panic!("No reservation")
    }
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        self.event(["page".into(), k.into(), o.clone()]);
        let l = [
            "task-board/operation",
            "task-board/run",
            "task-board/task",
            "task-board/delivery",
        ]
        .iter()
        .position(|v| *v == k)
        .unwrap();
        self.fail(&format!(
            "page:{}",
            ["operations", "runs", "tasks", "deliveries"][l]
        ))?;
        let count = {
            let mut used = self.used.borrow_mut();
            used[l] += 1;
            used[l]
        };
        if l == 2 && flag(self.input, "pausePage") && !self.page_paused.replace(true) {
            self.page_gate.wait().await;
        }
        if l == 0 && count == num(self.input, "invalidPage") as usize {
            return Err(IvyError::new("invalid_cursor", "Fixture cursor expired."));
        }
        let start = f(o, "cursor")
            .as_js_string()
            .map(|s| s.to_utf8().unwrap().parse::<usize>().unwrap())
            .unwrap_or(0);
        let limit = num(o, "limit") as usize;
        let next = if l == 0 && count == num(self.input, "regressPage") as usize {
            f(o, "cursor").clone()
        } else if start + limit < self.ids[l].len() {
            (start + limit).to_string().into()
        } else {
            Json::Null
        };
        Ok(Json::object([
            (
                "items",
                Json::Array(
                    self.ids[l]
                        .iter()
                        .skip(start)
                        .take(limit)
                        .map(|id| {
                            Json::object([
                                ("objectId", Json::String(id.clone())),
                                ("revision", Json::Number(1.0)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("nextCursor", next),
        ]))
    }
}
impl SchedulerStore for World<'_> {
    fn free_bytes(&self) -> IvyResult<Option<f64>> {
        self.event(["free".into()]);
        self.fail("free")?;
        Ok(self
            .input
            .get("freeBytes")
            .map(Json::as_f64)
            .unwrap_or(Some(1073741824.0)))
    }
}
impl ReconcileStore for World<'_> {
    async fn collect_local(&self) -> IvyResult<()> {
        self.event(["collect".into()]);
        self.fail("collect")
    }
    fn pending_coordination_page(
        &self,
        after: Option<&JsString>,
        limit: i64,
    ) -> IvyResult<Vec<PendingGate>> {
        self.event([
            "gates".into(),
            after.cloned().map(Json::String).unwrap_or(Json::Null),
            Json::Number(limit as f64),
        ]);
        self.fail("gates")?;
        Ok((0..num(self.input, "gates") as usize)
            .filter(|n| after.is_none_or(|a| id(500 + *n) > *a))
            .take(limit as usize)
            .map(|n| PendingGate {
                gate_id: id(500 + n),
                operation_id: id(200 + n),
            })
            .collect())
    }
}
impl ReconcileEngine for World<'_> {
    fn service_node_id(&self) -> &JsString {
        &self.service
    }
    fn page_size(&self) -> f64 {
        self.input
            .get("pageSize")
            .and_then(Json::as_f64)
            .unwrap_or(50.0)
    }
    fn now_millis(&self) -> f64 {
        self.millis.get()
    }
    fn now(&self) -> JsString {
        self.at.borrow().clone()
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        self.event(["owner".into()]);
        self.fail("owner")
    }
    async fn recover_operation(&self, id: &JsString) -> IvyResult<()> {
        self.work("recover", id).await
    }
    async fn sync_native_comments(&self, id: &JsString) -> IvyResult<()> {
        self.work("sync", id).await
    }
    async fn drain_native(&self, id: &JsString) -> IvyResult<bool> {
        self.work("drain", id).await?;
        Ok(!(flag(self.input, "backoff")
            || (flag(self.input, "backoffAfterFault") && self.backoff.get())))
    }
    async fn native_signals(&self, id: &JsString) -> IvyResult<()> {
        self.work("signals", id).await
    }
    async fn schedule(&self, id: &JsString) -> IvyResult<()> {
        self.work("schedule", id).await
    }
    async fn delivery(&self, id: &JsString) -> IvyResult<()> {
        self.work("delivery", id).await
    }
}
fn pump(
    tasks: &Tasks<'_>,
    tick: &mut Option<Work<'_>>,
    stop: &mut Option<Work<'_>>,
    cx: &mut Context<'_>,
) {
    for _ in 0..8 {
        if tick
            .as_mut()
            .is_some_and(|f| f.as_mut().poll(cx).is_ready())
        {
            *tick = None;
        }
        tasks.pump(cx);
        if stop
            .as_mut()
            .is_some_and(|f| f.as_mut().poll(cx).is_ready())
        {
            *stop = None;
        }
    }
}
#[test]
fn original_reconciler_matches_fair_scans_concurrent_admissions_stop_and_diagnostics() {
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);
    let mut steps = 0;
    let mut calls = 0;
    for row in data().as_array().unwrap() {
        let world = World::new(f(row, "input"));
        let tasks = Tasks::default();
        let subject = Reconciler::new(&world, &world, &tasks);
        let stopped = Cell::new(false);
        let mut pending_tick: Option<Work<'_>> = None;
        let mut pending_stop: Option<Work<'_>> = None;
        let mut actual = vec![];
        for (action, expected) in f(row, "actions")
            .as_array()
            .unwrap()
            .iter()
            .zip(f(row, "observations").as_array().unwrap())
        {
            steps += 1;
            world.millis.set(num(expected, "millis"));
            *world.at.borrow_mut() = s(expected, "at").clone();
            let a = action.as_array().unwrap();
            match a[0].as_str().unwrap().as_str() {
                "tick" => {
                    let mut f = Box::pin(subject.tick());
                    assert!(
                        f.as_mut().poll(&mut cx).is_ready(),
                        "Unexpected blocked tick {:?}",
                        s(row, "name")
                    );
                }
                "beginTick" => {
                    assert!(pending_tick.is_none());
                    pending_tick = Some(Box::pin(subject.tick()));
                }
                "finishTick" => {
                    pump(&tasks, &mut pending_tick, &mut pending_stop, &mut cx);
                    assert!(pending_tick.is_none());
                }
                "beginStop" => {
                    pending_stop = Some(Box::pin(async {
                        subject.stop().await;
                        stopped.set(true);
                    }));
                }
                "finishStop" => {
                    pump(&tasks, &mut pending_tick, &mut pending_stop, &mut cx);
                    assert!(pending_stop.is_none());
                    assert!(stopped.get());
                }
                "taskChanged" => subject.task_changed(),
                "advance" => {}
                "releasePage" => world.page_gate.release(),
                "release" => world.release(Some(a[1].as_js_string().unwrap())),
                "flush" => {
                    for _ in 0..20 {
                        world.release(None);
                        pump(&tasks, &mut pending_tick, &mut pending_stop, &mut cx);
                        if world.holds.borrow().is_empty() {
                            break;
                        }
                    }
                    let mut f = Box::pin(subject.settled());
                    assert!(f.as_mut().poll(&mut cx).is_ready());
                }
                k => panic!("Unknown {k}"),
            }
            pump(&tasks, &mut pending_tick, &mut pending_stop, &mut cx);
            let batch = std::mem::take(&mut *world.trace.borrow_mut());
            let is_work = |e: &Json| {
                [
                    "sync", "drain", "signals", "recover", "schedule", "delivery",
                ]
                .contains(&e.as_array().unwrap()[0].as_str().unwrap().as_str())
            };
            let mut keys = batch
                .iter()
                .filter(|e| is_work(e))
                .map(|e| e.as_array().unwrap()[1].as_js_string().unwrap().clone())
                .collect::<Vec<_>>();
            keys.sort();
            keys.dedup();
            let work_order = Json::Array(
                keys.iter()
                    .map(|k| {
                        Json::Array(vec![
                            Json::String(k.clone()),
                            Json::Array(
                                batch
                                    .iter()
                                    .filter(|e| {
                                        is_work(e)
                                            && &e.as_array().unwrap()[1] == &Json::String(k.clone())
                                    })
                                    .map(|e| e.as_array().unwrap()[0].clone())
                                    .collect(),
                            ),
                        ])
                    })
                    .collect(),
            );
            let mut frames = batch
                .into_iter()
                .filter(|e| !is_work(e))
                .collect::<Vec<_>>();
            frames.sort_by_cached_key(|e| hash_json(e).unwrap());
            calls += frames.len()
                + work_order
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_array().unwrap()[1].as_array().unwrap().len())
                    .sum::<usize>();
            let mut holds = world
                .holds
                .borrow()
                .iter()
                .map(|(k, _)| k.clone())
                .collect::<Vec<_>>();
            holds.sort();
            let observation = Json::object([
                ("recovered", subject.recovered().into()),
                ("diagnostics", Json::Array(subject.diagnostics())),
                (
                    "holds",
                    Json::Array(holds.into_iter().map(Json::String).collect()),
                ),
                ("stopDone", stopped.get().into()),
                ("calls", Json::Array(frames)),
                ("workOrder", work_order),
                ("millis", Json::Number(world.millis.get())),
                ("at", Json::String(world.at.borrow().clone())),
            ]);
            assert_eq!(
                hash_json(&observation).unwrap(),
                hash_json(expected).unwrap(),
                "{:?} action {action:?}: {observation:?}, expected {expected:?}",
                s(row, "name")
            );
            actual.push(observation);
        }
        assert_eq!(
            hash_json(&Json::Array(actual)).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap()
        );
        assert!(tasks.0.borrow().is_empty());
        assert!(pending_tick.is_none() && pending_stop.is_none());
    }
    assert_eq!(steps, 1182);
    assert_eq!(calls, 2765);
    assert_eq!(data().as_array().unwrap().len(), 45);
}
