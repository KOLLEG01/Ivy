use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
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
    task::Poll,
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
        let p = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-categories-oracle.mjs",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(p.status.success(), "{}", String::from_utf8_lossy(&p.stderr));
        parse(
            std::str::from_utf8(&p.stdout).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
fn error(v: &Json) -> IvyError {
    IvyError::new_js(s(v, "code").clone(), s(v, "message").clone())
        .with_outcome(match f(v, "outcome").as_str().unwrap().as_str() {
            "unknown" => Outcome::Unknown,
            "completed" => Outcome::Completed,
            _ => Outcome::NotExecuted,
        })
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
                ("outcome", e.outcome.as_str().into()),
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
struct Client(Arc<Replay>);
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move { self.0.call("rpc", [m.into(), a]) })
    }
}
struct World<'a> {
    row: &'a Json,
    root: RefCell<Option<JsString>>,
    client: Client,
    replay: Arc<Replay>,
}
impl CategoryStore for World<'_> {
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
impl CategoryLocale for World<'_> {
    fn lowercase_default(&self, v: &JsString) -> JsString {
        let row = f(f(self.row, "locale"), "lower")
            .as_array()
            .unwrap()
            .iter()
            .find(|r| f(r, "input") == &Json::String(v.clone()))
            .unwrap_or_else(|| panic!("Unbound locale lowercase {v:?}: {:?}", self.replay.name));
        s(row, "output").clone()
    }
    fn compare_default(&self, a: &JsString, b: &JsString) -> Ordering {
        let rank = |v: &JsString| {
            f(f(self.row, "locale"), "ranks")
                .as_array()
                .unwrap()
                .iter()
                .find(|r| f(r, "input") == &Json::String(v.clone()))
                .unwrap_or_else(|| panic!("Unbound locale rank {v:?}"))
                .required("rank")
                .unwrap()
                .as_f64()
                .unwrap()
        };
        rank(a).partial_cmp(&rank(b)).unwrap()
    }
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
        let mut f = Box::pin(f);
        let mut output = None;
        poll_fn(|cx| {
            if output.is_none() {
                if let Poll::Ready(v) = f.as_mut().poll(cx) {
                    output = Some(v);
                }
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
        let world = World {
            row,
            root: RefCell::new(f(f(row, "initial"), "root").as_js_string().cloned()),
            client: Client(replay.clone()),
            replay: replay.clone(),
        };
        let executor = Executor::default();
        let mut principal = s(f(row, "initial"), "principal").clone();
        let mut projection = CategoryProjection::new(&world, &principal, &world, &executor);
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
                    executor.drive(projection.settled()).await;
                    if step.get("root").is_some() {
                        *world.root.borrow_mut() = f(step, "root").as_js_string().cloned();
                    }
                    if let Some(p) = step.get("principal") {
                        principal = p.as_js_string().unwrap().clone();
                    }
                    projection = CategoryProjection::new(&world, &principal, &world, &executor);
                    outcome(Ok(Json::Null))
                }
                "dropCategories" => {
                    drop(projection.categories());
                    outcome(Ok(Json::Null))
                }
                "drain" => {
                    executor.drive(projection.settled()).await;
                    outcome(Ok(Json::Null))
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
        executor.drive(projection.settled()).await;
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
