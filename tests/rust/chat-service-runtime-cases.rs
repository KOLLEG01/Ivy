use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    generation::{ChatGeneration, ChatGenerationConfig, settled},
    notice_input::NoticeInstructions,
    runtime::{ChatHealth, ChatService, ChatTransport, HealthObservation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    json::{canonical, parse, stringify},
};
use ivy_sdk::{clock::Clock, service::ServiceFuture};
use std::{
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command as AsyncCommand},
    sync::watch,
};
#[path = "chat-service-active-cases.rs"]
mod active;
#[path = "chat-wa-integration-cases.rs"]
mod whatsapp;
fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    v.required(key).unwrap()
}
fn text(v: &Json, key: &str) -> String {
    field(v, key).as_str().unwrap()
}
fn flag(v: &Json, key: &str) -> bool {
    v.get(key).and_then(Json::as_bool) == Some(true)
}
fn equal(a: &Json, b: &Json, label: &str) {
    assert_eq!(
        canonical(a, usize::MAX).unwrap(),
        canonical(b, usize::MAX).unwrap(),
        "{label}"
    );
}
fn result(value: IvyResult<()>) -> Json {
    match value {
        Ok(()) => Json::object([("value", Json::Null)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                (
                    "outcome",
                    match e.outcome {
                        ivy_contracts::Outcome::Unknown => "unknown",
                        ivy_contracts::Outcome::Completed => "completed",
                        _ => "not_executed",
                    }
                    .into(),
                ),
            ]),
        )]),
    }
}
struct Fixed(i64, Instant);
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> Instant {
        self.1
    }
}
async fn until(mut predicate: impl FnMut() -> bool, label: &str) {
    for _ in 0..2000 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("Timed out: {label}");
}
fn oracle() -> &'static Json {
    static CORPUS: OnceLock<Json> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let out = Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .args([
                "tests/rust/fixtures/chat-service-runtime-peer.mjs",
                "oracle",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if let Some(path) = std::env::var_os("IVY_CHAT_RUNTIME_ORACLE_CAPTURE") {
            std::fs::write(path, &out.stdout).unwrap();
        }
        parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap()
    })
}
struct Peer {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    url: String,
}
impl Peer {
    async fn start() -> Self {
        Self::fixture("tests/rust/fixtures/chat-service-runtime-peer.mjs").await
    }
    async fn fixture(path: &str) -> Self {
        let mut child = AsyncCommand::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .args([path, "peer"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        let line = tokio::time::timeout(Duration::from_secs(30), output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let url = text(&parse(&line, usize::MAX).unwrap(), "url");
        Self {
            child,
            input,
            output,
            url,
        }
    }
    async fn command(&mut self, value: Json) -> Json {
        let mut bytes = canonical(&value, usize::MAX).unwrap().into_bytes();
        bytes.push(b'\n');
        self.input.write_all(&bytes).await.unwrap();
        self.input.flush().await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(20), self.output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let value = parse(&line, usize::MAX).unwrap();
        assert!(value.get("error").is_none(), "peer: {line}");
        field(&value, "value").clone()
    }
    async fn finish(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
    }
}
struct Probe {
    label: String,
    health: Mutex<Vec<Json>>,
    events: Mutex<Vec<Json>>,
    health_failed: AtomicBool,
    attach_failed: AtomicBool,
    control: AtomicBool,
    closes: AtomicUsize,
    close_gate: Mutex<Option<watch::Receiver<bool>>>,
    settled_gate: Mutex<Option<watch::Receiver<bool>>>,
    ready_gate: Mutex<Option<watch::Receiver<bool>>>,
    control_gate: Mutex<Option<watch::Receiver<bool>>>,
    control_finished: AtomicUsize,
}
async fn gate(mut receiver: Option<watch::Receiver<bool>>) {
    if let Some(r) = &mut receiver {
        while !*r.borrow_and_update() {
            r.changed().await.unwrap();
        }
    }
}
impl Probe {
    fn new(label: String) -> Arc<Self> {
        Arc::new(Self {
            label,
            health: Mutex::new(vec![]),
            events: Mutex::new(vec![]),
            health_failed: AtomicBool::new(false),
            attach_failed: AtomicBool::new(false),
            control: AtomicBool::new(false),
            closes: AtomicUsize::new(0),
            close_gate: Mutex::new(None),
            settled_gate: Mutex::new(None),
            ready_gate: Mutex::new(None),
            control_gate: Mutex::new(None),
            control_finished: AtomicUsize::new(0),
        })
    }
    fn health(self: &Arc<Self>) -> ChatHealth {
        let owner = self.clone();
        let control = self.clone();
        ChatHealth {
            write: Arc::new(move |v: HealthObservation| {
                owner.health.lock().unwrap().push(Json::object([
                    ("ready", v.ready.into()),
                    (
                        "generation",
                        v.generation
                            .map(|g| Json::Number(g as f64))
                            .unwrap_or(Json::Null),
                    ),
                    ("details", Json::String(v.details.clone())),
                ]));
                let fail = (owner.label == "initial-health-failure"
                    || owner.label == "queued-health-failure"
                        && v.details == JsString::from("Hive ChatBridge connection is not ready."))
                    && !owner.health_failed.swap(true, Ordering::SeqCst);
                let held = if v.ready {
                    owner.ready_gate.lock().unwrap().clone()
                } else {
                    None
                };
                Box::pin(async move {
                    gate(held).await;
                    if fail {
                        Err(IvyError::new(
                            "fixture_health_unavailable",
                            "Original health write is unavailable.",
                        ))
                    } else {
                        Ok(())
                    }
                })
            }),
            control: Arc::new(move || {
                control.events.lock().unwrap().push("control".into());
                let held = control.control_gate.lock().unwrap().clone();
                let control = control.clone();
                Box::pin(async move {
                    gate(held).await;
                    control.control_finished.fetch_add(1, Ordering::SeqCst);
                    Ok(control.control.load(Ordering::SeqCst))
                })
            }),
        }
    }
}
impl ChatTransport for Probe {
    fn start(&self) {
        self.events.lock().unwrap().push("start".into());
    }
    fn attach(&self, g: Option<Arc<ChatGeneration>>) -> IvyResult<()> {
        let has_generation = g.is_some();
        self.events.lock().unwrap().push(Json::object([(
            "attach",
            g.map(|g| Json::Number(g.generation() as f64))
                .unwrap_or(Json::Null),
        )]));
        if has_generation
            && self.label == "transport-attach-failure"
            && !self.attach_failed.swap(true, Ordering::SeqCst)
        {
            return Err(IvyError::new(
                "fixture_transport_attach",
                "Original transport attachment failed.",
            )
            .with_outcome(ivy_contracts::Outcome::Unknown));
        }
        Ok(())
    }
    fn changed(&self) {
        self.events.lock().unwrap().push("changed".into());
    }
    fn notification(&self, _: &Json) {
        self.events.lock().unwrap().push("notification".into());
    }
    fn state(&self) -> JsString {
        "fixture-connected".into()
    }
    fn created_main(&self, id: &str) -> IvyResult<()> {
        self.events.lock().unwrap().push(Json::object([
            ("created", format!("listener-new:{id}").into()),
            ("value", true.into()),
        ]));
        Ok(())
    }
    fn settled(&self) -> ServiceFuture<()> {
        self.events.lock().unwrap().push("settled".into());
        let held = self.settled_gate.lock().unwrap().clone();
        Box::pin(async move {
            gate(held).await;
            Ok(())
        })
    }
    fn close(&self) -> ServiceFuture<()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        self.events.lock().unwrap().push("close".into());
        let held = self.close_gate.lock().unwrap().clone();
        let failure = self.label == "transport-close-failure";
        Box::pin(async move {
            gate(held).await;
            if failure {
                Err(IvyError::new(
                    "fixture_transport_close",
                    "Original transport close failed.",
                )
                .with_outcome(ivy_contracts::Outcome::Unknown))
            } else {
                Ok(())
            }
        })
    }
}
fn unique(values: impl IntoIterator<Item = Json>) -> Json {
    let mut values: Vec<_> = values
        .into_iter()
        .map(|v| (canonical(&v, usize::MAX).unwrap(), v))
        .collect();
    values.sort_by(|a, b| a.0.cmp(&b.0));
    values.dedup_by(|a, b| a.0 == b.0);
    Json::Array(values.into_iter().map(|v| v.1).collect())
}
fn rpc_set(peer: &Json) -> Json {
    unique(
        field(peer, "calls")
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v.get("method") != Some(&Json::from("service.heartbeat")))
            .cloned(),
    )
}
fn attached(events: &Json) -> Json {
    Json::Array(
        events
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.get("attach"))
            .filter(|v| !matches!(v, Json::Null))
            .cloned()
            .collect(),
    )
}
fn database(path: &Path) -> Json {
    use rusqlite::types::ValueRef;
    let db = rusqlite::Connection::open(path).unwrap();
    let names: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    Json::Array(
        names
            .into_iter()
            .map(|name| {
                assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
                let mut query = db.prepare(&format!("SELECT * FROM {name}")).unwrap();
                let columns = query.column_count();
                let mut rows: Vec<_> = query
                    .query_map([], |r| {
                        Ok(Json::Array(
                            (0..columns)
                                .map(|i| match r.get_ref(i).unwrap() {
                                    ValueRef::Null => Json::Null,
                                    ValueRef::Integer(v) => {
                                        Json::Array(vec!["integer".into(), v.to_string().into()])
                                    }
                                    ValueRef::Real(v) => {
                                        Json::Array(vec!["real".into(), v.to_string().into()])
                                    }
                                    ValueRef::Text(v) => Json::Array(vec![
                                        "text".into(),
                                        String::from_utf8(v.to_vec()).unwrap().into(),
                                    ]),
                                    ValueRef::Blob(v) => {
                                        Json::Array(vec!["blob".into(), STANDARD.encode(v).into()])
                                    }
                                })
                                .collect(),
                        ))
                    })
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                rows.sort_by_key(|v| canonical(v, usize::MAX).unwrap());
                Json::object([("table", name.into()), ("rows", Json::Array(rows))])
            })
            .collect(),
    )
}
#[tokio::test]
async fn service_start_reconnect_health_control_and_close_match_original_main() {
    let corpus = oracle();
    let root = std::env::temp_dir().join(format!("ivy-rust-chat-service-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    eprintln!("Service runtime fixture: {}", root.display());
    let mut peer = Peer::start().await;
    let mut captures = vec![];
    for (index, row) in field(&corpus, "cases")
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let label = text(row, "label");
        eprintln!("Service lifecycle case: {label}");
        peer.command(Json::object([
            ("action", "begin".into()),
            ("spec", row.clone()),
        ]))
        .await;
        let dir = root.join(index.to_string());
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("chat-bridge.sqlite");
        std::fs::write(&path, STANDARD.decode(text(row, "database")).unwrap()).unwrap();
        let generation = Arc::new(ChatGenerationConfig {
            admission: ChatAdmission::new(field(row, "definition")).unwrap(),
            catalogs: CheckedNativeContracts::new(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
            ),
            instructions: parse(
                &String::from_utf8(STANDARD.decode(text(&corpus, "instructions")).unwrap())
                    .unwrap(),
                usize::MAX,
            )
            .unwrap()
            .into_js_string()
            .unwrap(),
            notice_instructions: NoticeInstructions::from_json(
                &parse(
                    &String::from_utf8(
                        STANDARD
                            .decode(text(&corpus, "noticeInstructions"))
                            .unwrap(),
                    )
                    .unwrap(),
                    usize::MAX,
                )
                .unwrap(),
            )
            .unwrap(),
            service_node_id: "chat-provider".into(),
            data_root: Some(dir.clone()),
            language: "en".into(),
        });
        let mut settings = Json::object([
            ("pollMs", Json::Number(60000.0)),
            ("definition", field(row, "definition").clone()),
        ]);
        if flag(row, "transport") {
            settings.insert("whatsappConfigPath", "synthetic-account-lifecycle".into());
        }
        let config = Json::object([
            ("componentId", "chat-bridge".into()),
            ("credential", "fixture-token".into()),
            ("serviceNodeId", "chat-provider".into()),
            ("hostId", "fixture-host".into()),
            ("version", "1.0.0".into()),
            ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
            ("publicBaseUrl", peer.url.clone().into()),
            ("settings", settings),
        ]);
        let probe = Probe::new(label.clone());
        let ready_release = if label == "held-ready-health" {
            let (release, receive) = watch::channel(false);
            *probe.ready_gate.lock().unwrap() = Some(receive);
            Some(release)
        } else {
            None
        };
        let control_release = if label == "held-control-poll" {
            let (release, receive) = watch::channel(false);
            *probe.control_gate.lock().unwrap() = Some(receive);
            Some(release)
        } else {
            None
        };
        let transport: Option<Arc<dyn ChatTransport>> = if flag(row, "transport") {
            Some(probe.clone())
        } else {
            None
        };
        let started = ChatService::with_clock(
            &config,
            generation,
            probe.health(),
            transport,
            Arc::new(Fixed(
                field(&corpus, "now").as_f64().unwrap() as i64,
                Instant::now(),
            )),
        )
        .await;
        equal(
            &result(started.as_ref().map(|_| ()).map_err(Clone::clone)),
            field(row, "started"),
            &label,
        );
        let mut observed = Json::object([("label", label.clone().into())]);
        if let Ok(app) = started {
            if label == "held-ready-health" {
                until(
                    || {
                        probe
                            .health
                            .lock()
                            .unwrap()
                            .iter()
                            .any(|h| flag(h, "ready"))
                    },
                    "ready health entered",
                )
                .await;
                equal(
                    &Json::Bool(app.service().ready()),
                    field(row, "readyWhileHealthHeld"),
                    "health must precede readiness acknowledgement",
                );
                let closing = tokio::spawn(app.close());
                tokio::time::sleep(Duration::from_millis(30)).await;
                equal(
                    &Json::Bool(!closing.is_finished()),
                    field(row, "closePendingWhileHealthHeld"),
                    "close flushes accepted health work",
                );
                equal(
                    &Json::Array(probe.health.lock().unwrap().clone()),
                    field(row, "healthBeforeRelease"),
                    "later health writes must not overtake blocked ready write",
                );
                ready_release.unwrap().send_replace(true);
                equal(
                    &result(closing.await.unwrap()),
                    field(row, "closed"),
                    "held health close",
                );
            } else {
                // Observe cold contract validation without changing the SDK's
                // dependency deadline or any expected generation/health/RPC.
                let ready = app
                    .service()
                    .wait_ready(Some(ivy_sdk::rpc::RequestOptions {
                        timeout: Duration::from_secs(120),
                        ..Default::default()
                    }))
                    .await;
                if let Err(error) = ready {
                    let report = peer
                        .command(Json::object([("action", "summary".into())]))
                        .await;
                    let failure = Json::object([
                        ("label", label.clone().into()),
                        ("root", dir.to_string_lossy().as_ref().into()),
                        ("error", result(Err(error.clone()))),
                        ("peer", report),
                        ("health", Json::Array(probe.health.lock().unwrap().clone())),
                        ("events", Json::Array(probe.events.lock().unwrap().clone())),
                    ]);
                    if let Some(path) =
                        std::env::var_os("IVY_CHAT_SERVICE_READINESS_FAILURE_CAPTURE")
                    {
                        std::fs::write(path, canonical(&failure, usize::MAX).unwrap()).unwrap();
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(30), app.close()).await;
                    panic!(
                        "Service readiness failed: {}",
                        canonical(&failure, usize::MAX).unwrap()
                    );
                }
                let mut reconciled = false;
                for _ in 0..2000 {
                    let snapshot = peer
                        .command(Json::object([("action", "summary".into())]))
                        .await;
                    let calls = field(&snapshot, "calls").as_array().unwrap();
                    let diagnostic = calls
                        .iter()
                        .any(|c| c.get("method") == Some(&Json::from("system.diagnostics")));
                    let settings = calls
                        .iter()
                        .any(|c| c.get("method") == Some(&Json::from("settings.get")));
                    let notice = field(&snapshot, "heartbeats")
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|h| !field(h, "diagnostics").as_array().unwrap().is_empty());
                    if diagnostic
                        && if label == "diagnostic-read-failure" {
                            notice
                        } else {
                            settings
                        }
                    {
                        reconciled = true;
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                assert!(
                    reconciled,
                    "diagnostic work must finish before observing lifecycle"
                );
                if label == "reconnect-held-retirement" {
                    let old = app.runtime().unwrap();
                    let (release, receive) = watch::channel(false);
                    *probe.settled_gate.lock().unwrap() = Some(receive);
                    peer.command(Json::object([("action", "disconnect".into())]))
                        .await;
                    until(
                        || {
                            app.service()
                                .connection()
                                .is_ok_and(|c| c.generation() == 8)
                        },
                        "reconnected generation",
                    )
                    .await;
                    let lease = old.accept().unwrap();
                    let readable = lease
                        .store()
                        .named("chat-bridge/main", "retiring-runtime")
                        .await
                        .map(|_| ())
                        .map_err(|e| e.into_ivy());
                    equal(
                        &result(readable),
                        field(row, "oldReadable"),
                        "transport settling retains old store",
                    );
                    drop(lease);
                    release.send_replace(true);
                    until(|| old.accept().is_err(), "old admission retired").await;
                    settled(old.retire()).await.unwrap();
                }
                let number = app.service().connection().unwrap().generation();
                if label == "held-idempotent-close" {
                    let (release, receive) = watch::channel(false);
                    *probe.close_gate.lock().unwrap() = Some(receive);
                    drop(app.close()); // Dropping the first observer must not cancel accepted cleanup.
                    until(
                        || probe.closes.load(Ordering::SeqCst) == 1,
                        "transport close entered",
                    )
                    .await;
                    equal(
                        &Json::Bool(app.service().ready()),
                        field(row, "readyWhileClosing"),
                        "SDK remains available until transport close settles",
                    );
                    let invoked = peer
                        .command(Json::object([
                            ("action", "invoke".into()),
                            ("generation", Json::Number(number as f64)),
                        ]))
                        .await;
                    compare_invocation(&invoked, field(row, "invoked"));
                    release.send_replace(true);
                    equal(
                        &result(app.close().await),
                        field(row, "closed"),
                        "held close",
                    );
                    equal(
                        &result(app.close().await),
                        field(row, "closedAgain"),
                        "idempotent close",
                    );
                } else if label == "held-control-poll" {
                    until(
                        || {
                            probe
                                .events
                                .lock()
                                .unwrap()
                                .iter()
                                .any(|e| e == &Json::from("control"))
                        },
                        "control read entered",
                    )
                    .await;
                    tokio::time::sleep(Duration::from_millis(1100)).await;
                    let polls = probe
                        .events
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|e| e == &&Json::from("control"))
                        .count();
                    equal(
                        &Json::Number(polls as f64),
                        field(row, "pollsWhileHeld"),
                        "control ticks cannot overlap an accepted read",
                    );
                    probe.control.store(true, Ordering::SeqCst);
                    equal(
                        &result(app.close().await),
                        field(row, "closed"),
                        "close does not wait for control poll",
                    );
                    equal(
                        &Json::Bool(probe.control_finished.load(Ordering::SeqCst) == 0),
                        field(row, "controlPendingAfterClose"),
                        "stop preserves accepted control poll",
                    );
                    control_release.unwrap().send_replace(true);
                    until(
                        || probe.control_finished.load(Ordering::SeqCst) == 1,
                        "accepted control read settled",
                    )
                    .await;
                } else if label == "control-close" {
                    probe.control.store(true, Ordering::SeqCst);
                    until(|| probe.closes.load(Ordering::SeqCst) == 1, "control close").await;
                    equal(
                        &result(app.close().await),
                        field(row, "closed"),
                        "control close result",
                    );
                } else {
                    let invoked = peer
                        .command(Json::object([
                            ("action", "invoke".into()),
                            ("generation", Json::Number(number as f64)),
                        ]))
                        .await;
                    compare_invocation(&invoked, field(row, "invoked"));
                    equal(
                        &result(app.close().await),
                        field(row, "closed"),
                        "close outcome",
                    );
                }
            }
            assert!(!app.service().ready(), "closed SDK is not ready");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        let actual = peer
            .command(Json::object([("action", "summary".into())]))
            .await;
        equal(
            field(&actual, "mismatches"),
            &Json::Array(vec![]),
            "caught domain errors must not hide unexpected RPCs",
        );
        equal(
            field(&actual, "handshakes"),
            field(field(row, "peer"), "handshakes"),
            "identity, complete registry and notification filters",
        );
        equal(
            &rpc_set(&actual),
            &rpc_set(field(row, "peer")),
            "exact concurrent pure RPC request/response sets",
        );
        equal(
            &unique(
                field(&actual, "heartbeats")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned(),
            ),
            &unique(
                field(field(row, "peer"), "heartbeats")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned(),
            ),
            "readiness diagnostic observations",
        );
        let health = Json::Array(probe.health.lock().unwrap().clone());
        equal(
            &health,
            field(row, "health"),
            "ordered serialized health outcomes",
        );
        equal(
            &attached(&Json::Array(probe.events.lock().unwrap().clone())),
            &attached(field(row, "events")),
            "generation attachments",
        );
        assert_eq!(
            probe.closes.load(Ordering::SeqCst),
            field(row, "closeCount").as_f64().unwrap() as usize,
            "one transport close"
        );
        if label == "initial-health-failure" {
            assert!(field(&actual, "calls").as_array().unwrap().is_empty());
        }
        if label == "transport-close-failure" {
            assert!(
                !probe
                    .health
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|v| v.get("details") == Some(&Json::from("ChatBridge stopped."))),
                "preserve original close-error health behavior"
            );
        }
        let expected = dir.join("original.sqlite");
        std::fs::write(
            &expected,
            STANDARD.decode(text(row, "retainedDatabase")).unwrap(),
        )
        .unwrap();
        equal(
            &database(&path),
            &database(&expected),
            "all SQLite table rows",
        );
        observed.insert("peer", actual);
        observed.insert("health", health);
        observed.insert("events", Json::Array(probe.events.lock().unwrap().clone()));
        observed.insert(
            "database",
            STANDARD.encode(std::fs::read(&path).unwrap()).into(),
        );
        captures.push(observed);
        if let Some(path) = std::env::var_os("IVY_CHAT_RUNTIME_ACTUAL_CAPTURE") {
            std::fs::write(
                path,
                stringify(&Json::Array(captures.clone()), usize::MAX).unwrap(),
            )
            .unwrap();
        }
    }
    peer.finish().await;
    // This exact directory was created above and owns only this synthetic corpus.
    std::fs::remove_dir_all(&root).unwrap();
    eprintln!(
        "Qualified {} service lifecycle cases against unchanged original main.ts; four catalogs, actual SDK sockets/provider calls and exact SQLite rows. Host/account/installed/platform acceptance excluded.",
        captures.len()
    );
}
fn compare_invocation(actual: &Json, expected: &Json) {
    let mut actual = actual.clone();
    let mut expected = expected.clone();
    actual.remove("id").unwrap();
    expected.remove("id").unwrap();
    equal(&actual, &expected, "actual provider dispatch result");
}
