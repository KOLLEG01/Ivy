use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    native_inspector::{InspectionStore, NativeInspector},
    native_signals::NativeSignals,
    operations::Document,
    read_evidence::ReadEvidenceStore,
};
use std::{
    cell::RefCell,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};
type Trace = Arc<Mutex<Vec<Json>>>;
fn text(v: &Json, key: &str) -> JsString {
    v.required(key).unwrap().as_js_string().unwrap().clone()
}
fn record(trace: &Trace, values: impl IntoIterator<Item = Json>) {
    trace
        .lock()
        .unwrap()
        .push(Json::Array(values.into_iter().collect()));
}
fn checkpoint(value: Option<&Json>) -> Option<Document> {
    value.map(|d| Document {
        pin: d.required("pin").unwrap().clone(),
        value: d.required("value").unwrap().clone(),
        parent_id: d
            .required("metadata")
            .unwrap()
            .get("parentId")
            .and_then(Json::as_js_string)
            .cloned(),
        content_hash: String::new(),
    })
}
struct Client {
    input: Json,
    trace: Trace,
    status: Mutex<usize>,
    pending: Mutex<usize>,
    pages: Mutex<usize>,
}
impl Client {
    fn page(&self, key: &str, index: &Mutex<usize>) -> Json {
        let mut index = index.lock().unwrap();
        let pages = self.input.required(key).unwrap().as_array().unwrap();
        let result = pages
            .get(*index)
            .unwrap_or_else(|| pages.last().unwrap())
            .clone();
        *index += 1;
        result
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
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
                                    ("annotations", Json::object([("readOnlyHint", true.into())])),
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
            match params.required("qualifiedName")?.as_str()?.as_str() {
                "agent.status" => {
                    let mut index = self.status.lock().unwrap();
                    let value = self
                        .input
                        .get("statuses")
                        .and_then(Json::as_array)
                        .and_then(|values| values.get(*index))
                        .unwrap_or(self.input.required("status")?)
                        .clone();
                    *index += 1;
                    Ok(value)
                }
                "agent.inputs" => Ok(self.page("pending", &self.pending)),
                "agent.notifications" => Ok(self.page("pages", &self.pages)),
                _ => panic!("Unexpected RPC"),
            }
        })
    }
}
struct Store {
    input: Json,
    pin: Json,
    time: JsString,
    tools: ToolClients,
    trace: Trace,
    saved: RefCell<Option<Document>>,
    writes: RefCell<Vec<Json>>,
    owners: RefCell<usize>,
    write_index: RefCell<usize>,
    pass: RefCell<usize>,
}
impl BlobStore for Store {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<Json> {
        panic!("Signal observations must not save native evidence bytes")
    }
    fn read(&self, _: ChunkKey, _: &Json, _: &JsString) -> IvyResult<Vec<u8>> {
        panic!("Signal observations must not read native evidence bytes")
    }
}
impl ReadEvidenceStore for Store {
    fn validate(&self, _: &str, _: &Json) -> IvyResult<()> {
        panic!("No read-evidence persistence is used by signal attention")
    }
    async fn read_document(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document> {
        record(
            &self.trace,
            ["read".into(), key.into(), address.clone(), upgrade.into()],
        );
        let value = self
            .input
            .get("runs")
            .and_then(Json::as_array)
            .and_then(|runs| runs.get(*self.pass.borrow()))
            .unwrap_or(self.input.required("run")?)
            .clone();
        Ok(Document {
            pin: self.pin.clone(),
            value,
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
        let mut index = self.write_index.borrow_mut();
        let failure = self
            .input
            .get("writeErrors")
            .and_then(Json::as_array)
            .and_then(|values| values.get(*index))
            .and_then(Json::as_js_string)
            .cloned();
        *index += 1;
        if let Some(failure) = failure {
            *self.saved.borrow_mut() = checkpoint(self.input.get("winner"));
            return Err(IvyError::new(failure.to_utf8()?, "fixture write"));
        }
        let revision = self
            .saved
            .borrow()
            .as_ref()
            .map(|saved| saved.pin.get("revision").unwrap().as_f64().unwrap())
            .unwrap_or(0.0)
            + 1.0;
        let pin = Json::object([
            ("objectId", "00000000-0000-4000-8000-000000000900".into()),
            ("revision", Json::Number(revision)),
        ]);
        *self.saved.borrow_mut() = Some(Document {
            pin: pin.clone(),
            value: value.clone(),
            parent_id: Some(text(&self.pin, "objectId")),
            content_hash: String::new(),
        });
        self.writes.borrow_mut().push(Json::object([
            ("pin", pin.clone()),
            ("value", value.clone()),
        ]));
        Ok(pin)
    }
}
impl InspectionStore for Store {
    async fn verify_owner(&self) -> IvyResult<()> {
        record(&self.trace, ["owner".into()]);
        let mut count = self.owners.borrow_mut();
        *count += 1;
        if self.input.get("failOwner").and_then(Json::as_f64) == Some(*count as f64) {
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
#[tokio::test]
async fn advisory_signals_match_original_context_cursor_checkpoint_and_conflict_traces() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-signals-oracle.mjs",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixtures = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let catalogs = CheckedNativeContracts::new(root);
    for row in fixtures.required("rows").unwrap().as_array().unwrap() {
        let input = row.required("input").unwrap();
        let trace: Trace = Arc::default();
        let client = Arc::new(Client {
            input: input.clone(),
            trace: trace.clone(),
            status: Mutex::default(),
            pending: Mutex::default(),
            pages: Mutex::default(),
        });
        let store = Store {
            input: input.clone(),
            pin: fixtures.required("pin").unwrap().clone(),
            time: text(&fixtures, "time"),
            tools: ToolClients::new(client),
            trace: trace.clone(),
            saved: RefCell::new(checkpoint(input.get("saved"))),
            writes: RefCell::default(),
            owners: RefCell::default(),
            write_index: RefCell::default(),
            pass: RefCell::default(),
        };
        let mut subject = NativeSignals::new(NativeInspector {
            store: &store,
            catalogs: &catalogs,
        });
        let mut pins = Vec::new();
        let mut failure = None;
        for pass in 0..input.get("passes").and_then(Json::as_f64).unwrap_or(1.0) as usize {
            *store.pass.borrow_mut() = pass;
            match subject.step(&text(&store.pin, "objectId")).await {
                Ok(pin) => pins.push(pin.unwrap_or(Json::Null)),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        let result = match failure {
            Some(error) => Json::object([(
                "error",
                Json::object([
                    ("code", error.code.into()),
                    ("message", error.message.into()),
                ]),
            )]),
            None => Json::object([("ok", Json::Array(pins))]),
        };
        let actual = Json::object([
            ("result", result),
            ("trace", Json::Array(trace.lock().unwrap().clone())),
            ("writes", Json::Array(store.writes.into_inner())),
        ]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            row.required("expectedHash").unwrap().as_str().unwrap(),
            "{}: {}",
            text(row, "name").to_utf8().unwrap(),
            ivy_contracts::json::canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
        );
    }
}
