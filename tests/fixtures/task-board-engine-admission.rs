use ivy_contracts::checked_native_contract::CheckedNativeContracts;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_task_board::{
    engine_admission::{AdmissionExecution, AdmissionRuntime, EngineAdmission, EngineOwner},
    engine_read::ReadContext,
    journal_store::Contracts,
    operations::{Document, Operations, Store},
    publication_coordination::CoordinationStore,
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
                "tests/fixtures/task-board-engine-admission-oracle.mjs",
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
    fn read_record(&self, k: &str, a: &Json, p: bool, _archived: bool) -> IvyResult<Json> {
        self.replay
            .call("read", [k.into(), a.clone(), Json::Bool(p)])
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
        Ok((!v.is_null()).then(|| document(&v)))
    }
}
impl AdmissionExecution for Bindings {
    async fn execute(&self, operation: Document) -> IvyResult<Json> {
        self.0.call("execute", [operation.pin, operation.value])
    }
}
#[tokio::test(flavor = "current_thread")]
async fn public_admission_matches_original_identity_plan_and_recovery_boundaries() {
    let catalogs = CheckedNativeContracts::new(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../specs/native"),
    );
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
        let client = Arc::new(Client(replay.clone()));
        let bindings = Bindings(replay.clone());
        let owner = EngineOwner {
            service_node_id: "task-node".into(),
            generation: 7.0,
        };
        let runtime = AdmissionRuntime {
            client,
            contracts: &bindings,
            catalogs: &catalogs,
        };
        let operations = Operations {
            store: World {
                replay: replay.clone(),
                root: f(row, "root").as_js_string().cloned(),
            },
            now: || "2026-01-02T03:04:05.006Z".to_owned(),
        };
        let mut outcomes = vec![];
        match EngineAdmission::new(&operations, f(row, "settings"), &owner, &runtime, &bindings) {
            Err(e) => outcomes.push(outcome(Err(e))),
            Ok(engine) => {
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
                    let result = match s(step, "method").to_utf8().unwrap().as_str() {
                        "invoke" => engine.invoke(f(step, "request"), &context).await,
                        "configuration" => engine.configuration().await,
                        "schedule" => engine.schedule(f(step, "request")).await,
                        "nativeUpdate" => engine.native_update(f(step, "request")).await,
                        "nativeComment" => engine.native_comment(f(step, "request")).await,
                        "recoverOperation" => engine.recover_operation(s(step, "objectId")).await,
                        "recoverPublication" => engine.recover_publication().await,
                        m => panic!("Unknown admission scenario {m}"),
                    };
                    outcomes.push(outcome(result));
                }
            }
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
            "{:?}: unconsumed original boundaries",
            replay.name
        );
    }
}
