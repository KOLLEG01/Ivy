use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use ivy_task_board::{
    allocation::{AllocationRuntime, AllocationStore, Allocator},
    native_intent::TaskPrompts,
    operations::{Document, Store},
    scheduler::{
        Scheduler, SchedulerAllocation, SchedulerEngine, SchedulerOperations, SchedulerPolicies,
        SchedulerStore,
    },
};
use std::{
    cmp::Ordering,
    collections::{HashMap, VecDeque},
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};
#[path = "task-board-scheduler-journal.rs"]
mod journal;

fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn undefined() -> Json {
    Json::object([("undefined", true.into())])
}
fn nullable(v: Option<&JsString>) -> Json {
    v.cloned().map(Json::String).unwrap_or_else(undefined)
}
fn doc(v: &Json) -> Document {
    Document {
        pin: f(v, "pin").clone(),
        value: f(v, "value").clone(),
        content_hash: hash_json(f(v, "value")).unwrap(),
        parent_id: f(f(v, "metadata"), "parentId").as_js_string().cloned(),
    }
}
fn json_doc(v: &Document) -> Json {
    Json::object([
        ("pin", v.pin.clone()),
        ("value", v.value.clone()),
        (
            "metadata",
            Json::object([(
                "parentId",
                v.parent_id.clone().map(Json::String).unwrap_or(Json::Null),
            )]),
        ),
    ])
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
fn outcome(r: IvyResult<Json>) -> Json {
    match r {
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
fn data() -> &'static Json {
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        let o = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-scheduler-oracle.mjs",
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
struct Replay {
    name: JsString,
    steps: Mutex<VecDeque<Json>>,
    aliases: Mutex<HashMap<JsString, JsString>>,
}
impl Replay {
    fn call(&self, kind: &str, args: impl IntoIterator<Item = Json>) -> IvyResult<Json> {
        let args = Json::Array(args.into_iter().collect());
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("Unexpected {kind} in {:?}: {args:?}", self.name));
        assert_eq!(
            f(&step, "kind"),
            &Json::from(kind),
            "{:?}: {args:?}",
            self.name
        );
        assert_eq!(
            hash_json(f(&step, "args")).unwrap(),
            hash_json(&args).unwrap(),
            "{:?}: {kind} expected {:?}, got {args:?}",
            self.name,
            f(&step, "args")
        );
        match step.get("error") {
            Some(e) => Err(error(e)),
            None => Ok(f(&step, "value").clone()),
        }
    }
    fn rpc_args(&self, mut a: Json) -> Json {
        // Model-list IDs are deliberately fresh in both implementations. Validate their
        // real wire identity before replacing only this nondeterministic observation.
        if f(&a, "qualifiedName") == &Json::from("codex.model/list") {
            let id = s(&a, "operationId").clone();
            let wire = id.to_utf8().unwrap();
            let parts = wire.split(':').collect::<Vec<_>>();
            assert_eq!(parts.len(), 3);
            assert_eq!(parts[0], "11111111-1111-4111-8111-111111111111");
            assert!(parts[1].parse::<u64>().unwrap() > 0);
            let uuid = uuid::Uuid::parse_str(parts[2]).unwrap();
            assert_eq!(uuid.get_version_num(), 4);
            let mut aliases = self.aliases.lock().unwrap();
            let next = aliases.len() + 1;
            let alias = aliases
                .entry(id)
                .or_insert_with(|| format!("generated-{next}").into())
                .clone();
            a.insert("operationId", Json::String(alias));
        }
        a
    }
}
struct Client(Arc<Replay>);
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move { self.0.call("rpc", [m.into(), self.0.rpc_args(a)]) })
    }
}
struct World<'a> {
    row: &'a Json,
    replay: Arc<Replay>,
    client: Arc<Client>,
    tools: ToolClients,
    service: JsString,
    principal: JsString,
}
impl<'a> World<'a> {
    fn new(row: &'a Json) -> Self {
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
            aliases: Mutex::default(),
        });
        let client = Arc::new(Client(replay.clone()));
        let tools = ToolClients::new(client.clone());
        Self {
            row,
            replay,
            client,
            tools,
            service: "task-board-node".into(),
            principal: "caller".into(),
        }
    }
    fn input(&self) -> &Json {
        f(self.row, "input")
    }
    fn compare(&self, a: &JsString, b: &JsString) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        let c = f(self.row, "compare")
            .as_array()
            .unwrap()
            .iter()
            .find(|r| s(r, "a") == a && s(r, "b") == b)
            .unwrap();
        f(c, "result").as_f64().unwrap().partial_cmp(&0.0).unwrap()
    }
    fn task(&self) -> Document {
        doc(f(f(self.row, "initial"), "task"))
    }
}
impl Store for World<'_> {
    fn root_object_id(&self) -> Option<JsString> {
        Some("root".into())
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        panic!("Unexpected retirement")
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        panic!("Unexpected local key")
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
        let v = self
            .replay
            .call("named", [k.into(), n.into(), nullable(p)])?;
        Ok((!v.is_null()).then(|| doc(&v)))
    }
    async fn read(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        self.replay
            .call("read", [k.into(), a.clone(), p.into()])
            .map(|v| doc(&v))
    }
    async fn write(&self, k: &str, v: &Json, m: &str, d: &Json) -> IvyResult<Json> {
        self.replay
            .call("write", [k.into(), v.clone(), m.into(), d.clone()])
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("Unexpected preparation")
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        panic!("Unexpected prepared write")
    }
}
impl AllocationStore for World<'_> {
    async fn named_plan(&self, n: &JsString) -> IvyResult<Option<Document>> {
        let v = self.replay.call(
            "named",
            [
                "task-board/native-plan".into(),
                Json::String(n.clone()),
                undefined(),
            ],
        )?;
        Ok((!v.is_null()).then(|| doc(&v)))
    }
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        self.replay.call("page", [k.into(), o.clone()])
    }
    async fn workspace_owner(
        &self,
        h: &JsString,
        p: &JsString,
        t: &JsString,
    ) -> IvyResult<Option<JsString>> {
        Ok(self
            .replay
            .call(
                "workspace-owner",
                [
                    Json::String(h.clone()),
                    Json::String(p.clone()),
                    Json::String(t.clone()),
                ],
            )?
            .as_js_string()
            .cloned())
    }
}
impl SchedulerStore for World<'_> {
    fn free_bytes(&self) -> IvyResult<Option<f64>> {
        Ok(f(&self.replay.call("free", [])?, "freeBytes").as_f64())
    }
}
impl TaskPrompts for World<'_> {
    fn task_prompt(&self, t: &Json, o: &JsString, c: Option<&Json>) -> IvyResult<JsString> {
        Ok(self
            .replay
            .call(
                "prompt",
                [
                    t.clone(),
                    Json::String(o.clone()),
                    c.cloned().unwrap_or_else(undefined),
                ],
            )?
            .as_js_string()
            .unwrap()
            .clone())
    }
    fn task_update_prompt(&self, _: &Json, _: &JsString, _: bool) -> IvyResult<JsString> {
        panic!("Unexpected update prompt")
    }
}
impl AllocationRuntime for World<'_> {
    fn client(&self) -> Arc<dyn RpcClient> {
        self.client.clone()
    }
    fn tools(&self) -> &ToolClients {
        &self.tools
    }
    fn service_node_id(&self) -> &JsString {
        &self.service
    }
    fn compare_default(&self, a: &JsString, b: &JsString) -> Ordering {
        self.compare(a, b)
    }
}
impl SchedulerPolicies for World<'_> {
    fn enabled(&self) -> bool {
        f(self.input(), "enabled") != &false.into()
    }
    fn interval_ms(&self) -> f64 {
        f(self.input(), "interval").as_f64().unwrap_or(1000.0)
    }
    fn now(&self) -> JsString {
        "2026-01-02T03:04:05.006Z".into()
    }
    fn compare_default(&self, a: &JsString, b: &JsString) -> Ordering {
        self.compare(a, b)
    }
    fn coordination_pending(&self, _: &Json) -> IvyResult<bool> {
        Ok(f(self.input(), "coordination") == &true.into())
    }
    fn peer_comment(&self, c: &Json) -> IvyResult<bool> {
        Ok(f(c, "peer") == &true.into())
    }
    async fn forward_delegated_review(&self, t: &Document) -> IvyResult<()> {
        self.replay.call("forward", [json_doc(t)]).map(|_| ())
    }
    async fn unstarted_missing_context(&self, t: &Document) -> IvyResult<bool> {
        Ok(self
            .replay
            .call("missing", [json_doc(t)])?
            .as_bool()
            .unwrap())
    }
    async fn unstarted_archived_context(&self, t: &Document) -> IvyResult<bool> {
        Ok(self
            .replay
            .call("restore", [json_doc(t)])?
            .as_bool()
            .unwrap())
    }
    async fn failed_rate_limit_retry_at(&self, t: &Document) -> IvyResult<Option<JsString>> {
        Ok(self
            .replay
            .call("retry", [json_doc(t)])?
            .as_js_string()
            .cloned())
    }
    async fn check_dependencies(&self, fields: &Json, t: &JsString, d: bool) -> IvyResult<()> {
        self.replay
            .call(
                "dependencies",
                [fields.clone(), Json::String(t.clone()), d.into()],
            )
            .map(|_| ())
    }
    async fn check_blocker(&self, b: &Json) -> IvyResult<()> {
        self.replay.call("blocker", [b.clone()]).map(|_| ())
    }
    async fn schedule(&self, r: &Json) -> IvyResult<Json> {
        self.replay.call("schedule", [r.clone()])
    }
    async fn recover_operation(&self, id: &JsString) -> IvyResult<Json> {
        self.replay.call("recover", [Json::String(id.clone())])
    }
}
impl SchedulerEngine for World<'_> {
    fn principal_id(&self) -> &JsString {
        &self.principal
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        self.replay.call("owner", []).map(|_| ())
    }
    async fn scoped_operation_id(&self, s: &Json) -> IvyResult<JsString> {
        Ok(self
            .replay
            .call("scope", [s.clone()])?
            .as_js_string()
            .unwrap()
            .clone())
    }
}
impl SchedulerAllocation for World<'_> {
    async fn allocate(
        &self,
        t: &Document,
        o: &JsString,
        feedback: Option<&JsString>,
    ) -> IvyResult<Json> {
        self.replay.call(
            "allocate",
            [json_doc(t), Json::String(o.clone()), nullable(feedback)],
        )
    }
}
impl SchedulerOperations for World<'_> {
    async fn find(&self, p: &JsString, o: &JsString) -> IvyResult<Option<Document>> {
        let v = self
            .replay
            .call("find", [Json::String(p.clone()), Json::String(o.clone())])?;
        Ok((!v.is_null()).then(|| doc(&v)))
    }
}
#[tokio::test(flavor = "current_thread")]
async fn original_scheduler_allocation_and_attempt_effects_are_preserved() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let contracts = CheckedNativeContracts::new(&root);
    let mut cases = 0;
    let mut boundaries = 0;
    for group in data().as_array().unwrap() {
        for row in f(group, "rows").as_array().unwrap() {
            cases += 1;
            boundaries += f(row, "boundaries").as_array().unwrap().len();
            let w = World::new(row);
            let mut actual = vec![];
            if f(group, "mode") == &Json::from("allocate") {
                let operation = f(w.input(), "operation")
                    .as_js_string()
                    .cloned()
                    .unwrap_or_else(|| "original".into());
                actual.push(outcome(
                    Allocator {
                        store: &w,
                        engine: &w,
                        catalogs: &contracts,
                    }
                    .allocate(&w.task(), &operation, None)
                    .await,
                ));
            } else {
                let scheduler = Scheduler {
                    store: &w,
                    engine: &w,
                    allocator: &w,
                    operations: &w,
                };
                for _ in f(row, "expected").as_array().unwrap() {
                    actual.push(outcome(scheduler.task(s(&w.task().pin, "objectId")).await));
                }
            }
            assert_eq!(
                hash_json(&Json::Array(actual)).unwrap(),
                s(row, "expectedHash").to_utf8().unwrap(),
                "{:?}",
                s(row, "name")
            );
            assert!(
                w.replay.steps.lock().unwrap().is_empty(),
                "Unconsumed effects {:?}",
                s(row, "name")
            );
        }
    }
    assert_eq!(cases, 152);
    assert_eq!(boundaries, 2074);
}
