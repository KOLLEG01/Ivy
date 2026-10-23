use ivy_contracts::{
    Outcome,
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_task_board::{
    engine_read::{EngineReads, ReadContext, ReadRuntime, execution_condition, workspace_identity},
    journal_store::Contracts,
    native_lifecycle::{LifecycleDocument, LifecycleStore},
    operations::{Document, Operations, Store},
};
use std::{
    collections::VecDeque,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};
#[path = "task-board-engine-read-journal.rs"]
mod journal;
#[path = "task-board-peer-state.rs"]
mod peer_state;
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
                "tests/fixtures/task-board-engine-read-oracle.mjs",
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
struct Client(Arc<Replay>);
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move { self.0.call("rpc", [m.into(), a]) })
    }
}
struct Bindings(Arc<Replay>);
impl Contracts for Bindings {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.8.0".into())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        unreachable!()
    }
    fn readable_version(&self, _: &str, _: &JsString) -> IvyResult<bool> {
        unreachable!()
    }
    fn validate_record(&self, _: &str, _: &JsString, _: &Json) -> IvyResult<()> {
        unreachable!()
    }
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        self.0.call("validate", [n.into(), v.clone()]).map(|_| ())
    }
}
struct World {
    replay: Arc<Replay>,
    root: Option<JsString>,
}
fn document(v: &Json) -> Document {
    Document {
        pin: v.required("pin").unwrap().clone(),
        value: v.required("value").unwrap().clone(),
        parent_id: f(f(v, "metadata"), "parentId").as_js_string().cloned(),
        content_hash: hash_json(v.required("value").unwrap()).unwrap(),
    }
}
impl World {
    fn read_record(&self, k: &str, a: &Json, p: bool, archived: bool) -> IvyResult<Json> {
        self.replay.call(
            "read",
            [k.into(), a.clone(), Json::Bool(p), Json::Bool(archived)],
        )
    }
}
impl Store for World {
    fn root_object_id(&self) -> Option<JsString> {
        self.root.clone()
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        Ok("1.8.0".into())
    }
    fn retired_operation(&self, c: &JsString, id: &JsString) -> IvyResult<bool> {
        Ok(self
            .replay
            .call(
                "retired",
                [Json::String(c.clone()), Json::String(id.clone())],
            )?
            .as_bool()
            .unwrap())
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        unreachable!()
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        unreachable!()
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
        Ok((!v.is_null()).then(|| document(&v)))
    }
    async fn read(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        Ok(document(&self.read_record(k, a, p, false)?))
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
impl LifecycleStore for World {
    async fn lifecycle_read(
        &self,
        k: &str,
        a: &Json,
        p: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        let v = self.read_record(k, a, p, archived)?;
        Ok(LifecycleDocument {
            document: document(&v),
            metadata: v.required("metadata")?.clone(),
        })
    }
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        self.replay.call("page", [k.into(), o.clone()])
    }
    async fn archive(&self, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
    async fn stat(&self, _: &JsString) -> IvyResult<Json> {
        unreachable!()
    }
    fn release_workspace(&self, _: &JsString) -> IvyResult<()> {
        unreachable!()
    }
}
#[tokio::test(flavor = "current_thread")]
async fn engine_reads_match_original_queries_peer_paths_conditions_and_marker_retries() {
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
        let client = Client(replay.clone());
        let bindings = Bindings(replay.clone());
        let principal = JsString::from("board");
        let node = JsString::from("task-node");
        let runtime = ReadRuntime {
            client: &client,
            contracts: &bindings,
            principal_id: &principal,
            service_node_id: &node,
            generation: 7.0,
        };
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
        let reads = EngineReads {
            operations: &operations,
            runtime: &runtime,
        };
        let mut outcomes = vec![];
        for step in f(row, "steps").as_array().unwrap() {
            let caller = f(step, "caller")
                .as_js_string()
                .cloned()
                .unwrap_or_else(|| "caller".into());
            let operation_id = f(step, "operationId")
                .as_js_string()
                .or_else(|| f(f(step, "request"), "operationId").as_js_string())
                .cloned()
                .unwrap_or_else(|| "context".into());
            let cancelled = || replay.call("cancel", []).map(|_| ());
            let context = ReadContext {
                caller_principal_id: &caller,
                operation_id: &operation_id,
                generation: f(step, "generation").as_f64().unwrap_or(7.0),
                check_cancelled: &cancelled,
            };
            let r = match s(step, "kind").to_utf8().unwrap().as_str() {
                "condition" => execution_condition(f(step, "task")),
                "workspace" => workspace_identity(&operations.store, f(step, "task"))
                    .await
                    .map(|v| v.map(Json::String).unwrap_or(Json::Null)),
                "task" => {
                    reads
                        .task(
                            f(step, "query"),
                            f(step, "principal")
                                .as_js_string()
                                .unwrap_or(&"caller".into()),
                        )
                        .await
                }
                "list" => reads.list(f(step, "query")).await,
                "mark" => reads.mark_comments_read(f(step, "request"), &context).await,
                "operation" => reads.operation(s(step, "operationId"), &context).await,
                k => panic!("Unknown read scenario {k}"),
            };
            outcomes.push(outcome(r));
        }
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
            "{:?}: unconsumed reference boundaries",
            replay.name
        );
    }
}
