#[path = "reference-data.rs"]
mod reference_data;
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
#[path = "task-board-route-migration.rs"]
mod route_migration;
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
        pin: ivy_task_board::operations::ObjectPin::from_json(v.required("pin").unwrap().clone())
            .unwrap(),
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
        let o = reference_data::read(include_bytes!("task-board-lifecycle-cases.json.gz"));
        parse(std::str::from_utf8(&o).unwrap(), MANAGEMENT_FRAME_BYTES).unwrap()
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
    cleanups: RefCell<Vec<Json>>,
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
        if k == "task-board/native-call" {
            if let Some(code) = self.input().get("migrationCallError") {
                return Err(IvyError::new_js(
                    code.as_js_string().unwrap().clone(),
                    "The exact local call is unavailable.".into(),
                )
                .with_outcome(if flag(self.input(), "migrationCallUncertain") {
                    ivy_contracts::Outcome::Unknown
                } else {
                    ivy_contracts::Outcome::NotExecuted
                }));
            }
        }
        let migration_key =
            if k == "task-board/run" && a.get("objectId") == Some(&Json::String(id(500))) {
                "migrationOtherRun"
            } else {
                match k {
                    "task-board/run" => "migrationRun",
                    "task-board/native-call" => "migrationCall",
                    _ => "",
                }
            };
        if let Some(source) = self.input().get(migration_key) {
            let mut document = doc(source);
            if k == "task-board/run"
                && a.as_js_string().is_some()
                && flag(self.input(), "migrationRunChanged")
            {
                document.pin.revision = 2.0;
            }
            return Ok(LifecycleDocument {
                document,
                metadata: Json::Null,
            });
        }
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
    async fn write_prepared(
        &self,
        _: &str,
        _: &Json,
        _: &JsString,
        _: &Json,
        _version: Option<&JsString>,
        _references: Option<&Json>,
    ) -> IvyResult<Json> {
        panic!("Unexpected prepared write")
    }
}
impl LifecycleStore for World<'_> {
    async fn task_header(
        &self,
        address: &Json,
        primary: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        let mut result = World::read(self, "task-board/task", address, primary, archived).await?;
        if flag(self.input(), "compactHeader") {
            assert!(
                primary && archived,
                "Retained lifecycle reads keep both original policies."
            );
            result
                .document
                .value
                .insert("comments", Json::Array(vec![]));
            result
                .document
                .value
                .insert("commentDeliveries", Json::Array(vec![]));
            result.document.value.insert("commentStore", compact_tree());
        }
        Ok(result)
    }

    async fn migration_evidence(
        &self,
        catalogs: &CheckedNativeContracts,
        call: &Document,
        expected: &ivy_task_board::native_intent::EvidenceIdentity,
    ) -> IvyResult<Json> {
        route_migration::retained_operation(self, catalogs, call, expected).await
    }
    fn migration_quiescent(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        if flag(self.input(), "migrationPending") {
            return Err(IvyError::new(
                "task_board_execution_unresolved",
                "Fixture original lifecycle outcome is unresolved.",
            ));
        }
        Ok(())
    }
    async fn lifecycle_read(
        &self,
        k: &str,
        a: &Json,
        p: bool,
        r: bool,
    ) -> IvyResult<LifecycleDocument> {
        assert!(
            k != "task-board/task" || !flag(self.input(), "compactHeader"),
            "A compact lifecycle must not request the whole conversation."
        );
        World::read(self, k, a, p, r).await
    }
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        event(&self.trace, ["page".into(), k.into(), o.clone()]);
        if k == "task-board/run" {
            if let Some(run) = self.input().get("migrationRun") {
                let mut items = vec![run.required("pin")?.clone()];
                if let Some(other) = self.input().get("migrationOtherRun") {
                    items.push(other.required("pin")?.clone());
                }
                return Ok(Json::object([
                    ("items", Json::Array(items)),
                    ("nextCursor", Json::Null),
                ]));
            }
        }
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
        assert_eq!(t.pin.to_json(), self.task().pin.to_json());
        self.fail("allocate")?;
        Ok(merge(
            self.base.required("allocation")?,
            f(self.input(), "allocation"),
        ))
    }
    async fn unstarted_missing_context(&self, t: &Document) -> IvyResult<bool> {
        event(&self.trace, ["proof".into(), self.initial("task").clone()]);
        assert_eq!(t.pin.to_json(), self.task().pin.to_json());
        Ok(flag(self.input(), "proof"))
    }
}
impl LifecycleEngine for World<'_> {
    async fn cleanup_task_outputs(
        &self,
        node: &JsString,
        backend: Option<&ivy_contracts::native_backend::BackendIdentity>,
        task_key: &JsString,
        operation_id: &JsString,
    ) -> IvyResult<()> {
        self.cleanups.borrow_mut().push(
            ivy_contracts::native_backend::BackendIdentity::select_arguments(
                backend,
                Json::object([
                    ("serviceNodeId", Json::String(node.clone())),
                    ("taskKey", Json::String(task_key.clone())),
                    ("operationId", Json::String(operation_id.clone())),
                ]),
            )
            .unwrap(),
        );
        self.fail("cleanup")
    }
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
        assert_ne!(
            scope.as_array().and_then(|a| a.first()),
            Some(&"task-board-output-cleanup".into()),
            "Output cleanup must survive replacement of the live identity cache"
        );
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
    async fn write_task_change(
        &mut self,
        value: &Json,
        destination: &Json,
        change: ivy_task_board::operations::TaskChange,
    ) -> IvyResult<Json> {
        assert!(matches!(
            change,
            ivy_task_board::operations::TaskChange::Header
        ));
        assert_eq!(value.get("commentStore"), Some(&compact_tree()));
        assert!(value.required("comments")?.as_array().unwrap().is_empty());
        assert!(
            value
                .required("commentDeliveries")?
                .as_array()
                .unwrap()
                .is_empty()
        );
        self.write("task-board/task", value, destination).await
    }

    fn pin(&self) -> IvyResult<Json> {
        Ok(pin(300, 1.0))
    }
    async fn write(&mut self, k: &str, v: &Json, d: &Json) -> IvyResult<Json> {
        if k == "task-board/task" {
            assert!(v.get("waiting").is_none());
            assert!(v.required("fields")?.get("nextReviewAt").is_none());
            assert!(v.get("runtimeWait").is_some());
        }
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
    // Original trace hashes project task writes to the current writer fields.
    let corpus = data().as_array().unwrap();
    let expected_rows = corpus
        .iter()
        .map(|g| g.required("rows").unwrap().as_array().unwrap().len())
        .sum::<usize>();
    let expected_frames = corpus
        .iter()
        .flat_map(|g| g.required("rows").unwrap().as_array().unwrap())
        .map(|r| r.required("rpcReplies").unwrap().as_array().unwrap().len())
        .sum::<usize>();
    let mut rows = 0;
    let mut frames = 0;
    for group in corpus {
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
                cleanups: RefCell::new(vec![]),
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
            let cleanups = world.cleanups.borrow();
            if !flag(world.input(), "archived") {
                assert!(
                    cleanups.is_empty(),
                    "Restoration and new contexts must preserve outputs"
                );
            }
            for cleanup in cleanups.iter() {
                assert_eq!(cleanup.get("taskKey"), task.value.get("taskKey"));
            }
            rows += 1;
        }
    }
    assert_eq!(rows, expected_rows);
    assert_eq!(frames, expected_frames);
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_archive_and_output_cleanup_retain_accounts_and_reject_a_foreign_status() {
    use ivy_contracts::native_backend::{BackendIdentity, NativeProvider};
    let group = &data().as_array().unwrap()[0];
    let original = group
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| s(row, "name") == &JsString::from("archive already archived"))
        .unwrap();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for provider in [NativeProvider::Codex, NativeProvider::Claude] {
        let selected = BackendIdentity::new("primary".into(), provider, "one".into()).unwrap();
        let foreign = BackendIdentity::new("primary".into(), provider, "two".into()).unwrap();
        for wrong in [false, true] {
            let mut row = original.clone();
            let mut initial = row.required("initial").unwrap().clone();
            let mut task = initial.required("task").unwrap().clone();
            let mut value = task.required("value").unwrap().clone();
            let mut primary = value.required("primaryResourceRef").unwrap().clone();
            primary.insert("namespace", selected.id().into());
            value.insert("primaryResourceRef", primary);
            value.insert("primaryBackend", selected.json());
            task.insert("value", value);
            initial.insert("task", task);
            row.insert("initial", initial);
            let mut replies = Vec::new();
            for reply in original.required("rpcReplies").unwrap().as_array().unwrap() {
                let mut reply = reply.clone();
                if f(&reply, "method").is_str("serviceNodes.get") {
                    let mut value = reply.required("value").unwrap().clone();
                    value.remove("nativeVersion");
                    reply.insert("value", value);
                }
                if f(&reply, "method").is_str("tools.call") {
                    let mut args = reply.required("args").unwrap().clone();
                    args.insert(
                        "arguments",
                        selected
                            .arguments(args.required("arguments").unwrap().clone())
                            .unwrap(),
                    );
                    reply.insert("args", args.clone());
                    let mut value = selected
                        .arguments(reply.required("value").unwrap().clone())
                        .unwrap();
                    if wrong && f(&args, "qualifiedName").is_str("agent.status") {
                        value.insert("backend", foreign.json());
                    }
                    reply.insert("value", value);
                }
                replies.push(reply);
                if wrong
                    && f(
                        replies.last().unwrap().required("args").unwrap(),
                        "qualifiedName",
                    )
                    .is_str("agent.status")
                {
                    break;
                }
            }
            let trace: Trace = Arc::default();
            let client = Arc::new(Client {
                case_name: "scoped archive".into(),
                replies: Mutex::new(replies.into()),
                trace: trace.clone(),
            });
            let world = World {
                base: group.required("base").unwrap(),
                row: &row,
                live: RefCell::new(
                    row.required("initial")
                        .unwrap()
                        .required("liveOperation")
                        .unwrap()
                        .clone(),
                ),
                trace,
                tools: Arc::new(ToolClients::new(client.clone())),
                client,
                consumed: Cell::new(false),
                cleanups: RefCell::default(),
            };
            let subject = NativeLifecycle {
                store: &world,
                engine: &world,
                operations: &world,
                catalogs: &catalogs,
            };
            let mut steps = Steps {
                world: &world,
                count: 0,
            };
            let result = subject
                .publish(
                    &doc(world.initial("operation")),
                    &mut steps,
                    Json::object([
                        ("task", Json::Null),
                        ("history", Json::Null),
                        ("archive", Json::Null),
                    ]),
                )
                .await;
            if wrong {
                assert_eq!(result.unwrap_err().code, "task_board_native_unavailable");
                assert!(world.cleanups.borrow().is_empty());
            } else {
                result.unwrap();
                assert_eq!(world.cleanups.borrow().len(), 1);
                assert!(BackendIdentity::matches(
                    &world.cleanups.borrow()[0],
                    Some(&selected)
                ));
            }
            assert!(world.client.replies.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_new_context_retains_account_even_when_two_accounts_return_the_same_thread_id() {
    use ivy_contracts::native_backend::{BackendIdentity, NativeProvider};
    let group = &data().as_array().unwrap()[0];
    let original = group
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| s(row, "name") == &JsString::from("explicit new native context"))
        .unwrap();
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for provider in [NativeProvider::Codex, NativeProvider::Claude] {
        let selected = BackendIdentity::new("primary".into(), provider, "new".into()).unwrap();
        for same_account in [false, true] {
            let old = if same_account {
                selected.clone()
            } else {
                BackendIdentity::new("primary".into(), provider, "old".into()).unwrap()
            };
            let mut base = group.required("base").unwrap().clone();
            let mut plan = base.required("plan").unwrap().clone();
            let source = catalogs
                .checked_native_contract(
                    s(&plan, "nativeVersion"),
                    Some(&Json::object([(
                        "sourceHash",
                        plan.required("catalogSourceHash").unwrap().clone(),
                    )])),
                )
                .unwrap()
                .with_backend(Some(selected.clone()));
            plan.insert("backend", selected.json());
            plan.insert(
                "definitions",
                Json::object(
                    [
                        ("threadStart", "thread/start"),
                        ("threadResume", "thread/resume"),
                        ("turnStart", "turn/start"),
                        ("turnInterrupt", "turn/interrupt"),
                    ]
                    .into_iter()
                    .map(|(slot, method)| {
                        (
                            slot,
                            Json::from(source.definition_hash(&method.into()).unwrap()),
                        )
                    }),
                ),
            );
            base.insert("plan", plan);
            let mut allocation = base.required("allocation").unwrap().clone();
            allocation.insert(
                "target",
                selected
                    .arguments(allocation.required("target").unwrap().clone())
                    .unwrap(),
            );
            base.insert("allocation", allocation);
            let mut row = original.clone();
            let mut initial = row.required("initial").unwrap().clone();
            let mut task = initial.required("task").unwrap().clone();
            let mut value = task.required("value").unwrap().clone();
            let mut primary = value.required("primaryResourceRef").unwrap().clone();
            primary.insert("namespace", old.id().into());
            let thread_id = primary.required("nativeId").unwrap().clone();
            value.insert("primaryResourceRef", primary);
            value.insert("primaryBackend", old.json());
            task.insert("value", value);
            initial.insert("task", task);
            row.insert("initial", initial);
            let mut replies = Vec::new();
            for reply in original.required("rpcReplies").unwrap().as_array().unwrap() {
                let mut reply = reply.clone();
                if f(&reply, "method").is_str("tools.list")
                    && f(reply.required("args").unwrap(), "namespace").is_str("codex")
                {
                    let mut args = reply.required("args").unwrap().clone();
                    args.insert("namespace", selected.id().into());
                    reply.insert("args", args);
                    let mut value = reply.required("value").unwrap().clone();
                    let mut item = value.required("items").unwrap().as_array().unwrap()[0].clone();
                    item.insert(
                        "qualifiedName",
                        format!("{}.thread/start", selected.id()).into(),
                    );
                    item.insert(
                        "definition",
                        source.definition(&"thread/start".into()).unwrap(),
                    );
                    item.insert(
                        "definitionHash",
                        source
                            .definition_hash(&"thread/start".into())
                            .unwrap()
                            .into(),
                    );
                    value.insert("items", Json::Array(vec![item]));
                    reply.insert("value", value);
                }
                if f(&reply, "method").is_str("tools.call") {
                    let mut args = reply.required("args").unwrap().clone();
                    args.insert(
                        "arguments",
                        selected
                            .arguments(args.required("arguments").unwrap().clone())
                            .unwrap(),
                    );
                    reply.insert("args", args.clone());
                    let mut value = selected
                        .arguments(reply.required("value").unwrap().clone())
                        .unwrap();
                    if f(&args, "qualifiedName").is_str("agent.operation") {
                        let mut response = value.required("reply").unwrap().clone();
                        let mut result = response.required("result").unwrap().clone();
                        let mut thread = result.required("thread").unwrap().clone();
                        thread.insert("id", thread_id.clone());
                        result.insert("thread", thread);
                        response.insert("result", result);
                        value.insert("reply", response);
                    }
                    reply.insert("value", value);
                }
                replies.push(reply);
            }
            let trace: Trace = Arc::default();
            let client = Arc::new(Client {
                case_name: "scoped new context".into(),
                replies: Mutex::new(replies.into()),
                trace: trace.clone(),
            });
            let world = World {
                base: &base,
                row: &row,
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
                cleanups: RefCell::default(),
            };
            let subject = NativeLifecycle {
                store: &world,
                engine: &world,
                operations: &world,
                catalogs: &catalogs,
            };
            let mut steps = Steps {
                world: &world,
                count: 0,
            };
            let result = subject
                .publish(
                    &doc(world.initial("operation")),
                    &mut steps,
                    Json::object([
                        ("task", Json::Null),
                        ("history", Json::Null),
                        ("archive", Json::Null),
                    ]),
                )
                .await;
            if same_account {
                assert_eq!(result.unwrap_err().code, "task_board_primary_mismatch");
            } else {
                result.unwrap();
                let trace = trace.lock().unwrap();
                let saved = trace
                    .iter()
                    .filter_map(Json::as_array)
                    .rev()
                    .find(|row| row[0].is_str("step-write") && row[1].is_str("task-board/task"))
                    .unwrap();
                assert_eq!(
                    saved[2].required("primaryBackend").unwrap(),
                    &selected.json()
                );
                assert_eq!(
                    saved[2]
                        .required("primaryResourceRef")
                        .unwrap()
                        .required("nativeId")
                        .unwrap(),
                    &thread_id
                );
            }
            assert!(world.client.replies.lock().unwrap().is_empty());
            assert!(world.cleanups.borrow().is_empty());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn archive_cleanup_failure_keeps_original_request_pending_until_retry() {
    let group = data().as_array().unwrap().last().unwrap();
    let mut row = group
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.get("name") == Some(&"archive original durable thread".into()))
        .unwrap()
        .clone();
    let mut input = row.required("input").unwrap().clone();
    input.insert(
        "fault",
        Json::object([
            ("where", "cleanup".into()),
            ("code", "task_output_cleanup_failed".into()),
        ]),
    );
    row.insert("input", input);
    let trace: Trace = Arc::default();
    let client = Arc::new(Client {
        case_name: s(&row, "name").clone(),
        replies: Mutex::new(
            row.required("rpcReplies")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec()
                .into(),
        ),
        trace: trace.clone(),
    });
    let world = World {
        base: group.required("base").unwrap(),
        row: &row,
        live: RefCell::new(
            row.required("initial")
                .unwrap()
                .required("liveOperation")
                .unwrap()
                .clone(),
        ),
        trace,
        tools: Arc::new(ToolClients::new(client.clone())),
        client,
        consumed: Cell::new(false),
        cleanups: RefCell::new(vec![]),
    };
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let subject = NativeLifecycle {
        store: &world,
        engine: &world,
        operations: &world,
        catalogs: &catalogs,
    };
    let operation = doc(world.initial("operation"));
    let mut steps = Steps {
        world: &world,
        count: 0,
    };
    let empty = Json::object([
        ("task", Json::Null),
        ("history", Json::Null),
        ("archive", Json::Null),
    ]);
    let error = subject
        .publish(&operation, &mut steps, empty.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, "task_board_output_cleanup_pending");
    assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
    assert_eq!(
        world
            .client
            .replies
            .lock()
            .unwrap()
            .front()
            .unwrap()
            .get("method"),
        Some(&"objects.archive".into())
    );
    subject
        .publish(&operation, &mut steps, empty)
        .await
        .unwrap();
    let cleanups = world.cleanups.borrow();
    assert_eq!(cleanups.len(), 2);
    assert_eq!(cleanups[0], cleanups[1]);
    assert!(world.client.replies.lock().unwrap().is_empty());
}

fn compact_tree() -> Json {
    Json::object([
        ("kind", "tree".into()),
        (
            "root",
            Json::object([("object", pin(999, 1.0)), ("length", Json::Number(4096.0))]),
        ),
        ("length", Json::Number(4096.0)),
        (
            "summary",
            Json::object([
                ("directCount", Json::Number(4096.0)),
                ("questions", Json::Array(vec![])),
            ]),
        ),
    ])
}
#[tokio::test(flavor = "current_thread")]
async fn retained_lifecycle_compact_headers_preserve_original_outcomes_and_native_effects() {
    async fn probe(
        group: &Json,
        source: &Json,
        compact: bool,
        catalogs: &CheckedNativeContracts,
    ) -> Json {
        let mut row = source.clone();
        row.get_mut("input")
            .unwrap()
            .insert("compactHeader", compact.into());
        let trace: Trace = Arc::default();
        let client = Arc::new(Client {
            case_name: s(&row, "name").clone(),
            replies: Mutex::new(
                row.required("rpcReplies")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .to_vec()
                    .into(),
            ),
            trace: trace.clone(),
        });
        let world = World {
            base: group.required("base").unwrap(),
            row: &row,
            live: RefCell::new(
                row.required("initial")
                    .unwrap()
                    .required("liveOperation")
                    .unwrap()
                    .clone(),
            ),
            trace,
            tools: Arc::new(ToolClients::new(client.clone())),
            client,
            consumed: Cell::new(false),
            cleanups: RefCell::default(),
        };
        let subject = NativeLifecycle {
            store: &world,
            engine: &world,
            operations: &world,
            catalogs,
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
            let result = if f(world.input(), "entry").is_str("restore") {
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
            results.push(outcome(result));
        }
        assert!(world.client.replies.lock().unwrap().is_empty());
        Json::object([
            ("outcomes", Json::Array(results)),
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
            ("cleanup", Json::Array(world.cleanups.borrow().clone())),
        ])
    }
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut cases = 0;
    for group in data().as_array().unwrap() {
        for row in group.required("rows").unwrap().as_array().unwrap() {
            let original = probe(group, row, false, &catalogs).await;
            let compact = probe(group, row, true, &catalogs).await;
            assert_eq!(
                hash_json(&compact).unwrap(),
                hash_json(&original).unwrap(),
                "{:?}",
                s(row, "name")
            );
            cases += 1;
        }
    }
    println!(
        "Compact lifecycle preserves {cases} original retained scenarios without full Task reads."
    );
}
