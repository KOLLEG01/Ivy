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
fn json(value: &str) -> Json {
    parse(value, MANAGEMENT_FRAME_BYTES).unwrap()
}
fn merge(value: &Json, patch: Json) -> Json {
    let mut value = value.clone();
    for (key, item) in patch.as_object().unwrap() {
        value.insert(key.clone(), item.clone());
    }
    value
}
fn pending_input(id: f64, patch: Json) -> Json {
    let mut value = json(
        r#"{"identity":{"serviceNodeId":"native-agent","epoch":"epoch","requestId":1},"method":"item/tool/requestUserInput","params":{"questions":[]},"observedAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","threadId":"thread","turnId":"turn","state":"pending","answerOperationId":null,"answerCallerPrincipalId":null,"reply":null,"code":null}"#,
    );
    value.insert(
        "identity",
        Json::object([
            ("serviceNodeId", "native-agent".into()),
            ("epoch", "epoch".into()),
            ("requestId", Json::Number(id)),
        ]),
    );
    merge(&value, patch)
}
fn notification(sequence: f64, patch: Json) -> Json {
    let mut value = json(
        r#"{"serviceNodeId":"native-agent","nativeVersion":"0.159.2","epoch":"epoch","sequence":1,"method":"turn/updated","observedAt":"2026-01-02T03:04:05.006Z","params":{"threadId":"thread","turnId":"turn"}}"#,
    );
    value.insert("sequence", Json::Number(sequence));
    merge(&value, patch)
}
fn pending(items: Vec<Json>, patch: Json) -> Json {
    let mut value = json(r#"{"epoch":"epoch","items":[],"truncated":false}"#);
    value.insert("items", Json::Array(items));
    merge(&value, patch)
}
fn page(items: Vec<Json>, patch: Json) -> Json {
    let mut value = json(
        r#"{"epoch":"epoch","firstAvailableSequence":1,"throughSequence":0,"hasMore":false,"gap":false,"items":[]}"#,
    );
    value.insert(
        "throughSequence",
        items
            .last()
            .map(|v| v.required("sequence").unwrap().clone())
            .unwrap_or(Json::Number(0.0)),
    );
    value.insert("items", Json::Array(items));
    merge(&value, patch)
}
fn saved(patch: Json) -> Json {
    let mut saved = json(
        r#"{"pin":{"objectId":"00000000-0000-4000-8000-000000000900","revision":1},"metadata":{"parentId":"00000000-0000-4000-8000-000000000800"},"value":{"schemaVersion":1,"run":{"objectId":"00000000-0000-4000-8000-000000000800","revision":1},"serviceNodeId":"native-agent","nativeVersion":"0.159.2","epoch":"epoch","threadId":"thread","turnId":"turn","inputsTruncated":false,"inputs":[],"notificationCursor":0,"notificationGap":false,"notificationsPending":false,"activity":[],"observedAt":"2026-01-02T03:04:05.006Z"}}"#,
    );
    saved.insert("value", merge(saved.required("value").unwrap(), patch));
    saved
}

#[tokio::test]
async fn advisory_signals_preserve_context_cursors_checkpoints_and_conflicts() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let source = catalogs
        .checked_native_contract(&"0.159.2".into(), None)
        .unwrap();
    let run_pin = json(r#"{"objectId":"00000000-0000-4000-8000-000000000800","revision":2}"#);
    let mut run = json(
        r#"{"phase":"running","target":{"serviceNodeId":"native-agent","hostId":"fixture"},"plan":{"nativeVersion":"0.159.2"},"primaryResourceRef":{"namespace":"codex","kind":"thread","serviceNodeId":"native-agent","nativeId":"thread"},"turnId":"turn","nativeEpoch":"epoch"}"#,
    );
    run.insert(
        "plan",
        merge(
            run.required("plan").unwrap(),
            Json::object([(
                "catalogSourceHash",
                source.catalog().required("sourceHash").unwrap().clone(),
            )]),
        ),
    );
    let mut status = json(
        r#"{"serviceNodeId":"native-agent","hostId":"fixture","nativeVersion":"0.159.2","epoch":"epoch","pid":42,"state":"ready","observedAt":"2026-01-02T03:04:05.006Z","code":null,"initialized":{},"pendingInputs":0,"operations":{"retained":0,"maximum":100,"bytes":0,"maximumBytes":1048576},"observedMethods":[]}"#,
    );
    status.insert(
        "nativeExecutableHash",
        source
            .catalog()
            .required("nativeExecutableHash")
            .unwrap()
            .clone(),
    );
    status.insert("catalogHash", source.catalog_hash().into());
    let empty = || Json::Object(vec![]);
    let base = Json::object([
        ("run", run.clone()),
        ("status", status.clone()),
        ("pending", Json::Array(vec![pending(vec![], empty())])),
        ("pages", Json::Array(vec![page(vec![], empty())])),
    ]);
    let mut rows = Vec::new();
    let mut add =
        |name: &str, patch: Json, result: Result<Vec<Option<f64>>, &'static str>, writes: usize| {
            rows.push((name.to_owned(), merge(&base, patch), result, writes));
        };
    add("fresh empty", empty(), Ok(vec![Some(1.0)]), 1);
    for phase in ["completed", "failed", "cancelled"] {
        add(
            "finished",
            Json::object([("run", merge(&run, Json::object([("phase", phase.into())])))]),
            Ok(vec![None]),
            0,
        );
    }
    add(
        "missing turn",
        Json::object([("run", merge(&run, json(r#"{"turnId":null}"#)))]),
        Ok(vec![None]),
        0,
    );
    add(
        "sorted inputs",
        Json::object([(
            "pending",
            Json::Array(vec![pending(
                vec![
                    pending_input(3.0, empty()),
                    pending_input(1.0, empty()),
                    pending_input(
                        2.0,
                        json(
                            r#"{"state":"answering","answerOperationId":"answer","answerCallerPrincipalId":"caller","reply":{"result":{}}}"#,
                        ),
                    ),
                ],
                empty(),
            )]),
        )]),
        Ok(vec![Some(1.0)]),
        1,
    );
    add(
        "filtered inputs",
        Json::object([(
            "pending",
            Json::Array(vec![pending(
                vec![
                    pending_input(1.0, json(r#"{"threadId":"other"}"#)),
                    pending_input(2.0, json(r#"{"turnId":"other"}"#)),
                    pending_input(
                        3.0,
                        json(
                            r#"{"identity":{"serviceNodeId":"native-agent","epoch":"old","requestId":3}}"#,
                        ),
                    ),
                    pending_input(4.0, json(r#"{"state":"expired"}"#)),
                    pending_input(5.0, json(r#"{"turnId":null}"#)),
                ],
                json(r#"{"truncated":true}"#),
            )]),
        )]),
        Ok(vec![Some(1.0)]),
        1,
    );
    add(
        "duplicate input",
        Json::object([(
            "pending",
            Json::Array(vec![pending(
                vec![pending_input(1.0, empty()), pending_input(1.0, empty())],
                empty(),
            )]),
        )]),
        Err("task_board_native_input_conflict"),
        0,
    );
    add(
        "wrong input owner",
        Json::object([(
            "pending",
            Json::Array(vec![pending(
                vec![pending_input(
                    1.0,
                    json(r#"{"identity":{"serviceNodeId":"other","epoch":"epoch","requestId":1}}"#),
                )],
                empty(),
            )]),
        )]),
        Err("task_board_target_mismatch"),
        0,
    );
    add(
        "filtered activity",
        Json::object([(
            "pages",
            Json::Array(vec![page(
                vec![
                    notification(1.0, empty()),
                    notification(2.0, json(r#"{"params":{"threadId":"other"}}"#)),
                    notification(3.0, json(r#"{"epoch":"old"}"#)),
                    notification(
                        4.0,
                        json(r#"{"params":{"threadId":"thread","turn":{"id":"turn"}}}"#),
                    ),
                    notification(
                        5.0,
                        json(r#"{"params":{"threadId":"thread","turnId":"other"}}"#),
                    ),
                ],
                empty(),
            )]),
        )]),
        Ok(vec![Some(1.0)]),
        1,
    );
    for patch in [
        json(r#"{"serviceNodeId":"other"}"#),
        json(r#"{"nativeVersion":"other"}"#),
        json(r#"{"sequence":1}"#),
    ] {
        add(
            "notification identity",
            Json::object([(
                "pages",
                Json::Array(vec![page(
                    vec![notification(1.0, empty()), notification(2.0, patch)],
                    json(r#"{"throughSequence":2}"#),
                )]),
            )]),
            Err("task_board_native_notification_conflict"),
            0,
        );
    }
    add(
        "input epoch changed",
        Json::object([(
            "pending",
            Json::Array(vec![pending(vec![], json(r#"{"epoch":"other"}"#))]),
        )]),
        Err("task_board_native_epoch_changed"),
        0,
    );
    add(
        "notification epoch changed",
        Json::object([(
            "pages",
            Json::Array(vec![page(vec![], json(r#"{"epoch":"other"}"#))]),
        )]),
        Err("task_board_native_epoch_changed"),
        0,
    );
    add(
        "saved unchanged",
        Json::object([("saved", saved(empty()))]),
        Ok(vec![Some(1.0)]),
        0,
    );
    add(
        "volatile cursor",
        Json::object([
            ("saved", saved(empty())),
            ("passes", Json::Number(2.0)),
            (
                "pages",
                Json::Array(vec![
                    page(vec![], json(r#"{"throughSequence":5,"hasMore":true}"#)),
                    page(vec![], json(r#"{"throughSequence":8}"#)),
                ]),
            ),
        ]),
        Ok(vec![Some(1.0), Some(1.0)]),
        0,
    );
    add(
        "finished forgets cursor",
        Json::object([
            ("saved", saved(empty())),
            ("passes", Json::Number(3.0)),
            (
                "runs",
                Json::Array(vec![
                    run.clone(),
                    merge(&run, json(r#"{"phase":"completed"}"#)),
                    run.clone(),
                ]),
            ),
            (
                "pages",
                Json::Array(vec![
                    page(vec![], json(r#"{"throughSequence":5}"#)),
                    page(vec![], json(r#"{"throughSequence":8}"#)),
                ]),
            ),
        ]),
        Ok(vec![Some(1.0), None, Some(1.0)]),
        0,
    );
    for patch in [
        json(r#"{"serviceNodeId":"other"}"#),
        json(r#"{"nativeVersion":"other"}"#),
        json(r#"{"threadId":"other"}"#),
        json(r#"{"turnId":"other"}"#),
        json(r#"{"run":{"objectId":"other","revision":1}}"#),
        json(r#"{"run":{"objectId":"00000000-0000-4000-8000-000000000800","revision":3}}"#),
    ] {
        add(
            "saved identity",
            Json::object([("saved", saved(patch))]),
            Err("task_board_identity_conflict"),
            0,
        );
    }
    add(
        "saved parent",
        Json::object([(
            "saved",
            merge(
                &saved(empty()),
                json(r#"{"metadata":{"parentId":"other"}}"#),
            ),
        )]),
        Err("task_board_identity_conflict"),
        0,
    );
    add(
        "new epoch",
        Json::object([
            ("saved", saved(empty())),
            ("status", merge(&status, json(r#"{"epoch":"new"}"#))),
            (
                "pending",
                Json::Array(vec![pending(
                    vec![pending_input(1.0, empty())],
                    json(r#"{"epoch":"new"}"#),
                )]),
            ),
            (
                "pages",
                Json::Array(vec![page(vec![], json(r#"{"epoch":"new"}"#))]),
            ),
        ]),
        Ok(vec![Some(2.0)]),
        1,
    );
    let old_activity = (1..=99).map(|n| json(&format!(r#"{{"epoch":"old","sequence":{n},"method":"old","observedAt":"2026-01-02T03:04:05.006Z"}}"#))).collect();
    add(
        "bounded activity",
        Json::object([
            (
                "saved",
                saved(Json::object([("activity", Json::Array(old_activity))])),
            ),
            (
                "pages",
                Json::Array(vec![page(
                    vec![notification(1.0, empty()), notification(2.0, empty())],
                    empty(),
                )]),
            ),
        ]),
        Ok(vec![Some(2.0)]),
        1,
    );
    add(
        "replace activity",
        Json::object([
            (
                "saved",
                saved(json(
                    r#"{"activity":[{"epoch":"epoch","sequence":1,"method":"old","observedAt":"2026-01-02T03:04:05.006Z"}]}"#,
                )),
            ),
            (
                "pages",
                Json::Array(vec![page(vec![notification(1.0, empty())], empty())]),
            ),
        ]),
        Ok(vec![Some(2.0)]),
        1,
    );
    add(
        "cursor regression",
        Json::object([
            ("saved", saved(json(r#"{"notificationCursor":5}"#))),
            (
                "pages",
                Json::Array(vec![page(vec![], json(r#"{"throughSequence":4}"#))]),
            ),
        ]),
        Err("task_board_cursor_regression"),
        0,
    );
    add(
        "explicit empty gap",
        Json::object([
            ("saved", saved(json(r#"{"notificationCursor":5}"#))),
            (
                "pages",
                Json::Array(vec![page(
                    vec![],
                    json(r#"{"throughSequence":4,"gap":true}"#),
                )]),
            ),
        ]),
        Ok(vec![Some(2.0)]),
        1,
    );
    add(
        "has more must advance",
        Json::object([(
            "pages",
            Json::Array(vec![page(vec![], json(r#"{"hasMore":true}"#))]),
        )]),
        Err("task_board_cursor_regression"),
        0,
    );
    add(
        "nonempty gap cannot regress",
        Json::object([(
            "pages",
            Json::Array(vec![page(
                vec![notification(2.0, empty())],
                json(r#"{"throughSequence":1,"gap":true}"#),
            )]),
        )]),
        Err("task_board_cursor_regression"),
        0,
    );
    add(
        "owner replaced",
        Json::object([(
            "statuses",
            Json::Array(vec![
                status.clone(),
                merge(&status, json(r#"{"epoch":"new"}"#)),
            ]),
        )]),
        Err("task_board_native_epoch_changed"),
        0,
    );
    add(
        "owner fence",
        json(r#"{"failOwner":3}"#),
        Err("fixture_failed"),
        0,
    );
    for code in ["revision_conflict", "mutation_conflict", "fixture_failed"] {
        add(
            "write failure",
            Json::object([
                ("writeErrors", Json::Array(vec![code.into()])),
                ("winner", saved(empty())),
            ]),
            if code == "fixture_failed" {
                Err("fixture_failed")
            } else {
                Ok(vec![Some(1.0)])
            },
            0,
        );
    }
    add(
        "conflict without winner",
        json(r#"{"writeErrors":["revision_conflict"]}"#),
        Err("revision_conflict"),
        0,
    );
    add(
        "checkpoint next pass",
        Json::object([
            ("passes", Json::Number(2.0)),
            (
                "pages",
                Json::Array(vec![
                    page(vec![notification(1.0, empty())], empty()),
                    page(vec![notification(2.0, empty())], empty()),
                ]),
            ),
        ]),
        Ok(vec![Some(1.0), Some(2.0)]),
        2,
    );
    assert_eq!(rows.len(), 39);
    let mut add =
        |name: &str, patch: Json, result: Result<Vec<Option<f64>>, &'static str>, writes: usize| {
            rows.push((name.to_owned(), merge(&base, patch), result, writes));
        };
    for value in [
        Json::Null,
        Json::Bool(false),
        Json::Bool(true),
        Json::Number(0.0),
        Json::Number(1.0),
        "".into(),
        Json::Array(vec![]),
        empty(),
    ] {
        add(
            "ineligible typed turn",
            Json::object([(
                "run",
                merge(&run, Json::object([("turnId", value.clone())])),
            )]),
            Ok(vec![None]),
            0,
        );
        add(
            "invalid saved epoch",
            Json::object([("run", merge(&run, Json::object([("nativeEpoch", value)])))]),
            Err("task_board_turn_mismatch"),
            0,
        );
    }
    let mut no_turn = run.clone();
    no_turn.remove("turnId");
    add(
        "ineligible absent turn",
        Json::object([("run", no_turn)]),
        Ok(vec![None]),
        0,
    );
    for value in [
        Json::Null,
        Json::Number(0.0),
        Json::Number(1.0),
        "".into(),
        "true".into(),
        Json::Array(vec![]),
        empty(),
    ] {
        add(
            "typed saved gap",
            Json::object([("saved", saved(Json::object([("notificationGap", value)])))]),
            Ok(vec![Some(2.0)]),
            1,
        );
    }
    add(
        "retained true gap",
        Json::object([("saved", saved(json(r#"{"notificationGap":true}"#)))]),
        Ok(vec![Some(1.0)]),
        0,
    );
    add(
        "unicode turn",
        Json::object([("run", merge(&run, json(r#"{"turnId":"turn-ü-😀"}"#)))]),
        Ok(vec![Some(1.0)]),
        1,
    );
    assert_eq!(rows.len(), 65);
    for (name, input, expected, expected_writes) in &rows {
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
            pin: run_pin.clone(),
            time: "2026-01-02T03:04:05.006Z".into(),
            tools: ToolClients::new(client),
            trace: trace.clone(),
            saved: RefCell::new(checkpoint(input.get("saved"))),
            writes: RefCell::default(),
            owners: RefCell::default(),
            write_index: RefCell::default(),
            pass: RefCell::default(),
        };
        let subject = NativeSignals::new(NativeInspector {
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
        match expected {
            Err(code) => assert_eq!(failure.expect(name).code, *code, "{name}"),
            Ok(revisions) => {
                assert!(failure.is_none(), "{name}: {failure:?}");
                assert_eq!(pins.len(), revisions.len(), "{name}");
                for (pin, revision) in pins.iter().zip(revisions) {
                    match revision {
                        None => assert_eq!(pin, &Json::Null, "{name}"),
                        Some(revision) => {
                            assert_eq!(
                                pin.get("objectId"),
                                Some(&Json::from("00000000-0000-4000-8000-000000000900")),
                                "{name}"
                            );
                            assert_eq!(
                                pin.get("revision").and_then(Json::as_f64),
                                Some(*revision),
                                "{name}"
                            );
                        }
                    }
                }
            }
        }
        let writes = store.writes.borrow();
        assert_eq!(writes.len(), *expected_writes, "{name}");
        for write in writes.iter() {
            let value = write.required("value").unwrap();
            assert_eq!(value.get("run"), Some(&run_pin), "{name}");
            assert_eq!(
                value.get("turnId"),
                input.required("run").unwrap().get("turnId"),
                "{name}"
            );
            assert_eq!(
                value.get("serviceNodeId"),
                Some(&Json::from("native-agent")),
                "{name}"
            );
            assert_eq!(
                value.get("observedAt"),
                Some(&Json::from("2026-01-02T03:04:05.006Z")),
                "{name}"
            );
            let activity = value.required("activity").unwrap().as_array().unwrap();
            assert!(activity.len() <= 100, "{name}");
            assert_eq!(
                value.get("notificationGap").and_then(Json::as_bool),
                Some(name == "new epoch" || name == "explicit empty gap"),
                "{name}"
            );
        }
        if let Some(write) = writes.last() {
            let value = write.required("value").unwrap();
            let inputs = value.required("inputs").unwrap().as_array().unwrap();
            let activity = value.required("activity").unwrap().as_array().unwrap();
            match name.as_str() {
                "sorted inputs" => {
                    assert_eq!(inputs.len(), 3);
                    let hashes: Vec<_> = inputs
                        .iter()
                        .map(|v| hash_json(v.required("identity").unwrap()).unwrap())
                        .collect();
                    assert!(hashes.windows(2).all(|p| p[0] < p[1]));
                }
                "filtered inputs" => {
                    assert_eq!(inputs.len(), 1);
                    assert_eq!(
                        inputs[0].required("identity").unwrap().get("requestId"),
                        Some(&Json::Number(5.0))
                    );
                    assert_eq!(value.get("inputsTruncated"), Some(&Json::Bool(true)));
                }
                "filtered activity" => assert_eq!(
                    activity
                        .iter()
                        .map(|v| v.required("sequence").unwrap().as_f64().unwrap())
                        .collect::<Vec<_>>(),
                    vec![1.0, 4.0]
                ),
                "new epoch" => {
                    assert!(inputs.is_empty());
                    assert_eq!(value.get("epoch"), Some(&Json::from("new")));
                }
                "bounded activity" => {
                    assert_eq!(activity.len(), 100);
                    assert_eq!(activity[0].get("sequence"), Some(&Json::Number(2.0)));
                }
                "replace activity" => {
                    assert_eq!(activity.len(), 1);
                    assert_eq!(activity[0].get("method"), Some(&Json::from("turn/updated")));
                }
                _ => {}
            }
        }
        let trace = trace.lock().unwrap();
        let notifications: Vec<_> = trace
            .iter()
            .filter_map(|v| {
                let row = v.as_array().unwrap();
                (row[0] == Json::from("rpc")
                    && row[2].get("qualifiedName") == Some(&Json::from("agent.notifications")))
                .then(|| {
                    row[2]
                        .required("arguments")
                        .unwrap()
                        .required("afterSequence")
                        .unwrap()
                        .as_f64()
                        .unwrap()
                })
            })
            .collect();
        match name.as_str() {
            "volatile cursor" => assert_eq!(notifications, vec![0.0, 5.0]),
            "finished forgets cursor" => assert_eq!(notifications, vec![0.0, 0.0]),
            "checkpoint next pass" => assert_eq!(notifications, vec![0.0, 1.0]),
            _ => {}
        }
        if name.starts_with("ineligible")
            || name == "invalid saved epoch"
            || name == "finished"
            || name == "missing turn"
        {
            assert!(
                !trace
                    .iter()
                    .any(|v| v.as_array().unwrap()[0] == Json::from("rpc")),
                "{name}"
            );
        }
    }
    println!("{} native advisory signal scenarios", rows.len());
}
