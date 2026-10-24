use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, hash::hash_json,
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

fn changed(value: &Json, key: &str, replacement: Json) -> Json {
    let mut value = value.clone();
    value.insert(key, replacement);
    value
}
fn page(data: Vec<Json>, cursor: Json) -> Json {
    Json::object([(
        "reply",
        Json::object([(
            "result",
            Json::object([("data", Json::Array(data)), ("nextCursor", cursor)]),
        )]),
    )])
}
fn found() -> Json {
    page(
        vec![Json::object([
            ("id", "turn".into()),
            ("status", "completed".into()),
        ])],
        Json::Null,
    )
}
fn metadata(thread: &str, ephemeral: bool) -> Json {
    Json::object([(
        "reply",
        Json::object([(
            "result",
            Json::object([(
                "thread",
                Json::object([("id", thread.into()), ("ephemeral", Json::Bool(ephemeral))]),
            )]),
        )]),
    )])
}
fn native_error() -> Json {
    Json::object([(
        "reply",
        Json::object([(
            "error",
            Json::object([
                ("code", Json::Number(-32601.0)),
                ("message", "unsupported".into()),
            ]),
        )]),
    )])
}

#[tokio::test]
async fn native_inspection_preserves_original_identity_and_search_checkpoints() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let source = catalogs
        .checked_native_contract(&"0.159.2".into(), None)
        .unwrap();
    let time: JsString = "2026-01-02T03:04:05.006Z".into();
    let run_pin = Json::object([
        ("objectId", "00000000-0000-4000-8000-000000000800".into()),
        ("revision", Json::Number(2.0)),
    ]);
    let run = Json::object([
        (
            "target",
            Json::object([
                ("serviceNodeId", "native-agent".into()),
                ("hostId", "fixture".into()),
            ]),
        ),
        (
            "plan",
            Json::object([
                ("nativeVersion", "0.159.2".into()),
                (
                    "catalogSourceHash",
                    source.catalog().required("sourceHash").unwrap().clone(),
                ),
            ]),
        ),
        (
            "primaryResourceRef",
            Json::object([
                ("namespace", "codex".into()),
                ("kind", "thread".into()),
                ("serviceNodeId", "native-agent".into()),
                ("nativeId", "thread".into()),
            ]),
        ),
        ("turnId", "turn".into()),
        ("nativeEpoch", "epoch".into()),
        (
            "originalRequest",
            Json::object([("original", Json::Bool(true))]),
        ),
    ]);
    let status = Json::object([
        ("serviceNodeId", "native-agent".into()),
        ("hostId", "fixture".into()),
        ("nativeVersion", "0.159.2".into()),
        (
            "nativeExecutableHash",
            source
                .catalog()
                .required("nativeExecutableHash")
                .unwrap()
                .clone(),
        ),
        ("catalogHash", source.catalog_hash().into()),
        ("epoch", "epoch".into()),
        ("pid", Json::Number(42.0)),
        ("state", "ready".into()),
        ("observedAt", Json::String(time.clone())),
        ("code", Json::Null),
        ("initialized", Json::Object(vec![])),
        ("pendingInputs", Json::Number(0.0)),
        (
            "operations",
            Json::object([
                ("retained", Json::Number(0.0)),
                ("maximum", Json::Number(100.0)),
                ("bytes", Json::Number(0.0)),
                ("maximumBytes", Json::Number(1048576.0)),
            ]),
        ),
        ("observedMethods", Json::Array(vec![])),
    ]);
    let context_value = Json::object([
        ("run", run_pin.clone()),
        ("nativeVersion", "0.159.2".into()),
        (
            "catalogSourceHash",
            source.catalog().required("sourceHash").unwrap().clone(),
        ),
        ("serviceNodeId", "native-agent".into()),
        ("epoch", "epoch".into()),
        ("threadId", "thread".into()),
        ("turnId", "turn".into()),
    ]);
    let mut saved_value = context_value.clone();
    saved_value.remove("catalogSourceHash");
    for (key, value) in [
        ("schemaVersion", Json::Number(1.0)),
        ("run", changed(&run_pin, "revision", Json::Number(1.0))),
        ("phase", "searching".into()),
        ("nextCursor", "next".into()),
        ("pageCount", Json::Number(1.0)),
        (
            "visitedCursorHashes",
            Json::Array(vec![hash_json(&Json::Null).unwrap().into()]),
        ),
        ("createdAt", Json::String(time.clone())),
        ("updatedAt", Json::String(time.clone())),
    ] {
        saved_value.insert(key, value);
    }
    let saved = Json::object([("pin", pin(1.0)), ("value", saved_value.clone())]);
    let base = Json::object([
        ("run", run.clone()),
        ("context", context_value.clone()),
        ("status", status.clone()),
        (
            "steps",
            Json::Array(vec![found(), metadata("thread", false), found()]),
        ),
    ]);
    let mut rows = Vec::new();
    let mut add = |name: &str, patch: Json, expected: Result<Vec<&'static str>, &'static str>| {
        let mut input = base.clone();
        for (key, value) in patch.as_object().unwrap() {
            input.insert(key.clone(), value.clone());
        }
        rows.push((name.to_owned(), input, expected));
    };
    let steps = |values| Json::object([("steps", Json::Array(values))]);
    add("fresh", Json::Object(vec![]), Ok(vec!["found"]));
    add(
        "one item",
        Json::object([("pageSize", Json::Number(1.0))]),
        Ok(vec!["found"]),
    );
    add(
        "absent",
        steps(vec![page(vec![], Json::Null)]),
        Ok(vec!["absent"]),
    );
    add(
        "checkpoint",
        Json::object([
            ("budget", Json::Number(1.0)),
            ("steps", Json::Array(vec![page(vec![], "next".into())])),
        ]),
        Ok(vec!["pending"]),
    );
    add(
        "resume checkpoint",
        Json::object([
            ("budget", Json::Number(1.0)),
            ("passes", Json::Number(2.0)),
            (
                "steps",
                Json::Array(vec![
                    page(vec![], "next".into()),
                    found(),
                    metadata("thread", false),
                    found(),
                ]),
            ),
        ]),
        Ok(vec!["pending", "found"]),
    );
    add(
        "resume saved",
        Json::object([("saved", saved.clone())]),
        Ok(vec!["found"]),
    );
    add(
        "finish saved absent",
        Json::object([
            ("saved", saved.clone()),
            ("steps", Json::Array(vec![page(vec![], Json::Null)])),
        ]),
        Ok(vec!["absent"]),
    );
    add(
        "restart finished",
        Json::object([(
            "saved",
            changed(
                &saved,
                "value",
                changed(
                    &changed(&saved_value, "phase", "finished".into()),
                    "nextCursor",
                    Json::Null,
                ),
            ),
        )]),
        Ok(vec!["found"]),
    );
    for budget in [0.0, 65.0, 1.5, -1.0] {
        add(
            "invalid budget",
            Json::object([("budget", Json::Number(budget))]),
            Err("task_board_inspection_budget"),
        );
    }
    for key in ["turnId", "nativeEpoch", "primaryResourceRef"] {
        add(
            "missing execution identity",
            Json::object([("run", changed(&run, key, Json::Null))]),
            Err("task_board_turn_mismatch"),
        );
    }
    add(
        "wrong primary owner",
        Json::object([(
            "run",
            changed(
                &run,
                "primaryResourceRef",
                changed(
                    run.required("primaryResourceRef").unwrap(),
                    "serviceNodeId",
                    "other".into(),
                ),
            ),
        )]),
        Err("task_board_turn_mismatch"),
    );
    add(
        "wrong status owner",
        Json::object([("status", changed(&status, "serviceNodeId", "other".into()))]),
        Err("task_board_native_unavailable"),
    );
    add(
        "wrong catalog",
        Json::object([(
            "status",
            changed(
                &status,
                "catalogHash",
                format!("sha256:{}", "0".repeat(64)).into(),
            ),
        )]),
        Err("task_board_native_unavailable"),
    );
    add(
        "owner fence",
        Json::object([("failOwner", Json::Number(1.0))]),
        Err("fixture_failed"),
    );
    add(
        "checkpoint owner fence",
        Json::object([
            ("budget", Json::Number(1.0)),
            ("steps", Json::Array(vec![page(vec![], "next".into())])),
            ("failOwner", Json::Number(3.0)),
        ]),
        Err("fixture_failed"),
    );
    add(
        "cursor loop",
        steps(vec![
            page(vec![], "next".into()),
            page(vec![], "next".into()),
        ]),
        Err("task_board_native_cursor_loop"),
    );
    add(
        "invalid cursor",
        steps(vec![page(vec![], Json::Number(17.0))]),
        Err("task_board_native_cursor_unsupported"),
    );
    add(
        "duplicate turn",
        steps(vec![page(
            vec![
                Json::object([("id", "turn".into())]),
                Json::object([("id", "turn".into())]),
            ],
            Json::Null,
        )]),
        Err("task_board_turn_mismatch"),
    );
    for fresh in [false, true] {
        let oversized = page(
            vec![
                Json::object([("id", "a".into())]),
                Json::object([("id", "turn".into())]),
            ],
            Json::Null,
        );
        add(
            "page bound",
            Json::object([
                ("pageSize", Json::Number(1.0)),
                (
                    "steps",
                    Json::Array(if fresh {
                        vec![found(), metadata("thread", false), oversized]
                    } else {
                        vec![oversized]
                    }),
                ),
            ]),
            Err("task_board_native_page_limit"),
        );
    }
    add(
        "moved turn",
        steps(vec![
            found(),
            metadata("thread", false),
            page(vec![Json::object([("id", "other".into())])], Json::Null),
        ]),
        Err("task_board_native_state_changed"),
    );
    for (thread, ephemeral) in [("thread", true), ("other", false)] {
        add(
            "metadata identity",
            steps(vec![found(), metadata(thread, ephemeral), found()]),
            Err("task_board_evidence_mismatch"),
        );
    }
    add(
        "backwards observation",
        steps(vec![
            found(),
            metadata("thread", false),
            changed(&found(), "observedAt", "2026-01-02T03:04:05.005Z".into()),
        ]),
        Err("task_board_evidence_mismatch"),
    );
    for (key, value) in [
        ("epoch", "other".into()),
        ("requestHash", format!("sha256:{}", "0".repeat(64)).into()),
    ] {
        add(
            "read identity",
            steps(vec![changed(&found(), key, value)]),
            Err("task_board_evidence_mismatch"),
        );
    }
    add(
        "original native error",
        steps(vec![native_error()]),
        Err("task_board_native_read_failed"),
    );
    for (key, value) in [
        ("threadId", "other".into()),
        ("epoch", "other".into()),
        ("pageCount", Json::Number(2.0)),
        ("run", changed(&run_pin, "revision", Json::Number(3.0))),
    ] {
        add(
            "retained identity",
            Json::object([(
                "saved",
                changed(&saved, "value", changed(&saved_value, key, value)),
            )]),
            Err("task_board_evidence_mismatch"),
        );
    }
    add(
        "changed original request",
        Json::object([
            ("saved", saved.clone()),
            (
                "originalRun",
                changed(
                    &run,
                    "originalRequest",
                    Json::object([("original", Json::Bool(false))]),
                ),
            ),
        ]),
        Err("task_board_evidence_mismatch"),
    );
    add(
        "retained cursor loop",
        Json::object([(
            "saved",
            changed(
                &saved,
                "value",
                changed(&saved_value, "nextCursor", Json::Null),
            ),
        )]),
        Err("task_board_native_cursor_loop"),
    );
    let history = Json::Array(
        (0..1024)
            .map(|n| hash_json(&Json::Number(n as f64)).unwrap().into())
            .collect(),
    );
    add(
        "search bound",
        Json::object([(
            "saved",
            changed(
                &saved,
                "value",
                changed(
                    &changed(&saved_value, "pageCount", Json::Number(1024.0)),
                    "visitedCursorHashes",
                    history,
                ),
            ),
        )]),
        Err("task_board_native_search_limit"),
    );
    add(
        "write failure",
        Json::object([
            ("budget", Json::Number(1.0)),
            ("steps", Json::Array(vec![page(vec![], "next".into())])),
            ("writeFailure", Json::Bool(true)),
        ]),
        Err("fixture_failed"),
    );
    add(
        "read original error",
        Json::object([
            ("action", "read".into()),
            ("allowError", Json::Bool(true)),
            ("steps", Json::Array(vec![native_error()])),
        ]),
        Ok(vec!["read-error"]),
    );
    add(
        "no stopped epoch",
        Json::object([
            ("action", "stopped".into()),
            ("run", changed(&run, "nativeEpoch", Json::Null)),
        ]),
        Ok(vec!["false"]),
    );
    add(
        "same epoch",
        Json::object([("action", "stopped".into())]),
        Ok(vec!["false"]),
    );
    let connection = Json::object([
        ("mode", "owned-stdio".into()),
        ("ownsServer", Json::Bool(true)),
        ("ownsHome", Json::Bool(true)),
        ("actualHome", "fixture".into()),
        ("actualVersion", "0.159.2".into()),
        ("socketPath", Json::Null),
        ("serverPid", Json::Number(42.0)),
        ("lifecycle", "owned-stdio".into()),
        ("mcpIdentity", "instance-environment".into()),
    ]);
    let mut stopped = changed(&status, "epoch", "replacement".into());
    stopped.insert("connection", connection.clone());
    stopped.insert(
        "processStops",
        Json::Array(vec![Json::object([
            ("epoch", "epoch".into()),
            ("observedAt", Json::String(time.clone())),
        ])]),
    );
    add(
        "owned predecessor stopped",
        Json::object([("action", "stopped".into()), ("status", stopped.clone())]),
        Ok(vec!["true"]),
    );
    add(
        "external owner",
        Json::object([
            ("action", "stopped".into()),
            (
                "status",
                changed(
                    &stopped,
                    "connection",
                    changed(
                        &changed(&connection, "mode", "external-proxy".into()),
                        "ownsServer",
                        Json::Bool(false),
                    ),
                ),
            ),
        ]),
        Ok(vec!["false"]),
    );
    assert_eq!(rows.len(), 45);
    // Malformed saved values must not grant a native read or a stop proof.
    let malformed = [
        Json::Null,
        Json::Bool(false),
        Json::Bool(true),
        Json::Number(0.0),
        Json::Number(1.0),
        "".into(),
        Json::Array(vec![]),
        Json::Object(vec![]),
    ];
    let mut add = |name: &str, patch: Json, expected: Result<Vec<&'static str>, &'static str>| {
        let mut input = base.clone();
        for (key, value) in patch.as_object().unwrap() {
            input.insert(key.clone(), value.clone());
        }
        rows.push((name.to_owned(), input, expected));
    };
    for value in malformed {
        for key in ["turnId", "nativeEpoch"] {
            add(
                "typed saved identity",
                Json::object([
                    ("action", "context".into()),
                    ("run", changed(&run, key, value.clone())),
                ]),
                Err("task_board_turn_mismatch"),
            );
        }
        add(
            "typed stopped identity",
            Json::object([
                ("action", "stopped".into()),
                ("run", changed(&run, "nativeEpoch", value.clone())),
            ]),
            Ok(vec!["false"]),
        );
        if value.as_bool().is_none() {
            add(
                "typed ownership flag",
                Json::object([
                    ("action", "stopped".into()),
                    (
                        "status",
                        changed(
                            &stopped,
                            "connection",
                            changed(&connection, "ownsServer", value),
                        ),
                    ),
                ]),
                Err("invalid_arguments"),
            );
        }
    }
    for key in ["turnId", "nativeEpoch"] {
        let mut missing = run.clone();
        missing.remove(key);
        add(
            "absent saved identity",
            Json::object([("action", "context".into()), ("run", missing)]),
            Err("task_board_turn_mismatch"),
        );
    }
    add(
        "explicit unowned server",
        Json::object([
            ("action", "stopped".into()),
            (
                "status",
                changed(
                    &stopped,
                    "connection",
                    changed(&connection, "ownsServer", Json::Bool(false)),
                ),
            ),
        ]),
        Ok(vec!["false"]),
    );
    add(
        "missing stop proof",
        Json::object([
            ("action", "stopped".into()),
            (
                "status",
                changed(&stopped, "processStops", Json::Array(vec![])),
            ),
        ]),
        Ok(vec!["false"]),
    );
    add(
        "different stopped epoch",
        Json::object([
            ("action", "stopped".into()),
            (
                "status",
                changed(
                    &stopped,
                    "processStops",
                    Json::Array(vec![Json::object([
                        ("epoch", "other".into()),
                        ("observedAt", Json::String(time.clone())),
                    ])]),
                ),
            ),
        ]),
        Ok(vec!["false"]),
    );
    let mut retry_adapter_passes = 0;
    for (name, input, expected) in &rows {
        let i = input;
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
                } else if action == "context" {
                    inspector.context(&run_pin).await?.value()
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
                            &inspector, &run_pin,
                        )
                        .await?
                    } else {
                        inspector.find(&run_pin, budget, size).await?
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
        match expected {
            Err(code) => {
                let error = result.unwrap_err();
                assert_eq!(error.code, *code, "{name}: {error:?}");
                assert!(store.writes.borrow().is_empty(), "{name}");
            }
            Ok(kinds) => {
                let value = result.unwrap_or_else(|e| panic!("{name}: {e:?}"));
                let values = value.as_array().unwrap();
                assert_eq!(values.len(), kinds.len(), "{name}");
                for (value, kind) in values.iter().zip(kinds) {
                    match *kind {
                        "true" | "false" => {
                            assert_eq!(value.as_bool(), Some(*kind == "true"), "{name}")
                        }
                        "read-error" => assert_eq!(
                            value.required("reply").unwrap().get("error"),
                            native_error().required("reply").unwrap().get("error"),
                            "{name}"
                        ),
                        _ => {
                            assert_eq!(
                                value.required("kind").unwrap(),
                                &Json::from(*kind),
                                "{name}"
                            );
                            if *kind == "found" {
                                assert_eq!(
                                    value
                                        .required("metadata")
                                        .unwrap()
                                        .required("method")
                                        .unwrap(),
                                    &Json::from("thread/read")
                                );
                                assert_eq!(
                                    value.required("turns").unwrap().required("method").unwrap(),
                                    &Json::from("thread/turns/list")
                                );
                                assert_eq!(
                                    value.required("turns").unwrap().required("epoch").unwrap(),
                                    &Json::from("epoch")
                                );
                            }
                        }
                    }
                }
                let writes = store.writes.borrow();
                let expected_writes = if name == "checkpoint" {
                    1
                } else if name == "resume checkpoint" {
                    2
                } else if i.get("saved").is_some() {
                    1
                } else {
                    0
                };
                assert_eq!(writes.len(), expected_writes, "{name}");
                for write in writes.iter() {
                    let saved = write.required("value").unwrap();
                    for key in [
                        "threadId",
                        "turnId",
                        "epoch",
                        "nativeVersion",
                        "serviceNodeId",
                    ] {
                        assert_eq!(saved.get(key), context_value.get(key), "{name}: {key}");
                    }
                    assert_eq!(saved.get("createdAt"), Some(&Json::String(time.clone())));
                    assert_eq!(
                        saved.required("pageCount").unwrap().as_f64().unwrap() as usize,
                        saved
                            .required("visitedCursorHashes")
                            .unwrap()
                            .as_array()
                            .unwrap()
                            .len()
                    );
                }
                if let Some(last) = writes.last() {
                    let checkpoint = last.required("value").unwrap();
                    assert_eq!(
                        checkpoint.required("phase").unwrap(),
                        &Json::from(if name == "checkpoint" {
                            "searching"
                        } else {
                            "finished"
                        })
                    );
                    assert_eq!(
                        checkpoint.get("run"),
                        Some(if i.get("saved").is_some() {
                            saved_value.required("run").unwrap()
                        } else {
                            &run_pin
                        })
                    );
                }
            }
        }
        if name == "typed saved identity"
            || name == "absent saved identity"
            || name == "typed stopped identity"
            || name == "no stopped epoch"
        {
            assert!(
                !trace
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|entry| entry.as_array().unwrap()[0] == Json::from("rpc")),
                "{name}"
            );
        }
    }
    assert!(retry_adapter_passes > 0);
    println!("{} native inspection scenarios", rows.len());
}
