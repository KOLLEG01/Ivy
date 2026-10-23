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
async fn run_fixture(fixture: &str, native: &str, count: usize) {
    let fixture = parse(
        &std::fs::read_to_string(std::env::var(fixture).unwrap()).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let cases = fixture.required("cases").unwrap().as_array().unwrap();
    assert_eq!(cases.len(), count);
    let bytes = fixture
        .required("bytes")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap() as u8)
        .collect::<Vec<_>>();
    let mut observations = vec![];
    for case in cases {
        let input = case.required("input").unwrap();
        let task = identity(input, "taskId", "task");
        let operation = identity(input, "operationId", "operation");
        let attachment = identity(input, "attachmentId", "attachment");
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        let client = Arc::new(RecoveryClient {
            input: input.clone(),
            saved: saved(input, &task, &bytes),
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
            .write_attachment(&task, &operation, &attachment, &bytes)
            .await;
        if let Some(expected) = client.refusal.lock().unwrap().as_ref() {
            assert_eq!(
                &result.as_ref().unwrap_err().to_wire(),
                expected,
                "Original typed wire metadata"
            );
        }
        let outcome = match result {
            Ok(value) => Json::object([("value", value)]),
            Err(error) => Json::object([(
                "error",
                Json::object([
                    ("kind", "ivy".into()),
                    ("code", Json::String(error.code_js())),
                    ("message", Json::String(error.message_js())),
                    ("outcome", error.outcome.to_json()),
                ]),
            )]),
        };
        let scope = Json::Array(vec![
            "task-board-attachment".into(),
            Json::String(operation),
        ]);
        if !client.identities.lock().unwrap().is_empty() {
            let expected = scoped_operation_id(client.clone(), &scope).await.unwrap();
            assert!(
                client
                    .identities
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|value| value == &expected)
            );
        }
        assert_eq!(*client.epoch_reads.lock().unwrap(), 1);
        let actual = Json::object([
            ("outcome", outcome),
            ("trace", Json::Array(client.trace.lock().unwrap().clone())),
            ("scopes", Json::Array(vec![scope])),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(case.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            text(case, "name")
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        observations.push(Json::object([
            ("name", case.required("name").unwrap().clone()),
            ("result", actual),
        ]));
    }
    if let Ok(path) = std::env::var(native) {
        std::fs::write(
            path,
            ivy_contracts::json::stringify(&Json::Array(observations), MANAGEMENT_FRAME_BYTES)
                .unwrap(),
        )
        .unwrap();
    }
}
#[tokio::test(flavor = "current_thread")]
async fn attachment_recovery_matches_frozen_identity_bytes_and_failure_order() {
    run_fixture(
        "IVY_TASK_BOARD_ATTACHMENT_RECOVERY_FIXTURE",
        "IVY_TASK_BOARD_ATTACHMENT_RECOVERY_NATIVE",
        27,
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn attachment_recovery_keeps_utf16_truthiness_and_short_circuit_semantics() {
    run_fixture(
        "IVY_TASK_BOARD_ATTACHMENT_EXTRA_FIXTURE",
        "IVY_TASK_BOARD_ATTACHMENT_EXTRA_NATIVE",
        24,
    )
    .await;
}
