use ivy_contracts::checked_native_contract::CheckedNativeContracts;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use ivy_task_board::{
    checks::{CheckStore, TaskChecks, date},
    native_lifecycle::{LifecycleDocument, LifecycleStore},
    operations::{Document, Store},
};
use std::{
    collections::VecDeque,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
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
                "tests/fixtures/task-board-checks-oracle.mjs",
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
    fn contract_version(&self, key: &str) -> IvyResult<String> {
        Ok(match key {
            "task-board/task" => "1.8.0",
            "task-board/configuration" => "1.2.0",
            "task-board/run" | "task-board/delivery" => "1.1.0",
            _ => "1.0.0",
        }
        .into())
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
    async fn prepared_mutation_id(&self, m: &str) -> IvyResult<JsString> {
        Ok(self
            .replay
            .call("prepared", [m.into()])?
            .as_js_string()
            .unwrap()
            .clone())
    }
    async fn write_prepared(&self, k: &str, v: &Json, m: &JsString, d: &Json) -> IvyResult<Json> {
        self.replay.call(
            "apply",
            [k.into(), v.clone(), Json::String(m.clone()), d.clone()],
        )
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
    fn release_workspace(&self, id: &JsString) -> IvyResult<()> {
        self.replay
            .call("release", [Json::String(id.clone())])
            .map(|_| ())
    }
}
struct Rpc(Arc<Replay>);
impl RpcClient for Rpc {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move { self.0.call("rpc", [method.into(), params]) })
    }
}
impl CheckStore for World {
    fn client(&self) -> Arc<dyn RpcClient> {
        Arc::new(Rpc(self.replay.clone()))
    }
    fn local_artifact(&self, pin: &Json) -> IvyResult<Option<Json>> {
        let v = self.replay.call("local", [pin.clone()])?;
        Ok((!v.is_null()).then_some(v))
    }
    async fn check_configuration(
        &self,
        value: &Json,
        node: Option<&JsString>,
        allow: bool,
    ) -> IvyResult<()> {
        self.replay
            .call(
                "configuration",
                [
                    value.clone(),
                    node.cloned().map(Json::String).unwrap_or(Json::Null),
                    allow.into(),
                ],
            )
            .map(|_| ())
    }
}
#[tokio::test(flavor = "current_thread")]
async fn task_checks_match_original_graph_artifact_creation_configuration_and_execution_rules() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for row in data().as_array().unwrap() {
        let replay = Arc::new(Replay {
            name: s(row, "name").clone(),
            steps: Mutex::new(
                f(row, "boundaries")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
        });
        let store = World {
            replay: replay.clone(),
            root: Some("root".into()),
        };
        let tools = ToolClients::new(store.client());
        let checks = TaskChecks {
            store: &store,
            catalogs: &catalogs,
            tools: &tools,
        };
        let args = f(row, "args").as_array().unwrap();
        let null = Json::Null;
        let arg = |n: usize| args.get(n).unwrap_or(&null);
        let id = |n: usize| arg(n).as_js_string();
        let mut results = vec![];
        for _ in 0..f(f(row, "options"), "repeat").as_f64().unwrap_or(1.0) as usize {
            let result = match s(row, "method").to_utf8().unwrap().as_str() {
                "date" => date(args.first()).map(|_| Json::Null),
                "artifact" => checks.artifact(arg(0)).await.map(|_| Json::Null),
                "content" => checks.content(arg(0)).await.map(|_| Json::Null),
                "dependencies" => checks
                    .dependencies(arg(0), id(1), arg(2).as_bool().unwrap_or(false))
                    .await
                    .map(|_| Json::Null),
                "fields" => checks.fields(arg(0), id(1)).await.map(|_| Json::Null),
                "initialState" => {
                    checks
                        .initial_state(arg(0), arg(1), arg(2), arg(3).as_js_string().unwrap())
                        .await
                }
                "deferral" => checks
                    .deferral(arg(0), id(1), arg(2).as_js_string().unwrap())
                    .await
                    .map(|_| Json::Null),
                "checkBlocker" => checks.check_blocker(args.first()).await.map(|_| Json::Null),
                "executionOptions" => checks
                    .execution_options(
                        arg(0),
                        (!arg(1).is_null()).then(|| arg(1)),
                        arg(2).as_bool().unwrap_or(false),
                    )
                    .await
                    .map(|_| Json::Null),
                "execution" => {
                    checks
                        .execution(
                            arg(0),
                            arg(1),
                            id(2).unwrap(),
                            id(3).unwrap(),
                            (!arg(4).is_null()).then(|| arg(4)),
                            (!arg(5).is_null()).then(|| arg(5)),
                            arg(6).as_bool().unwrap_or(false),
                        )
                        .await
                }
                v => panic!("Unknown {v}"),
            };
            results.push(outcome(result));
        }
        let actual = Json::object([("outcomes", Json::Array(results))]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: actual {actual:?}, expected {:?}",
            replay.name,
            f(row, "expected")
        );
        assert!(
            replay.steps.lock().unwrap().is_empty(),
            "{:?}: unconsumed source boundaries",
            replay.name
        );
    }
}
