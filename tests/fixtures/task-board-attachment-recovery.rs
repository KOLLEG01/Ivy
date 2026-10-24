use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

struct RecoveryClient {
    input: Json,
    saved: Json,
    trace: Mutex<Vec<Json>>,
    identities: Mutex<Vec<JsString>>,
    refusal: Mutex<Option<Json>>,
    epoch_reads: Mutex<usize>,
}
impl RecoveryClient {
    fn failure(&self, code: &str, message: &str, outcome: Outcome, retained: bool) -> IvyError {
        let error = IvyError::new(code, message)
            .with_outcome(outcome)
            .with_details(Json::String(JsString(vec![0xd800, 0x61, 0xdfff])));
        if retained {
            *self.refusal.lock().unwrap() = Some(error.to_wire());
        }
        error
    }
}
impl RpcClient for RecoveryClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        mut params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "system.status" {
                *self.epoch_reads.lock().unwrap() += 1;
                if let Some(code) = self.input.get("scopeError") {
                    return Err(self.failure(
                        &code.as_str()?,
                        "Original scope refusal.",
                        Outcome::Unknown,
                        true,
                    ));
                }
                return Ok(Json::object([(
                    "runtimeEpoch",
                    "11111111-1111-4111-8111-111111111111".into(),
                )]));
            }
            if method == "objects.write" {
                self.identities
                    .lock()
                    .unwrap()
                    .push(string(&params, "mutationId"));
                params.insert("mutationId", "retained-attachment-operation".into());
            }
            self.trace
                .lock()
                .unwrap()
                .push(Json::Array(vec![method.into(), params]));
            match method {
                "objects.write" => {
                    if self.input.get("direct") == Some(&Json::Bool(true)) {
                        return Ok(Json::object([
                            ("object", Json::object([("id", "new".into())])),
                            ("revision", Json::object([("revision", Json::Number(1.0))])),
                        ]));
                    }
                    let code = self
                        .input
                        .get("writeError")
                        .cloned()
                        .unwrap_or_else(|| "revision_conflict".into())
                        .as_str()?;
                    Err(self.failure(
                        &code,
                        "Original write refusal.",
                        Outcome::NotExecuted,
                        !matches!(code.as_str(), "revision_conflict" | "mutation_expired"),
                    ))
                }
                "objects.query" => {
                    if let Some(code) = self.input.get("queryError") {
                        return Err(self.failure(
                            &code.as_str()?,
                            "Original query refusal.",
                            Outcome::Unknown,
                            true,
                        ));
                    }
                    if let Some(reply) = self.input.get("queryReply") {
                        return Ok(reply.clone());
                    }
                    Ok(Json::object([
                        (
                            "items",
                            Json::Array(
                                (0..self
                                    .input
                                    .get("queryCount")
                                    .and_then(Json::as_f64)
                                    .unwrap_or(1.0) as usize)
                                    .map(|_| Json::object([("objectId", "original".into())]))
                                    .collect(),
                            ),
                        ),
                        (
                            "nextCursor",
                            self.input.get("cursor").cloned().unwrap_or(Json::Null),
                        ),
                    ]))
                }
                "objects.read" => {
                    if let Some(code) = self.input.get("readError") {
                        return Err(self.failure(
                            &code.as_str()?,
                            "Original read refusal.",
                            Outcome::Unknown,
                            true,
                        ));
                    }
                    Ok(self.saved.clone())
                }
                _ => panic!("Unexpected attachment RPC {method}"),
            }
        })
    }
}
fn identity(input: &Json, key: &str, default: &str) -> JsString {
    input
        .get(key)
        .and_then(Json::as_js_string)
        .cloned()
        .unwrap_or_else(|| default.into())
}
fn saved(input: &Json, task: &JsString, bytes: &[u8]) -> Json {
    if let Some(saved) = input.get("saved") {
        return saved.clone();
    }
    let mut saved = Json::object([
        (
            "object",
            Json::object([
                ("id", "original".into()),
                ("parentId", Json::String(task.clone())),
                ("ownerObjectId", Json::String(task.clone())),
                ("effectivelyArchived", false.into()),
                ("currentRevision", Json::Number(1.0)),
            ]),
        ),
        (
            "revision",
            Json::object([
                ("revision", Json::Number(1.0)),
                ("contractVersion", "1.0.0".into()),
                ("byteLength", Json::Number(bytes.len() as f64)),
                ("contentHash", digest(bytes).into()),
            ]),
        ),
        (
            "content",
            Json::object([
                ("encoding", "base64".into()),
                ("value", STANDARD.encode(bytes).into()),
            ]),
        ),
    ]);
    if let Some(variant) = input.get("base64Variant").and_then(Json::as_js_string) {
        let base = STANDARD.encode(bytes);
        let value = match variant.to_utf8().unwrap().as_str() {
            "no-padding" => base.trim_end_matches('=').to_owned(),
            "url-safe" => base.replace('+', "-").replace('/', "_"),
            "whitespace" => format!("{} \n\t{}", &base[..3], &base[3..]),
            "ignored-junk" => format!("{}$!{}", &base[..3], &base[3..]),
            _ => panic!("Unexpected base64 fixture"),
        };
        saved.insert(
            "content",
            Json::object([("encoding", "base64".into()), ("value", value.into())]),
        );
    }
    if let Some(change) = input.get("change") {
        let parts = change.required("parts").unwrap().as_array().unwrap();
        assert_eq!(parts.len(), 2);
        let parent = parts[0].as_str().unwrap();
        let mut value = saved.required(&parent).unwrap().clone();
        value.insert(
            parts[1].as_js_string().unwrap().clone(),
            change.required("value").unwrap().clone(),
        );
        saved.insert(parent.as_str(), value);
    }
    saved
}
enum Expected {
    Pin(&'static str),
    Error(&'static str),
    Original,
}

async fn check_recovery(input: Json, bytes: &[u8], expected: Expected, methods: &[&str]) {
    let task = identity(&input, "taskId", "task");
    let operation = identity(&input, "operationId", "operation");
    let attachment = identity(&input, "attachmentId", "attachment");
    let db = Connection::open_in_memory().unwrap();
    local_journal::initialize(&db).unwrap();
    let client = Arc::new(RecoveryClient {
        saved: saved(&input, &task, bytes),
        input,
        trace: Mutex::default(),
        identities: Mutex::default(),
        refusal: Mutex::default(),
        epoch_reads: Mutex::new(0),
    });
    let store = JournalStore {
        journal: LocalJournal {
            connection: &db,
            root_object_id: None,
        },
        client: client.clone(),
        contracts: FixtureContracts,
        limits: HistoryLimits::default(),
        now: || panic!("No attachment clock"),
        new_id: || panic!("No replacement attachment ID"),
        cache: None,
    };
    let result = store
        .write_attachment(&task, &operation, &attachment, bytes)
        .await;
    match expected {
        Expected::Pin(id) => assert_eq!(
            result.unwrap(),
            Json::object([("objectId", id.into()), ("revision", Json::Number(1.0)),])
        ),
        Expected::Error(code) => {
            let error = result.unwrap_err();
            assert_eq!(error.code, code, "{:?}", client.input);
            assert_eq!(
                error.outcome,
                if code == "internal_error" {
                    Outcome::Unknown
                } else {
                    Outcome::NotExecuted
                }
            );
            if code == "task_board_evidence_mismatch" {
                assert_eq!(
                    error.message,
                    if methods.len() == 2 {
                        "The original attachment identity is unavailable."
                    } else {
                        "The retained attachment must match its exact original bytes and owner."
                    }
                );
            }
        }
        Expected::Original => assert_eq!(
            result.unwrap_err().to_wire(),
            client
                .refusal
                .lock()
                .unwrap()
                .clone()
                .expect("Original typed error"),
        ),
    }
    let trace = client.trace.lock().unwrap().clone();
    assert_eq!(
        trace
            .iter()
            .map(|row| row.as_array().unwrap()[0].as_str().unwrap())
            .collect::<Vec<_>>(),
        methods,
        "{:?}",
        client.input
    );
    let mut name = JsString::from("Task attachment ");
    name.0.extend_from_slice(&attachment.0);
    for row in &trace {
        let row = row.as_array().unwrap();
        match row[0].as_str().unwrap().as_str() {
            "objects.write" => {
                assert_eq!(
                    row[1].required("content").unwrap(),
                    &Json::object([
                        ("encoding", "base64".into()),
                        ("value", STANDARD.encode(bytes).into()),
                    ])
                );
                assert_eq!(
                    row[1].required("create").unwrap(),
                    &Json::object([
                        ("parentId", Json::String(task.clone())),
                        ("ownerObjectId", Json::String(task.clone())),
                        ("contractKey", "task-board/attachment".into()),
                        ("name", Json::String(name.clone())),
                    ])
                );
            }
            "objects.query" => assert_eq!(
                row[1],
                Json::object([
                    ("contractKey", "task-board/attachment".into()),
                    ("limit", Json::Number(2.0)),
                    (
                        "where",
                        Json::object([
                            ("op", "and".into()),
                            (
                                "args",
                                Json::Array(vec![
                                    Json::object([
                                        ("op", "eq".into()),
                                        ("field", "object.parentId".into()),
                                        ("value", Json::String(task.clone()))
                                    ]),
                                    Json::object([
                                        ("op", "eq".into()),
                                        ("field", "object.name".into()),
                                        ("value", Json::String(name.clone()))
                                    ]),
                                ])
                            ),
                        ])
                    ),
                ])
            ),
            "objects.read" => assert_eq!(row[1], Json::object([("objectId", "original".into())])),
            _ => unreachable!(),
        }
    }
    let scope = Json::Array(vec![
        "task-board-attachment".into(),
        Json::String(operation),
    ]);
    if !client.identities.lock().unwrap().is_empty() {
        let expected = scoped_operation_id(client.clone(), &scope).await.unwrap();
        assert_eq!(
            client.identities.lock().unwrap().as_slice(),
            &[expected.clone()]
        );
        assert_eq!(
            scoped_operation_id(client.clone(), &scope).await.unwrap(),
            expected
        );
    }
    assert_eq!(*client.epoch_reads.lock().unwrap(), 1);
    for table in ["documents", "blobs"] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

fn changed(parent: &str, key: &str, value: Json) -> Json {
    Json::object([(
        "change",
        Json::object([
            ("parts", Json::Array(vec![parent.into(), key.into()])),
            ("value", value),
        ]),
    )])
}
const RECOVERY: &[&str] = &["objects.write", "objects.query", "objects.read"];

#[tokio::test(flavor = "current_thread")]
async fn attachment_recovery_preserves_identity_bytes_and_failure_order() {
    for length in [0, 1, 2, 3, 4, 31, 256, 4097, 8 * 1024 * 1024] {
        let bytes = (0..length)
            .map(|i| (i as u8).wrapping_mul(37).wrapping_add(11))
            .collect::<Vec<_>>();
        check_recovery(
            Json::object([("direct", true.into())]),
            &bytes,
            Expected::Pin("new"),
            &["objects.write"],
        )
        .await;
        check_recovery(
            Json::object([] as [(&str, Json); 0]),
            &bytes,
            Expected::Pin("original"),
            RECOVERY,
        )
        .await;
    }
    check_recovery(
        Json::object([("writeError", "mutation_expired".into())]),
        b"original",
        Expected::Pin("original"),
        RECOVERY,
    )
    .await;
    check_recovery(
        Json::object([
            ("taskId", Json::String(JsString(vec![0x4e2d, 0xd800]))),
            ("operationId", Json::String(JsString(vec![0x61, 0xdfff]))),
            ("attachmentId", "pièce 🚀".into()),
        ]),
        b"original",
        Expected::Pin("original"),
        RECOVERY,
    )
    .await;
    for key in ["scopeError", "writeError", "queryError", "readError"] {
        let methods = match key {
            "scopeError" => &[][..],
            "writeError" => &RECOVERY[..1],
            "queryError" => &RECOVERY[..2],
            _ => RECOVERY,
        };
        check_recovery(
            Json::object([(key, "service_unavailable".into())]),
            b"original",
            Expected::Original,
            methods,
        )
        .await;
    }
    for input in [
        Json::object([("queryCount", Json::Number(0.0))]),
        Json::object([("queryCount", Json::Number(2.0))]),
        Json::object([("cursor", "more".into())]),
    ] {
        check_recovery(
            input,
            b"original",
            Expected::Error("task_board_evidence_mismatch"),
            &RECOVERY[..2],
        )
        .await;
    }
    for (parent, key, value) in [
        ("object", "parentId", "elsewhere".into()),
        ("object", "ownerObjectId", "elsewhere".into()),
        ("object", "effectivelyArchived", true.into()),
        ("object", "currentRevision", Json::Number(2.0)),
        ("revision", "revision", Json::Number(2.0)),
        ("revision", "contractVersion", "2.0.0".into()),
        ("revision", "byteLength", Json::Number(99.0)),
        ("revision", "contentHash", digest(b"changed").into()),
        ("content", "encoding", "utf8".into()),
        ("content", "value", STANDARD.encode(b"changed").into()),
    ] {
        check_recovery(
            changed(parent, key, value),
            b"original",
            Expected::Error("task_board_evidence_mismatch"),
            RECOVERY,
        )
        .await;
    }
}

// Retained responses must carry the public cursor and archival field types.
// Wrong types and missing fields cannot stand in for null or false.
async fn check_attachment_metadata_types() {
    for cursor in [Json::Bool(false), Json::Number(0.0), "".into()] {
        check_recovery(
            Json::object([("cursor", cursor)]),
            b"original",
            Expected::Error("task_board_evidence_mismatch"),
            &RECOVERY[..2],
        )
        .await;
    }
    check_recovery(
        Json::object([(
            "queryReply",
            Json::object([(
                "items",
                Json::Array(vec![Json::object([("objectId", "original".into())])]),
            )]),
        )]),
        b"original",
        Expected::Error("task_board_evidence_mismatch"),
        &RECOVERY[..2],
    )
    .await;
    for archived in [
        Json::Null,
        Json::Number(0.0),
        Json::Number(1.0),
        "".into(),
        "false".into(),
        Json::Array(vec![]),
        Json::object([] as [(&str, Json); 0]),
    ] {
        check_recovery(
            changed("object", "effectivelyArchived", archived),
            b"original",
            Expected::Error("task_board_evidence_mismatch"),
            RECOVERY,
        )
        .await;
    }
    let mut missing = saved(
        &Json::object([] as [(&str, Json); 0]),
        &"task".into(),
        b"original",
    );
    let mut object = missing.required("object").unwrap().clone();
    object.remove("effectivelyArchived");
    missing.insert("object", object);
    check_recovery(
        Json::object([("saved", missing)]),
        b"original",
        Expected::Error("task_board_evidence_mismatch"),
        RECOVERY,
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn attachment_recovery_uses_native_base64_and_preserves_guard_order() {
    check_attachment_metadata_types().await;
    let bytes = [0xfb, 0xff]; // Canonical +/8= exercises both alphabet characters and padding.
    for variant in ["no-padding", "url-safe", "whitespace", "ignored-junk"] {
        check_recovery(
            Json::object([("base64Variant", variant.into())]),
            &bytes,
            Expected::Error("task_board_evidence_mismatch"),
            RECOVERY,
        )
        .await;
    }
    for content in [
        "+/9=",
        "+/8=ignored",
        "+/8==",
        "+/8",
        "+/8=\0",
        "+/8A",
        "",
        "====",
    ] {
        check_recovery(
            changed("content", "value", content.into()),
            &bytes,
            Expected::Error("task_board_evidence_mismatch"),
            RECOVERY,
        )
        .await;
    }
    for value in [
        JsString("+/8=".encode_utf16().map(|unit| unit + 0x100).collect()),
        JsString(vec![0xd800]),
    ] {
        check_recovery(
            changed("content", "value", Json::String(value)),
            &bytes,
            Expected::Error("task_board_evidence_mismatch"),
            RECOVERY,
        )
        .await;
    }
    check_recovery(
        changed("content", "value", Json::Null),
        &bytes,
        Expected::Error("internal_error"),
        RECOVERY,
    )
    .await;
    // An earlier owner refusal must not inspect the deliberately malformed content.
    let malformed = Json::object([(
        "saved",
        Json::object([("object", Json::object([("parentId", "elsewhere".into())]))]),
    )]);
    check_recovery(
        malformed,
        &bytes,
        Expected::Error("task_board_evidence_mismatch"),
        RECOVERY,
    )
    .await;
    check_recovery(
        Json::object([] as [(&str, Json); 0]),
        &bytes,
        Expected::Pin("original"),
        RECOVERY,
    )
    .await;
}
