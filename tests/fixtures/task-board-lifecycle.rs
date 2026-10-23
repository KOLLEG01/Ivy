use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use ivy_task_board::{
    native_lifecycle::{
        LifecycleDocument, LifecycleEngine, LifecycleOperations, LifecyclePolicies,
        LifecyclePublication, LifecycleStore, NativeLifecycle,
    },
    operations::{Document, Store},
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};
#[path = "task-board-lifecycle-journal.rs"]
mod journal;
type Trace = Arc<Mutex<Vec<Json>>>;
fn event(t: &Trace, parts: impl IntoIterator<Item = Json>) {
    t.lock()
        .unwrap()
        .push(Json::Array(parts.into_iter().collect()));
}
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn num(v: &Json, k: &str) -> f64 {
    v.required(k).unwrap().as_f64().unwrap()
}
fn flag(v: &Json, k: &str) -> bool {
    v.get(k) == Some(&true.into())
}
fn merge(v: &Json, patch: &Json) -> Json {
    let mut v = v.clone();
    for (k, a) in patch.as_object().unwrap_or(&[]) {
        v.insert(k.clone(), a.clone());
    }
    v
}
fn id(n: u64) -> JsString {
    format!("00000000-0000-4000-8000-{n:012}").into()
}
fn pin(n: u64, r: f64) -> Json {
    Json::object([
        ("objectId", Json::String(id(n))),
        ("revision", Json::Number(r)),
    ])
}
fn doc(v: &Json) -> Document {
    Document {
        pin: v.required("pin").unwrap().clone(),
        value: v.required("value").unwrap().clone(),
        parent_id: Some("root".into()),
        content_hash: hash_json(v.required("value").unwrap()).unwrap(),
    }
}
fn data() -> &'static Json {
    static DATA: OnceLock<Json> = OnceLock::new();
    DATA.get_or_init(|| {
        if let Some(path) = std::env::var_os("IVY_TASK_BOARD_LIFECYCLE_FIXTURE") {
            return parse(
                &std::fs::read_to_string(path).unwrap(),
                MANAGEMENT_FRAME_BYTES,
            )
            .unwrap();
        }
        let o = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-lifecycle-oracle.mjs",
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
struct Client {
    case_name: JsString,
    replies: Mutex<VecDeque<Json>>,
    trace: Trace,
}
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            event(&self.trace, ["rpc".into(), m.into(), a.clone()]);
            let r = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("Unexpected RPC {m}: {a:?}"));
            assert_eq!(f(&r, "method"), &Json::from(m), "{:?}", self.case_name);
            assert_eq!(
                hash_json(f(&r, "args")).unwrap(),
                hash_json(&a).unwrap(),
                "Unexpected {m}: {a:?}"
            );
            match r.get("error") {
                Some(e) => Err(error(e)),
                None => Ok(r.required("value")?.clone()),
            }
        })
    }
}
struct World<'a> {
    base: &'a Json,
    row: &'a Json,
    live: RefCell<Json>,
    trace: Trace,
    client: Arc<Client>,
    tools: Arc<ToolClients>,
    consumed: Cell<bool>,
}
impl World<'_> {
    fn input(&self) -> &Json {
        self.row.required("input").unwrap()
    }
    fn initial(&self, k: &str) -> &Json {
        self.row.required("initial").unwrap().required(k).unwrap()
    }
    fn fail(&self, w: &str) -> IvyResult<()> {
        let v = f(self.input(), "fault");
        if !self.consumed.get() && v.get("where") == Some(&Json::from(w)) {
            self.consumed.set(true);
            Err(IvyError::new_js(
                s(v, "code").clone(),
                "Fixture failure.".into(),
            ))
        } else {
            Ok(())
        }
    }
    fn task(&self) -> Document {
        doc(self.initial("task"))
    }
    async fn read(
        &self,
        k: &str,
        a: &Json,
        p: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        event(
            &self.trace,
            [
                "read".into(),
                k.into(),
                a.clone(),
                p.into(),
                archived.into(),
            ],
        );
        self.fail(&format!("read:{k}"))?;
        let value = match k {
            "task-board/operation" => self.live.borrow().clone(),
            "task-board/native-plan" => Json::object([
                ("pin", pin(400, 1.0)),
                ("value", self.base.required("plan")?.clone()),
            ]),
            "task-board/task" => {
                let mut t = self.initial("task").clone();
                if a.get("revision") == Some(&Json::Number(1.0)) {
                    if let Some(code) = self.input().get("oldReadError") {
                        return Err(IvyError::new_js(
                            code.as_js_string().unwrap().clone(),
                            "Fixture old task unavailable.".into(),
                        ));
                    }
                    t.insert("pin", pin(100, 1.0));
                    let mut v = t.required("value")?.clone();
                    v.insert(
                        "primaryResourceRef",
                        self.input()
                            .get("oldPrimary")
                            .unwrap_or(self.base.required("task")?.required("primaryResourceRef")?)
                            .clone(),
                    );
                    t.insert("value", v);
                } else if a.as_js_string().is_some() {
                    t.insert(
                        "pin",
                        pin(
                            100,
                            self.input()
                                .get("currentRevision")
                                .and_then(Json::as_f64)
                                .unwrap_or(2.0),
                        ),
                    );
                    t.insert(
                        "value",
                        merge(t.required("value")?, f(self.input(), "currentTask")),
                    );
                }
                t
            }
            "task-board/run" if a.get("objectId") == Some(&Json::String(id(200))) => {
                Json::object([
                    ("pin", pin(200, 1.0)),
                    (
                        "value",
                        Json::object([
                            (
                                "target",
                                self.base
                                    .required("allocation")?
                                    .required("target")?
                                    .clone(),
                            ),
                            (
                                "workspace",
                                self.base
                                    .required("allocation")?
                                    .required("workspace")?
                                    .clone(),
                            ),
                        ]),
                    ),
                ])
            }
            "task-board/run" | "task-board/history" => {
                let text = s(a, "objectId").to_utf8()?;
                let n: usize = text[24..].parse().unwrap();
                let v = if k == "task-board/run" {
                    f(self.input(), "runs").as_array().unwrap()[n - 500].clone()
                } else {
                    Json::object([(
                        "previousTask",
                        f(self.input(), "histories").as_array().unwrap()[n - 600].clone(),
                    )])
                };
                Json::object([("pin", a.clone()), ("value", v)])
            }
            _ => panic!("Unknown read {k}"),
        };
        Ok(LifecycleDocument {
            metadata: f(&value, "metadata").clone(),
            document: doc(&value),
        })
    }
}
impl Store for World<'_> {
    fn root_object_id(&self) -> Option<JsString> {
        Some("root".into())
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        panic!("Unexpected version")
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        panic!("Unexpected retired")
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        panic!("Unexpected key")
    }
    fn complete_local_workflow(&self, id: &JsString, t: &JsString) -> IvyResult<()> {
        event(
            &self.trace,
            [
                "complete".into(),
                Json::String(id.clone()),
                Json::String(t.clone()),
            ],
        );
        Ok(())
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        panic!("Unexpected named")
    }
    async fn read(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        Ok(World::read(self, k, a, p, false).await?.document)
    }
    async fn write(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Unexpected direct write")
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("Unexpected identity")
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        panic!("Unexpected prepared write")
    }
}
impl LifecycleStore for World<'_> {
    async fn lifecycle_read(
        &self,
        k: &str,
        a: &Json,
        p: bool,
        r: bool,
    ) -> IvyResult<LifecycleDocument> {
        World::read(self, k, a, p, r).await
    }
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        event(&self.trace, ["page".into(), k.into(), o.clone()]);
        let offset = o
            .get("cursor")
            .and_then(Json::as_js_string)
            .map(|s| s.to_utf8().unwrap().parse().unwrap())
            .unwrap_or(0);
        let len = f(
            self.input(),
            if k == "task-board/run" {
                "runs"
            } else {
                "histories"
            },
        )
        .as_array()
        .unwrap_or(&[])
        .len();
        let size = self
            .input()
            .get("pageSize")
            .and_then(Json::as_f64)
            .unwrap_or(100.0) as usize;
        let count = size.min(len - offset);
        Ok(Json::object([
            (
                "items",
                Json::Array(
                    (offset..offset + count)
                        .map(|n| {
                            pin(
                                (if k == "task-board/run" { 500 } else { 600 }) + n as u64,
                                1.0,
                            )
                        })
                        .collect(),
                ),
            ),
            (
                "nextCursor",
                if offset + size < len {
                    (offset + size).to_string().into()
                } else {
                    Json::Null
                },
            ),
        ]))
    }
    async fn archive(&self, p: &Json) -> IvyResult<Json> {
        self.client
            .request("objects.archive", p.clone(), RequestOptions::default())
            .await
    }
    async fn stat(&self, id: &JsString) -> IvyResult<Json> {
        self.client
            .request(
                "objects.stat",
                Json::object([("objectId", Json::String(id.clone()))]),
                RequestOptions::default(),
            )
            .await
    }
    fn release_workspace(&self, id: &JsString) -> IvyResult<()> {
        event(&self.trace, ["release".into(), Json::String(id.clone())]);
        Ok(())
    }
}
impl LifecyclePolicies for World<'_> {
    fn scheduler_enabled(&self) -> bool {
        self.input().get("enabled") != Some(&false.into())
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        event(
            &self.trace,
            ["aggregate".into(), t.clone(), Json::String(id.clone())],
        );
        self.fail("aggregate")
    }
    async fn allocate(&self, t: &Document, id: &JsString) -> IvyResult<Json> {
        event(
            &self.trace,
            [
                "allocate".into(),
                self.initial("task").clone(),
                Json::String(id.clone()),
            ],
        );
        assert_eq!(t.pin, self.task().pin);
        self.fail("allocate")?;
        Ok(merge(
            self.base.required("allocation")?,
            f(self.input(), "allocation"),
        ))
    }
    async fn unstarted_missing_context(&self, t: &Document) -> IvyResult<bool> {
        event(&self.trace, ["proof".into(), self.initial("task").clone()]);
        assert_eq!(t.pin, self.task().pin);
        Ok(flag(self.input(), "proof"))
    }
}
impl LifecycleEngine for World<'_> {
    fn client(&self) -> &dyn RpcClient {
        self.client.as_ref()
    }
    fn tools(&self) -> &Arc<ToolClients> {
        &self.tools
    }
    fn principal_id(&self) -> &JsString {
        static V: OnceLock<JsString> = OnceLock::new();
        V.get_or_init(|| "caller".into())
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        event(&self.trace, ["owner".into()]);
        self.fail("owner")
    }
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString> {
        event(&self.trace, ["scope".into(), scope.clone()]);
        Ok(format!("scoped-{}", &hash_json(scope)?[7..]).into())
    }
}
impl LifecycleOperations for World<'_> {
    async fn prepare_native(&self, id: &JsString, call: &Json) -> IvyResult<Json> {
        event(
            &self.trace,
            [
                "prepare-native".into(),
                Json::String(id.clone()),
                call.clone(),
            ],
        );
        let mut op = self.live.borrow_mut();
        let mut value = op.required("value")?.clone();
        let mut calls = value.required("nativeCalls")?.as_array().unwrap().to_vec();
        calls.push(call.clone());
        value.insert("nativeCalls", Json::Array(calls));
        op.insert("value", value);
        Ok(call.clone())
    }
    async fn observe_native(&self, id: &JsString, key: &JsString, o: &Json) -> IvyResult<Json> {
        event(
            &self.trace,
            [
                "observe-native".into(),
                Json::String(id.clone()),
                Json::String(key.clone()),
                o.clone(),
            ],
        );
        let mut op = self.live.borrow_mut();
        let mut value = op.required("value")?.clone();
        let mut calls = value.required("nativeCalls")?.as_array().unwrap().to_vec();
        let call = calls
            .iter_mut()
            .find(|v| v.get("key").and_then(Json::as_js_string) == Some(key))
            .unwrap();
        call.insert("observation", o.clone());
        let result = call.clone();
        value.insert("nativeCalls", Json::Array(calls));
        op.insert("value", value);
        Ok(result)
    }
    async fn archive_identity(&self, id: &JsString) -> IvyResult<JsString> {
        event(
            &self.trace,
            ["archive-identity".into(), Json::String(id.clone())],
        );
        Ok("archive-id".into())
    }
}
struct Steps<'a, 'b> {
    world: &'a World<'b>,
    count: usize,
}
impl LifecyclePublication for Steps<'_, '_> {
    fn pin(&self) -> IvyResult<Json> {
        Ok(pin(300, 1.0))
    }
    async fn write(&mut self, k: &str, v: &Json, d: &Json) -> IvyResult<Json> {
        event(
            &self.world.trace,
            ["step-write".into(), k.into(), v.clone(), d.clone()],
        );
        self.count += 1;
        self.world.fail(&format!("step:{}", self.count))?;
        Ok(Json::object([
            ("objectId", d.required("objectId")?.clone()),
            ("revision", Json::Number(num(d, "expectedRevision") + 1.0)),
        ]))
    }
    async fn create(&mut self, k: &str, v: &Json, l: &JsString) -> IvyResult<Json> {
        event(
            &self.world.trace,
            [
                "step-create".into(),
                k.into(),
                v.clone(),
                Json::String(l.clone()),
            ],
        );
        self.count += 1;
        self.world.fail(&format!("step:{}", self.count))?;
        Ok(pin(800, 1.0))
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_lifecycle_matches_original_retained_operations_archive_context_and_publication_fences()
 {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut rows = 0;
    let mut frames = 0;
    for group in data().as_array().unwrap() {
        for row in group.required("rows").unwrap().as_array().unwrap() {
            let trace: Trace = Arc::default();
            let replies = row.required("rpcReplies").unwrap().as_array().unwrap();
            frames += replies.len();
            let client = Arc::new(Client {
                case_name: s(row, "name").clone(),
                replies: Mutex::new(replies.to_vec().into()),
                trace: trace.clone(),
            });
            let world = World {
                base: group.required("base").unwrap(),
                row,
                live: RefCell::new(
                    row.required("initial")
                        .unwrap()
                        .required("liveOperation")
                        .unwrap()
                        .clone(),
                ),
                trace: trace.clone(),
                tools: Arc::new(ToolClients::new(client.clone())),
                client,
                consumed: Cell::new(false),
            };
            let subject = NativeLifecycle {
                store: &world,
                engine: &world,
                operations: &world,
                catalogs: &catalogs,
            };
            let operation = doc(world.initial("operation"));
            let task = world.task();
            let mut steps = Steps {
                world: &world,
                count: 0,
            };
            let mut results = vec![];
            for _ in 0..world
                .input()
                .get("passes")
                .and_then(Json::as_f64)
                .unwrap_or(1.0) as usize
            {
                let value = if f(world.input(), "entry") == &"restore".into() {
                    subject
                        .restore_for_execution(&operation, &task)
                        .await
                        .map(|_| Json::Null)
                } else {
                    subject
                        .publish(
                            &operation,
                            &mut steps,
                            merge(
                                &Json::object([
                                    ("task", Json::Null),
                                    ("history", Json::Null),
                                    ("archive", Json::Null),
                                ]),
                                f(world.input(), "outcome"),
                            ),
                        )
                        .await
                };
                results.push(outcome(value));
            }
            let actual = Json::object([
                ("outcomes", Json::Array(results)),
                ("trace", Json::Array(trace.lock().unwrap().clone())),
                (
                    "nativeCalls",
                    world
                        .live
                        .borrow()
                        .required("value")
                        .unwrap()
                        .required("nativeCalls")
                        .unwrap()
                        .clone(),
                ),
            ]);
            assert_eq!(
                hash_json(&actual).unwrap(),
                s(row, "expectedHash").to_utf8().unwrap(),
                "{} {}: {}",
                s(group, "path").to_utf8().unwrap(),
                s(row, "name").to_utf8().unwrap(),
                canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
            );
            assert!(world.client.replies.lock().unwrap().is_empty());
            rows += 1;
        }
    }
    assert_eq!(rows, 118);
    assert_eq!(frames, 625);
}
