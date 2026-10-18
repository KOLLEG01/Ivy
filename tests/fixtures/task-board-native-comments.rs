use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    tools::ToolClients,
};
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    native_comments::{NativeCommentStore, sync_native_comments},
    native_inspector::InspectionStore,
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
fn record(trace: &Trace, parts: impl IntoIterator<Item = Json>) {
    trace
        .lock()
        .unwrap()
        .push(Json::Array(parts.into_iter().collect()));
}
struct Client {
    input: Json,
    trace: Trace,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            record(&self.trace, ["rpc".into(), method.into(), params]);
            if method == "tools.list" {
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
                            ("qualifiedName", "agent.inputs".into()),
                            (
                                "definition",
                                Json::object([
                                    ("namespace", "agent".into()),
                                    ("name", "inputs".into()),
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
            if self.input.get("rpcFailure").and_then(Json::as_bool) == Some(true) {
                return Err(IvyError::new("fixture_failed", "input query"));
            }
            Ok(Json::object([
                ("epoch", "epoch".into()),
                ("items", self.input.required("items")?.clone()),
                ("truncated", false.into()),
            ]))
        })
    }
}
struct Store {
    input: Json,
    tools: ToolClients,
    trace: Trace,
    current: RefCell<Json>,
    posted: RefCell<Vec<Json>>,
    writes: RefCell<usize>,
}
impl BlobStore for Store {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<Json> {
        unreachable!()
    }
    fn read(&self, _: ChunkKey, _: &Json, _: &JsString) -> IvyResult<Vec<u8>> {
        unreachable!()
    }
}
impl ReadEvidenceStore for Store {
    fn validate(&self, _: &str, _: &Json) -> IvyResult<()> {
        unreachable!()
    }
    async fn read_document(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document> {
        record(
            &self.trace,
            ["read".into(), key.into(), address.clone(), upgrade.into()],
        );
        let (pin, value) = if key == "task-board/run" {
            (
                Json::object([("objectId", "run".into()), ("revision", Json::Number(1.0))]),
                self.input.required("run")?.clone(),
            )
        } else {
            let current = self.current.borrow();
            (
                current.required("pin")?.clone(),
                current.required("value")?.clone(),
            )
        };
        Ok(Document {
            pin,
            value,
            parent_id: None,
            content_hash: String::new(),
        })
    }
    async fn write_document(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
}
impl InspectionStore for Store {
    async fn verify_owner(&self) -> IvyResult<()> {
        unreachable!()
    }
    async fn named_document(&self, _: &str, _: &str, _: &JsString) -> IvyResult<Option<Document>> {
        unreachable!()
    }
    fn tools(&self) -> &ToolClients {
        &self.tools
    }
    fn principal_id(&self) -> JsString {
        unreachable!()
    }
    fn now(&self) -> JsString {
        unreachable!()
    }
}
impl NativeCommentStore for Store {
    async fn native_comment(&self, request: &Json) -> IvyResult<()> {
        record(&self.trace, ["comment".into(), request.clone()]);
        let mut writes = self.writes.borrow_mut();
        *writes += 1;
        let fails = self.input.get("failAt").and_then(Json::as_f64) == Some(*writes as f64);
        if fails && self.input.get("failAfterWrite").and_then(Json::as_bool) != Some(true) {
            return Err(IvyError::new("fixture_failed", "publication"));
        }
        let mut current = self.current.borrow_mut();
        let mut value = current.required("value")?.clone();
        let mut comments = value.required("comments")?.as_array().unwrap().to_vec();
        comments.push(Json::object([(
            "commentId",
            request.required("commentId")?.clone(),
        )]));
        value.insert("comments", Json::Array(comments));
        current.insert("value", value);
        let mut pin = current.required("pin")?.clone();
        let revision = pin.required("revision")?.as_f64().unwrap() + 1.0;
        pin.insert("revision", Json::Number(revision));
        current.insert("pin", pin);
        self.posted.borrow_mut().push(request.clone());
        if fails {
            return Err(
                IvyError::new("outcome_unknown", "publication").with_outcome(Outcome::Unknown)
            );
        }
        Ok(())
    }
}
#[tokio::test]
async fn native_comment_mirroring_matches_original_identity_confidentiality_and_publication_traces()
{
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-native-comments-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixture = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for case in fixture.required("rows").unwrap().as_array().unwrap() {
        let input = case.required("input").unwrap();
        let trace: Trace = Arc::default();
        let client = Arc::new(Client {
            input: input.clone(),
            trace: trace.clone(),
        });
        let current = Json::object([
            ("pin", fixture.required("taskPin").unwrap().clone()),
            ("value", input.required("task").unwrap().clone()),
        ]);
        let store = Store {
            input: input.clone(),
            tools: ToolClients::new(client),
            trace: trace.clone(),
            current: RefCell::new(current),
            posted: RefCell::default(),
            writes: RefCell::default(),
        };
        let mut result = Json::Null;
        for _ in 0..input.get("passes").and_then(Json::as_f64).unwrap_or(1.0) as usize {
            if let Err(error) = sync_native_comments(&store, &"run".into()).await {
                result = Json::object([
                    ("code", error.code.into()),
                    ("message", error.message.into()),
                    ("outcome", error.outcome.as_str().into()),
                ]);
                break;
            }
        }
        let actual = Json::object([
            ("result", result),
            ("trace", Json::Array(trace.lock().unwrap().clone())),
            ("posted", Json::Array(store.posted.into_inner())),
            ("current", store.current.into_inner()),
        ]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            case.required("expectedHash").unwrap().as_str().unwrap(),
            "{}: {}",
            case.required("name").unwrap().as_str().unwrap(),
            ivy_contracts::json::canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
        );
    }
}
