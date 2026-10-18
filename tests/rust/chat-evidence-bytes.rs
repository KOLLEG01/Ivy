use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    evidence_bytes::{read_chat_evidence, save_chat_evidence},
    limits::EVIDENCE_CHUNK_BYTES,
    store::{ChatStore, Pin, local::Limits},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, Outcome,
    hash::{digest, hash_json},
    json::{canonical, parse},
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestFuture, RequestOptions, RpcClient},
};
use rusqlite::{Connection, params};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    v.get(key).unwrap()
}
fn text(v: &Json, key: &str) -> String {
    field(v, key).as_str().unwrap()
}
fn array<'a>(v: &'a Json, key: &str) -> &'a [Json] {
    field(v, key).as_array().unwrap()
}
fn number(v: &Json, key: &str) -> usize {
    field(v, key).as_f64().unwrap() as usize
}
fn equal(actual: &Json, expected: &Json, label: &str) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap(),
        "{label}"
    );
}
fn result(value: IvyResult<Json>) -> Json {
    match value {
        Ok(value) => Json::object([("value", value)]),
        Err(error) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(error.code_js())),
                ("message", Json::String(error.message_js())),
                ("outcome", error.outcome.as_str().into()),
            ]),
        )]),
    }
}
fn error(value: &Json) -> IvyError {
    IvyError::new_js(
        field(value, "code").as_js_string().unwrap().clone(),
        field(value, "message").as_js_string().unwrap().clone(),
    )
    .with_outcome(match text(value, "outcome").as_str() {
        "unknown" => Outcome::Unknown,
        "completed" => Outcome::Completed,
        _ => Outcome::NotExecuted,
    })
}
fn pin(value: &Json) -> Pin {
    Pin {
        object_id: text(value, "objectId"),
        revision: number(value, "revision") as i64,
    }
}
fn summary(value: &Json) -> Json {
    let encoded = canonical(value, 32 * EVIDENCE_CHUNK_BYTES).unwrap();
    let mut result = Json::object([
        ("byteLength", Json::Number(encoded.len() as f64)),
        ("contentHash", digest(&encoded).into()),
    ]);
    if encoded.encode_utf16().count() < 512 {
        result.insert("value", value.clone());
    }
    result
}
fn body(spec: &Json) -> Json {
    let kind = spec
        .get("kind")
        .and_then(|value| value.as_str().ok())
        .unwrap_or_else(|| "operation".into());
    let mut value = field(field(oracle(), "bodies"), &kind).clone();
    if let Some(patch) = spec.get("patch") {
        for (key, v) in patch.as_object().unwrap() {
            value.insert(key.clone(), v.clone());
        }
    }
    let mut params = field(&value, "params").clone();
    if let Some(text) = spec.get("text") {
        params.insert("text", text.clone());
        value.insert("params", params.clone());
    }
    if spec.get("splitUtf8") == Some(&true.into()) {
        let encoded = canonical(&value, usize::MAX).unwrap();
        let prefix = encoded.find("\"text\":\"").unwrap() + 8;
        params.insert(
            "text",
            format!("{}😀end", "x".repeat(EVIDENCE_CHUNK_BYTES - 1 - prefix)).into(),
        );
        value.insert("params", params.clone());
    }
    if let Some(length) = spec.get("byteLength").and_then(Json::as_f64) {
        params.insert("text", "".into());
        value.insert("params", params.clone());
        let size = canonical(&value, usize::MAX).unwrap().len();
        params.insert("text", "x".repeat(length as usize - size).into());
        value.insert("params", params.clone());
    }
    if value.get("requestHash").is_some() {
        let hash = hash_json(&Json::object([
            ("method", field(&value, "method").clone()),
            ("params", params),
        ]))
        .unwrap();
        value.insert("requestHash", hash.into());
    }
    value
}
fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .arg("tests/rust/fixtures/chat-evidence-bytes-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_EVIDENCE_BYTES_ORACLE_CAPTURE") {
            std::fs::write(path, &output.stdout).unwrap();
        }
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
struct Fixed(i64);
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new(database: &Json) -> Self {
        let root = std::env::temp_dir().join(format!("ivy-chat-evidence-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("chat-bridge.sqlite"),
            STANDARD.decode(database.as_str().unwrap()).unwrap(),
        )
        .unwrap();
        Self(root)
    }
    fn db(&self) -> Connection {
        Connection::open(self.0.join("chat-bridge.sqlite")).unwrap()
    }
    fn manifest(&self, pin: &Pin) -> Json {
        let raw: String = self
            .db()
            .query_row(
                "SELECT value_json FROM revisions WHERE id=? ORDER BY revision DESC LIMIT 1",
                [&pin.object_id],
                |r| r.get(0),
            )
            .unwrap();
        parse(&raw, usize::MAX).unwrap()
    }
    fn patch(&self, pin: &Pin, value: &Json) {
        self.db().execute("UPDATE revisions SET value_json=? WHERE id=? AND revision=(SELECT current_revision FROM documents WHERE id=?)",params![canonical(value,usize::MAX).unwrap(),pin.object_id,pin.object_id]).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct State {
    calls: Vec<Json>,
    remote: Json,
    error: Option<IvyError>,
}
struct Client(Mutex<State>);
impl Client {
    fn new() -> Self {
        Self(Mutex::new(State {
            calls: vec![],
            error: None,
            remote: Json::object([
                (
                    "object",
                    Json::object([
                        ("id", "original-input".into()),
                        ("parentId", "root".into()),
                        ("name", "Input".into()),
                        ("contractKey", "chat-bridge/input".into()),
                        ("effectivelyArchived", false.into()),
                    ]),
                ),
                (
                    "revision",
                    Json::object([
                        ("revision", Json::Number(1.0)),
                        ("contractVersion", "1.3.0".into()),
                        ("mediaType", "application/json".into()),
                    ]),
                ),
                (
                    "content",
                    Json::object([
                        ("encoding", "json".into()),
                        ("value", Json::object([("workspaceId", "chat".into())])),
                    ]),
                ),
            ]),
        }))
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
            let mut state = self.0.lock().unwrap();
            state.calls.push(Json::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
            assert_eq!(method, "objects.read");
            if params.get("objectId") != Some(&"original-input".into()) {
                return Err(IvyError::new(
                    "object_missing",
                    "Original parent is unavailable.",
                ));
            }
            Ok(state.remote.clone())
        })
    }
}
fn normalize(value: &Json, aliases: &HashMap<String, String>) -> Json {
    match value {
        Json::String(text) => text
            .to_utf8()
            .ok()
            .and_then(|text| aliases.get(&text))
            .map(|v| v.as_str().into())
            .unwrap_or_else(|| value.clone()),
        Json::Array(values) => Json::Array(values.iter().map(|v| normalize(v, aliases)).collect()),
        Json::Object(values) => Json::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), normalize(v, aliases)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn snapshot(
    fixture: &Fixture,
    store: &ChatStore,
    client: &Client,
    output: Json,
    draft_calls: Vec<Json>,
    last: &mut i64,
    aliases: &mut HashMap<String, String>,
) -> Json {
    let db = fixture.db();
    let mut query=db.prepare("SELECT d.id,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/evidence' ORDER BY d.name").unwrap();
    let documents = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, name, parent, revision, raw) = r.unwrap();
            aliases.insert(id.clone(), format!("<local:{name}>"));
            Json::object([
                ("id", id.into()),
                ("name", name.into()),
                ("parent", parent.into()),
                ("revision", Json::Number(revision as f64)),
                ("value", parse(&raw, usize::MAX).unwrap()),
            ])
        })
        .collect();
    let mut query = db
        .prepare("SELECT id,key,name,parent_id,bytes FROM blobs ORDER BY name,parent_id")
        .unwrap();
    let blobs = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Vec<u8>>(4)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, key, name, parent, bytes) = r.unwrap();
            aliases.insert(id.clone(), format!("<local:{name}>"));
            let mut value = Json::object([
                ("id", id.into()),
                ("key", key.into()),
                ("name", name.into()),
                ("parent", parent.into()),
                ("byteLength", Json::Number(bytes.len() as f64)),
                ("contentHash", digest(&bytes).into()),
            ]);
            if bytes.len() < 64 {
                value.insert("base64", STANDARD.encode(&bytes).into());
            }
            value
        })
        .collect();
    let mut query = db
        .prepare("SELECT rowid,value_json FROM evidence_test_writes WHERE rowid>? ORDER BY rowid")
        .unwrap();
    let writes = query
        .query_map([*last], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|r| {
            let (id, raw) = r.unwrap();
            *last = id;
            parse(&raw, usize::MAX).unwrap()
        })
        .collect();
    let calls = std::mem::take(&mut client.0.lock().unwrap().calls);
    normalize(
        &Json::object([
            ("output", output),
            ("documents", Json::Array(documents)),
            ("blobs", Json::Array(blobs)),
            ("writes", Json::Array(writes)),
            ("calls", Json::Array(calls)),
            ("draftCalls", Json::Array(draft_calls)),
            ("changeVersion", Json::Number(store.change_version() as f64)),
        ]),
        aliases,
    )
}
async fn compare(group: &str) {
    let corpus = oracle();
    let clock = Arc::new(Fixed(number(corpus, "now") as i64));
    let parent = text(field(corpus, "parent"), "objectId");
    let mut compared = 0;
    for case in array(corpus, "cases") {
        if case
            .get("group")
            .and_then(|value| value.as_str().ok())
            .unwrap_or_else(|| "behavior".into())
            != group
        {
            continue;
        }
        let fixture = Fixture::new(field(corpus, "database"));
        let client = Arc::new(Client::new());
        let mut store = ChatStore::with_clock(
            client.clone(),
            Some("root".into()),
            Some(fixture.0.clone()),
            clock.clone(),
        )
        .await
        .unwrap();
        let mut pins = HashMap::<String, Pin>::new();
        let mut last = 0;
        let mut aliases = HashMap::new();
        for (index, step) in array(case, "steps").iter().enumerate() {
            let expected = &array(case, "expected")[index];
            let drafts = Mutex::new(Vec::new());
            // The required native-plan gate is replayed from the original dependency.
            // These cases qualify its call order/input, not Rust native schema evaluation.
            let validate = |value: &Json| -> IvyResult<()> {
                let mut calls = drafts.lock().unwrap();
                let original = &array(expected, "draftCalls")[calls.len()];
                let input = summary(value);
                equal(&input, field(original, "input"), "native draft input");
                calls.push(Json::object([
                    ("input", input),
                    ("outcome", field(original, "outcome").clone()),
                ]));
                match field(original, "outcome").get("error") {
                    Some(value) => Err(error(value)),
                    None => Ok(()),
                }
            };
            let action = text(step, "action");
            let selected_parent = match step
                .get("parent")
                .and_then(|value| value.as_str().ok())
                .as_deref()
            {
                Some("input") => "original-input",
                Some("missing") => "missing",
                _ => &parent,
            };
            let output = if action == "reopen" {
                store.close().await.unwrap();
                store = ChatStore::with_clock(
                    client.clone(),
                    Some("root".into()),
                    Some(fixture.0.clone()),
                    clock.clone(),
                )
                .await
                .unwrap();
                result(Ok(Json::Null))
            } else {
                result(
                    async {
                        match action.as_str() {
                            "save" => {
                                let original = body(field(step, "body"));
                                let before = original.clone();
                                let saved = save_chat_evidence(
                                    &store,
                                    selected_parent,
                                    &text(step, "format"),
                                    &original,
                                    &validate,
                                )
                                .await?;
                                assert_eq!(original, before);
                                let output = saved.json();
                                pins.insert(text(step, "name"), saved);
                                Ok(output)
                            }
                            "read" => {
                                let mut saved = pins.get(&text(step, "name")).unwrap().clone();
                                if step.get("revision").is_some() {
                                    saved.revision = number(step, "revision") as i64;
                                }
                                Ok(summary(
                                    &read_chat_evidence(
                                        &store,
                                        selected_parent,
                                        &saved,
                                        &text(step, "format"),
                                        &validate,
                                    )
                                    .await?,
                                ))
                            }
                            "limits" => {
                                store
                                    .set_limits(Limits {
                                        count: number(step, "count"),
                                        ..Limits::default()
                                    })
                                    .await
                                    .map_err(|e| e.into_ivy())?;
                                Ok(Json::Null)
                            }
                            "remote-error" => {
                                client.0.lock().unwrap().error = Some(error(field(step, "error")));
                                Ok(Json::Null)
                            }
                            "remote-patch" => {
                                let mut state = client.0.lock().unwrap();
                                for (key, patch) in field(step, "patch").as_object().unwrap() {
                                    let mut current = state.remote.get_key(key).unwrap().clone();
                                    for (name, value) in patch.as_object().unwrap() {
                                        current.insert(name.clone(), value.clone());
                                    }
                                    state.remote.insert(key.clone(), current);
                                }
                                Ok(Json::Null)
                            }
                            "patch-manifest" => {
                                let saved = pins.get("evidence").unwrap();
                                let mut value = fixture.manifest(saved);
                                for (key, v) in field(step, "patch").as_object().unwrap() {
                                    value.insert(key.clone(), v.clone());
                                }
                                fixture.patch(saved, &value);
                                Ok(Json::Null)
                            }
                            "patch-chunk" | "reverse-chunks" | "duplicate-chunk" => {
                                let saved = pins.get("evidence").unwrap();
                                let mut value = fixture.manifest(saved);
                                let mut chunks = array(&value, "chunks").to_vec();
                                if action == "reverse-chunks" {
                                    chunks.reverse();
                                } else if action == "duplicate-chunk" {
                                    chunks[1] = chunks[0].clone();
                                } else {
                                    for (key, v) in field(step, "patch").as_object().unwrap() {
                                        if key == &"object".into() {
                                            let mut object = field(&chunks[0], "object").clone();
                                            for (k, v) in v.as_object().unwrap() {
                                                object.insert(k.clone(), v.clone());
                                            }
                                            chunks[0].insert(key.clone(), object);
                                        } else {
                                            chunks[0].insert(key.clone(), v.clone());
                                        }
                                    }
                                }
                                value.insert("chunks", Json::Array(chunks));
                                fixture.patch(saved, &value);
                                Ok(Json::Null)
                            }
                            "blob-edit" => {
                                let saved = fixture.manifest(pins.get("evidence").unwrap());
                                let id =
                                    text(field(&array(&saved, "chunks")[0], "object"), "objectId");
                                let db = fixture.db();
                                match text(step, "mode").as_str() {
                                    "delete" => {
                                        db.execute("DELETE FROM blobs WHERE id=?", [id]).unwrap();
                                    }
                                    "parent" => {
                                        db.execute(
                                            "UPDATE blobs SET parent_id=? WHERE id=?",
                                            params!["foreign", id],
                                        )
                                        .unwrap();
                                    }
                                    _ => {
                                        let mut bytes: Vec<u8> = db
                                            .query_row(
                                                "SELECT bytes FROM blobs WHERE id=?",
                                                [&id],
                                                |r| r.get(0),
                                            )
                                            .unwrap();
                                        bytes[0] ^= 1;
                                        db.execute(
                                            "UPDATE blobs SET bytes=? WHERE id=?",
                                            params![bytes, id],
                                        )
                                        .unwrap();
                                    }
                                }
                                Ok(Json::Null)
                            }
                            "replace-body" => {
                                let encoded = canonical(
                                    &body(&Json::object([] as [(&str, Json); 0])),
                                    usize::MAX,
                                )?;
                                let bytes = match text(step, "mode").as_str() {
                                    "utf8" => vec![255],
                                    "whitespace" => format!(" {encoded}").into_bytes(),
                                    "duplicate" => encoded
                                        .replacen('{', "{\"schemaVersion\":1,", 1)
                                        .into_bytes(),
                                    "json" => b"not JSON".to_vec(),
                                    "shape" => b"{}".to_vec(),
                                    _ => format!("\u{feff}{encoded}").into_bytes(),
                                };
                                let saved = pins.get("evidence").unwrap();
                                let mut value = fixture.manifest(saved);
                                let hash = digest(&bytes);
                                value.insert("byteLength", Json::Number(bytes.len() as f64));
                                value.insert("contentHash", hash.clone().into());
                                let mut chunks = vec![];
                                for (index, part) in bytes.chunks(EVIDENCE_CHUNK_BYTES).enumerate()
                                {
                                    let object = store
                                        .save_local_blob(
                                            "chat-bridge/evidence-chunk",
                                            &format!(
                                                "Replaced evidence {} part {index}",
                                                &hash[7..]
                                            ),
                                            &parent,
                                            part,
                                        )
                                        .await
                                        .map_err(|e| e.into_ivy())?;
                                    chunks.push(Json::object([
                                        ("object", object.json()),
                                        ("contentHash", digest(part).into()),
                                        ("byteLength", Json::Number(part.len() as f64)),
                                        ("mediaType", "application/octet-stream".into()),
                                        (
                                            "label",
                                            format!("Native evidence part {}", index + 1).into(),
                                        ),
                                    ]));
                                }
                                value.insert("chunks", Json::Array(chunks));
                                fixture.patch(saved, &value);
                                Ok(Json::Null)
                            }
                            _ => panic!("Unknown evidence action {action}"),
                        }
                    }
                    .await,
                )
            };
            let draft_calls = drafts.into_inner().unwrap();
            assert_eq!(draft_calls.len(), array(expected, "draftCalls").len());
            let actual = snapshot(
                &fixture,
                &store,
                &client,
                output,
                draft_calls,
                &mut last,
                &mut aliases,
            );
            equal(
                &actual,
                expected,
                &format!("{} step{index}", text(case, "label")),
            );
            compared += 1;
        }
        store.close().await.unwrap();
    }
    assert!(compared > 0);
}
#[tokio::test]
async fn canonical_chunks_scope_integrity_and_recovery_match_original() {
    compare("behavior").await;
}
#[tokio::test]
async fn actual_chunk_and_sixteen_mib_limits_match_original() {
    compare("limits").await;
}
#[tokio::test]
async fn original_sqlite_evidence_is_reconstructed_without_replacement() {
    let corpus = oracle();
    for row in array(corpus, "snapshots") {
        let fixture = Fixture::new(field(row, "database"));
        let client = Arc::new(Client::new());
        let store = ChatStore::with_clock(
            client.clone(),
            Some("root".into()),
            Some(fixture.0.clone()),
            Arc::new(Fixed(number(corpus, "now") as i64)),
        )
        .await
        .unwrap();
        let draft = |_: &Json| -> IvyResult<()> {
            panic!("A read-observation body cannot bypass its real Agent shape check")
        };
        let saved = pin(field(field(row, "initialPins"), "evidence"));
        let value = read_chat_evidence(
            &store,
            &text(field(corpus, "parent"), "objectId"),
            &saved,
            "agent-read-observation/canonical-json",
            &draft,
        )
        .await
        .unwrap();
        equal(
            &summary(&value),
            field(row, "expectedBody"),
            "original database reconstruction",
        );
        assert!(client.0.lock().unwrap().calls.is_empty());
        store.close().await.unwrap();
    }
}
