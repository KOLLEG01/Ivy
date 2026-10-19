use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    engine_execution::{
        CachedExecution, ExecutionCategories, ExecutionComponents, ExecutionProofs,
        ExecutionPublication, ExecutionQueue, ExecutionReadScope, ExecutionStore,
    },
    operations::{Document, Operations, Store},
    publication_coordination::{CoordinationStore, PublicationCoordination},
    reconciler::ReconcileExecutor,
};
use std::{
    cell::RefCell,
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
                "tests/fixtures/task-board-engine-execution-oracle.mjs",
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
    fn call(&self, kind: &str, args: impl IntoIterator<Item = Json>) -> IvyResult<Json> {
        let args = Json::Array(args.into_iter().collect());
        let e = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("{:?}: unexpected {kind}: {args:?}", self.name));
        assert_eq!(
            f(&e, "kind"),
            &Json::from(kind),
            "{:?}: {args:?}",
            self.name
        );
        assert_eq!(
            hash_json(f(&e, "args")).unwrap(),
            hash_json(&args).unwrap(),
            "{:?} {kind}: expected {:?}, actual {args:?}",
            self.name,
            f(&e, "args")
        );
        if let Some(e) = e.get("error") {
            Err(error(e))
        } else {
            Ok(f(&e, "value").clone())
        }
    }
}
struct World {
    replay: Arc<Replay>,
    root: Option<JsString>,
}
fn doc(v: &Json) -> Document {
    Document {
        pin: v.required("pin").unwrap().clone(),
        value: v.required("value").unwrap().clone(),
        parent_id: f(f(v, "metadata"), "parentId").as_js_string().cloned(),
        content_hash: hash_json(v.required("value").unwrap()).unwrap(),
    }
}
impl Store for World {
    fn root_object_id(&self) -> Option<JsString> {
        self.root.clone()
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        unreachable!()
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>> {
        self.replay
            .call("localKey", [Json::String(id.clone())])
            .map(|v| v.as_js_string().map(|s| s.to_utf8().unwrap()))
    }
    fn complete_local_workflow(&self, id: &JsString, at: &JsString) -> IvyResult<()> {
        self.replay
            .call(
                "complete",
                [Json::String(id.clone()), Json::String(at.clone())],
            )
            .map(|_| ())
    }
    async fn named(&self, k: &str, n: &str, p: Option<&JsString>) -> IvyResult<Option<Document>> {
        let v = self.replay.call(
            "named",
            [
                k.into(),
                n.into(),
                p.cloned().map(Json::String).unwrap_or(Json::Null),
            ],
        )?;
        Ok((!v.is_null()).then(|| doc(&v)))
    }
    async fn read(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        Ok(doc(&self
            .replay
            .call("read", [k.into(), a.clone(), p.into()])?))
    }
    async fn write(&self, k: &str, v: &Json, m: &str, d: &Json) -> IvyResult<Json> {
        self.replay
            .call("write", [k.into(), v.clone(), m.into(), d.clone()])
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        unreachable!()
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
}
impl CoordinationStore for World {
    fn operation_gates(&self, id: &JsString) -> IvyResult<Vec<JsString>> {
        Ok(self
            .replay
            .call("gateNames", [Json::String(id.clone())])?
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_js_string().unwrap().clone())
            .collect())
    }
    fn retain_operation_gates(&self, id: &JsString, n: &[JsString]) -> IvyResult<()> {
        self.replay
            .call(
                "retainGates",
                [
                    Json::String(id.clone()),
                    Json::Array(n.iter().cloned().map(Json::String).collect()),
                ],
            )
            .map(|_| ())
    }
    fn pending_coordination(&self) -> IvyResult<Option<Json>> {
        self.replay
            .call("pending", [])
            .map(|v| (!v.is_null()).then_some(v))
    }
    async fn named_gate(&self, n: &JsString) -> IvyResult<Option<Document>> {
        let v = self.replay.call(
            "named",
            [
                "task-board/coordination".into(),
                Json::String(n.clone()),
                Json::Null,
            ],
        )?;
        Ok((!v.is_null()).then(|| doc(&v)))
    }
}
impl ExecutionStore for World {
    fn release_workspace_operation(&self, id: &JsString) -> IvyResult<()> {
        self.replay
            .call("workspaceRelease", [Json::String(id.clone())])
            .map(|_| ())
    }
}
struct Bindings(Arc<Replay>);
impl ExecutionCategories for Bindings {
    async fn normalize(&self, v: &JsString) -> IvyResult<JsString> {
        self.0
            .call("normalize", [Json::String(v.clone())])
            .map(|v| v.as_js_string().unwrap().clone())
    }
    async fn revalidate(&self, v: &JsString) -> IvyResult<JsString> {
        self.0
            .call("revalidate", [Json::String(v.clone())])
            .map(|v| v.as_js_string().unwrap().clone())
    }
}
impl ExecutionProofs for Bindings {
    type Transcript = Json;
    async fn owner_stopped(&self, v: &Json) -> IvyResult<bool> {
        self.0
            .call("ownerStopped", [v.clone()])
            .map(|v| v.as_bool().unwrap())
    }
    async fn verify_transcript(&self, r: &Json, a: &Json) -> IvyResult<Json> {
        self.0
            .call("transcript", ["board".into(), r.clone(), a.clone()])
    }
}
impl ExecutionPublication<Json> for Bindings {
    async fn publish(
        &self,
        op: &Document,
        t: Option<&Json>,
        n: Option<&JsString>,
    ) -> IvyResult<Json> {
        self.0.call(
            "publish",
            [
                op.pin.clone(),
                op.value.clone(),
                t.cloned().unwrap_or(Json::Null),
                n.cloned().map(Json::String).unwrap_or(Json::Null),
            ],
        )
    }
}
impl ExecutionReadScope for Bindings {
    async fn with_read_cache<F: Future<Output = IvyResult<Json>>>(
        &self,
        work: F,
    ) -> IvyResult<Json> {
        self.0.call("cache", [])?;
        work.await
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
        let mut result = None;
        poll_fn(|cx| {
            if result.is_none() {
                if let Poll::Ready(v) = f.as_mut().poll(cx) {
                    result = Some(v)
                }
            }
            let mut tasks = std::mem::take(&mut *self.work.borrow_mut());
            let mut pending = VecDeque::new();
            while let Some(mut f) = tasks.pop_front() {
                if f.as_mut().poll(cx).is_pending() {
                    pending.push_back(f)
                }
            }
            self.work.borrow_mut().extend(pending);
            if result.is_some() && self.work.borrow().is_empty() {
                Poll::Ready(result.take().unwrap())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}
#[tokio::test(flavor = "current_thread")]
async fn publication_gates_and_retained_execution_match_original_recovery() {
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
        let root = f(row, "options")
            .get("root")
            .map(|v| v.as_js_string().cloned())
            .unwrap_or(Some("root".into()));
        let operations = Operations {
            store: World {
                replay: replay.clone(),
                root,
            },
            now: || "2026-01-02T03:04:05.006Z".to_owned(),
        };
        let principal = JsString::from("board");
        let bindings = Bindings(replay.clone());
        let components = ExecutionComponents {
            categories: &bindings,
            proofs: &bindings,
            publication: &bindings,
            read_scope: &bindings,
        };
        let cached = CachedExecution {
            operations: &operations,
            principal_id: &principal,
            services: &components,
        };
        let executor = Executor::default();
        let queue = ExecutionQueue::new(&cached, &executor);
        let gates = PublicationCoordination {
            store: &operations.store,
            principal_id: &principal,
            now: &operations.now,
        };
        let mut outcomes = vec![];
        for step in f(row, "steps").as_array().unwrap() {
            let r = match s(step, "method").to_utf8().unwrap().as_str() {
                "patch" => Ok(Json::Null),
                "current" => gates
                    .current(&"TaskBoard publication gate".into())
                    .await
                    .map(|d| Json::object([("pin", d.pin), ("value", d.value)])),
                "acquire" => gates
                    .acquire(&doc(f(step, "operation")))
                    .await
                    .map(|_| Json::Null),
                "reserved" => gates
                    .reserved(&doc(f(step, "operation")))
                    .await
                    .map(Json::Bool),
                "release" => gates
                    .release(&"op".into(), f(step, "reserved").as_bool().unwrap_or(false))
                    .await
                    .map(|_| Json::Null),
                "unstarted" => gates.release_unstarted(&"op".into()).await.map(Json::Bool),
                "pending" => gates.pending().map(|v| v.unwrap_or(Json::Null)),
                "failure" => {
                    let e = f(step, "cause");
                    let error = IvyError::new_js(s(e, "code").clone(), s(e, "message").clone())
                        .with_outcome(
                            match f(e, "outcome").as_str().unwrap_or_default().as_str() {
                                "unknown" => Outcome::Unknown,
                                "completed" => Outcome::Completed,
                                _ => Outcome::NotExecuted,
                            },
                        );
                    cached.failed_publication(&"op".into(), error).await
                }
                "queue" => {
                    let first = queue.execute(doc(f(step, "operation"))).unwrap();
                    let second = queue.execute(doc(f(step, "operation"))).unwrap();
                    executor
                        .drive(async {
                            Ok(Json::Array(vec![
                                outcome(first.await),
                                outcome(second.await),
                            ]))
                        })
                        .await
                }
                "drop" => {
                    drop(queue.execute(doc(f(step, "operation"))).unwrap());
                    executor.drive(queue.settled()).await;
                    Ok(Json::Null)
                }
                "execute" => {
                    executor
                        .drive(queue.execute(doc(f(step, "operation"))).unwrap())
                        .await
                }
                k => panic!("Unknown execution action {k}"),
            };
            outcomes.push(outcome(r));
        }
        executor.drive(queue.settled()).await;
        let actual = Json::object([("outcomes", Json::Array(outcomes))]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: actual {actual:?}, expected {:?}",
            replay.name,
            f(row, "expected")
        );
        assert!(
            replay.steps.lock().unwrap().is_empty(),
            "{:?}: unconsumed execution boundaries",
            replay.name
        );
    }
}
