use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    admission::ChatAdmission,
    diagnostic_notifications::{DiagnosticMain, DiagnosticNotifications, MainFuture},
    operations::ChatOperations,
    store::{ChatStore, Document, Pin, local::Limits},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse},
    operation::parse_operation_id,
};
use ivy_sdk::{
    clock::Clock,
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::{ConnectionOptions, ServiceConnection},
};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command as AsyncCommand},
};

fn text(v: &Json, k: &str) -> String {
    v.required(k).unwrap().as_str().unwrap()
}
fn array<'a>(v: &'a Json, k: &str) -> &'a [Json] {
    v.required(k).unwrap().as_array().unwrap()
}
fn flag(v: &Json, k: &str) -> bool {
    v.get(k) == Some(&Json::Bool(true))
}
fn cancelled() -> IvyError {
    IvyError::new(
        "fixture_cancelled",
        "The original monitor signal was aborted.",
    )
    .with_outcome(Outcome::Unknown)
}
fn result(r: IvyResult<()>) -> Json {
    match r {
        Ok(()) => Json::object([("value", Json::Null)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.as_str().into()),
                ("details", e.details.unwrap_or(Json::Null)),
            ]),
        )]),
    }
}
struct Time(i64);
impl Clock for Time {
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
        let path =
            std::env::temp_dir().join(format!("ivy-chat-diagnostic-tick-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("chat-bridge.sqlite"),
            STANDARD.decode(database.as_str().unwrap()).unwrap(),
        )
        .unwrap();
        Self(path)
    }
    fn snapshots(&self, last: &mut i64) -> Vec<Json> {
        let db = rusqlite::Connection::open(self.0.join("chat-bridge.sqlite")).unwrap();
        let mut q = db
            .prepare(
                "SELECT rowid,value_json FROM diagnostic_test_writes WHERE rowid>? ORDER BY rowid",
            )
            .unwrap();
        q.query_map([*last], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|r| {
            let (id, value) = r.unwrap();
            *last = id;
            parse(&value, usize::MAX).unwrap()
        })
        .collect()
    }
    fn operation_count(&self) -> i64 {
        let db = rusqlite::Connection::open(self.0.join("chat-bridge.sqlite")).unwrap();
        db.query_row(
            "SELECT COUNT(*) FROM documents WHERE key='chat-bridge/operation'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Control {
    step: Json,
    calls: Vec<Json>,
    page: usize,
    options: RequestOptions,
}
struct Client {
    control: Mutex<Control>,
    connection: Option<ServiceConnection>,
    close_reason: Option<IvyError>,
}
impl Client {
    fn log(&self, method: &str, params: Json) {
        self.control.lock().unwrap().calls.push(Json::object([
            ("method", method.into()),
            ("params", params),
        ]));
    }
    fn step(&self) -> Json {
        self.control.lock().unwrap().step.clone()
    }
    fn cancel(&self, state: &Control) {
        if let Some(connection) = &self.connection {
            connection.close_with(self.close_reason.clone().unwrap());
            connection.close_with(IvyError::new(
                "later",
                "Do not replace the original reason.",
            ));
        } else {
            state.options.cancellation.cancel();
        }
    }
    fn abort(&self, at: &str) {
        let state = self.control.lock().unwrap();
        if state.step.get("abort") == Some(&Json::from(at)) {
            self.cancel(&state);
        }
    }
    fn execute(&self, method: &str, params: Json, options: RequestOptions) -> IvyResult<Json> {
        self.log(method, params);
        if options.cancellation.is_cancelled() {
            return Err(options.cancel_error);
        }
        let mut state = self.control.lock().unwrap();
        let step = state.step.clone();
        match method {
            "system.status" => {
                if step.get("failure") == Some(&Json::from("status")) {
                    return Err(IvyError::new(
                        "fixture_unavailable",
                        "Operation identity read failed.",
                    ));
                }
                Ok(Json::object([("runtimeEpoch", "chat".into())]))
            }
            "settings.get" => {
                if step.get("failure") == Some(&Json::from("settings")) {
                    return Err(IvyError::new(
                        "fixture_unavailable",
                        "Settings read failed.",
                    ));
                }
                if step.get("abort") == Some(&Json::from("settings")) {
                    self.cancel(&state);
                }
                Ok(Json::object([(
                    "settings",
                    Json::object([
                        ("schemaVersion", Json::Number(1.0)),
                        (
                            "quietHours",
                            step.get("quiet").cloned().unwrap_or(Json::Null),
                        ),
                    ]),
                )]))
            }
            "system.diagnostics" => {
                let index = state.page;
                state.page += 1;
                if step.get("failure")
                    == Some(&Json::from(if index == 0 {
                        "first-page"
                    } else {
                        "second-page"
                    }))
                {
                    return Err(IvyError::new(
                        "fixture_unavailable",
                        "Diagnostic page failed.",
                    ));
                }
                if step.get("abort") == Some(&Json::from(format!("page-{index}"))) {
                    self.cancel(&state);
                }
                Ok(array(&step, "pages")[index].clone())
            }
            _ => panic!("Unexpected RPC {method}"),
        }
    }
}
impl RpcClient for Client {
    fn service_connection(&self) -> Option<&ServiceConnection> {
        self.connection.as_ref()
    }
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move { self.execute(method, params, options) })
    }
}
// Reuse the owner's loopback provider fixture. Diagnostic reads stay synthetic;
// the cancellation token and first-close-reason owner are real SDK connections.
async fn connection() -> (Child, ServiceConnection) {
    let mut child = AsyncCommand::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/sdk-provider-peer.mjs",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let info = parse(&first, usize::MAX).unwrap();
    let connection = ServiceConnection::connect(
        &text(&info, "url"),
        "fixture-token",
        ConnectionOptions::new("fixture-node".into()),
        RequestOptions::default().cancellation,
    )
    .await
    .unwrap();
    (child, connection)
}
struct Main<'a> {
    ops: &'a ChatOperations<'a>,
    client: &'a Client,
    notice_state: &'a Json,
}
impl DiagnosticMain for Main<'_> {
    fn find(&self) -> MainFuture<'_, Option<Document>> {
        Box::pin(async move {
            self.client
                .log("main.find", Json::object([] as [(&str, Json); 0]));
            self.client.abort("find");
            if flag(&self.client.step(), "mainMissing") {
                return Ok(None);
            }
            Ok(Some(Document {
                key: "chat-bridge/main".into(),
                pin: Pin {
                    object_id: "main".into(),
                    revision: 1,
                },
                value: Json::object([(
                    "conversation",
                    Json::object([
                        ("objectId", "conversation".into()),
                        ("revision", Json::Number(1.0)),
                    ]),
                )]),
                metadata: Json::Null,
                revision: Json::Null,
            }))
        })
    }
    fn notify<'a>(&'a self, caller: &'a JsString, request: &'a Json) -> MainFuture<'a, Json> {
        Box::pin(async move {
            self.client.log(
                "main.notify",
                Json::object([
                    ("caller", Json::String(caller.clone())),
                    ("request", request.clone()),
                ]),
            );
            self.client.abort("notify");
            let step = self.client.step();
            if step.get("notifyError") == Some(&Json::from("before")) {
                return Err(IvyError::new(
                    "fixture_unavailable",
                    "Notice admission failed.",
                ));
            }
            let mut admitted = request.clone();
            admitted.insert("expectedBridge", self.ops.admission.expected(caller));
            admitted.insert(
                "channel",
                array(self.ops.admission.definition(), "channels")[0]
                    .required("channel")?
                    .clone(),
            );
            let op = self.ops.begin(caller, &admitted).await?;
            if step.get("notifyError") == Some(&Json::from("after")) {
                return Err(IvyError::new("outcome_unknown", "Notice reply was lost.")
                    .with_outcome(Outcome::Unknown));
            }
            if step.get("notifyError") == Some(&Json::from("returned-failed")) {
                Ok(Json::object([("phase", "failed".into())]))
            } else {
                Ok(op.value)
            }
        })
    }
    fn notice<'a>(&'a self, caller: &'a JsString, query: &'a Json) -> MainFuture<'a, Json> {
        Box::pin(async move {
            self.client.log(
                "main.notice",
                Json::object([
                    ("caller", Json::String(caller.clone())),
                    ("query", query.clone()),
                ]),
            );
            if flag(&self.client.step(), "noticeError") {
                return Err(IvyError::new(
                    "publication_pending",
                    "Original reply is not published yet.",
                ));
            }
            Ok(Json::object([(
                "data",
                Json::object([("state", self.notice_state.clone())]),
            )]))
        })
    }
    fn ready(&self) -> MainFuture<'_, ()> {
        Box::pin(async move {
            self.client
                .log("main.ready", Json::object([] as [(&str, Json); 0]));
            if flag(&self.client.step(), "readyError") {
                Err(IvyError::new(
                    "fixture_not_ready",
                    "Native owner is not ready.",
                ))
            } else {
                Ok(())
            }
        })
    }
}
// Only newly allocated UUID/timestamp identities vary. Preserve equality,
// distinctness, seed IDs, every receipt-hash relationship and the full write order.
fn normalize(value: &Json) -> Json {
    fn collect(v: &Json, ids: &mut Vec<JsString>) {
        match v {
            Json::String(s) => {
                if let Ok(text) = s.to_utf8() {
                    if let Ok(id) = parse_operation_id(&text) {
                        if id.runtime_epoch == "chat"
                            && !id.nonce.starts_with("seed-")
                            && !ids.contains(s)
                        {
                            uuid::Uuid::parse_str(&id.nonce)
                                .expect("Generated operation ID needs its original UUID nonce");
                            ids.push(s.clone());
                        }
                    }
                }
            }
            Json::Array(a) => {
                for v in a {
                    collect(v, ids);
                }
            }
            Json::Object(a) => {
                for (_, v) in a {
                    collect(v, ids);
                }
            }
            _ => {}
        }
    }
    fn replace(v: &Json, ids: &[JsString]) -> Json {
        match v {
            Json::String(s) => ids
                .iter()
                .position(|v| v == s)
                .map(|i| Json::from(format!("<generated:{}>", i + 1)))
                .unwrap_or_else(|| v.clone()),
            Json::Array(a) => Json::Array(a.iter().map(|v| replace(v, ids)).collect()),
            Json::Object(a) => Json::object(a.iter().map(|(k, v)| {
                let key = ids
                    .iter()
                    .enumerate()
                    .find(|(_, id)| {
                        *k == JsString::from(format!(
                            "receipt:{}",
                            hash_json(&Json::String((*id).clone())).unwrap()
                        ))
                    })
                    .map(|(i, _)| JsString::from(format!("receipt:<generated:{}>", i + 1)))
                    .unwrap_or_else(|| k.clone());
                (key, replace(v, ids))
            })),
            _ => v.clone(),
        }
    }
    let mut ids = Vec::new();
    collect(value, &mut ids);
    replace(value, &ids)
}

#[tokio::test]
async fn diagnostic_tick_journal_and_failures_match_original_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .current_dir(&root)
        .arg("tests/rust/fixtures/chat-diagnostic-runtime-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(path) = std::env::var_os("IVY_CHAT_DIAGNOSTIC_ORACLE_CAPTURE") {
        std::fs::write(path, &output.stdout).unwrap();
    }
    let corpus = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let admission = ChatAdmission::new(corpus.required("definition").unwrap()).unwrap();
    let scope = text(&corpus, "scope");
    for case in array(&corpus, "cases") {
        let fixture = Fixture::new(case.required("database").unwrap());
        let mut peer = None;
        let mut close_reason = None;
        let mut sdk_connection = None;
        if let Some(reason) = case.get("closeReason") {
            let (child, connection) = connection().await;
            peer = Some(child);
            sdk_connection = Some(connection);
            close_reason = Some(
                IvyError::new_js(
                    reason
                        .required("code")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .clone(),
                    reason
                        .required("message")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .clone(),
                )
                .with_outcome(match text(reason, "outcome").as_str() {
                    "completed" => Outcome::Completed,
                    "not_executed" => Outcome::NotExecuted,
                    _ => Outcome::Unknown,
                })
                .with_details(reason.required("details").unwrap().clone()),
            );
        }
        let client = Arc::new(Client {
            control: Mutex::new(Control {
                step: Json::Null,
                calls: vec![],
                page: 0,
                options: RequestOptions::default(),
            }),
            connection: sdk_connection,
            close_reason,
        });
        let mut store = ChatStore::open(client.clone(), None, Some(fixture.0.clone()))
            .await
            .unwrap();
        let mut actual = Vec::new();
        let mut last_write = 0;
        for step in array(case, "steps") {
            let mut options = RequestOptions::default();
            options.cancel_error = cancelled();
            if let Some(connection) = &client.connection {
                options.cancellation = connection.signal();
            }
            {
                let mut state = client.control.lock().unwrap();
                state.step = step.clone();
                state.calls.clear();
                state.page = 0;
                state.options = options.clone();
            }
            if text(step, "action") == "reopen" {
                store.close().await.unwrap();
                store = ChatStore::open(client.clone(), None, Some(fixture.0.clone()))
                    .await
                    .unwrap();
            }
            let r = async {
                if text(step, "action") == "reopen" {
                    return Ok(());
                }
                if text(step, "action") == "forgetQueued" {
                    let mut state = store
                        .diagnostic_notice_state(&scope)
                        .await
                        .map_err(|e| e.into_ivy())?;
                    for (key, mut entry) in state.clone().into_object().unwrap() {
                        if let Some(mut alert) =
                            entry.get("alert").filter(|v| !v.is_null()).cloned()
                        {
                            alert.insert("queued", false.into());
                            entry.insert("alert", alert);
                            state.insert(key, entry);
                        }
                    }
                    store
                        .save_diagnostic_notice_state(&scope, &mut state)
                        .await
                        .map_err(|e| e.into_ivy())?;
                } else {
                    if let Some(limits) = case.get("historyLimits") {
                        store
                            .set_diagnostic_history_limits(Limits {
                                count: limits.required("count")?.as_f64().unwrap() as usize,
                                bytes: limits.required("bytes")?.as_f64().unwrap() as usize,
                            })
                            .await
                            .map_err(|e| e.into_ivy())?;
                    }
                    let ops = ChatOperations::new(&store, &admission)?;
                    let pending_state = case
                        .get("noticeState")
                        .cloned()
                        .unwrap_or_else(|| "pending".into());
                    let main = Main {
                        ops: &ops,
                        client: &client,
                        notice_state: &pending_state,
                    };
                    let clock = Arc::new(Time(step.required("now")?.as_f64().unwrap() as i64));
                    if step.get("abort") == Some(&Json::from("before")) {
                        options.cancellation.cancel();
                    }
                    DiagnosticNotifications::with_clock(
                        &ops,
                        &main,
                        case.required("language")?.as_js_string().unwrap().clone(),
                        clock,
                    )
                    .tick(options)
                    .await?;
                }
                Ok(())
            }
            .await;
            actual.push(Json::object([
                ("result", result(r)),
                (
                    "calls",
                    Json::Array(client.control.lock().unwrap().calls.clone()),
                ),
                ("writes", Json::Array(fixture.snapshots(&mut last_write))),
                (
                    "state",
                    store.diagnostic_notice_state(&scope).await.unwrap(),
                ),
                (
                    "operationCount",
                    Json::Number(fixture.operation_count() as f64),
                ),
            ]));
        }
        store.close().await.unwrap();
        if let Some(mut peer) = peer {
            peer.kill().await.unwrap();
            peer.wait().await.unwrap();
        }
        let actual = normalize(&Json::Array(actual));
        let expected = normalize(case.required("expected").unwrap());
        for (i, (a, e)) in actual
            .as_array()
            .unwrap()
            .iter()
            .zip(expected.as_array().unwrap())
            .enumerate()
        {
            for part in ["result", "calls", "writes", "state", "operationCount"] {
                assert_eq!(
                    canonical(a.required(part).unwrap(), usize::MAX).unwrap(),
                    canonical(e.required(part).unwrap(), usize::MAX).unwrap(),
                    "{} step {i} {part}",
                    text(case, "label")
                );
            }
        }
    }
}
