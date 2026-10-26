#[path = "reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::{digest, hash_json},
    json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    collation::LocaleComparator,
    rpc::{RequestFuture, RequestOptions, RpcClient},
};
use ivy_task_board::{
    category_projection::{CategoryLocale, CategoryProjection, CategoryStore},
    reconciler::ReconcileExecutor,
};
use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::VecDeque,
    future::{Future, poll_fn},
    path::Path,
    pin::Pin,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    task::{Poll, Waker},
};
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn data() -> &'static Json {
    static D: OnceLock<Json> = OnceLock::new();
    D.get_or_init(|| {
        let p = reference_data::read(include_bytes!("task-board-categories-cases.json.gz"));
        parse(std::str::from_utf8(&p).unwrap(), MANAGEMENT_FRAME_BYTES).unwrap()
    })
}
fn error(v: &Json) -> IvyError {
    IvyError::new_js(s(v, "code").clone(), s(v, "message").clone())
        .with_provider_outcome(v.required("outcome").unwrap().clone())
        .with_details(f(v, "details").clone())
}
fn outcome(v: IvyResult<Json>) -> Json {
    match v {
        Ok(v) => Json::object([("ok", v)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.to_json()),
                ("details", e.details.unwrap_or(Json::Null)),
            ]),
        )]),
    }
}
struct Replay {
    steps: Mutex<VecDeque<Json>>,
    name: JsString,
}
impl Replay {
    fn call(&self, k: &str, args: impl IntoIterator<Item = Json>) -> IvyResult<Json> {
        let args = Json::Array(args.into_iter().collect());
        let e = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("Unexpected {k}: {:?} {args:?}", self.name));
        assert_eq!(f(&e, "kind"), &Json::from(k), "{:?}: {args:?}", self.name);
        assert_eq!(
            hash_json(f(&e, "args")).unwrap(),
            hash_json(&args).unwrap(),
            "{:?} {k}: expected {:?}, actual {args:?}",
            self.name,
            f(&e, "args")
        );
        if let Some(v) = e.get("error") {
            Err(error(v))
        } else {
            Ok(f(&e, "value").clone())
        }
    }
}
#[derive(Default)]
struct GateState {
    started: bool,
    open: bool,
    waiters: Vec<Waker>,
}
#[derive(Default)]
struct Gate(Mutex<GateState>);
impl Gate {
    fn update(&self, release: bool) {
        let waiters = {
            let mut state = self.0.lock().unwrap();
            state.started = true;
            state.open |= release;
            std::mem::take(&mut state.waiters)
        };
        for w in waiters {
            w.wake();
        }
    }
    async fn wait(&self, started: bool) {
        poll_fn(|cx| {
            let mut state = self.0.lock().unwrap();
            if if started { state.started } else { state.open } {
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
struct Client(Arc<Replay>, Arc<Gate>);
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            let pause = self
                .0
                .steps
                .lock()
                .unwrap()
                .front()
                .is_some_and(|v| f(v, "pause") == &Json::Bool(true));
            let reply = self.0.call("rpc", [m.into(), a]);
            if pause {
                self.1.update(false);
                self.1.wait(false).await;
            }
            reply
        })
    }
}
struct World {
    root: RefCell<Option<JsString>>,
    client: Client,
    replay: Arc<Replay>,
}
impl CategoryStore for World {
    fn root_object_id(&self) -> Option<JsString> {
        self.root.borrow().clone()
    }
    fn client(&self) -> &dyn RpcClient {
        &self.client
    }
    async fn task_page(&self, o: &Json) -> IvyResult<Json> {
        self.replay
            .call("page", ["task-board/task".into(), o.clone()])
    }
}
fn native_locale() -> LocaleComparator {
    LocaleComparator::system().unwrap()
}
#[derive(Default)]
struct Executor<'a> {
    work: RefCell<VecDeque<Pin<Box<dyn Future<Output = ()> + 'a>>>>,
}
impl<'a> ReconcileExecutor<'a> for Executor<'a> {
    fn spawn(&self, f: Pin<Box<dyn Future<Output = ()> + 'a>>) {
        self.work.borrow_mut().push_back(f)
    }
}
impl Executor<'_> {
    async fn drive<F: Future>(&self, f: F) -> F::Output {
        self.run(f, true).await
    }
    async fn until<F: Future>(&self, f: F) -> F::Output {
        self.run(f, false).await
    }
    async fn drain<F: Future<Output = ()>>(&self, f: F) {
        let mut f = Box::pin(f);
        self.drive(poll_fn(|cx| {
            let result = f.as_mut().poll(cx);
            if result.is_ready() {
                assert!(
                    self.work.borrow().is_empty(),
                    "Drain resolved before worker exit"
                );
            }
            result
        }))
        .await;
    }
    async fn run<F: Future>(&self, f: F, drain: bool) -> F::Output {
        let mut f = Box::pin(f);
        let mut output = None;
        poll_fn(|cx| {
            if output.is_none() {
                if let Poll::Ready(v) = f.as_mut().poll(cx) {
                    output = Some(v);
                }
            }
            if output.is_some() && !drain {
                return Poll::Ready(output.take().unwrap());
            }
            let mut tasks = std::mem::take(&mut *self.work.borrow_mut());
            let mut pending = VecDeque::new();
            while let Some(mut task) = tasks.pop_front() {
                if task.as_mut().poll(cx).is_pending() {
                    pending.push_back(task);
                }
            }
            self.work.borrow_mut().extend(pending);
            if output.is_some() && self.work.borrow().is_empty() {
                Poll::Ready(output.take().unwrap())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}
#[tokio::test(flavor = "current_thread")]
async fn category_projection_matches_original_event_snapshot_cache_and_queued_work() {
    let locale = native_locale();
    for row in data().as_array().unwrap() {
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
        let gate = Arc::new(Gate::default());
        let world = World {
            root: RefCell::new(f(f(row, "initial"), "root").as_js_string().cloned()),
            client: Client(replay.clone(), gate.clone()),
            replay: replay.clone(),
        };
        let executor = Executor::default();
        let mut principal = s(f(row, "initial"), "principal").clone();
        let mut projection = CategoryProjection::new(&world, &principal, &locale, &executor);
        let mut outcomes = vec![];
        for step in f(row, "steps").as_array().unwrap() {
            let r = match s(step, "kind").to_utf8().unwrap().as_str() {
                "categories" => outcome(executor.drive(projection.categories()).await),
                "normalize" => outcome(
                    executor
                        .drive(projection.normalize(f(step, "category").as_js_string()))
                        .await,
                ),
                "revalidate" => outcome(
                    executor
                        .drive(projection.revalidate(s(step, "category")))
                        .await,
                ),
                "observe" => {
                    projection.observe(
                        s(step, "objectId"),
                        f(step, "revision").as_f64().unwrap(),
                        f(step, "task"),
                    );
                    outcome(Ok(Json::Null))
                }
                "set" | "clearOptions" => outcome(Ok(Json::Null)),
                "restart" => {
                    executor.drain(projection.settled()).await;
                    if step.get("root").is_some() {
                        *world.root.borrow_mut() = f(step, "root").as_js_string().cloned();
                    }
                    if let Some(p) = step.get("principal") {
                        principal = p.as_js_string().unwrap().clone();
                    }
                    projection = CategoryProjection::new(&world, &principal, &locale, &executor);
                    outcome(Ok(Json::Null))
                }
                "dropCategories" => {
                    drop(projection.categories());
                    outcome(Ok(Json::Null))
                }
                "drain" => {
                    executor.drain(projection.settled()).await;
                    outcome(Ok(Json::Null))
                }
                "paused" | "pausedDrop" => {
                    let request = projection.categories();
                    executor.until(gate.wait(true)).await;
                    projection.observe(
                        s(step, "objectId"),
                        f(step, "revision").as_f64().unwrap(),
                        f(step, "task"),
                    );
                    let denial = outcome(projection.revalidate(&"work".into()).await);
                    let mut settled = Box::pin(projection.settled());
                    let drain_pending =
                        poll_fn(|cx| Poll::Ready(settled.as_mut().poll(cx).is_pending())).await;
                    assert!(drain_pending, "Drain resolved during pending transport");
                    let mut fields = vec![
                        ("denial", denial),
                        ("drainWasPending", Json::Bool(drain_pending)),
                    ];
                    if s(step, "kind") == &JsString::from("pausedDrop") {
                        drop(request);
                        gate.update(true);
                        executor.drain(settled).await;
                    } else {
                        gate.update(true);
                        fields.push(("result", outcome(executor.drive(request).await)));
                        executor.drain(settled).await;
                    }
                    outcome(Ok(Json::object(fields)))
                }
                "busy" => {
                    let first = projection.categories();
                    let denial = projection.revalidate(&"work".into());
                    let second = projection.categories();
                    executor
                        .drive(async {
                            let denial = outcome(denial.await);
                            let first = outcome(first.await);
                            let second = outcome(second.await);
                            outcome(Ok(Json::object([
                                ("denial", denial),
                                ("first", first),
                                ("second", second),
                            ])))
                        })
                        .await
                }
                "queuedFailure" => {
                    let first = projection.categories();
                    let second = projection.categories();
                    executor
                        .drive(async {
                            let first = outcome(first.await);
                            let second = outcome(second.await);
                            outcome(Ok(Json::object([("first", first), ("second", second)])))
                        })
                        .await
                }
                k => panic!("Unknown source step {k}"),
            };
            outcomes.push(r);
        }
        executor.drain(projection.settled()).await;
        let actual = Json::object([("outcomes", Json::Array(outcomes))]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: actual {actual:?}, expected {:?}",
            replay.name,
            f(row, "expected")
        );
        let remaining = replay.steps.lock().unwrap();
        assert!(
            remaining.is_empty(),
            "{:?}: remaining {:?}",
            replay.name,
            remaining.front()
        );
    }
}

const NATIVE_ID: &str = "00000000-0000-4000-8000-000000000001";
const NATIVE_TIME: &str = "2026-01-02T03:04:05.006Z";
fn native_subscription() -> Json {
    let scope = digest("owner:root");
    Json::object([
        (
            "name",
            format!("task-board-categories-{}", &scope[7..31]).into(),
        ),
        (
            "filter",
            Json::object([("topics", Json::Array(vec!["hive.object.changed".into()]))]),
        ),
        ("initialSequence", 0.0.into()),
        ("limit", 100.0.into()),
    ])
}
fn native_rpc(method: &str, args: Json, value: Json) -> Json {
    Json::object([
        ("kind", "rpc".into()),
        ("args", Json::Array(vec![method.into(), args])),
        ("value", value),
    ])
}
fn native_batch(sequence: u32, gap: Json, changed: bool) -> Json {
    Json::object([
        (
            "items",
            Json::Array(if changed {
                vec![Json::object([
                    ("sequence", f64::from(sequence).into()),
                    ("topic", "hive.object.changed".into()),
                    ("payload", Json::object([("objectId", NATIVE_ID.into())])),
                ])]
            } else {
                vec![]
            }),
        ),
        ("throughSequence", f64::from(sequence).into()),
        ("hasMore", false.into()),
        ("gap", gap),
    ])
}
fn native_ack(batch: &Json) -> Json {
    let mut args = Json::object([
        ("name", f(&native_subscription(), "name").clone()),
        ("throughSequence", f(batch, "throughSequence").clone()),
    ]);
    if f(batch, "gap").as_object().is_some() {
        args.insert(
            "gapThroughSequence",
            f(f(batch, "gap"), "prunedThroughSequence").clone(),
        );
    }
    native_rpc(
        "events.ack",
        args,
        Json::object([("throughSequence", f(batch, "throughSequence").clone())]),
    )
}
fn native_item(category: &str) -> Json {
    Json::object([
        ("objectId", NATIVE_ID.into()),
        ("revision", 1.0.into()),
        (
            "values",
            Json::object([
                ("data:/fields/category", category.into()),
                ("data:/createdAt", NATIVE_TIME.into()),
            ]),
        ),
    ])
}
fn native_page(items: Vec<Json>, cursor: Json) -> Json {
    Json::object([("items", Json::Array(items)), ("nextCursor", cursor)])
}
fn native_page_call(value: Json, cursor: Option<Json>, lookup: bool) -> Json {
    let mut options = Json::object([
        ("limit", if lookup { 1.0 } else { 200.0 }.into()),
        (
            "select",
            Json::Array(vec![
                "data:/fields/category".into(),
                "data:/createdAt".into(),
            ]),
        ),
        (
            "orderBy",
            Json::Array(vec![Json::object([
                ("field", "object.id".into()),
                ("direction", "asc".into()),
            ])]),
        ),
    ]);
    if let Some(cursor) = cursor {
        options.insert("cursor", cursor);
    }
    if lookup {
        options.insert(
            "where",
            Json::object([
                ("op", "in".into()),
                ("field", "object.id".into()),
                ("value", Json::Array(vec![NATIVE_ID.into()])),
            ]),
        );
    }
    Json::object([
        ("kind", "page".into()),
        ("args", Json::Array(vec!["task-board/task".into(), options])),
        ("value", value),
    ])
}
fn native_world(name: &str, pages: Vec<Json>, first: Json) -> World {
    let mut steps = VecDeque::from([native_rpc(
        "events.subscribe",
        native_subscription(),
        first.clone(),
    )]);
    let mut cursor = Json::Null;
    for page in pages {
        let next = f(&page, "nextCursor").clone();
        steps.push_back(native_page_call(
            page,
            (!cursor.is_null()).then_some(cursor),
            false,
        ));
        cursor = next;
    }
    steps.push_back(native_rpc(
        "events.head",
        Json::object(Vec::<(&str, Json)>::new()),
        Json::object([("throughSequence", f(&first, "throughSequence").clone())]),
    ));
    steps.push_back(native_ack(&first));
    let replay = Arc::new(Replay {
        steps: Mutex::new(steps),
        name: name.into(),
    });
    World {
        root: RefCell::new(Some("root".into())),
        client: Client(replay.clone(), Arc::new(Gate::default())),
        replay,
    }
}
fn native_poll(world: &World, batch: Json) {
    world.replay.steps.lock().unwrap().extend([
        native_rpc(
            "events.head",
            Json::object(Vec::<(&str, Json)>::new()),
            Json::object([("throughSequence", f(&batch, "throughSequence").clone())]),
        ),
        native_rpc("events.subscribe", native_subscription(), batch.clone()),
        native_ack(&batch),
    ]);
}
fn native_categories(value: Json, expected: &[&str]) {
    assert_eq!(
        value,
        Json::object([(
            "categories",
            Json::Array(expected.iter().map(|value| Json::from(*value)).collect())
        )])
    );
}
async fn native_typed_inputs(locale: &LocaleComparator) {
    let mut checked = 0;
    let invalid_cursors = [
        false.into(),
        true.into(),
        0.0.into(),
        1.0.into(),
        "".into(),
        Json::Array(vec![]),
        Json::object(Vec::<(&str, Json)>::new()),
    ];
    for cursor in &invalid_cursors {
        let world = native_world(
            "invalid snapshot cursor",
            vec![native_page(vec![native_item("Work")], cursor.clone())],
            native_batch(0, Json::Null, false),
        );
        world.replay.steps.lock().unwrap().truncate(2);
        let executor = Executor::default();
        let projection = CategoryProjection::new(&world, &"owner".into(), locale, &executor);
        assert_eq!(
            executor
                .drive(projection.categories())
                .await
                .unwrap_err()
                .code,
            "task_board_category_projection_invalid"
        );
        assert!(
            world.replay.steps.lock().unwrap().is_empty(),
            "Invalid snapshot cursor must not advance or acknowledge"
        );
        checked += 1;
    }
    for page in [
        native_page(vec![native_item("Work")], Json::Null),
        Json::object([("items", Json::Array(vec![native_item("Work")]))]),
    ] {
        let world = native_world(
            "snapshot end",
            vec![page],
            native_batch(0, Json::Null, false),
        );
        let executor = Executor::default();
        let projection = CategoryProjection::new(&world, &"owner".into(), locale, &executor);
        native_categories(
            executor.drive(projection.categories()).await.unwrap(),
            &["Work"],
        );
        assert!(world.replay.steps.lock().unwrap().is_empty());
        checked += 1;
    }
    let cursor = Json::String(JsString(vec![0x63, 0, 0xd800, 0x4e2d]));
    let world = native_world(
        "lossless snapshot cursor",
        vec![
            native_page(vec![native_item("Work")], cursor),
            native_page(vec![], Json::Null),
        ],
        native_batch(0, Json::Null, false),
    );
    let executor = Executor::default();
    let projection = CategoryProjection::new(&world, &"owner".into(), locale, &executor);
    native_categories(
        executor.drive(projection.categories()).await.unwrap(),
        &["Work"],
    );
    assert!(world.replay.steps.lock().unwrap().is_empty());
    checked += 1;
    for cursor in &invalid_cursors {
        let world = native_world(
            "bounded lookup cursor",
            vec![native_page(vec![native_item("Work")], Json::Null)],
            native_batch(0, Json::Null, false),
        );
        let executor = Executor::default();
        let projection = CategoryProjection::new(&world, &"owner".into(), locale, &executor);
        native_categories(
            executor.drive(projection.categories()).await.unwrap(),
            &["Work"],
        );
        native_poll(&world, native_batch(1, Json::Null, true));
        {
            let mut steps = world.replay.steps.lock().unwrap();
            steps.pop_back();
            steps.push_back(native_page_call(
                native_page(vec![], cursor.clone()),
                None,
                true,
            ));
        }
        assert_eq!(
            executor
                .drive(projection.categories())
                .await
                .unwrap_err()
                .code,
            "task_board_category_projection_invalid"
        );
        assert!(
            world.replay.steps.lock().unwrap().is_empty(),
            "Invalid bounded lookup must not read metadata or acknowledge"
        );
        native_poll(&world, native_batch(1, Json::Null, false));
        native_categories(
            executor.drive(projection.categories()).await.unwrap(),
            &["Work"],
        );
        assert!(world.replay.steps.lock().unwrap().is_empty());
        checked += 1;
    }
    for gap in [
        false.into(),
        true.into(),
        0.0.into(),
        1.0.into(),
        "".into(),
        "gap".into(),
        Json::Array(vec![]),
    ] {
        let world = native_world(
            "invalid event gap",
            vec![native_page(vec![native_item("Work")], Json::Null)],
            native_batch(0, gap, false),
        );
        world.replay.steps.lock().unwrap().pop_back();
        let executor = Executor::default();
        let projection = CategoryProjection::new(&world, &"owner".into(), locale, &executor);
        assert_eq!(
            executor
                .drive(projection.categories())
                .await
                .unwrap_err()
                .code,
            "task_board_category_projection_invalid"
        );
        assert!(
            world.replay.steps.lock().unwrap().is_empty(),
            "Malformed gaps must not be acknowledged"
        );
        native_poll(&world, native_batch(0, Json::Null, false));
        native_categories(
            executor.drive(projection.categories()).await.unwrap(),
            &["Work"],
        );
        assert!(world.replay.steps.lock().unwrap().is_empty());
        checked += 1;
    }
    let gap = Json::object([
        ("prunedThroughSequence", 5.0.into()),
        ("resumeAfterSequence", 5.0.into()),
    ]);
    let world = native_world(
        "initial gap covered by snapshot",
        vec![native_page(vec![native_item("Work")], Json::Null)],
        native_batch(5, gap.clone(), false),
    );
    let executor = Executor::default();
    let projection = CategoryProjection::new(&world, &"owner".into(), locale, &executor);
    native_categories(
        executor.drive(projection.categories()).await.unwrap(),
        &["Work"],
    );
    assert!(world.replay.steps.lock().unwrap().is_empty());
    checked += 1;
    let world = native_world(
        "bounded gap and rebuild",
        vec![native_page(vec![native_item("Work")], Json::Null)],
        native_batch(0, Json::Null, false),
    );
    let executor = Executor::default();
    let projection = CategoryProjection::new(&world, &"owner".into(), locale, &executor);
    native_categories(
        executor.drive(projection.categories()).await.unwrap(),
        &["Work"],
    );
    native_poll(&world, native_batch(5, gap.clone(), false));
    world.replay.steps.lock().unwrap().pop_back();
    assert_eq!(
        executor
            .drive(projection.revalidate(&"changed".into()))
            .await
            .unwrap_err()
            .code,
        "task_board_category_projection_pending"
    );
    assert!(world.replay.steps.lock().unwrap().is_empty());
    native_poll(&world, native_batch(5, gap, false));
    {
        let mut steps = world.replay.steps.lock().unwrap();
        let before_ack = steps.len() - 1;
        steps.insert(
            before_ack,
            native_page_call(
                native_page(vec![native_item("Changed")], Json::Null),
                None,
                false,
            ),
        );
    }
    native_categories(
        executor.drive(projection.categories()).await.unwrap(),
        &["Changed"],
    );
    assert!(world.replay.steps.lock().unwrap().is_empty());
    checked += 1;
    for archived in [
        None,
        Some(Json::Null),
        Some(false.into()),
        Some(true.into()),
        Some(0.0.into()),
        Some(1.0.into()),
        Some("".into()),
        Some("yes".into()),
        Some(Json::Array(vec![])),
        Some(Json::object(Vec::<(&str, Json)>::new())),
    ] {
        let world = native_world(
            "typed archive authority",
            vec![native_page(vec![native_item("Work")], Json::Null)],
            native_batch(0, Json::Null, false),
        );
        let executor = Executor::default();
        let projection = CategoryProjection::new(&world, &"owner".into(), locale, &executor);
        native_categories(
            executor.drive(projection.categories()).await.unwrap(),
            &["Work"],
        );
        let retained_archive = archived.as_ref() == Some(&Json::Bool(true));
        let batch = native_batch(1, Json::Null, true);
        native_poll(&world, batch.clone());
        let mut metadata = Json::object([
            ("contractKey", "task-board/task".into()),
            ("parentId", "root".into()),
            ("currentRevision", 4.0.into()),
        ]);
        if let Some(archived) = archived {
            metadata.insert("effectivelyArchived", archived);
        }
        {
            let mut steps = world.replay.steps.lock().unwrap();
            steps.pop_back();
            steps.extend([
                native_page_call(native_page(vec![], Json::Null), None, true),
                native_rpc(
                    "objects.stat",
                    Json::object([("objectId", NATIVE_ID.into())]),
                    metadata,
                ),
                native_ack(&batch),
            ]);
        }
        native_categories(executor.drive(projection.categories()).await.unwrap(), &[]);
        let restored = Json::object([
            ("createdAt", NATIVE_TIME.into()),
            ("fields", Json::object([("category", "Restored".into())])),
        ]);
        projection.observe(&NATIVE_ID.into(), 2.0, &restored);
        native_poll(&world, native_batch(1, Json::Null, false));
        native_categories(
            executor.drive(projection.categories()).await.unwrap(),
            if retained_archive { &[] } else { &["Restored"] },
        );
        if retained_archive {
            projection.observe(&NATIVE_ID.into(), 4.0, &restored);
            native_poll(&world, native_batch(1, Json::Null, false));
            native_categories(
                executor.drive(projection.categories()).await.unwrap(),
                &["Restored"],
            );
        }
        assert!(world.replay.steps.lock().unwrap().is_empty());
        checked += 1;
    }
    assert_eq!(checked, 36);
}
#[tokio::test(flavor = "current_thread")]
async fn native_casing_errors_preserve_the_projection_and_release_queued_requests() {
    struct FaultLocale {
        native: LocaleComparator,
        failures: RefCell<VecDeque<(JsString, IvyError)>>,
    }
    impl CategoryLocale for FaultLocale {
        fn lowercase_default(&self, value: &JsString) -> IvyResult<JsString> {
            let mut failures = self.failures.borrow_mut();
            if failures.front().is_some_and(|(input, _)| input == value) {
                return Err(failures.pop_front().unwrap().1);
            }
            self.native.lowercase_default(value)
        }
        fn compare_default(&self, left: &JsString, right: &JsString) -> Ordering {
            self.native.compare_default(left, right)
        }
    }
    let world = native_world(
        "native casing errors",
        vec![native_page(vec![native_item("Work")], Json::Null)],
        native_batch(0, Json::Null, false),
    );
    let replay = world.replay.clone();
    for _ in 0..3 {
        native_poll(&world, native_batch(0, Json::Null, false));
    }
    let failures: Vec<_> = [
        ("Work", "locale_unavailable"),
        ("work", "content_too_large"),
        ("WORK", "locale_unavailable"),
    ]
    .into_iter()
    .map(|(value, code)| {
        (
            JsString::from(value),
            IvyError::new_js(code.into(), JsString(vec![0x41, 0, 0xd800]))
                .with_details(Json::object([("boundary", "lowercase_default".into())])),
        )
    })
    .collect();
    let locale = FaultLocale {
        native: native_locale(),
        failures: RefCell::new(failures.iter().cloned().collect()),
    };
    let executor = Executor::default();
    let projection = CategoryProjection::new(&world, &"owner".into(), &locale, &executor);
    assert_eq!(projection.normalize(None).await.unwrap(), Json::Null);
    assert_eq!(locale.failures.borrow().len(), 3);
    let categories = projection.categories();
    let normalize = projection.normalize(Some(&"work".into()));
    executor
        .drive(async {
            assert_eq!(
                outcome(categories.await),
                outcome(Err(failures[0].1.clone()))
            );
            assert_eq!(
                outcome(normalize.await),
                outcome(Err(failures[1].1.clone()))
            );
        })
        .await;
    assert_eq!(
        outcome(executor.drive(projection.revalidate(&"WORK".into())).await),
        outcome(Err(failures[2].1.clone()))
    );
    assert_eq!(
        executor.drive(projection.categories()).await.unwrap(),
        Json::object([("categories", Json::Array(vec!["Work".into()]))])
    );
    executor.drain(projection.settled()).await;
    assert!(locale.failures.borrow().is_empty());
    assert!(replay.steps.lock().unwrap().is_empty());
    native_typed_inputs(&locale.native).await;
}
