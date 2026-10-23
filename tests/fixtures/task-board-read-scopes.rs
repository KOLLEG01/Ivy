use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    engine_execution::{ExecutionReadScope, ReadScopeExecutor},
    journal_store::{Contracts, JournalStore, ReadScopes},
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    operations::Store,
    reconciler::ReconcileExecutor,
};
use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque},
    future::{Future, poll_fn},
    path::Path,
    pin::Pin,
    process::Command,
    rc::Rc,
    sync::{Arc, Mutex, OnceLock},
    task::Poll,
};
const KEY: &str = "task-board/history";
const TIME: &str = "2026-01-02T03:04:05.000Z";
fn clock() -> String {
    TIME.into()
}
fn new_id() -> JsString {
    panic!("Cache qualification must not create local records")
}
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    v.required(key).unwrap()
}
fn s<'a>(v: &'a Json, key: &str) -> &'a JsString {
    f(v, key).as_js_string().unwrap()
}
fn data() -> &'static Json {
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        let result = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-read-scopes-oracle.mjs",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        parse(
            std::str::from_utf8(&result.stdout).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
fn failure(v: &Json) -> IvyError {
    let mut error = IvyError::new_js(s(v, "code").clone(), s(v, "message").clone()).with_provider_outcome(v.required("outcome").unwrap().clone());
    if !f(v, "details").is_null() {
        error = error.with_details(f(v, "details").clone());
    }
    error
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
    pending: Mutex<VecDeque<Json>>,
    name: JsString,
}
impl Replay {
    fn new(row: &Json) -> Self {
        Self {
            pending: Mutex::new(
                f(row, "boundaries")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            name: s(row, "name").clone(),
        }
    }
    fn finished(&self) {
        assert!(
            self.pending.lock().unwrap().is_empty(),
            "{:?}: unconsumed RPCs",
            self.name
        );
    }
}
impl RpcClient for Replay {
    fn request<'a>(&'a self, method: &'a str, args: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            let frame = self
                .pending
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("{:?}: unexpected {method} {args:?}", self.name));
            assert_eq!(f(&frame, "method"), &Json::from(method), "{:?}", self.name);
            assert_eq!(
                hash_json(f(&frame, "args")).unwrap(),
                hash_json(&args).unwrap(),
                "{:?}: {method} {args:?}",
                self.name
            );
            // A miss suspends before its cache insertion; concurrent misses must
            // both reach the original RPC boundary rather than coalescing.
            let mut first = true;
            poll_fn(|cx| {
                if std::mem::replace(&mut first, false) {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            })
            .await;
            match frame.get("error") {
                Some(e) => Err(failure(e)),
                None => Ok(f(&frame, "value").clone()),
            }
        })
    }
}
// Exact finite value admission/identity upgrade, matching the source probe.
// Current contract policy is deliberately not supplied by this cache fixture.
struct Binding {
    allowed: HashSet<String>,
}
impl Contracts for Binding {
    fn version(&self, key: &str) -> IvyResult<String> {
        assert_eq!(key, KEY);
        Ok("1.0.0".into())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        assert_eq!(key, KEY);
        Ok(false)
    }
    fn readable_version(&self, key: &str, v: &JsString) -> IvyResult<bool> {
        assert_eq!(key, KEY);
        Ok(v == &JsString::from("1.0.0"))
    }
    fn validate_record(&self, key: &str, v: &JsString, value: &Json) -> IvyResult<()> {
        assert_eq!(key, KEY);
        assert_eq!(v, &JsString::from("1.0.0"));
        assert!(
            self.allowed.contains(&hash_json(value)?),
            "Fixture value was not admitted"
        );
        Ok(())
    }
    fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
        panic!("No policy admission in cache qualification")
    }
}
type Journal<'a> = JournalStore<'a, Replay, Binding, fn() -> String, fn() -> JsString>;
type Job<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;
#[derive(Default)]
struct Executor<'a> {
    jobs: RefCell<VecDeque<Job<'a>>>,
}
impl<'a> ReconcileExecutor<'a> for Executor<'a> {
    fn spawn(&self, work: Job<'a>) {
        self.jobs.borrow_mut().push_back(work);
    }
}
impl Executor<'_> {
    async fn drain(&self) {
        loop {
            let work = self.jobs.borrow_mut().pop_front();
            match work {
                Some(work) => work.await,
                None => return,
            }
        }
    }
}
async fn read(store: &Journal<'_>, step: &Json) -> Json {
    let result = store
        .read_record(
            KEY,
            f(step, "address"),
            step.get("primary").and_then(Json::as_bool).unwrap_or(true),
            step.get("includeArchived")
                .and_then(Json::as_bool)
                .unwrap_or(false),
        )
        .await;
    match result {
        Ok(mut d) => {
            let result = outcome(Ok(Json::object([
                ("pin", d.pin),
                ("value", d.value.clone()),
                (
                    "parentId",
                    d.parent_id.map(Json::String).unwrap_or(Json::Null),
                ),
                ("contentHash", d.content_hash.into()),
            ])));
            if step.get("mutate") == Some(&Json::Bool(true)) {
                d.value.insert("mark", "caller mutation".into());
                d.value.insert("nested", Json::Null);
            }
            result
        }
        Err(e) => outcome(Err(e)),
    }
}
// Submitted source probes contain only reads and nested read scopes, so their
// retained futures need no reference back into the executor that owns them.
fn submitted<'a>(
    store: &'a Journal<'a>,
    scopes: &'a ReadScopes,
    steps: Vec<Json>,
) -> Pin<Box<dyn Future<Output = Json> + 'a>> {
    Box::pin(async move {
        let mut results = vec![];
        for step in steps {
            results.push(match s(&step, "method").to_utf8().unwrap().as_str() {
                "read" => read(store, &step).await,
                "scope" => {
                    scopes
                        .with_read_cache(submitted(
                            store,
                            scopes,
                            f(&step, "steps").as_array().unwrap().to_vec(),
                        ))
                        .await
                }
                method => panic!("Unexpected submitted {method}"),
            });
        }
        Json::Array(results)
    })
}
fn run<'a, 's: 'a>(
    store: &'s Journal<'s>,
    scopes: &'s ReadScopes,
    executor: &'a Executor<'s>,
    completed: Rc<RefCell<Vec<Json>>>,
    steps: &'a [Json],
) -> Pin<Box<dyn Future<Output = Json> + 'a>> {
    Box::pin(async move {
        let mut results = vec![];
        for step in steps {
            let result = match s(step, "method").to_utf8().unwrap().as_str() {
                "scope" => {
                    scopes
                        .with_read_cache(run(
                            store,
                            scopes,
                            executor,
                            completed.clone(),
                            f(step, "steps").as_array().unwrap(),
                        ))
                        .await
                }
                "parallel" => {
                    let steps = f(step, "steps").as_array().unwrap();
                    assert_eq!(steps.len(), 2);
                    let (a, b) = tokio::join!(biased;
                        run(store,scopes,executor,completed.clone(),std::slice::from_ref(&steps[0])),
                        run(store,scopes,executor,completed.clone(),std::slice::from_ref(&steps[1])));
                    Json::Array(vec![a, b])
                }
                "submit" => {
                    let steps = f(step, "steps").as_array().unwrap().to_vec();
                    let label = f(step, "label").clone();
                    let completed = completed.clone();
                    ReadScopeExecutor { scopes, executor }.spawn(Box::pin(async move {
                        let outcomes = submitted(store, scopes, steps).await;
                        completed
                            .borrow_mut()
                            .push(Json::object([("label", label), ("outcomes", outcomes)]));
                    }));
                    Json::Null
                }
                "drain" => {
                    executor.drain().await;
                    Json::Array(completed.borrow().clone())
                }
                "read" => read(store, step).await,
                "writePrepared" => outcome(
                    store
                        .write_prepared(
                            KEY,
                            f(step, "value"),
                            &"retained-cache-fixture".into(),
                            f(step, "destination"),
                        )
                        .await,
                ),
                method => panic!("Unknown cache action {method}"),
            };
            results.push(result);
        }
        Json::Array(results)
    })
}
#[tokio::test(flavor = "current_thread")]
async fn action_scopes_match_original_async_local_store_including_retained_submissions() {
    for row in data().as_array().unwrap() {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let scopes = ReadScopes::default();
        let client = Arc::new(Replay::new(row));
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Binding {
                allowed: f(row, "allowed")
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect(),
            },
            limits: HistoryLimits::default(),
            now: clock as fn() -> String,
            new_id: new_id as fn() -> JsString,
            cache: Some(scopes.cache()),
        };
        journal
            .journal
            .create(
                KEY,
                &"Local".into(),
                Some(&"root".into()),
                &Json::object([
                    ("mark", "v1".into()),
                    (
                        "nested",
                        Json::object([("list", Json::Array(vec![Json::Number(1.0)]))]),
                    ),
                ]),
                &TIME.into(),
                &"local".into(),
            )
            .unwrap();
        let executor = Executor::default();
        let outcomes = run(
            &journal,
            &scopes,
            &executor,
            Rc::default(),
            f(row, "steps").as_array().unwrap(),
        )
        .await;
        assert!(
            executor.jobs.borrow().is_empty(),
            "{:?}: retained work was not drained",
            s(row, "name")
        );
        let actual = Json::object([("outcomes", outcomes)]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: actual {actual:?}, expected {:?}",
            s(row, "name"),
            f(row, "expected")
        );
        client.finished();
    }
}
#[tokio::test(flavor = "current_thread")]
async fn cancelled_and_panicking_polls_restore_the_preceding_action_context() {
    let row = data()
        .as_array()
        .unwrap()
        .iter()
        .find(|r| s(r, "name") == &JsString::from("outside actions never cache"))
        .unwrap();
    for cancelled in [true, false] {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let scopes = ReadScopes::default();
        let client = Arc::new(Replay::new(row));
        let journal = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Binding {
                allowed: f(row, "allowed")
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect(),
            },
            limits: HistoryLimits::default(),
            now: clock as fn() -> String,
            new_id: new_id as fn() -> JsString,
            cache: Some(scopes.cache()),
        };
        let step = &f(row, "steps").as_array().unwrap()[0];
        if cancelled {
            let mut work = Box::pin(scopes.with_read_cache(read(&journal, step)));
            poll_fn(|cx| {
                assert!(work.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(work);
        } else {
            let mut panicking = Box::pin(scopes.with_read_cache(async {
                panic!("Injected child poll unwind");
            }));
            ExecutionReadScope::with_read_cache(&scopes, async {
                assert!(read(&journal, step).await.get("ok").is_some());
                poll_fn(|cx| {
                    assert!(
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| panicking
                            .as_mut()
                            .poll(cx)))
                        .is_err()
                    );
                    Poll::Ready(())
                })
                .await;
                assert!(read(&journal, step).await.get("ok").is_some());
                Ok(Json::Null)
            })
            .await
            .unwrap();
        }
        // Both reads are outside every action. A leaked context would cache the
        // first and leave one of the exact original RPC frames unconsumed.
        for _ in 0..2 {
            assert!(read(&journal, step).await.get("ok").is_some());
        }
        client.finished();
    }
}
