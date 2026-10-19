use super::snapshot;
use ivy_chat_bridge::whatsapp::{
    command_native::{
        CommandOwner, CommandParameters, CommandSession, NativeCommandDispatcher, NativeCommands,
        RetryIdentity,
    },
    commands::{CommandMain, DesktopCommands, WhatsAppCommands, parse_command},
    identity::identity,
    journal_io::JournalIo,
    journal_owner::Journal,
    messages::Translator,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    json::{canonical, parse},
};
use ivy_sdk::{
    clock::Clock, native_operation::NativeOperationCall, native_owner::NativeDispatchGuard,
    rpc::RequestFuture,
};
use std::{
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    v.get(k).unwrap()
}
fn s(v: &Json, k: &str) -> String {
    f(v, k).as_str().unwrap()
}
fn a(values: Vec<Json>) -> Json {
    Json::Array(values)
}
fn push(events: &Mutex<Vec<Json>>, values: Vec<Json>) {
    events.lock().unwrap().push(a(values));
}
fn result(v: IvyResult<Json>) -> Json {
    match v {
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
fn snap(j: &Journal) -> Json {
    j.access(|io| Ok(snapshot(io))).unwrap()
}
fn save(value: &Json) {
    if let Some(p) = std::env::var_os("IVY_CHAT_COMMANDS_ACTUAL_CAPTURE") {
        std::fs::write(p, canonical(value, usize::MAX).unwrap()).unwrap();
    }
}
fn equal(actual: &Json, expected: &Json, label: &str) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap(),
        "{label}"
    );
}
struct Main {
    spec: Json,
    events: Arc<Mutex<Vec<Json>>>,
    finds: AtomicUsize,
}
impl CommandMain for Main {
    fn find(&self) -> RequestFuture<'_> {
        Box::pin(async {
            push(&self.events, vec!["main".into()]);
            let rows = f(&self.spec, "mainReads").as_array().unwrap();
            let i = self
                .finds
                .fetch_add(1, Ordering::SeqCst)
                .min(rows.len() - 1);
            Ok(rows[i].clone())
        })
    }
    fn binding(&self, pin: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            push(
                &self.events,
                vec!["binding".into(), "chat-bridge/binding".into(), pin],
            );
            Ok(f(&self.spec, "binding").clone())
        })
    }
    fn plan(&self) -> RequestFuture<'_> {
        Box::pin(async {
            push(&self.events, vec!["plan".into()]);
            Ok(f(&self.spec, "plan").clone())
        })
    }
    fn expected(&self, sender: &JsString) -> Json {
        push(
            &self.events,
            vec!["expected".into(), Json::String(sender.clone())],
        );
        Json::object([("sender", Json::String(sender.clone()))])
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            push(&self.events, vec!["operationId".into(), scope]);
            Ok("command-operation".into())
        })
    }
    fn create<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async {
            push(
                &self.events,
                vec![
                    "create".into(),
                    Json::String(sender.clone()),
                    request.clone(),
                ],
            );
            Ok(f(&self.spec, "newResult").clone())
        })
    }
}
struct Native {
    spec: Json,
    events: Arc<Mutex<Vec<Json>>>,
    journal: Journal,
}
impl NativeCommands for Native {
    fn command<'a>(
        &'a self,
        key: &'a str,
        method: &'a str,
        params: &'a dyn CommandParameters,
        guard: Option<&'a dyn NativeDispatchGuard>,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            push(
                &self.events,
                vec!["command".into(), key.into(), method.into()],
            );
            let params = params.build().await?;
            push(&self.events, vec!["params".into(), params]);
            if let Some(guard) = guard {
                push(&self.events, vec!["guard".into()]);
                guard.check().await?;
            }
            push(&self.events, vec!["dispatch".into(), snap(&self.journal)]);
            Ok(self
                .spec
                .get("nativeResult")
                .cloned()
                .unwrap_or_else(|| match method {
                    "model/list" => f(&self.spec, "models").clone(),
                    "thread/list" => f(&self.spec, "threads").clone(),
                    _ => Json::object([("ok", true.into())]),
                }))
        })
    }
    fn read<'a>(&'a self, method: &'a str, params: Json) -> RequestFuture<'a> {
        Box::pin(async move {
            push(&self.events, vec!["read".into(), method.into(), params]);
            Ok(f(&self.spec, "read").clone())
        })
    }
    fn invalidate(&self) {
        push(&self.events, vec!["invalidate".into()]);
    }
}
struct Desktop(Arc<Mutex<Vec<Json>>>);
impl DesktopCommands for Desktop {
    fn run<'a>(&'a self, action: &'a JsString, key: &'a str) -> RequestFuture<'a> {
        Box::pin(async move {
            push(
                &self.0,
                vec!["desktop".into(), Json::String(action.clone()), key.into()],
            );
            Ok("desktop-result".into())
        })
    }
}
struct Stats {
    busy: bool,
    revision: u64,
    prepared: bool,
}
struct Session {
    spec: Json,
    events: Arc<Mutex<Vec<Json>>>,
    stats: Mutex<Stats>,
    histories: AtomicUsize,
}
impl CommandSession for Session {
    fn read<'a>(&'a self, _: &'a str, _: Json) -> RequestFuture<'a> {
        Box::pin(async { panic!("Unexpected read boundary") })
    }
    fn turn_page<'a>(&'a self, thread: &'a JsString, cursor: Json) -> RequestFuture<'a> {
        Box::pin(async move {
            push(
                &self.events,
                vec!["history".into(), Json::String(thread.clone()), cursor],
            );
            let rows = f(&self.spec, "history").as_array().unwrap();
            Ok(rows[self
                .histories
                .fetch_add(1, Ordering::SeqCst)
                .min(rows.len() - 1)]
            .clone())
        })
    }
    fn busy_revision(&self) -> u64 {
        self.stats.lock().unwrap().revision
    }
    fn started(&self, revision: u64) {
        let mut s = self.stats.lock().unwrap();
        if s.revision == revision {
            s.busy = true;
        }
    }
    fn rejected_start(&self) {
        self.stats.lock().unwrap().prepared = false;
        self.invalidate();
    }
    fn invalidate(&self) {
        push(&self.events, vec!["invalidate".into()]);
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
struct Owner<'a> {
    spec: Json,
    events: Arc<Mutex<Vec<Json>>>,
    journal: Journal,
    session: &'a Session,
}
impl CommandOwner for Owner<'_> {
    fn binding<'a>(&'a self, method: &'a JsString) -> RequestFuture<'a> {
        Box::pin(async {
            push(
                &self.events,
                vec!["binding".into(), Json::String(method.clone())],
            );
            Ok(Json::object([(
                "definitionHash",
                format!("sha256:{}", "a".repeat(64)).into(),
            )]))
        })
    }
    fn operation<'a>(&'a self, call: &'a NativeOperationCall) -> RequestFuture<'a> {
        Box::pin(async {
            push(&self.events, vec!["operation".into(), call_json(call)]);
            Ok(f(&self.spec, "legacy").clone())
        })
    }
    fn interaction<'a>(&'a self, id: &'a JsString) -> RequestFuture<'a> {
        Box::pin(async {
            push(
                &self.events,
                vec!["interaction".into(), Json::String(id.clone())],
            );
            if let Some(e) = self.spec.get("error") {
                return Err(error(e));
            }
            Ok(Json::object([(
                "reply",
                self.spec.get("reply").cloned().unwrap_or_else(|| {
                    Json::object([("result", Json::object([("native", true.into())]))])
                }),
            )]))
        })
    }
    fn interact<'a>(
        &'a self,
        call: &'a NativeOperationCall,
        guard: &'a dyn NativeDispatchGuard,
    ) -> RequestFuture<'a> {
        Box::pin(async {
            push(&self.events, vec!["interact".into(), call_json(call)]);
            guard.check().await?;
            push(&self.events, vec!["effect".into(), snap(&self.journal)]);
            if self.spec.get("advanceBusy") == Some(&true.into()) {
                self.session.stats.lock().unwrap().revision += 1;
            }
            if let Some(e) = self.spec.get("error") {
                return Err(error(e));
            }
            Ok(Json::object([(
                "reply",
                self.spec.get("reply").cloned().unwrap_or_else(|| {
                    Json::object([("result", Json::object([("native", true.into())]))])
                }),
            )]))
        })
    }
}
struct Parameters<'a> {
    spec: &'a Json,
    events: &'a Mutex<Vec<Json>>,
}
impl CommandParameters for Parameters<'_> {
    fn build(&self) -> RequestFuture<'_> {
        Box::pin(async {
            push(self.events, vec!["parameters".into()]);
            Ok(f(self.spec, "params").clone())
        })
    }
}
struct Guard<'a> {
    spec: &'a Json,
    events: &'a Mutex<Vec<Json>>,
}
impl NativeDispatchGuard for Guard<'_> {
    fn check(&self) -> ivy_sdk::native_owner::GuardFuture<'_> {
        Box::pin(async {
            push(self.events, vec!["guard".into()]);
            if let Some(e) = self.spec.get("guardError") {
                return Err(error(e));
            }
            Ok(())
        })
    }
}
struct Fixed(Instant);
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        1000
    }
    fn monotonic(&self) -> Instant {
        self.0
    }
}
struct Ids;
impl RetryIdentity for Ids {
    fn next(&self) -> JsString {
        "wa-retry-00000000-0000-4000-8000-000000000001".into()
    }
}

#[tokio::test]
async fn original_command_policy_and_native_intent_recovery_match() {
    let out = Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-whatsapp-commands-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let corpus = parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap();
    let mut actual_parsers = vec![];
    let mut actual_policies = vec![];
    let mut actual_dispatches = vec![];
    for row in f(&corpus, "parsers").as_array().unwrap() {
        let parsed = parse_command(f(row, "text").as_js_string().unwrap())
            .map(|c| {
                Json::object([
                    ("name", Json::String(c.name)),
                    ("args", Json::String(c.args)),
                ])
            })
            .unwrap_or(Json::Null);
        actual_parsers.push(Json::object([
            ("text", f(row, "text").clone()),
            ("value", parsed),
        ]));
    }
    equal(
        &a(actual_parsers.clone()),
        f(&corpus, "parsers"),
        "parser matrix",
    );
    for row in f(&corpus, "policies").as_array().unwrap() {
        let spec = f(row, "spec");
        let events = Arc::new(Mutex::new(vec![]));
        let journal = Journal::new(JournalIo::open(":memory:").unwrap());
        if let Some(p) = spec.get("preferences") {
            journal.set("preferences", p).unwrap();
        }
        let main = Main {
            spec: spec.clone(),
            events: events.clone(),
            finds: AtomicUsize::new(0),
        };
        let native = Native {
            spec: spec.clone(),
            events: events.clone(),
            journal: journal.clone(),
        };
        let desktop = Desktop(events.clone());
        let commands = WhatsAppCommands {
            main: &main,
            journal: journal.clone(),
            native: &native,
            desktop: &desktop,
            translator: Translator::new(spec.get("language").and_then(Json::as_js_string)),
        };
        let mut steps = vec![];
        for (index, action) in f(spec, "actions").as_array().unwrap().iter().enumerate() {
            let id = action
                .get("id")
                .map(|v| v.as_str().unwrap())
                .unwrap_or_else(|| format!("row-{index}"));
            journal
                .access(|j| {
                    j.accept(&id, "sender", "fixture", &id, index as i64)?;
                    if let Some(request) = spec.get("savedRequest") {
                        j.save_request(&id, request)?;
                    }
                    Ok(())
                })
                .unwrap();
            let inbox = journal
                .access(|j| Ok(j.pending()?.into_iter().find(|r| r.id == id).unwrap()))
                .unwrap();
            let value = commands
                .run(&inbox, f(action, "text").as_js_string().unwrap())
                .await
                .map(|v| v.map(Json::String).unwrap_or(Json::Null));
            steps.push(Json::object([
                ("result", result(value)),
                ("snapshot", snap(&journal)),
                ("preferences", commands.preferences().unwrap()),
                ("options", commands.turn_options().unwrap()),
            ]));
        }
        let actual = Json::object([
            ("spec", spec.clone()),
            ("steps", a(steps)),
            ("events", a(events.lock().unwrap().clone())),
        ]);
        actual_policies.push(actual.clone());
        save(&Json::object([
            ("parsers", a(actual_parsers.clone())),
            ("policies", a(actual_policies.clone())),
            ("dispatches", a(actual_dispatches.clone())),
        ]));
        equal(&actual, row, &s(spec, "label"));
        journal.close().await.unwrap();
    }
    for row in f(&corpus, "dispatches").as_array().unwrap() {
        let spec = f(row, "spec");
        let events = Arc::new(Mutex::new(vec![]));
        let journal = Journal::new(JournalIo::open(":memory:").unwrap());
        let key = s(spec, "key");
        let method = s(spec, "method");
        let saved_method = spec
            .get("savedMethod")
            .map(|v| v.as_str().unwrap())
            .unwrap_or(method.clone());
        let call = Json::object([
            (
                "operationId",
                format!(
                    "wa-{}",
                    identity(&[
                        key.clone().into(),
                        saved_method.clone().into(),
                        f(spec, "params").clone()
                    ])
                    .unwrap()
                )
                .into(),
            ),
            ("method", saved_method.into()),
            ("params", f(spec, "params").clone()),
            (
                "definitionHash",
                format!("sha256:{}", "a".repeat(64)).into(),
            ),
        ]);
        if spec.get("attempted") == Some(&true.into())
            || spec.get("savedMethod").is_some()
            || spec.get("savedResult").is_some()
        {
            let mut saved = Json::object([
                ("call", call.clone()),
                (
                    "attempted",
                    spec.get("attempted").cloned().unwrap_or(false.into()),
                ),
            ]);
            if let Some(v) = spec.get("savedResult") {
                saved.insert("result", v.clone());
            }
            journal.set(&format!("interaction:{key}"), &saved).unwrap();
        }
        if spec.get("legacy").is_some() {
            journal
                .set(
                    &format!("native:{key}"),
                    &Json::object([
                        ("call", call),
                        (
                            "attempted",
                            spec.get("legacyAttempted").cloned().unwrap_or(true.into()),
                        ),
                    ]),
                )
                .unwrap();
        }
        let session = Session {
            spec: spec.clone(),
            events: events.clone(),
            stats: Mutex::new(Stats {
                busy: false,
                revision: 0,
                prepared: true,
            }),
            histories: AtomicUsize::new(0),
        };
        let owner = Owner {
            spec: spec.clone(),
            events: events.clone(),
            journal: journal.clone(),
            session: &session,
        };
        let clock = Fixed(Instant::now());
        let native = NativeCommandDispatcher::new(
            journal.clone(),
            &owner,
            &session,
            &clock,
            &Ids,
            Translator::default(),
        );
        let params = Parameters {
            spec,
            events: &events,
        };
        let guard = Guard {
            spec,
            events: &events,
        };
        let mut steps = vec![];
        for _ in 0..f(spec, "actions").as_f64().unwrap() as usize {
            let value = native.command(&key, &method, &params, Some(&guard)).await;
            let state = {
                let stats = session.stats.lock().unwrap();
                Json::object([
                    ("busy", stats.busy.into()),
                    ("revision", Json::Number(stats.revision as f64)),
                    (
                        "lastDispatchMs",
                        native
                            .last_dispatch_ms()
                            .map(|v| Json::Number(v as f64))
                            .unwrap_or(Json::Null),
                    ),
                    (
                        "prepared",
                        if stats.prepared {
                            "prepared".into()
                        } else {
                            Json::Null
                        },
                    ),
                ])
            };
            steps.push(Json::object([
                ("result", result(value)),
                ("snapshot", snap(&journal)),
                ("state", state),
            ]));
        }
        let actual = Json::object([
            ("spec", spec.clone()),
            ("steps", a(steps)),
            ("events", a(events.lock().unwrap().clone())),
        ]);
        actual_dispatches.push(actual.clone());
        save(&Json::object([
            ("parsers", a(actual_parsers.clone())),
            ("policies", a(actual_policies.clone())),
            ("dispatches", a(actual_dispatches.clone())),
        ]));
        equal(&actual, row, &s(spec, "label"));
        journal.close().await.unwrap();
    }
    assert_eq!(f(&corpus, "parsers").as_array().unwrap().len(), 121);
}

struct DiskClient {
    path: std::path::PathBuf,
    trace: Mutex<Vec<Json>>,
    effects: AtomicUsize,
}
impl ivy_sdk::rpc::RpcClient for DiskClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: ivy_sdk::rpc::RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(Json::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            if method == "tools.list" {
                let namespace = s(&params, "namespace");
                let prefix = s(&params, "namePrefix");
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
                        a(vec![Json::object([
                            ("qualifiedName", format!("{namespace}.{prefix}").into()),
                            (
                                "definition",
                                Json::object([
                                    ("namespace", namespace.clone().into()),
                                    ("name", prefix.into()),
                                    (
                                        "interfaceVersion",
                                        if namespace == "agent" {
                                            "1.0.0".into()
                                        } else {
                                            "0.159.2".into()
                                        },
                                    ),
                                    ("inputSchema", Json::object([] as [(&str, Json); 0])),
                                    ("outputSchema", Json::object([] as [(&str, Json); 0])),
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
                            ("catalogHash", format!("sha256:{}", "c".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", Json::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            assert_eq!(params.get("qualifiedName"), Some(&"agent.interact".into()));
            let db = rusqlite::Connection::open_with_flags(
                &self.path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let raw: String = db
                .query_row(
                    "SELECT value FROM kv WHERE key='interaction:key'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let saved = parse(&raw, usize::MAX).unwrap();
            assert_eq!(
                saved.get("attempted"),
                Some(&true.into()),
                "Actual SDK effect ran before the committed attempt fence"
            );
            let args = f(&params, "arguments");
            equal(
                f(f(&saved, "call"), "operationId"),
                f(args, "operationId"),
                "durable operation identity",
            );
            equal(
                f(f(&saved, "call"), "params"),
                f(args, "params"),
                "durable original parameters",
            );
            assert_eq!(
                params.get("expectedCallerPrincipalId"),
                Some(&"caller".into())
            );
            self.effects.fetch_add(1, Ordering::SeqCst);
            Ok(Json::object([
                ("operationId", f(args, "operationId").clone()),
                ("epoch", "epoch".into()),
                (
                    "reply",
                    Json::object([("result", Json::object([("native", true.into())]))]),
                ),
            ]))
        })
    }
}

#[tokio::test]
async fn real_native_owner_waits_for_disk_commit_and_refuses_failed_writer() {
    let parent = std::env::var_os("IVY_TEST_TEMP")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let parent = std::fs::canonicalize(parent).unwrap();
    let mut proofs = vec![];
    for reject in [false, true] {
        let root = parent.join(format!("chat-command-fence-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        assert!(root.starts_with(&parent) && root != parent);
        let path = root.join("whatsapp.sqlite");
        let journal = Journal::new(JournalIo::open(&path).unwrap());
        if reject {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch("CREATE TRIGGER refuse_attempt BEFORE UPDATE ON kv WHEN NEW.key='interaction:key' AND NEW.value LIKE '%\"attempted\":true%' BEGIN SELECT RAISE(ABORT,'synthetic writer refusal'); END;").unwrap();
        }
        let client = Arc::new(DiskClient {
            path: path.clone(),
            trace: Mutex::new(vec![]),
            effects: AtomicUsize::new(0),
        });
        let owner = ivy_sdk::native_owner::NativeOwner::new(
            Arc::new(ivy_sdk::tools::ToolClients::new(client.clone())),
            "caller".into(),
            ivy_sdk::native_evidence::NativeOwnerIdentity {
                service_node_id: "native-agent".into(),
                host_id: None,
                native_version: "0.159.2".into(),
                native_executable_hash: format!("sha256:{}", "d".repeat(64)).into(),
                catalog_hash: format!("sha256:{}", "e".repeat(64)).into(),
            },
            ivy_sdk::rpc::RequestOptions::default(),
        );
        let events = Arc::new(Mutex::new(vec![]));
        let session = Session {
            spec: Json::Null,
            events,
            stats: Mutex::new(Stats {
                busy: false,
                revision: 0,
                prepared: true,
            }),
            histories: AtomicUsize::new(0),
        };
        let clock = Fixed(Instant::now());
        let native = NativeCommandDispatcher::new(
            journal.clone(),
            &owner,
            &session,
            &clock,
            &Ids,
            Translator::default(),
        );
        let params=ivy_chat_bridge::whatsapp::command_native::ReadyParameters(parse(r#"{"threadId":"thread","expectedTurnId":"turn","input":[],"clientUserMessageId":"original"}"#,usize::MAX).unwrap());
        let dispatched = native.command("key", "turn/steer", &params, None).await;
        if reject {
            assert_eq!(dispatched.as_ref().unwrap_err().code, "storage_unavailable");
            assert_eq!(client.effects.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(
                dispatched.as_ref().unwrap().get("native"),
                Some(&true.into())
            );
            assert_eq!(client.effects.load(Ordering::SeqCst), 1);
        }
        let memory = snap(&journal);
        let closed = journal.close().await;
        if reject {
            assert_eq!(closed.unwrap_err().code, "storage_unavailable");
        } else {
            closed.unwrap();
        }
        let reopened = Journal::new(JournalIo::open(&path).unwrap());
        let disk = snap(&reopened);
        if !reject {
            let before = client.trace.lock().unwrap().len();
            let replay = NativeCommandDispatcher::new(
                reopened.clone(),
                &owner,
                &session,
                &clock,
                &Ids,
                Translator::default(),
            );
            equal(
                &replay
                    .command("key", "turn/steer", &params, None)
                    .await
                    .unwrap(),
                dispatched.as_ref().unwrap(),
                "same-file retained result",
            );
            assert_eq!(
                client.trace.lock().unwrap().len(),
                before,
                "Restart replay must not reach the native owner"
            );
        }
        reopened.close().await.unwrap();
        proofs.push(Json::object([
            ("writerRejected", reject.into()),
            ("result", result(dispatched)),
            ("memory", memory),
            ("disk", disk),
            (
                "effects",
                Json::Number(client.effects.load(Ordering::SeqCst) as f64),
            ),
            ("trace", a(client.trace.lock().unwrap().clone())),
        ]));
        assert!(
            root.starts_with(&parent)
                && root
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("chat-command-fence-")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    if let Some(path) = std::env::var_os("IVY_CHAT_COMMANDS_FENCE_CAPTURE") {
        std::fs::write(path, canonical(&a(proofs), usize::MAX).unwrap()).unwrap();
    }
}
