use super::snapshot;
use ivy_chat_bridge::whatsapp::{
    command_native::{CommandOwner, RetryIdentity},
    commands::CommandMain,
    journal_io::JournalIo,
    journal_owner::Journal,
    messages::Translator,
    native::{SessionIdentity, SessionMain, SessionOwner, WhatsAppNative},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    checked_native_contract::CheckedNativeContracts,
    json::{canonical, parse},
};
use ivy_sdk::{
    clock::Clock,
    native_operation::NativeOperationCall,
    native_owner::NativeDispatchGuard,
    native_turn_contents::{ContractFuture, NativeTurnOwner},
    rpc::RequestFuture,
};
use std::{
    collections::HashMap,
    process::Command,
    sync::{Arc, Mutex},
    time::Instant,
};
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s(v: &Json, k: &str) -> String {
    f(v, k).as_str().unwrap()
}
fn result(value: IvyResult<Json>) -> Json {
    match value {
        Ok(v) => Json::object([("value", v)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.as_str().into()),
            ]),
        )]),
    }
}
fn error(v: &Json) -> IvyError {
    IvyError::new_js(
        f(v, "code").as_js_string().unwrap().clone(),
        f(v, "message").as_js_string().unwrap().clone(),
    )
    .with_outcome(match s(v, "outcome").as_str() {
        "unknown" => Outcome::Unknown,
        "completed" => Outcome::Completed,
        _ => Outcome::NotExecuted,
    })
}
fn equal(a: &Json, b: &Json, label: &str) {
    assert_eq!(
        canonical(a, usize::MAX).unwrap(),
        canonical(b, usize::MAX).unwrap(),
        "{label}"
    );
}
#[derive(Default)]
struct State {
    version: u64,
    now: i64,
    epoch: JsString,
    stamp: bool,
    turn: Option<JsString>,
    observation: u64,
    counts: HashMap<&'static str, usize>,
}
struct Probe {
    spec: Json,
    identity: Json,
    events: Mutex<Vec<Json>>,
    state: Mutex<State>,
    catalogs: Arc<CheckedNativeContracts>,
    instructions: JsString,
    journal: Journal,
    session: Mutex<Option<WhatsAppNative>>,
    start: Instant,
}
impl Probe {
    fn event(&self, values: Vec<Json>) {
        self.events.lock().unwrap().push(Json::Array(values));
    }
    fn take(&self, key: &'static str, values: &Json) -> Json {
        let mut state = self.state.lock().unwrap();
        let i = state.counts.entry(key).or_default();
        let values = values.as_array().unwrap();
        let value = values[(*i).min(values.len() - 1)].clone();
        *i += 1;
        value
    }
    fn unwrap(&self, v: Json) -> IvyResult<Json> {
        if let Some(e) = v.get("throw") {
            Err(error(e))
        } else {
            Ok(v)
        }
    }
    fn snapshot(&self) -> Json {
        self.journal.access(|j| Ok(snapshot(j))).unwrap()
    }
    async fn observed(
        &self,
        method: &str,
        params: Json,
        epoch: Option<&JsString>,
        allow: bool,
    ) -> IvyResult<Json> {
        self.event(vec![
            "read".into(),
            method.into(),
            params.clone(),
            epoch.cloned().map(Json::String).unwrap_or(Json::Null),
            allow.into(),
        ]);
        let reply = if method == "thread/items/list" {
            let mut reply = if self.spec.get("itemReplies").is_some() {
                self.take("items", f(&self.spec, "itemReplies"))
            } else {
                Json::object([("result", self.take("items", f(&self.spec, "itemPages")))])
            };
            if let Some(mut value) = reply.get("result").cloned() {
                let data = f(&value, "data")
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| {
                        Json::object([
                            ("turnId", f(&params, "turnId").clone()),
                            ("item", item.clone()),
                        ])
                    })
                    .collect();
                value.insert("data", Json::Array(data));
                reply.insert("result", value);
            }
            reply
        } else if self.spec.get("reads").is_some() {
            self.take("reads", f(&self.spec, "reads"))
        } else if method == "thread/turns/list" {
            let mut turn = f(&self.spec, "turn").clone();
            {
                let state = self.state.lock().unwrap();
                if state.stamp {
                    turn.insert("completedAt", Json::Number(11.0));
                }
                if let Some(id) = &state.turn {
                    turn.insert("id", Json::String(id.clone()));
                }
            }
            if f(&params, "itemsView") == &Json::from("full") {
                let status = self
                    .spec
                    .get("fullStatus")
                    .unwrap_or(f(&turn, "status"))
                    .clone();
                turn.insert("status", status);
                turn.insert("itemsView", "full".into());
                turn.insert(
                    "items",
                    Json::Array(
                        f(&self.spec, "itemPages")
                            .as_array()
                            .unwrap()
                            .iter()
                            .flat_map(|p| f(p, "data").as_array().unwrap().iter().cloned())
                            .collect(),
                    ),
                );
            }
            Json::object([(
                "result",
                Json::object([
                    (
                        "data",
                        self.spec
                            .get("metadataData")
                            .cloned()
                            .unwrap_or(Json::Array(vec![turn])),
                    ),
                    ("nextCursor", Json::Null),
                ]),
            )])
        } else {
            Json::object([(
                "result",
                Json::object([(
                    "thread",
                    Json::object([
                        ("status", Json::object([("type", "idle".into())])),
                        ("canAcceptDirectInput", true.into()),
                    ]),
                )]),
            )])
        };
        let mut state = self.state.lock().unwrap();
        state.observation += 1;
        let mut observed = self.identity.clone();
        for (key, value) in [
            ("method", method.into()),
            ("params", params),
            (
                "epoch",
                Json::String(epoch.cloned().unwrap_or_else(|| state.epoch.clone())),
            ),
            (
                "observationId",
                format!("observation-{}", state.observation).into(),
            ),
            ("reply", reply),
        ] {
            observed.insert(key, value);
        }
        Ok(observed)
    }
}
impl CommandMain for Probe {
    fn find(&self) -> RequestFuture<'_> {
        Box::pin(async {
            self.event(vec!["main".into()]);
            self.unwrap(self.take("main", f(&self.spec, "mainReads")))
        })
    }
    fn binding(&self, pin: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            self.event(vec!["binding".into(), "chat-bridge/binding".into(), pin]);
            self.unwrap(self.take("binding", f(&self.spec, "bindingReads")))
        })
    }
    fn plan(&self) -> RequestFuture<'_> {
        Box::pin(async {
            self.event(vec!["plan".into()]);
            Ok(f(&self.spec, "plan").clone())
        })
    }
    fn expected(&self, sender: &JsString) -> Json {
        self.event(vec!["expected".into(), Json::String(sender.clone())]);
        Json::object([("sender", Json::String(sender.clone()))])
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            self.event(vec!["operationId".into(), scope]);
            Ok("recovery-operation".into())
        })
    }
    fn create<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async {
            self.event(vec![
                "create".into(),
                Json::String(sender.clone()),
                request.clone(),
            ]);
            Ok(f(&self.spec, "createResult").clone())
        })
    }
}
impl SessionMain for Probe {
    fn change_version(&self) -> u64 {
        self.state.lock().unwrap().version
    }
    fn catalogs(&self) -> &CheckedNativeContracts {
        &self.catalogs
    }
    fn instructions(&self) -> &JsString {
        &self.instructions
    }
    fn admit(&self, sender: &JsString, request: &Json) -> IvyResult<()> {
        self.event(vec![
            "admit".into(),
            Json::String(sender.clone()),
            request.clone(),
        ]);
        Ok(())
    }
    fn input<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async {
            self.event(vec![
                "input".into(),
                Json::String(sender.clone()),
                request.clone(),
            ]);
            Ok(f(&self.spec, "imageInput").clone())
        })
    }
}
fn call_json(call: &NativeOperationCall) -> Json {
    Json::object([
        ("operationId", Json::String(call.operation_id.clone())),
        ("method", Json::String(call.method.clone())),
        ("params", call.params.clone()),
        ("definitionHash", Json::String(call.definition_hash.clone())),
    ])
}
impl CommandOwner for Probe {
    fn binding<'a>(&'a self, method: &'a JsString) -> RequestFuture<'a> {
        Box::pin(async {
            self.event(vec!["nativeBinding".into(), Json::String(method.clone())]);
            Ok(Json::object([(
                "definitionHash",
                format!("sha256:{}", "a".repeat(64)).into(),
            )]))
        })
    }
    fn operation<'a>(&'a self, call: &'a NativeOperationCall) -> RequestFuture<'a> {
        Box::pin(async {
            self.event(vec!["operation".into(), call_json(call)]);
            Ok(Json::object([
                ("phase", "succeeded".into()),
                (
                    "reply",
                    Json::object([("result", Json::object([("legacy", true.into())]))]),
                ),
            ]))
        })
    }
    fn interaction<'a>(&'a self, id: &'a JsString) -> RequestFuture<'a> {
        Box::pin(async {
            self.event(vec!["interaction".into(), Json::String(id.clone())]);
            if let Some(e) = self.spec.get("sendThrow") {
                return Err(error(e));
            }
            Ok(Json::object([(
                "reply",
                Json::object([(
                    "result",
                    Json::object([("turn", Json::object([("id", "started".into())]))]),
                )]),
            )]))
        })
    }
    fn interact<'a>(
        &'a self,
        call: &'a NativeOperationCall,
        guard: &'a dyn NativeDispatchGuard,
    ) -> RequestFuture<'a> {
        Box::pin(async {
            self.event(vec!["interact".into(), call_json(call)]);
            if call.method == JsString::from("turn/start")
                && f(&self.spec, "changeBeforeGuard") == &Json::Bool(true)
            {
                self.state.lock().unwrap().version += 1;
            }
            guard.check().await?;
            let reply = if call.method == JsString::from("turn/start") {
                self.event(vec!["dispatch".into(), self.snapshot()]);
                if let Some(e) = self.spec.get("sendThrow") {
                    return Err(error(e));
                }
                Json::object([(
                    "result",
                    Json::object([("turn", Json::object([("id", "started".into())]))]),
                )])
            } else if call.method == JsString::from("thread/resume") {
                if f(&self.spec, "resumeBusy") == &Json::Bool(true) {
                    self.session
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .set_busy(true);
                }
                self.take("resume", f(&self.spec, "resumeReplies"))
            } else {
                self.take("account", f(&self.spec, "accountReplies"))
            };
            Ok(Json::object([("reply", reply)]))
        })
    }
}
impl SessionOwner for Probe {
    fn status(&self) -> RequestFuture<'_> {
        Box::pin(async {
            self.event(vec!["status".into()]);
            let values = self
                .spec
                .get("statusReads")
                .cloned()
                .unwrap_or(Json::Array(vec![Json::object([(
                    "hostId",
                    "native-host".into(),
                )])]));
            self.unwrap(self.take("status", &values))
        })
    }
    fn observation<'a>(&'a self, method: &'a str, params: Json) -> RequestFuture<'a> {
        Box::pin(self.observed(method, params, None, true))
    }
}
impl NativeTurnOwner for Probe {
    fn check_read(
        &self,
        value: &Json,
        method: &str,
        params: &Json,
        epoch: &JsString,
    ) -> IvyResult<()> {
        self.event(vec![
            "checkRead".into(),
            method.into(),
            params.clone(),
            Json::String(epoch.clone()),
            f(value, "observationId").clone(),
        ]);
        if f(value, "method") == &Json::from(method)
            && f(value, "params") == params
            && f(value, "epoch").as_js_string() == Some(epoch)
        {
            Ok(())
        } else {
            Err(IvyError::new(
                "native_read_mismatch",
                "Scripted observation identity changed.",
            ))
        }
    }
    fn read<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        epoch: &'a JsString,
        allow: bool,
    ) -> RequestFuture<'a> {
        Box::pin(self.observed(method, params, Some(epoch), allow))
    }
    fn selected_contract(&self) -> ContractFuture<'_> {
        Box::pin(async {
            self.event(vec!["selectedContract".into()]);
            self.catalogs
                .checked_native_contract(&"0.159.2".into(), None)
        })
    }
}
impl Clock for Probe {
    fn utc_millis(&self) -> i64 {
        self.state.lock().unwrap().now
    }
    fn monotonic(&self) -> Instant {
        self.start
    }
}
struct Ids;
impl SessionIdentity for Ids {
    fn next(&self, kind: &str) -> JsString {
        format!("wa-{kind}-00000000-0000-4000-8000-000000000001").into()
    }
}
impl RetryIdentity for Ids {
    fn next(&self) -> JsString {
        "wa-retry-00000000-0000-4000-8000-000000000001".into()
    }
}
async fn action(native: &WhatsAppNative, probe: &Probe, action: &Json) -> IvyResult<Json> {
    match s(action, "op").as_str() {
        "bound" => native.bound().await,
        "prepare" => native.prepare().await.map(|_| Json::Null),
        "host" => native
            .same_host(
                &f(action, "host")
                    .as_js_string()
                    .cloned()
                    .unwrap_or_else(|| "native-host".into()),
            )
            .await
            .map(Json::Bool),
        "version" => {
            probe.state.lock().unwrap().version += 1;
            Ok(Json::Null)
        }
        "invalidate" => {
            native.invalidate(f(action, "connection") == &Json::Bool(true));
            Ok(Json::Null)
        }
        "busy" => {
            native.set_busy(f(action, "value") == &Json::Bool(true));
            Ok(Json::Null)
        }
        "advance" => {
            probe.state.lock().unwrap().now += f(action, "ms").as_f64().unwrap() as i64;
            Ok(Json::Null)
        }
        "read" => {
            native
                .read(&s(action, "method"), f(action, "params").clone())
                .await
        }
        "send" => {
            native
                .send(&"sender".into(), f(&probe.spec, "request"))
                .await
        }
        "page" => native.turn_page(&"thread".into(), Json::Null).await,
        "epoch" => {
            probe.state.lock().unwrap().epoch = "next-epoch".into();
            Ok(Json::Null)
        }
        "stamp" => {
            probe.state.lock().unwrap().stamp = true;
            Ok(Json::Null)
        }
        "turn" => {
            probe.state.lock().unwrap().turn =
                Some(f(action, "id").as_js_string().unwrap().clone());
            Ok(Json::Null)
        }
        "parallel" => {
            let a = Json::object([("op", f(action, "method").clone())]);
            let (left, right) = tokio::join!(
                Box::pin(action_run(native, probe, &a)),
                Box::pin(action_run(native, probe, &a))
            );
            Ok(Json::Array(vec![left?, right?]))
        }
        _ => panic!("Unknown fixture action"),
    }
}
fn action_run<'a>(
    native: &'a WhatsAppNative,
    probe: &'a Probe,
    value: &'a Json,
) -> RequestFuture<'a> {
    Box::pin(action(native, probe, value))
}
fn capture(rows: &[Json]) {
    if let Some(path) = std::env::var_os("IVY_CHAT_SESSION_ACTUAL_CAPTURE") {
        std::fs::write(
            path,
            canonical(
                &Json::object([("rows", Json::Array(rows.to_vec()))]),
                usize::MAX,
            )
            .unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn original_bound_session_direct_input_and_verified_history_match() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .current_dir(&root)
        .arg("tests/rust/fixtures/chat-whatsapp-session-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let catalogs = Arc::new(CheckedNativeContracts::new(&root));
    let mut actual_rows = vec![];
    for row in f(&corpus, "rows").as_array().unwrap() {
        let spec = f(row, "spec");
        let journal = Journal::new(JournalIo::open(":memory:").unwrap());
        journal
            .access(|j| {
                j.accept("message", "sender", "fixture", "original", 1)?;
                j.save_request(
                    "message",
                    spec.get("savedRequest").unwrap_or(f(spec, "request")),
                )?;
                Ok(())
            })
            .unwrap();
        for (key, value) in f(spec, "kv").as_object().unwrap_or(&[]) {
            journal.set(&key.to_utf8().unwrap(), value).unwrap();
        }
        let probe = Arc::new(Probe {
            spec: spec.clone(),
            identity: f(&corpus, "catalogIdentity").clone(),
            events: Mutex::new(vec![]),
            state: Mutex::new(State {
                now: 1000,
                epoch: "epoch".into(),
                ..Default::default()
            }),
            catalogs: catalogs.clone(),
            instructions: f(spec, "instructions").as_js_string().unwrap().clone(),
            journal: journal.clone(),
            session: Mutex::new(None),
            start: Instant::now(),
        });
        let native = WhatsAppNative::with_dependencies(
            probe.clone(),
            probe.clone(),
            journal.clone(),
            Translator::new(spec.get("language").and_then(Json::as_js_string)),
            probe.clone(),
            Arc::new(Ids),
            Arc::new(Ids),
        );
        *probe.session.lock().unwrap() = Some(native.clone());
        let mut steps = vec![];
        for a in f(spec, "actions").as_array().unwrap() {
            steps.push(Json::object([
                ("result", result(action(&native, &probe, a).await)),
                ("snapshot", probe.snapshot()),
            ]));
        }
        let actual = Json::object([
            ("spec", spec.clone()),
            ("steps", Json::Array(steps)),
            ("events", Json::Array(probe.events.lock().unwrap().clone())),
        ]);
        actual_rows.push(actual.clone());
        capture(&actual_rows);
        equal(&actual, row, &s(spec, "label"));
        probe.session.lock().unwrap().take();
        drop(native);
        journal.close().await.unwrap();
    }
    assert_eq!(actual_rows.len(), 68);
}

struct SdkClient {
    probe: Arc<Probe>,
    path: std::path::PathBuf,
    calls: Mutex<Vec<Json>>,
    corrupt_caller: std::sync::atomic::AtomicBool,
    corrupt_epoch: std::sync::atomic::AtomicBool,
}
impl SdkClient {
    fn reply(&self, value: Json) -> IvyResult<Json> {
        let mut calls = self.calls.lock().unwrap();
        calls.last_mut().unwrap().insert("reply", value.clone());
        if let Some(path) = std::env::var_os("IVY_CHAT_SESSION_SDK_CAPTURE") {
            std::fs::write(
                path,
                canonical(
                    &Json::object([("calls", Json::Array(calls.clone()))]),
                    usize::MAX,
                )
                .unwrap(),
            )
            .unwrap();
        }
        Ok(value)
    }
}
impl ivy_sdk::rpc::RpcClient for SdkClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: ivy_sdk::rpc::RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(Json::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            if method == "tools.list" {
                let namespace = s(&params, "namespace");
                let name = s(&params, "namePrefix");
                return self.reply(Json::object([
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
                            ("qualifiedName", format!("{namespace}.{name}").into()),
                            (
                                "definition",
                                Json::object([
                                    ("namespace", namespace.clone().into()),
                                    ("name", name.into()),
                                    (
                                        "interfaceVersion",
                                        if namespace == "agent" {
                                            "1.0.0".into()
                                        } else {
                                            "0.159.2".into()
                                        },
                                    ),
                                    ("inputSchema", Json::Object(vec![])),
                                    ("outputSchema", Json::Object(vec![])),
                                ]),
                            ),
                            (
                                "definitionHash",
                                format!(
                                    "sha256:{}",
                                    if namespace == "agent" { "a" } else { "b" }.repeat(64)
                                )
                                .into(),
                            ),
                            (
                                "catalogHash",
                                f(&self.probe.identity, "catalogHash").clone(),
                            ),
                        ])]),
                    ),
                    ("nextCursor", Json::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            let args = f(&params, "arguments");
            match s(&params, "qualifiedName").as_str() {
                "agent.status" => {
                    let mut status = self.probe.identity.clone();
                    for (key, value) in [
                        ("serviceNodeId", "native-agent".into()),
                        ("hostId", "native-host".into()),
                        ("epoch", "epoch".into()),
                        ("pid", Json::Number(42.0)),
                        ("state", "ready".into()),
                        ("observedAt", "2026-01-02T03:04:05.006Z".into()),
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
                    ] {
                        status.insert(key, value);
                    }
                    self.reply(status)
                }
                "agent.read" => {
                    let method = s(args, "method");
                    let mut observed = self
                        .probe
                        .observed(&method, f(args, "params").clone(), None, true)
                        .await?;
                    let hash = ivy_contracts::hash::hash_json(&Json::object([
                        ("method", method.clone().into()),
                        ("params", f(args, "params").clone()),
                    ]))?;
                    for (key, value) in [
                        ("schemaVersion", Json::Number(1.0)),
                        ("serviceNodeId", "native-agent".into()),
                        (
                            "callerPrincipalId",
                            if self
                                .corrupt_caller
                                .load(std::sync::atomic::Ordering::SeqCst)
                            {
                                "other".into()
                            } else {
                                "caller".into()
                            },
                        ),
                        ("observedAt", "2026-01-02T03:04:05.006Z".into()),
                        (
                            "requestId",
                            format!("request-{}", s(&observed, "observationId")).into(),
                        ),
                        ("requestHash", hash.into()),
                    ] {
                        observed.insert(key, value);
                    }
                    if method == "thread/items/list"
                        && self.corrupt_epoch.load(std::sync::atomic::Ordering::SeqCst)
                    {
                        observed.insert("epoch", "changed-epoch".into());
                    }
                    self.reply(observed)
                }
                "agent.interact" => {
                    let method = s(args, "method");
                    if method == "turn/start" {
                        let disk = rusqlite::Connection::open_with_flags(
                            &self.path,
                            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                        )
                        .unwrap();
                        let raw: String = disk
                            .query_row(
                                "SELECT value FROM kv WHERE key='interaction:input:message'",
                                [],
                                |r| r.get(0),
                            )
                            .unwrap();
                        let saved = parse(&raw, usize::MAX).unwrap();
                        assert_eq!(f(&saved, "attempted"), &Json::Bool(true));
                        equal(
                            f(f(&saved, "call"), "operationId"),
                            f(args, "operationId"),
                            "Original committed input operation",
                        );
                        equal(
                            f(f(&saved, "call"), "params"),
                            f(args, "params"),
                            "Original committed input parameters",
                        );
                    }
                    let reply = if method == "thread/resume" {
                        Json::object([(
                            "result",
                            Json::object([(
                                "thread",
                                Json::object([
                                    ("status", Json::object([("type", "idle".into())])),
                                    ("canAcceptDirectInput", true.into()),
                                ]),
                            )]),
                        )])
                    } else if method == "account/read" {
                        Json::object([(
                            "result",
                            Json::object([("requiresOpenaiAuth", false.into())]),
                        )])
                    } else {
                        Json::object([(
                            "result",
                            Json::object([("turn", Json::object([("id", "sdk-turn".into())]))]),
                        )])
                    };
                    self.reply(Json::object([
                        ("operationId", f(args, "operationId").clone()),
                        ("reply", reply),
                    ]))
                }
                value => panic!("Unexpected SDK call: {value}"),
            }
        })
    }
}

#[tokio::test]
async fn real_sdk_session_keeps_disk_fence_and_rejects_changed_history_authority() {
    let corpus_path = std::env::var_os("IVY_CHAT_SESSION_ORACLE_CAPTURE");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let corpus = if let Some(path) = corpus_path.filter(|p| std::path::Path::new(p).exists()) {
        parse(&std::fs::read_to_string(path).unwrap(), usize::MAX).unwrap()
    } else {
        let output = Command::new("node")
            .current_dir(&root)
            .arg("tests/rust/fixtures/chat-whatsapp-session-oracle.mjs")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    };
    let row = f(&corpus, "rows")
        .as_array()
        .unwrap()
        .iter()
        .find(|r| s(f(r, "spec"), "label") == "send-cache-replay")
        .unwrap();
    let spec = f(row, "spec");
    let parent = std::fs::canonicalize(
        std::env::var_os("IVY_TEST_TEMP")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir),
    )
    .unwrap();
    let temporary = parent.join(format!("chat-session-sdk-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temporary).unwrap();
    let temporary = std::fs::canonicalize(temporary).unwrap();
    assert!(temporary.starts_with(&parent) && temporary != parent);
    let path = temporary.join("whatsapp.sqlite");
    let journal = Journal::new(JournalIo::open(&path).unwrap());
    journal
        .access(|j| {
            j.accept("message", "sender", "fixture", "original", 1)?;
            j.save_request("message", f(spec, "request"))?;
            Ok(())
        })
        .unwrap();
    let probe = Arc::new(Probe {
        spec: spec.clone(),
        identity: f(&corpus, "catalogIdentity").clone(),
        events: Mutex::new(vec![]),
        state: Mutex::new(State {
            now: 1000,
            epoch: "epoch".into(),
            ..Default::default()
        }),
        catalogs: Arc::new(CheckedNativeContracts::new(&root)),
        instructions: f(spec, "instructions").as_js_string().unwrap().clone(),
        journal: journal.clone(),
        session: Mutex::new(None),
        start: Instant::now(),
    });
    let client = Arc::new(SdkClient {
        probe: probe.clone(),
        path,
        calls: Mutex::new(vec![]),
        corrupt_caller: false.into(),
        corrupt_epoch: false.into(),
    });
    let owner = Arc::new(ivy_sdk::native_owner::NativeOwner::new(
        Arc::new(ivy_sdk::tools::ToolClients::new(client.clone())),
        "caller".into(),
        ivy_sdk::native_evidence::NativeOwnerIdentity {
            service_node_id: "native-agent".into(),
            host_id: None,
            native_version: "0.159.2".into(),
            native_executable_hash: f(&probe.identity, "nativeExecutableHash")
                .as_js_string()
                .unwrap()
                .clone(),
            catalog_hash: f(&probe.identity, "catalogHash")
                .as_js_string()
                .unwrap()
                .clone(),
        },
        Default::default(),
    ));
    let native = WhatsAppNative::with_dependencies(
        probe.clone(),
        owner,
        journal.clone(),
        Translator::default(),
        probe.clone(),
        Arc::new(Ids),
        Arc::new(Ids),
    );
    assert!(native.same_host(&"native-host".into()).await.unwrap());
    let sent = native
        .send(&"sender".into(), f(spec, "request"))
        .await
        .unwrap();
    assert_eq!(f(f(&sent, "turn"), "id"), &Json::from("sdk-turn"));
    let after = client.calls.lock().unwrap().len();
    equal(
        &native
            .send(&"sender".into(), f(spec, "request"))
            .await
            .unwrap(),
        &sent,
        "Retained input result",
    );
    assert_eq!(client.calls.lock().unwrap().len(), after);
    let page = native
        .turn_page(&"thread".into(), Json::Null)
        .await
        .unwrap();
    let count = client.calls.lock().unwrap().len();
    equal(
        &native
            .turn_page(&"thread".into(), Json::Null)
            .await
            .unwrap(),
        &page,
        "Terminal history cache",
    );
    assert_eq!(
        client.calls.lock().unwrap().len(),
        count + 1,
        "Metadata is freshly read on every hit"
    );
    client
        .corrupt_caller
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        native
            .turn_page(&"thread".into(), Json::Null)
            .await
            .unwrap_err()
            .code,
        "native_evidence_mismatch"
    );
    client
        .corrupt_caller
        .store(false, std::sync::atomic::Ordering::SeqCst);
    probe.state.lock().unwrap().stamp = true;
    client
        .corrupt_epoch
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        native
            .turn_page(&"thread".into(), Json::Null)
            .await
            .unwrap_err()
            .code,
        "native_evidence_mismatch"
    );
    client
        .corrupt_epoch
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let recovered = native
        .turn_page(&"thread".into(), Json::Null)
        .await
        .unwrap();
    assert_eq!(
        f(&f(&recovered, "data").as_array().unwrap()[0], "completedAt").as_f64(),
        Some(11.0)
    );
    if let Some(path) = std::env::var_os("IVY_CHAT_SESSION_SDK_CAPTURE") {
        std::fs::write(
            path,
            canonical(
                &Json::object([
                    ("calls", Json::Array(client.calls.lock().unwrap().clone())),
                    ("sent", sent),
                    ("page", page),
                    ("freshPage", recovered),
                    ("journal", probe.snapshot()),
                ]),
                usize::MAX,
            )
            .unwrap(),
        )
        .unwrap();
    }
    drop(native);
    drop(client);
    drop(probe);
    journal.close().await.unwrap();
    std::fs::remove_dir_all(temporary).unwrap();
}

struct GatedMain {
    probe: Arc<Probe>,
    entered: tokio::sync::mpsc::UnboundedSender<usize>,
    releases: Mutex<Vec<Option<tokio::sync::oneshot::Receiver<IvyResult<Json>>>>>,
    calls: std::sync::atomic::AtomicUsize,
}
impl CommandMain for GatedMain {
    fn find(&self) -> RequestFuture<'_> {
        Box::pin(async {
            let index = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let receiver = self.releases.lock().unwrap()[index].take().unwrap();
            self.entered.send(index).unwrap();
            receiver.await.unwrap()
        })
    }
    fn binding(&self, pin: Json) -> RequestFuture<'_> {
        CommandMain::binding(self.probe.as_ref(), pin)
    }
    fn plan(&self) -> RequestFuture<'_> {
        CommandMain::plan(self.probe.as_ref())
    }
    fn expected(&self, sender: &JsString) -> Json {
        CommandMain::expected(self.probe.as_ref(), sender)
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        CommandMain::operation_id(self.probe.as_ref(), scope)
    }
    fn create<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        CommandMain::create(self.probe.as_ref(), sender, request)
    }
}
impl SessionMain for GatedMain {
    fn change_version(&self) -> u64 {
        SessionMain::change_version(self.probe.as_ref())
    }
    fn catalogs(&self) -> &CheckedNativeContracts {
        SessionMain::catalogs(self.probe.as_ref())
    }
    fn instructions(&self) -> &JsString {
        SessionMain::instructions(self.probe.as_ref())
    }
    fn admit(&self, sender: &JsString, request: &Json) -> IvyResult<()> {
        SessionMain::admit(self.probe.as_ref(), sender, request)
    }
    fn input<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        SessionMain::input(self.probe.as_ref(), sender, request)
    }
}
#[tokio::test]
async fn pending_binding_survives_caller_cancellation_and_old_failure_keeps_new_work() {
    let journal = Journal::new(JournalIo::open(":memory:").unwrap());
    let main = Json::object([
        (
            "pin",
            Json::object([("objectId", "main".into()), ("revision", Json::Number(1.0))]),
        ),
        (
            "value",
            Json::object([(
                "binding",
                Json::object([
                    ("objectId", "binding".into()),
                    ("revision", Json::Number(1.0)),
                ]),
            )]),
        ),
    ]);
    let binding = Json::object([
        (
            "pin",
            Json::object([
                ("objectId", "binding".into()),
                ("revision", Json::Number(1.0)),
            ]),
        ),
        ("value", Json::Object(vec![])),
    ]);
    let probe = Arc::new(Probe {
        spec: Json::object([("bindingReads", Json::Array(vec![binding]))]),
        identity: Json::Null,
        events: Mutex::new(vec![]),
        state: Mutex::new(State::default()),
        catalogs: Arc::new(CheckedNativeContracts::new("unused")),
        instructions: "".into(),
        journal: journal.clone(),
        session: Mutex::new(None),
        start: Instant::now(),
    });
    let (entered, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let (release1, work1) = tokio::sync::oneshot::channel();
    let (release2, work2) = tokio::sync::oneshot::channel();
    let (release3, work3) = tokio::sync::oneshot::channel();
    let gated = Arc::new(GatedMain {
        probe: probe.clone(),
        entered,
        releases: Mutex::new(vec![Some(work1), Some(work2), Some(work3)]),
        calls: 0.into(),
    });
    let native = WhatsAppNative::with_dependencies(
        gated.clone(),
        probe.clone(),
        journal.clone(),
        Translator::default(),
        probe.clone(),
        Arc::new(Ids),
        Arc::new(Ids),
    );
    let first = {
        let native = native.clone();
        tokio::spawn(async move { native.bound().await })
    };
    assert_eq!(arrivals.recv().await, Some(0));
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let second = {
        let native = native.clone();
        tokio::spawn(async move { native.bound().await })
    };
    release1.send(Ok(main.clone())).unwrap();
    second.await.unwrap().unwrap();
    native.bound().await.unwrap();
    assert_eq!(gated.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    native.invalidate(false);
    let old = {
        let native = native.clone();
        tokio::spawn(async move { native.bound().await })
    };
    assert_eq!(arrivals.recv().await, Some(1));
    native.invalidate(false);
    let new = {
        let native = native.clone();
        tokio::spawn(async move { native.bound().await })
    };
    assert_eq!(arrivals.recv().await, Some(2));
    release2
        .send(Err(IvyError::new(
            "service_unavailable",
            "Old lookup failed.",
        )))
        .unwrap();
    assert_eq!(old.await.unwrap().unwrap_err().code, "service_unavailable");
    release3.send(Ok(main)).unwrap();
    let latest = new.await.unwrap().unwrap();
    equal(
        &native.bound().await.unwrap(),
        &latest,
        "Old failure must not clear its replacement",
    );
    assert_eq!(gated.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    if let Some(path) = std::env::var_os("IVY_CHAT_SESSION_CACHE_CAPTURE") {
        std::fs::write(
            path,
            canonical(
                &Json::object([
                    ("lookups", Json::Number(3.0)),
                    ("cancelledCaller", true.into()),
                    ("replacementRetained", true.into()),
                    ("events", Json::Array(probe.events.lock().unwrap().clone())),
                ]),
                usize::MAX,
            )
            .unwrap(),
        )
        .unwrap();
    }
    drop(native);
    drop(gated);
    drop(probe);
    journal.close().await.unwrap();
}
