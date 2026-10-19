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
    evidence_bytes::{BlobStore, ChunkKey},
    native_inspector::{Inspection, InspectionContext, InspectionStore, NativeInspector, PageSize},
    operations::Document,
    read_evidence::ReadEvidenceStore,
    turn_output::turns_params,
};
use std::{
    cell::RefCell,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};
type Trace = Arc<Mutex<Vec<Json>>>;
fn text(v: &Json, k: &str) -> JsString {
    v.required(k).unwrap().as_js_string().unwrap().clone()
}
fn pin(revision: f64) -> Json {
    Json::object([
        ("objectId", "00000000-0000-4000-8000-000000000900".into()),
        ("revision", Json::Number(revision)),
    ])
}
fn record(trace: &Trace, v: impl IntoIterator<Item = Json>) {
    trace
        .lock()
        .unwrap()
        .push(Json::Array(v.into_iter().collect()));
}
fn flag(v: &Json, k: &str) -> bool {
    v.get(k).and_then(Json::as_bool) == Some(true)
}
struct Client {
    input: Json,
    time: JsString,
    trace: Trace,
    reads: Mutex<usize>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            record(&self.trace, ["rpc".into(), method.into(), params.clone()]);
            if method == "tools.list" {
                let name = params.required("namePrefix")?.as_str()?;
                return Ok(Json::object([
                    (
                        "provider",
                        Json::object([(
                            "node",
                            Json::object([("serviceNodeId", "native-agent".into())]),
                        )]),
                    ),
                    (
                        "items",
                        Json::Array(vec![Json::object([
                            ("qualifiedName", format!("agent.{name}").into()),
                            (
                                "definition",
                                Json::object([
                                    ("namespace", "agent".into()),
                                    ("name", name.into()),
                                    ("interfaceVersion", "1.0.0".into()),
                                    (
                                        "annotations",
                                        Json::object([("readOnlyHint", Json::Bool(true))]),
                                    ),
                                    ("inputSchema", Json::Object(vec![])),
                                    ("outputSchema", Json::Object(vec![])),
                                ]),
                            ),
                            (
                                "definitionHash",
                                format!("sha256:{}", "a".repeat(64)).into(),
                            ),
                            ("catalogHash", format!("sha256:{}", "b".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", Json::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            if params.required("qualifiedName")?.as_str()? == "agent.status" {
                return Ok(self.input.required("status")?.clone());
            }
            let args = params.required("arguments")?;
            let index = {
                let mut n = self.reads.lock().unwrap();
                *n += 1;
                *n
            };
            let step = &self.input.required("steps")?.as_array().unwrap()[index - 1];
            let status = self.input.required("status")?;
            // Observations are from the original selected catalog, even in a status-mismatch case.
            let base = self.input.required("context")?;
            let mut value = Json::object([
                ("schemaVersion", Json::Number(1.0)),
                ("observationId", format!("read-{index}").into()),
                ("callerPrincipalId", "caller".into()),
                ("serviceNodeId", "native-agent".into()),
                ("nativeVersion", base.required("nativeVersion")?.clone()),
                (
                    "nativeExecutableHash",
                    status.required("nativeExecutableHash")?.clone(),
                ),
                ("catalogHash", status.required("catalogHash")?.clone()),
                ("epoch", "epoch".into()),
                ("requestId", Json::Number(index as f64)),
                ("observedAt", Json::String(self.time.clone())),
                ("method", args.required("method")?.clone()),
                ("params", args.required("params")?.clone()),
                (
                    "requestHash",
                    hash_json(&Json::object([
                        ("method", args.required("method")?.clone()),
                        ("params", args.required("params")?.clone()),
                    ]))?
                    .into(),
                ),
            ]);
            for (k, v) in step.as_object().unwrap() {
                value.insert(k.clone(), v.clone());
            }
            Ok(value)
        })
    }
}
struct Memory {
    input: Json,
    tools: ToolClients,
    time: JsString,
    trace: Trace,
    owners: RefCell<usize>,
    saved: RefCell<Option<Document>>,
    writes: RefCell<Vec<Json>>,
}
impl BlobStore for Memory {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<Json> {
        Err(IvyError::internal())
    }
    fn read(&self, _: ChunkKey, _: &Json, _: &JsString) -> IvyResult<Vec<u8>> {
        Err(IvyError::internal())
    }
}
impl ReadEvidenceStore for Memory {
    fn validate(&self, _: &str, _: &Json) -> IvyResult<()> {
        Err(IvyError::internal())
    }
    async fn read_document(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document> {
        record(
            &self.trace,
            [
                "read".into(),
                key.into(),
                address.clone(),
                Json::Bool(upgrade),
            ],
        );
        assert_eq!(key, "task-board/run");
        let run = self.input.required("run")?;
        let value = if address.get("revision").and_then(Json::as_f64) == Some(1.0) {
            self.input.get("originalRun").unwrap_or(run)
        } else {
            run
        };
        Ok(Document {
            pin: address.clone(),
            value: value.clone(),
            parent_id: None,
            content_hash: String::new(),
        })
    }
    async fn write_document(
        &self,
        key: &str,
        value: &Json,
        id: &str,
        destination: &Json,
    ) -> IvyResult<Json> {
        record(
            &self.trace,
            [
                "write".into(),
                key.into(),
                value.clone(),
                id.into(),
                destination.clone(),
            ],
        );
        if flag(&self.input, "writeFailure") {
            return Err(IvyError::new("fixture_failed", "write"));
        }
        let revision = self
            .saved
            .borrow()
            .as_ref()
            .map(|d| d.pin.get("revision").unwrap().as_f64().unwrap())
            .unwrap_or(0.0)
            + 1.0;
        let pin = pin(revision);
        let doc = Document {
            pin: pin.clone(),
            value: value.clone(),
            parent_id: None,
            content_hash: hash_json(value)?,
        };
        self.writes.borrow_mut().push(Json::object([
            ("pin", pin.clone()),
            ("value", value.clone()),
        ]));
        *self.saved.borrow_mut() = Some(doc);
        Ok(pin)
    }
}
impl InspectionStore for Memory {
    async fn verify_owner(&self) -> IvyResult<()> {
        record(&self.trace, ["owner".into()]);
        let mut n = self.owners.borrow_mut();
        *n += 1;
        if self.input.get("failOwner").and_then(Json::as_f64) == Some(*n as f64) {
            Err(IvyError::new("fixture_failed", "owner"))
        } else {
            Ok(())
        }
    }
    async fn named_document(
        &self,
        key: &str,
        name: &str,
        parent: &JsString,
    ) -> IvyResult<Option<Document>> {
        record(
            &self.trace,
            [
                "named".into(),
                key.into(),
                name.into(),
                Json::String(parent.clone()),
            ],
        );
        Ok(self.saved.borrow().clone())
    }
    fn tools(&self) -> &ToolClients {
        &self.tools
    }
    fn principal_id(&self) -> JsString {
        "caller".into()
    }
    fn now(&self) -> JsString {
        self.time.clone()
    }
}
fn context(v: &Json) -> InspectionContext {
    InspectionContext {
        run: v.required("run").unwrap().clone(),
        native_version: text(v, "nativeVersion"),
        catalog_source_hash: text(v, "catalogSourceHash"),
        service_node_id: text(v, "serviceNodeId"),
        epoch: text(v, "epoch"),
        thread_id: text(v, "threadId"),
        turn_id: text(v, "turnId"),
    }
}

#[tokio::test]
async fn native_inspection_matches_original_traces_and_search_checkpoints() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-inspector-oracle.mjs",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let catalogs = CheckedNativeContracts::new(root);
    let time = text(&data, "time");
    let rows = data.required("rows").unwrap().as_array().unwrap();
    let mut retry_adapter_passes = 0;
    for row in rows {
        let i = row.required("input").unwrap();
        let trace: Trace = Arc::default();
        let client = Arc::new(Client {
            input: i.clone(),
            time: time.clone(),
            trace: trace.clone(),
            reads: Mutex::new(0),
        });
        let saved = i.get("saved").map(|d| Document {
            pin: d.required("pin").unwrap().clone(),
            value: d.required("value").unwrap().clone(),
            parent_id: None,
            content_hash: String::new(),
        });
        let store = Memory {
            input: i.clone(),
            tools: ToolClients::new(client),
            time: time.clone(),
            trace: trace.clone(),
            owners: RefCell::new(0),
            saved: RefCell::new(saved),
            writes: RefCell::default(),
        };
        let inspector = NativeInspector {
            store: &store,
            catalogs: &catalogs,
        };
        let result: IvyResult<Json> = async {
            let mut values = Vec::new();
            for _ in 0..i.get("passes").and_then(Json::as_f64).unwrap_or(1.0) as usize {
                let action = i
                    .get("action")
                    .map(Json::as_str)
                    .transpose()?
                    .unwrap_or_default();
                values.push(if action == "stopped" {
                    Json::Bool(inspector.owner_stopped(i.required("run")?).await?)
                } else if action == "read" {
                    inspector
                        .read(
                            &context(i.required("context")?),
                            &"thread/turns/list".into(),
                            turns_params(&"thread".into(), None),
                            flag(i, "allowError"),
                        )
                        .await?
                } else {
                    let size = if i.get("pageSize").and_then(Json::as_f64) == Some(1.0) {
                        PageSize::One
                    } else {
                        PageSize::Hundred
                    };
                    let budget = i.get("budget").and_then(Json::as_f64).unwrap_or(8.0);
                    let inspected = if budget == 8.0 && matches!(size, PageSize::Hundred) {
                        retry_adapter_passes += 1;
                        ivy_task_board::native_rate_limit::RetryInspection::find(
                            &inspector,
                            data.required("pin")?,
                        )
                        .await?
                    } else {
                        inspector.find(data.required("pin")?, budget, size).await?
                    };
                    match inspected {
                        Inspection::Found { metadata, turns } => Json::object([
                            ("kind", "found".into()),
                            ("metadata", metadata),
                            ("turns", turns),
                        ]),
                        Inspection::Pending { search } => {
                            Json::object([("kind", "pending".into()), ("search", search)])
                        }
                        Inspection::Absent => Json::object([("kind", "absent".into())]),
                    }
                });
            }
            Ok(Json::Array(values))
        }
        .await;
        let result = match result {
            Ok(v) => Json::object([("ok", v)]),
            Err(e) => Json::object([(
                "error",
                Json::object([
                    ("code", Json::String(e.code_js())),
                    ("message", Json::String(e.message_js())),
                ]),
            )]),
        };
        let actual = Json::object([
            ("result", result),
            ("trace", Json::Array(trace.lock().unwrap().clone())),
            ("writes", Json::Array(store.writes.borrow().clone())),
        ]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            row.required("expectedHash").unwrap().as_str().unwrap(),
            "{}: {}",
            row.required("name").unwrap().as_str().unwrap(),
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
        );
    }
    assert!(retry_adapter_passes > 0);
    println!("{} native inspection comparisons", rows.len());
}
