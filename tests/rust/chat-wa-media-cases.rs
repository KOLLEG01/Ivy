use ivy_chat_bridge::whatsapp::{
    journal::Inbox,
    journal_io::JournalIo,
    journal_owner::Journal,
    media::{
        MediaDependencies, MediaFuture, MediaMain, MediaStream, TranscriptionRequest,
        TranscriptionResponse, WhatsAppMedia,
    },
    messages::Translator,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, Outcome,
    hash::digest,
    json::{parse, stringify},
};
use ivy_sdk::{clock::Clock, rpc::RequestFuture};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant, UNIX_EPOCH},
};

// Compile the canonical Host module at the required filesystem boundary.
#[allow(dead_code)]
#[path = "../../packages/host-runtime/rust/src/node_path.rs"]
mod host_node_path;

fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    v.required(key).unwrap()
}
fn text(v: &Json, key: &str) -> String {
    field(v, key).as_str().unwrap()
}
fn hash(bytes: &[u8]) -> Json {
    digest(bytes)[7..].into()
}
fn bytes(v: &Json) -> Vec<u8> {
    if let Some(values) = v.as_array() {
        values.iter().map(|n| n.as_f64().unwrap() as u8).collect()
    } else {
        vec![
            v.get("byte").and_then(Json::as_f64).unwrap_or(0.0) as u8;
            field(v, "length").as_f64().unwrap() as usize
        ]
    }
}
fn captured(value: IvyResult<Json>) -> Json {
    match value {
        Ok(v) => Json::object([("value", v)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", e.code.into()),
                ("message", e.message.into()),
                ("outcome", e.outcome.as_str().into()),
            ]),
        )]),
    }
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("ivy-rust-wa-media-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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
struct Probe {
    spec: Json,
    calls: Mutex<Vec<Json>>,
    seen: Mutex<HashMap<String, usize>>,
    directory: PathBuf,
}
impl Probe {
    fn call(&self, args: Vec<Json>) {
        self.calls.lock().unwrap().push(Json::Array(args));
    }
    fn fail(&self, key: &str) -> IvyResult<()> {
        let mut seen = self.seen.lock().unwrap();
        let count = seen.entry(key.into()).or_default();
        let first = *count == 0;
        *count += 1;
        if self.spec.get("fail") == Some(&key.into())
            || (first && self.spec.get("failOnce") == Some(&key.into()))
        {
            let outcome = if self.spec.get("outcome") == Some(&"unknown".into()) {
                Outcome::Unknown
            } else {
                Outcome::NotExecuted
            };
            Err(
                IvyError::new(format!("fixture_{key}"), format!("Original {key}"))
                    .with_outcome(outcome),
            )
        } else {
            Ok(())
        }
    }
}
struct Stream {
    probe: Arc<Probe>,
    parts: Vec<Json>,
    at: usize,
}
impl MediaStream for Stream {
    fn next(&mut self) -> MediaFuture<'_, Option<Vec<u8>>> {
        Box::pin(async {
            let Some(part) = self.parts.get(self.at) else {
                return Ok(None);
            };
            self.probe.fail("next")?;
            self.at += 1;
            Ok(Some(bytes(part)))
        })
    }
    fn destroy(&mut self) {
        self.probe.call(vec!["destroy".into()]);
    }
}
struct Dependencies(Arc<Probe>);
impl MediaDependencies for Dependencies {
    fn filesystem_path(&self, logical: &ivy_contracts::JsString) -> IvyResult<PathBuf> {
        host_node_path::node_filesystem_path(logical).map_err(|_| IvyError::internal())
    }
    fn download<'a>(
        &'a self,
        media: &'a Json,
        kind: &'static str,
        timeout_ms: u64,
    ) -> MediaFuture<'a, Box<dyn MediaStream>> {
        Box::pin(async move {
            let mut args = vec![
                "download".into(),
                kind.into(),
                false.into(),
                Json::Number(timeout_ms as f64),
            ];
            args.extend(
                ["url", "directPath", "thumbnailDirectPath"]
                    .iter()
                    .map(|k| media.get(k).cloned().unwrap_or(Json::Null)),
            );
            self.0.call(args);
            self.0.fail("download")?;
            if self.0.spec.get("collision") == Some(&true.into()) {
                fs::write(self.0.directory.join("original"), [9]).unwrap();
            }
            let parts = self
                .0
                .spec
                .get("chunks")
                .and_then(Json::as_array)
                .map(|v| v.to_vec())
                .unwrap_or_else(|| {
                    vec![
                        Json::Array(vec![Json::Number(1.0), Json::Number(2.0)]),
                        Json::Array(vec![Json::Number(3.0)]),
                    ]
                });
            Ok(Box::new(Stream {
                probe: self.0.clone(),
                parts,
                at: 0,
            }) as Box<dyn MediaStream>)
        })
    }
    fn voice_wav<'a>(&'a self, input: &'a [u8], _: Translator) -> MediaFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.0.call(vec![
                "voice".into(),
                Json::Number(input.len() as f64),
                hash(input),
            ]);
            self.0.fail("voice")?;
            Ok(vec![
                7;
                self.0
                    .spec
                    .get("wavLength")
                    .and_then(Json::as_f64)
                    .unwrap_or(48.0) as usize
            ])
        })
    }
    fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> MediaFuture<'_, Box<dyn TranscriptionResponse>> {
        Box::pin(async move {
            let mut keys = vec!["file".into(), "model".into(), "response_format".into()];
            if request.language.is_some() {
                keys.push("language".into());
            }
            self.0.call(vec![
                "fetch".into(),
                request.url.into(),
                "POST".into(),
                Json::String(request.authorization),
                if request.redirect_error {
                    "error".into()
                } else {
                    "follow".into()
                },
                Json::Number(request.timeout_ms as f64),
                Json::Array(keys),
                Json::String(request.model),
                "json".into(),
                request.language.map(Json::String).unwrap_or(Json::Null),
                "audio.wav".into(),
                "audio/wav".into(),
                hash(&request.wav),
            ]);
            self.0.fail("fetch")?;
            Ok(Box::new(Response(self.0.clone())) as Box<dyn TranscriptionResponse>)
        })
    }
}
struct Response(Arc<Probe>);
impl TranscriptionResponse for Response {
    fn ok(&self) -> bool {
        self.0.spec.get("ok") != Some(&false.into())
    }
    fn json(&mut self) -> RequestFuture<'_> {
        Box::pin(async {
            self.0.call(vec!["json".into()]);
            self.0.fail("json")?;
            Ok(self
                .0
                .spec
                .get("response")
                .cloned()
                .unwrap_or_else(|| Json::object([("text", "  Spoken words  ".into())])))
        })
    }
}
impl MediaMain for Probe {
    fn main(&self) -> RequestFuture<'_> {
        Box::pin(async {
            self.call(vec!["main".into()]);
            self.fail("main")?;
            Ok(Json::object([(
                "conversation",
                Json::object([("objectId", "conversation".into())]),
            )]))
        })
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            self.call(vec!["operationId".into(), scope]);
            self.fail("operationId")?;
            Ok("original-image-operation".into())
        })
    }
    fn root(&self) -> Json {
        "root".into()
    }
    fn write(&self, request: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            self.call(vec!["objects.write".into(), request]);
            self.fail("write")?;
            Ok(Json::object([
                ("object", Json::object([("id", "image".into())])),
                ("revision", Json::object([("revision", Json::Number(7.0))])),
            ]))
        })
    }
}
fn snapshot(journal: &Journal) -> Json {
    journal
        .access(|j| {
            let mut tables = Vec::new();
            for table in [
                "kv",
                "inbox",
                "outgoing",
                "retired_inbox",
                "retired_outgoing",
            ] {
                let mut query = j
                    .connection()
                    .prepare(&format!(
                        "SELECT * FROM {table} ORDER BY {}",
                        if table == "kv" { "key" } else { "id" }
                    ))
                    .unwrap();
                let count = query.column_count();
                let rows = query
                    .query_map([], |r| {
                        Ok(Json::Array(
                            (0..count)
                                .map(|i| match r.get_ref(i).unwrap() {
                                    rusqlite::types::ValueRef::Null => Json::Null,
                                    rusqlite::types::ValueRef::Integer(n) => Json::Number(n as f64),
                                    rusqlite::types::ValueRef::Real(n) => Json::Number(n),
                                    rusqlite::types::ValueRef::Text(s) => {
                                        std::str::from_utf8(s).unwrap().into()
                                    }
                                    _ => panic!("Unexpected BLOB"),
                                })
                                .collect(),
                        ))
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                tables.push((table, Json::Array(rows)));
            }
            Ok(Json::object(tables))
        })
        .unwrap()
}
fn normalize(value: Json, root: &Path) -> Json {
    match &value {
        Json::String(s) => {
            let root = root.to_string_lossy();
            let s = String::from_utf16(&s.0).unwrap();
            s.replace(&root.replace('\\', "\\\\"), "<root>")
                .replace(root.as_ref(), "<root>")
                .into()
        }
        Json::Array(a) => Json::Array(a.iter().map(|v| normalize(v.clone(), root)).collect()),
        Json::Object(a) => Json::Object(
            a.iter()
                .map(|(k, v)| (k.clone(), normalize(v.clone(), root)))
                .collect(),
        ),
        v => v.clone(),
    }
}
fn compare(actual: &Json, expected: &Json, label: &str, part: &str) {
    if actual != expected {
        if let Ok(path) = std::env::var("IVY_CHAT_WA_MEDIA_FAILURE_CAPTURE") {
            fs::write(
                path,
                stringify(
                    &Json::object([
                        ("label", label.into()),
                        ("part", part.into()),
                        ("actual", actual.clone()),
                        ("expected", expected.clone()),
                    ]),
                    usize::MAX,
                )
                .unwrap(),
            )
            .unwrap();
        }
        panic!("Original media case {label}: {part} differs (see scoped capture)");
    }
}

#[tokio::test]
async fn original_media_payloads_files_failures_and_caches_match() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .current_dir(&root)
        .arg("tests/rust/fixtures/chat-whatsapp-media-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Original media oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(1500)
            .collect::<String>()
    );
    let corpus = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let rows = field(&corpus, "rows").as_array().unwrap();
    let temp = Temp::new();
    let mut completed = Vec::new();
    for (index, oracle) in rows.iter().enumerate() {
        let spec = field(oracle, "spec");
        let label = text(spec, "label");
        let directory = temp.0.join(index.to_string());
        let id = "a".repeat(64);
        let media_dir = directory.join("whatsapp-media").join(&id);
        fs::create_dir(&directory).unwrap();
        let journal = Journal::new(JournalIo::open(":memory:").unwrap());
        if let Some(v) = spec.get("saved") {
            journal.set(&format!("payload:{id}"), v).unwrap();
        }
        if let Some(v) = spec.get("transcript") {
            journal.set(&format!("transcript:{id}"), v).unwrap();
        }
        if spec.get("original").is_some() || spec.get("documentFile").is_some() {
            fs::create_dir_all(&media_dir).unwrap();
            if let Some(v) = spec.get("original") {
                fs::write(media_dir.join("original"), bytes(v)).unwrap();
            }
            if let Some(v) = spec.get("documentFile") {
                fs::write(media_dir.join("file-notes.txt"), bytes(v)).unwrap();
            }
        }
        let probe = Arc::new(Probe {
            spec: spec.clone(),
            calls: Mutex::new(vec![]),
            seen: Mutex::new(HashMap::new()),
            directory: media_dir.clone(),
        });
        let language = spec.get("language").and_then(Json::as_js_string);
        let media = WhatsAppMedia::new(
            journal.clone(),
            spec.get("config")
                .cloned()
                .unwrap_or_else(|| Json::object([] as [(&str, Json); 0])),
            directory.clone(),
            Arc::new(Dependencies(probe.clone())),
            Translator::new(language),
        );
        let row = Inbox {
            id: id.clone(),
            sender: "sender".into(),
            jid: "fixture@invalid".into(),
            message: text(field(oracle, "row"), "message"),
            state: "pending".into(),
            request: None,
            received: 1,
        };
        let texts = field(oracle, "texts");
        for (key, result) in [
            ("command", media.command_text(&row)),
            ("message", media.message_text(&row)),
        ] {
            if let Some(value) = texts
                .get("value")
                .and_then(|v| v.get(key))
                .and_then(Json::as_js_string)
            {
                compare(
                    &Json::String(result.unwrap()),
                    &Json::String(value.clone()),
                    &label,
                    key,
                );
            } else {
                assert!(result.is_err(), "Non-string or malformed {key}: {label}");
            }
        }
        let mut outcomes = Vec::new();
        for _ in 0..spec.get("repeats").and_then(Json::as_f64).unwrap_or(1.0) as usize {
            outcomes.push(captured(
                media
                    .payload(
                        &row,
                        probe.as_ref(),
                        spec.get("sameHost") != Some(&false.into()),
                    )
                    .await,
            ));
        }
        let mut files = Vec::new();
        if media_dir.exists() {
            let mut paths = fs::read_dir(&media_dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect::<Vec<_>>();
            paths.sort();
            for path in paths {
                let logical = ivy_contracts::JsString::from(path.to_string_lossy().as_ref());
                let bytes = fs::read(
                    Dependencies(probe.clone())
                        .filesystem_path(&logical)
                        .unwrap(),
                )
                .unwrap();
                files.push(Json::Array(vec![
                    path.file_name().unwrap().to_string_lossy().as_ref().into(),
                    Json::Number(bytes.len() as f64),
                    hash(&bytes),
                ]));
            }
        }
        let actual = Json::object([
            ("outcomes", Json::Array(outcomes)),
            ("calls", Json::Array(probe.calls.lock().unwrap().clone())),
            ("files", Json::Array(files)),
            ("snapshot", normalize(snapshot(&journal), &directory)),
        ]);
        completed.push(Json::object([
            ("label", label.as_str().into()),
            ("actual", actual.clone()),
        ]));
        if let Ok(path) = std::env::var("IVY_CHAT_WA_MEDIA_ACTUAL_CAPTURE") {
            fs::write(
                path,
                stringify(&Json::Array(completed.clone()), usize::MAX).unwrap(),
            )
            .unwrap();
        }
        for part in ["outcomes", "calls", "files", "snapshot"] {
            compare(field(&actual, part), field(oracle, part), &label, part);
        }
        journal.close().await.unwrap();
    }
    let r = field(&corpus, "retention");
    let directory = temp.0.join("retention");
    let names = field(r, "names").as_array().unwrap();
    let now = field(r, "now").as_f64().unwrap() as i64;
    let journal = Journal::new(JournalIo::open(":memory:").unwrap());
    for (i, name) in names.iter().enumerate() {
        let name = name.as_str().unwrap();
        let path = directory.join("whatsapp-media").join(&name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("original"), [i as u8]).unwrap();
        let millis = now - (if i == 2 { 6 } else { 8 }) * 86400000;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.access_mode(0x100).custom_flags(0x02000000);
        }
        options
            .open(&path)
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + Duration::from_millis(millis as u64)),
            )
            .unwrap();
        journal
            .set(
                &format!("local-input:{name}"),
                &Json::Array(vec![Json::object([("type", "image".into())])]),
            )
            .unwrap();
        journal
            .set(
                &format!("payload:{name}"),
                &Json::object([
                    ("text", "Retained payload".into()),
                    ("images", Json::Array(vec![])),
                ]),
            )
            .unwrap();
    }
    for (key, done) in [("pending", false), ("complete", true)] {
        for id in field(r, key).as_array().unwrap() {
            let id = id.as_str().unwrap();
            journal
                .access(|j| {
                    j.accept(&id, "sender", "fixture@invalid", "{}", 1)?;
                    if done {
                        j.finish(&id)?;
                    }
                    Ok(())
                })
                .unwrap();
        }
    }
    let probe = Arc::new(Probe {
        spec: Json::Null,
        calls: Mutex::new(vec![]),
        seen: Mutex::new(HashMap::new()),
        directory: directory.clone(),
    });
    let media = WhatsAppMedia::with_clock(
        journal.clone(),
        Json::Null,
        directory.clone(),
        Arc::new(Dependencies(probe)),
        Translator::new(None),
        Arc::new(Fixed(now)),
    );
    let result = captured(media.prune().await.map(|_| Json::Null));
    let mut remaining = fs::read_dir(directory.join("whatsapp-media"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().as_ref().into())
        .collect::<Vec<Json>>();
    remaining.sort_by_key(|v| v.as_str().unwrap());
    let kv = field(&snapshot(&journal), "kv").clone();
    completed.push(Json::object([
        ("label", "retention".into()),
        (
            "actual",
            Json::object([
                ("outcome", result.clone()),
                ("remaining", Json::Array(remaining.clone())),
                ("kv", kv.clone()),
            ]),
        ),
    ]));
    if let Ok(path) = std::env::var("IVY_CHAT_WA_MEDIA_ACTUAL_CAPTURE") {
        fs::write(
            path,
            stringify(&Json::Array(completed), usize::MAX).unwrap(),
        )
        .unwrap();
    }
    compare(&result, field(r, "outcome"), "retention", "outcome");
    compare(
        &Json::Array(remaining),
        field(r, "remaining"),
        "retention",
        "remaining",
    );
    compare(&kv, field(r, "kv"), "retention", "kv");
    journal.close().await.unwrap();
    assert!(rows.len() >= 120);
    eprintln!(
        "whatsapp_media_original_scenarios={} retention=1",
        rows.len()
    );
}
