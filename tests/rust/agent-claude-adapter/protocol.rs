use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_agent_manager::claude::{
    protocol::{Adapter, AdapterClock, Effect, Environment},
    store::SessionStore,
};
use ivy_store::rusqlite::{Connection, OpenFlags, types::ValueRef};

fn original(root: &Path, spec: &J) -> J {
    let mut spec = spec.clone();
    spec.insert("root", J::String(node_path::from_native(root)));
    let mut child = Command::new("node")
        .arg("tests/fixtures/agent-claude-protocol-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(&spec, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap()
}
pub(super) fn normalize(v: &J, root: &Path) -> J {
    match v {
        J::String(s) => {
            let needle = node_path::from_native(root).0;
            let mut out = vec![];
            let mut i = 0;
            while i < s.0.len() {
                if s.0[i..].starts_with(&needle) {
                    out.extend("__ROOT__".encode_utf16());
                    i += needle.len()
                } else {
                    out.push(s.0[i]);
                    i += 1
                }
            }
            J::String(JsString(out))
        }
        J::Array(a) => J::Array(a.iter().map(|v| normalize(v, root)).collect()),
        J::Object(o) => J::object(o.iter().map(|(k, v)| (k.clone(), normalize(v, root)))),
        _ => v.clone(),
    }
}
fn table(path: &Path, name: &str) -> J {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut statement = db
        .prepare(&format!("select * from {name} order by id"))
        .unwrap();
    let names = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let rows = statement
        .query_map([], |row| {
            Ok(J::object(names.iter().enumerate().map(|(i, n)| {
                let v = match row.get_ref(i).unwrap() {
                    ValueRef::Null => J::Null,
                    ValueRef::Integer(n) => J::Number(n as f64),
                    ValueRef::Real(n) => J::Number(n),
                    ValueRef::Text(s) => String::from_utf8_lossy(s).into_owned().into(),
                    ValueRef::Blob(_) => panic!("Unexpected fixture blob"),
                };
                (n.clone(), v)
            })))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    J::Array(rows)
}
struct Driver {
    adapter: Adapter,
    spec: J,
    messages: Vec<J>,
    calls: Vec<J>,
    contexts: Vec<J>,
    replies: Vec<J>,
    defer_calls: bool,
    deferred: Vec<(u64, Result<J>)>,
    watchdogs: std::collections::BTreeSet<JsString>,
}

// The Node oracle assigns these explicit fixture values through process.env.
// Its UTF-8 environment boundary replaces lone UTF-16 surrogates before the
// server reads them. Saved JSON strings take no such environment round trip.
fn environment_input(spec: &J) -> J {
    J::object(
        f(spec, "environment")
            .as_object()
            .unwrap_or(&[])
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    J::from(String::from_utf16_lossy(
                        &value
                            .as_js_string()
                            .expect("Environment fixture values must be strings.")
                            .0,
                    )),
                )
            }),
    )
}
impl Driver {
    fn effects(&mut self, effects: Vec<Effect>) {
        let mut queue = VecDeque::from(effects);
        while let Some(effect) = queue.pop_front() {
            match effect {
                Effect::Send { peer, message } => self
                    .messages
                    .push(J::object([("peer", J::String(peer)), ("message", message)])),
                Effect::Begin { turn_id } => {
                    queue.extend(self.adapter.begin_turn(&turn_id).unwrap())
                }
                Effect::Run { turn_id, context } => {
                    self.calls.push(J::object([
                        ("method", "run".into()),
                        ("turnId", J::String(turn_id)),
                    ]));
                    self.contexts.push(context)
                }
                Effect::Runtime {
                    call_id,
                    method,
                    params,
                } => {
                    let mut call = J::object([("method", method.into())]);
                    let result = match method {
                        "models" => Ok(f(&self.spec, "models").clone()),
                        "usage" => {
                            if let Some(error) = self.spec.get("usageError") {
                                Err(AdapterError::new(s(error).clone()))
                            } else {
                                Ok(f(&self.spec, "usage").clone())
                            }
                        }
                        "steer" => {
                            call.insert("threadId", f(&params, "threadId").clone());
                            call.insert("prompt", f(&params, "prompt").clone());
                            Ok(J::Null)
                        }
                        "interrupt" => {
                            call.insert("threadId", f(&params, "threadId").clone());
                            Ok(J::Null)
                        }
                        _ => panic!("Unexpected runtime method: {method}"),
                    };
                    self.calls.push(call);
                    if self.defer_calls {
                        self.deferred.push((call_id, result))
                    } else {
                        queue.extend(self.adapter.runtime_reply(call_id, result).unwrap());
                    }
                }
                Effect::Permission { callback, result } => self
                    .replies
                    .push(J::object([("callback", callback), ("result", result)])),
                Effect::Diff { turn_id, .. } => {
                    queue.extend(self.adapter.diff_result(&turn_id, "".into()).unwrap())
                }
                Effect::Watchdog { turn_id, .. } => {
                    self.watchdogs.insert(turn_id);
                }
                Effect::CancelWatchdog { turn_id } => {
                    self.watchdogs.remove(&turn_id);
                }
            }
        }
    }
}
fn native(home: &Path, spec: &J) -> J {
    for file in f(spec, "files").as_array().unwrap_or(&[]) {
        let path = home.join(f(file, "path").as_str().unwrap());
        assert!(path.starts_with(home));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            STANDARD
                .decode(f(file, "base64").as_str().unwrap())
                .unwrap(),
        )
        .unwrap()
    }
    if let Some(config) = spec.get("config") {
        fs::write(
            home.join("config.json"),
            stringify(config, usize::MAX).unwrap(),
        )
        .unwrap();
    }
    let clock_state = Arc::new(Mutex::new(
        f(spec, "now").as_f64().unwrap_or(1700000000123.0),
    ));
    let c = clock_state.clone();
    let clock: ivy_agent_manager::claude::store::Clock =
        Arc::new(move || (*c.lock().unwrap() / 1000.0).floor());
    let c = clock_state.clone();
    let millis: ivy_agent_manager::claude::store::Clock = Arc::new(move || *c.lock().unwrap());
    let id_state = Arc::new(Mutex::new(0));
    let ids_values = f(spec, "ids")
        .as_array()
        .unwrap()
        .iter()
        .map(|v| s(v).clone())
        .collect::<Vec<_>>();
    let index = id_state.clone();
    let ids: Arc<dyn Fn() -> JsString + Send + Sync> = Arc::new(move || {
        let mut i = index.lock().unwrap();
        let value = ids_values[*i].clone();
        *i += 1;
        value
    });
    let compare = Arc::new(|a: &JsString, b: &JsString| {
        ivy_agent_manager::runtime::compare_names(a, b, false)
    });
    let projects =
        ProjectStore::open(home, Style::Windows, compare, clock.clone(), ids.clone()).unwrap();
    let db = home.join("state.sqlite");
    let store = SessionStore::with_clock(home, &db, clock.clone()).unwrap();
    let mut process_environment = v(
        r#"{"CLAUDE_CODEX_DEFAULT_MODEL":"sonnet","CLAUDE_CODEX_DEFAULT_EFFORT":"medium","CLAUDE_CODEX_DISABLE_CODEX_PROXY":"1"}"#,
    );
    for (key, value) in environment_input(spec).as_object().unwrap_or(&[]) {
        process_environment.insert(key.clone(), value.clone());
    }
    let env = Environment {
        environment: process_environment,
        discovered_router_models: vec![],
        cwd: "C:/fixture".into(),
        codex_home: node_path::from_native(home),
        platform_os: "windows".into(),
        cpu: "x86_64".into(),
        compat_version: "0.142.3".into(),
        version_suffix: "claude-codex".into(),
        default_model: "sonnet".into(),
        default_effort: "medium".into(),
        mcp_servers: J::Null,
        startup_statuses: vec![],
        config_overrides: J::object([] as [(&str, J); 0]),
        config_path: home.join("config.json"),
        prepare_worktree: Arc::new(|_, cwd| Ok(cwd.clone())),
        read_base64: Arc::new(|path| {
            Ok(STANDARD
                .encode(fs::read(node_path::to_native(path))?)
                .into())
        }),
        subagent_completed: None,
        subagent_watchdog_ms: Some(
            f(f(spec, "environment"), "CLAUDE_CODEX_SUBAGENT_TIMEOUT_MS")
                .as_str()
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1_800_000),
        ),
    };
    let mut driver = Driver {
        adapter: Adapter::new(
            store,
            projects,
            RateLimitStore::open(home),
            env,
            AdapterClock {
                seconds: clock,
                millis,
            },
            ids,
        ),
        spec: spec.clone(),
        messages: vec![],
        calls: vec![],
        contexts: vec![],
        replies: vec![],
        defer_calls: false,
        deferred: vec![],
        watchdogs: std::collections::BTreeSet::new(),
    };
    let mut records = vec![];
    for raw_step in f(spec, "steps").as_array().unwrap() {
        let step = super::sdk_cases::replace(raw_step, home);
        let step = &step;
        if let Some(now) = step.get("now") {
            *clock_state.lock().unwrap() = now.as_f64().unwrap()
        }
        let action = f(step, "action").as_str().unwrap();
        let peer = f(step, "peer")
            .as_js_string()
            .cloned()
            .unwrap_or_else(|| "p1".into());
        let result = match action.as_str() {
            "request" | "reply" | "notification" => {
                driver.adapter.receive(&peer, f(step, "message"))
            }
            "batch" => {
                driver.defer_calls = true;
                for message in f(step, "messages").as_array().unwrap() {
                    let effects = driver.adapter.receive(&peer, message).unwrap();
                    driver.effects(effects)
                }
                driver.defer_calls = false;
                let deferred = std::mem::take(&mut driver.deferred);
                for (call_id, result) in deferred {
                    let effects = driver.adapter.runtime_reply(call_id, result).unwrap();
                    driver.effects(effects)
                }
                Ok(vec![])
            }
            "event" => driver
                .adapter
                .runtime_event(s(f(step, "turnId")), f(step, "event")),
            "permission" => driver.adapter.permission_request(
                s(f(step, "turnId")),
                f(step, "event"),
                f(step, "callback").clone(),
            ),
            "input" => driver.adapter.user_input_request(
                s(f(step, "turnId")),
                f(step, "event"),
                f(step, "callback").clone(),
            ),
            "finish" => driver.adapter.runtime_finished(
                s(f(step, "turnId")),
                if let Some(error) = step.get("error") {
                    Err(AdapterError::new(s(error).clone()))
                } else {
                    Ok(())
                },
            ),
            "disconnect" => {
                driver.adapter.close_peer(&peer);
                Ok(vec![])
            }
            "sql" => {
                let sql = f(step, "sql").as_str().unwrap();
                assert!(sql.starts_with("UPDATE threads SET status_json="));
                Connection::open(&db).unwrap().execute_batch(&sql).unwrap();
                Ok(vec![])
            }
            "wait" => {
                let waiting = driver.watchdogs.iter().cloned().collect::<Vec<_>>();
                for turn in waiting {
                    let thread = driver
                        .contexts
                        .iter()
                        .find(|c| f(c, "turnId").as_js_string() == Some(&turn))
                        .map(|c| f(c, "threadId").clone())
                        .unwrap();
                    let effects = driver.adapter.watchdog_elapsed(&turn).unwrap();
                    driver.effects(effects);
                    driver.calls.push(J::object([
                        ("method", "interrupt".into()),
                        ("threadId", thread),
                    ]));
                }
                Ok(vec![])
            }
            "stop" => {
                driver.calls.push(J::object([("method", "stop".into())]));
                driver.adapter.stop().map(|_| vec![])
            }
            _ => panic!("Unexpected action"),
        };
        let failure = match result {
            Ok(effects) => {
                driver.effects(effects);
                J::Null
            }
            Err(e) => J::object([("name", e.name.into()), ("message", J::String(e.message))]),
        };
        records.push(J::object([
            ("action", action.into()),
            ("failure", failure),
            (
                "state",
                J::object([
                    ("threads", table(&db, "threads")),
                    ("turns", table(&db, "turns")),
                    ("messages", J::Array(driver.messages.clone())),
                    ("calls", J::Array(driver.calls.clone())),
                    ("contexts", J::Array(driver.contexts.clone())),
                    ("replies", J::Array(driver.replies.clone())),
                    ("idCalls", J::Number(*id_state.lock().unwrap() as f64)),
                    ("active", driver.adapter.has_active_turns().into()),
                ]),
            ),
        ]));
    }
    driver.adapter.stop().unwrap();
    J::Array(records)
}
fn paired(label: &str, spec: J) {
    let root = Root::new();
    let old = root.0.join("original");
    let new = root.0.join("native");
    fs::create_dir(&old).unwrap();
    fs::create_dir(&new).unwrap();
    let expected = original(&old, &spec);
    assert_eq!(
        environment_input(&spec),
        f(&expected, "environment").clone(),
        "actual Node environment admission: {label}"
    );
    let actual = native(&new, &spec);
    let expected = f(&expected, "records").clone();
    compare(label, normalize(&actual, &new), normalize(&expected, &old));
    if spec.get("config").is_some()
        || f(&spec, "files").as_array().is_some_and(|files| {
            files
                .iter()
                .any(|file| f(file, "path") == &J::from("config.json"))
        })
    {
        assert_eq!(
            fs::read(new.join("config.json")).ok(),
            fs::read(old.join("config.json")).ok(),
            "persisted config bytes: {label}"
        );
    }
}

#[test]
fn corrupt_store_refuses_reads_without_rewriting_original_bytes() {
    for input in [
        "{",
        "[invalid]",
        "😀",
        "x12345678😀context beyond twenty-one",
        "{\r\n\"😀\":true,\r\nbad}",
    ]
    .into_iter()
    {
        let sql = format!(
            "UPDATE threads SET status_json='{}' WHERE id='t1'",
            input.replace('\'', "''")
        );
        let mut steps = vec![
            v(
                r#"{"action":"request","message":{"id":"start","method":"thread/start","params":{"cwd":"C:/fixture"}}}"#,
            ),
            J::object([("action", "sql".into()), ("sql", sql.into())]),
        ];
        for method in ["thread/read", "thread/list", "thread/resume"] {
            steps.push(J::object([
                ("action", "request".into()),
                (
                    "message",
                    J::object([
                        ("id", method.into()),
                        ("method", method.into()),
                        ("params", J::object([("threadId", "t1".into())])),
                    ]),
                ),
            ]));
        }
        let spec = J::object([
            ("ids", J::Array(vec!["t1".into()])),
            ("steps", J::Array(steps)),
        ]);
        let root = Root::new();
        let new = root.0.join("native");
        fs::create_dir(&new).unwrap();
        let actual = native(&new, &spec);
        let state = f(actual.as_array().unwrap().last().unwrap(), "state");
        assert_eq!(
            f(&f(state, "threads").as_array().unwrap()[0], "status_json"),
            &J::from(input)
        );
        let messages = f(state, "messages").as_array().unwrap();
        let mut errors = 0;
        for item in messages {
            let message = f(item, "message");
            if ["thread/read", "thread/list", "thread/resume"]
                .iter()
                .any(|id| f(message, "id") == &J::from(*id))
            {
                let error = f(message, "error");
                assert_eq!(f(error, "code"), &J::Number(-32000.0));
                assert!(!s(f(error, "message")).0.is_empty());
                errors += 1;
            }
        }
        assert_eq!(errors, 3);
    }
}

#[test]
fn provider_loop_config_read_matches_original_without_injected_projection() {
    let rows = parse(
        include_str!("../../fixtures/agent-claude-config-cases.json"),
        usize::MAX,
    )
    .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 35);
    let mut rows = rows.as_array().unwrap().to_vec();
    rows.push(v(r#"{"config":{"overrides":{"provider_loop_provider":"unknown\ud800","provider_loop_agent_loop":"TOKEN=syntheticFixture123 \udfff"}}}"#));
    rows.push(v(r#"{"config":{"overrides":{"providerLoopProvider":"API_KEY=syntheticFixture123 ghp_syntheticFixture123","providerLoopAgentLoop":"sk-ant-syntheticFixture123 -----BEGIN RSA PRIVATE KEY-----\ud800\nfixture-----END RSA PRIVATE KEY-----"}}}"#));
    for (index, mut spec) in rows.into_iter().enumerate() {
        spec.insert("ids", J::Array(vec![]));
        spec.insert(
            "steps",
            v(r#"[{"action":"request","message":{"id":"config","method":"config/read"}}]"#),
        );
        paired(&format!("provider/loop config {index}"), spec);
    }
}

#[test]
fn persisted_config_defaults_repair_and_ignored_invalid_bytes_match_original() {
    let mut rows = vec![
        v(
            r#"{"config":{"model":"custom-model\ud800","model_reasoning_effort":"minimal","overrides":{"providerLoopProvider":"codex","approval_policy":"never","tools":{"fixture":true},"unmanaged":{"0":"zero","2":"two","quote":"\"\\"}}}}"#,
        ),
        v(
            r#"{"config":{"model":"gpt-fixture","model_reasoning_effort":"high","overrides":{"provider_loop_provider":"codex","provider_loop_agent_loop":"native-claude-code-sdk","unmanaged":[{},[],"\udfff"]}}}"#,
        ),
        v(
            r#"{"config":{"model":"   ","model_reasoning_effort":"invalid","overrides":{"sandbox_mode":null,"service_tier":false}}}"#,
        ),
        v(
            r#"{"environment":{"CLAUDE_CODEX_DEFAULT_MODEL":"custom-default","CLAUDE_CODEX_DEFAULT_EFFORT":"xhigh"},"config":{"model":"codex-disabled","overrides":[]}}"#,
        ),
        v(
            r#"{"environment":{"CLAUDE_CODEX_DEFAULT_EFFORT":"invalid"},"config":{"model":7,"model_reasoning_effort":null,"overrides":false}}"#,
        ),
        v(
            r#"{"environment":{"CLAUDE_CODEX_DEFAULT_MODEL":"","CLAUDE_CODEX_MODELS":"[{\"id\":\"fixture-default\",\"isDefault\":true},{\"model\":\"other\"}]"},"config":{"model":"gpt-disabled","model_reasoning_effort":"max","overrides":{"providerLoopAgentLoop":"native-claude-code-sdk"}}}"#,
        ),
        v(
            r#"{"environment":{"CLAUDE_CODEX_DEFAULT_MODEL":"","CLAUDE_CODEX_MODELS":" alpha ,beta "},"config":{"model":"gpt-disabled"}}"#,
        ),
        v(
            r#"{"environment":{"CLAUDE_CODEX_DEFAULT_MODEL":"","CLAUDE_CODEX_MODELS":"[]"},"config":{"model":"gpt-disabled"}}"#,
        ),
        v(
            r#"{"config":{"model":"","model_reasoning_effort":"low","overrides":{"providerLoopProvider":7,"provider_loop_config.providerId":"codex"}}}"#,
        ),
    ];
    rows.push(J::object([(
        "files",
        J::Array(vec![J::object([
            ("path", "config.json".into()),
            ("base64", STANDARD.encode(b"{invalid saved config").into()),
        ])]),
    )]));
    for (index, mut spec) in rows.into_iter().enumerate() {
        spec.insert("ids", J::Array(vec![]));
        spec.insert("steps",v(r#"[{"action":"request","message":{"id":1,"method":"config/read"}},{"action":"request","message":{"id":2,"method":"collaborationMode/list"}}]"#));
        paired(&format!("saved config {index}"), spec);
    }
}

#[test]
fn managed_provider_selection_routes_sdk_and_blocks_codex_before_dispatch() {
    for (index, environment) in [
        v(r#"{"CLAUDE_CODEX_PROVIDER":"codex"}"#),
        v(r#"{"CLAUDE_CODEX_RUNTIME_TYPE":"codex-exec"}"#),
        v(r#"{"CLAUDE_CODEX_PROVIDER":"claude-code","CLAUDE_CODEX_RUNTIME_TYPE":"sdk"}"#),
    ]
    .into_iter()
    .enumerate()
    {
        let mut steps = v(r#"[
          {"action":"request","message":{"id":1,"method":"config/read"}},
          {"action":"request","message":{"id":2,"method":"thread/start","params":{"cwd":"C:/fixture"}}},
          {"action":"request","message":{"id":3,"method":"turn/start","params":{"threadId":"id0","input":[{"type":"text","text":"fixture"}]}}}
        ]"#).as_array().unwrap().to_vec();
        if index == 2 {
            steps.push(v(r#"{"action":"event","turnId":"id1","event":{"type":"text_delta","delta":"answer"}}"#));
            steps.push(v(r#"{"action":"finish","turnId":"id1"}"#));
        }
        steps.push(v(r#"{"action":"request","message":{"id":4,"method":"thread/start","params":{"model":"gpt-disabled"}}}"#));
        paired(
            &format!("managed route {index}"),
            J::object([
                (
                    "ids",
                    J::Array((0..20).map(|i| format!("id{i}").into()).collect()),
                ),
                ("environment", environment),
                ("steps", J::Array(steps)),
            ]),
        );
    }
}
#[test]
fn native_protocol_history_and_turn_order_match_frozen_server() {
    paired(
        "native request and turn order",
        v(
            r#"{"ids":["t1","turn1","user1","a1","r1","a2","turn2","user2","plan1"],"models":[{"value":"sonnet","displayName":"Sonnet","description":"fixture"}],"steps":[
      {"action":"request","message":{"id":1,"method":"initialize","params":{"clientInfo":{"name":"fixture","version":"1"}}}},
      {"action":"request","message":{"id":2,"method":"thread/start","params":{"cwd":"C:/fixture"}}},
      {"action":"request","message":{"id":3,"method":"turn/start","params":{"threadId":"t1","input":[{"type":"text","text":"hello"}]}}},
      {"action":"event","turnId":"turn1","event":{"type":"text_delta","delta":"Hello\ud800"}},
      {"action":"event","turnId":"turn1","event":{"type":"reasoning_delta","delta":"Think"}},
      {"action":"event","turnId":"turn1","event":{"type":"text_delta","delta":"Done"}},
      {"action":"event","turnId":"turn1","event":{"type":"completed","success":true,"claudeSessionId":"session-id"}},
      {"action":"finish","turnId":"turn1"},
      {"action":"request","message":{"id":"history","method":"thread/read","params":{"threadId":"t1"}}},
      {"action":"request","message":{"id":5,"method":"thread/turns/list","params":{"threadId":"t1"}}},
      {"action":"request","message":{"id":6,"method":"thread/items/list","params":{"threadId":"t1","limit":2,"sortDirection":"desc"}}},
      {"action":"request","peer":"p2","message":{"id":7,"method":"thread/resume","params":{"threadId":"t1","excludeTurns":true}}},
      {"action":"request","message":{"id":8,"method":"turn/start","params":{"threadId":"t1","collaborationMode":{"mode":"plan"},"input":[{"type":"text","text":"plan"}]}}},
      {"action":"event","turnId":"turn2","event":{"type":"text_delta","delta":"A plan"}},
      {"action":"request","message":{"id":9,"method":"turn/interrupt","params":{"threadId":"t1"}}},
      {"action":"event","turnId":"turn2","event":{"type":"text_delta","delta":"late ignored"}},
      {"action":"finish","turnId":"turn2"},
      {"action":"request","message":{"id":10,"method":"thread/list"}},
      {"action":"request","message":{"id":11,"method":"thread/loaded/list"}},
      {"action":"request","message":{"id":12,"method":"model/list"}},
      {"action":"request","message":{"id":13,"method":"collaborationMode/list"}},
      {"action":"request","message":{"id":14,"method":"permissionProfile/list","params":{"limit":1,"cursor":"1"}}},
      {"action":"request","message":{"id":15,"method":"account/rateLimits/read"}},
      {"action":"request","message":{"id":16,"method":"thread/archive","params":{"threadId":"t1"}}},
      {"action":"request","message":{"id":17,"method":"thread/unarchive","params":{"threadId":"t1"}}},
      {"action":"stop"}
    ]}"#,
        ),
    );
}
#[test]
fn native_approvals_steering_tools_and_reconnect_match_frozen_server() {
    paired(
        "native reverse requests and tools",
        v(
            r#"{"ids":["t1","turn1","user1","command1","approval1","steer1","command2","edit1","approval2","ask1","question1","hookRun","hook1","read1","web1","spare"],"steps":[
      {"action":"request","message":{"id":1,"method":"thread/start","params":{"cwd":"C:/fixture","permissions":":workspace"}}},
      {"action":"request","message":{"id":2,"method":"turn/start","params":{"threadId":"t1","input":[{"type":"text","text":"work"}]}}},
      {"action":"permission","turnId":"turn1","callback":"cb1","event":{"toolUseId":"bash1","toolName":"Bash","input":{"command":"echo fixture","cwd":"C:/fixture"}}},
      {"action":"request","message":{"id":3,"method":"turn/steer","params":{"threadId":"t1","expectedTurnId":"wrong","input":[]}}},
      {"action":"request","message":{"id":4,"method":"turn/steer","params":{"threadId":"t1","expectedTurnId":"turn1","input":[{"type":"text","text":"also this"}]}}},
      {"action":"request","peer":"p2","message":{"id":5,"method":"thread/resume","params":{"threadId":"t1"}}},
      {"action":"reply","peer":"p2","message":{"id":"approval1","result":{"decision":"acceptForSession"}}},
      {"action":"event","turnId":"turn1","event":{"type":"tool_use","toolUseId":"bash1","toolName":"Bash","input":{"command":"echo fixture","cwd":"C:/fixture"}}},
      {"action":"event","turnId":"turn1","event":{"type":"tool_result","toolUseId":"bash1","content":"ok\nExit code: 7"}},
      {"action":"permission","turnId":"turn1","callback":"cb2","event":{"toolUseId":"bash2","toolName":"Bash","input":{"command":"echo fixture","cwd":"C:/fixture"}}},
      {"action":"event","turnId":"turn1","event":{"type":"tool_output_delta","toolUseId":"bash2","delta":"partial"}},
      {"action":"event","turnId":"turn1","event":{"type":"tool_result","toolUseId":"bash2","content":"complete","isError":true}},
      {"action":"permission","turnId":"turn1","callback":"cb3","event":{"toolUseId":"edit","toolName":"Edit","input":{"file_path":"C:/fixture/a","old_string":"old","new_string":"new"}}},
      {"action":"reply","peer":"p1","message":{"id":"approval2","result":{"decision":"accept"}}},
      {"action":"event","turnId":"turn1","event":{"type":"tool_result","toolUseId":"edit","content":"changed"}},
      {"action":"input","turnId":"turn1","callback":"cb4","event":{"toolUseId":"ask","questions":[{"id":"q0","header":"Pick","question":"Which?","isOther":false,"isSecret":false,"options":[{"label":"A","description":"first"}]}]}},
      {"action":"reply","peer":"p2","message":{"id":"question1","result":{"answers":{"q0":{"answers":["Other"],"notes":"custom"}}}}},
      {"action":"event","turnId":"turn1","event":{"type":"notice","level":"warning","message":"fixture warning"}},
      {"action":"event","turnId":"turn1","event":{"type":"notice","level":"warning","message":"fixture warning"}},
      {"action":"event","turnId":"turn1","event":{"type":"usage","usage":{"input_tokens":10,"cache_creation_input_tokens":3,"cache_read_input_tokens":7,"output_tokens":5}}},
      {"action":"event","turnId":"turn1","event":{"type":"usage","usage":{"input_tokens":2,"output_tokens":1}}},
      {"action":"event","turnId":"turn1","event":{"type":"hook","hookName":"PreToolUse","status":"ok","decision":"allow","message":"fixture hook"}},
      {"action":"event","turnId":"turn1","event":{"type":"tool_use","toolUseId":"read","toolName":"Read","input":{"file_path":"C:/fixture/file.txt"}}},
      {"action":"event","turnId":"turn1","event":{"type":"tool_result","toolUseId":"read","content":{"data":"value"}}},
      {"action":"event","turnId":"turn1","event":{"type":"tool_use","toolUseId":"web","toolName":"WebSearch","input":{"query":"fixture"}}},
      {"action":"event","turnId":"turn1","event":{"type":"tool_result","toolUseId":"web","content":"https://example.invalid/a"}},
      {"action":"finish","turnId":"turn1"},
      {"action":"request","message":{"id":6,"method":"thread/read","params":{"threadId":"t1"}}}
    ]}"#,
        ),
    );
}
#[test]
fn bridge_callbacks_controls_and_discovery_close_without_provider() {
    let output = Command::new("node")
        .args(["tests/fixtures/agent-claude-bridge-fake.mjs", "--check"])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    assert_eq!(f(&value, "queries"), &J::Number(6.0));
    assert_eq!(f(&value, "allClosedOnce"), &J::Bool(true));
    compare("private SDK bridge lifetime", value.clone(), value);
}
#[test]
fn subagent_completion_capabilities_and_terminal_cleanup_match_frozen_server() {
    for modern in [false, true] {
        for terminal in ["result", "finish", "interrupt", "stop"] {
            let mut spec = J::object([
                (
                    "ids",
                    J::Array((0..40).map(|i| format!("id{i}").into()).collect()),
                ),
                ("steps", J::Array(vec![])),
            ]);
            let mut steps = vec![
                v(&format!(
                    r#"{{"action":"request","message":{{"id":0,"method":"initialize","params":{{"capabilities":{{"subAgentActivityCompleted":{modern}}}}}}}}}"#
                )),
                v(
                    r#"{"action":"request","message":{"id":1,"method":"thread/start","params":{"cwd":"C:/fixture","permissions":":workspace"}}}"#,
                ),
                v(
                    r#"{"action":"request","message":{"id":2,"method":"turn/start","params":{"threadId":"id0","input":[{"type":"text","text":"delegate"}]}}}"#,
                ),
                v(
                    r#"{"action":"event","turnId":"id1","event":{"type":"tool_use","toolUseId":"task","toolName":"Agent","input":{"prompt":"child prompt","subagent_type":"general-purpose","model":"haiku"}}}"#,
                ),
                v(
                    r#"{"action":"event","turnId":"id1","event":{"type":"text_delta","delta":"hidden child text"}}"#,
                ),
                v(
                    r#"{"action":"request","message":{"id":3,"method":"thread/read","params":{"threadId":"id3","includeTurns":false}}}"#,
                ),
                v(
                    r#"{"action":"request","peer":"p2","message":{"id":4,"method":"thread/resume","params":{"threadId":"id0","excludeTurns":true}}}"#,
                ),
            ];
            if terminal == "result" {
                steps.push(v(r#"{"action":"event","turnId":"id1","event":{"type":"tool_result","toolUseId":"task","content":"child result\nagentId: abcdef12 (continue this agent)\n<usage>total_tokens: 42\ntool_uses: 3\nduration_ms: 1200</usage>"}}"#));
                steps.push(v(r#"{"action":"finish","turnId":"id1"}"#));
            } else if terminal == "interrupt" {
                steps.push(v(r#"{"action":"request","message":{"id":5,"method":"turn/interrupt","params":{"threadId":"id0"}}}"#));
            } else {
                steps.push(J::object([
                    ("action", terminal.into()),
                    ("turnId", "id1".into()),
                ]));
            }
            steps.push(v(r#"{"action":"request","message":{"id":6,"method":"thread/read","params":{"threadId":"id0"}}}"#));
            // A stopped server has closed its original database and intentionally
            // accepts no further native request. Stop is the last fixture action.
            if terminal == "stop" {
                steps.pop();
            }
            spec.insert("steps", J::Array(steps));
            paired(&format!("subagent {modern}/{terminal}"), spec);
        }
    }
}
#[test]
fn native_project_replay_config_and_binary_file_reads_match_frozen_server() {
    paired(
        "project config and file native methods",
        v(
            r#"{"ids":["p1","p2","t1","temporary"],"files":[{"path":"sample.bin","base64":"AAECA/8="}],"steps":[
      {"action":"request","message":{"id":null,"method":"config/read"}},
      {"action":"request","message":{"id":1,"method":"project/create","params":{"idempotencyKey":"create1","name":"One","roots":[{"path":"C:/fixture"}],"metadata":{"z":"last","a":"first"}}}},
      {"action":"request","message":{"id":2,"method":"project/create","params":{"idempotencyKey":"create1","name":"One","roots":[{"path":"C:/fixture"}],"metadata":{"a":"first","z":"last"}}}},
      {"action":"request","message":{"id":3,"method":"project/create","params":{"idempotencyKey":"create1","name":"different","roots":[]}}},
      {"action":"request","message":{"id":4,"method":"project/create","params":{"idempotencyKey":"create2","name":"Two","roots":[]}}},
      {"action":"request","message":{"id":5,"method":"project/list"}},
      {"action":"request","message":{"id":6,"method":"thread/start","params":{"projectId":"p1"}}},
      {"action":"request","message":{"id":7,"method":"thread/start","params":{"projectId":"p1","ephemeral":true}}},
      {"action":"request","message":{"id":8,"method":"project/read","params":{"projectId":"p1"}}},
      {"action":"request","message":{"id":9,"method":"thread/list","params":{"projectId":"p1"}}},
      {"action":"request","message":{"id":10,"method":"project/update","params":{"projectId":"p1","name":"Updated","position":2}}},
      {"action":"request","message":{"id":11,"method":"fs/readFile","params":{"filePath":"__ROOT__/sample.bin"}}},
      {"action":"request","message":{"id":12,"method":"project/delete","params":{"projectId":"p1"}}},
      {"action":"request","message":{"id":13,"method":"project/create","params":{"idempotencyKey":"create1","name":"One","roots":[{"path":"C:/fixture"}],"metadata":{"a":"first","z":"last"}}}},
      {"action":"request","message":{"id":14,"method":"thread/read","params":{"threadId":"t1"}}},
      {"action":"request","message":{"id":15,"method":"thread/items/list","params":{"threadId":"t1","cursor":"0"}}},
      {"action":"request","message":{"id":16,"method":"permissionProfile/list","params":{"cursor":"bad"}}}
      ,{"action":"request","message":{"id":17,"method":"fixture/missing"}}
    ]}"#,
        ),
    );
}
#[test]
fn concurrent_usage_refresh_replies_in_request_order_and_reuses_the_clock() {
    for error in [false, true] {
        let mut spec = v(
            r#"{"ids":[],"usage":{"five_hour":{"utilization":25,"resets_at":1700001000}},"steps":[
      {"action":"batch","messages":[{"id":"first","method":"account/rateLimits/read"},{"id":"second","method":"account/rateLimits/read"}]},
      {"action":"batch","messages":[{"id":"cached1","method":"account/rateLimits/read"},{"id":"cached2","method":"account/rateLimits/read"}]},
      {"action":"batch","now":1700000060122,"messages":[{"id":"before","method":"account/rateLimits/read"}]},
      {"action":"batch","now":1700000060123,"messages":[{"id":"refresh1","method":"account/rateLimits/read"},{"id":"refresh2","method":"account/rateLimits/read"}]}
    ]}"#,
        );
        if error {
            spec.insert("usageError", "fixture unavailable".into())
        }
        paired(&format!("usage refresh coalescing error={error}"), spec)
    }
}

#[test]
fn workflow_local_listing_empty_summary_and_metrics_match_frozen_server() {
    paired(
        "bare workflows local listing",
        v(r#"{"ids":["id0","id1","id2","id3"],"steps":[
        {"action":"request","message":{"id":1,"method":"thread/start","params":{"cwd":"C:/fixture"}}},
        {"action":"request","message":{"id":2,"method":"turn/start","params":{"threadId":"id0","input":[{"type":"text","text":"\u2003/workflows\r\n"}]}}},
        {"action":"request","message":{"id":3,"method":"thread/read","params":{"threadId":"id0","includeTurns":true}}}
    ]}"#),
    );
    paired(
        "workflow history child status and local listing",
        v(
            r#"{"ids":["id0","id1","id2","id3","id4","id5","id6","id7","id8","id9","id10","id11"],"steps":[
        {"action":"request","message":{"id":1,"method":"thread/start","params":{"cwd":"C:/fixture"}}},
        {"action":"request","message":{"id":2,"method":"turn/start","params":{"threadId":"id0","input":[{"type":"text","text":"/workflows First\r\n  task \ud800"}]}}},
        {"action":"event","turnId":"id1","event":{"type":"tool_use","toolUseId":"child","toolName":"Agent","input":{"prompt":"work","subagent_type":"workflow"}}},
        {"action":"event","turnId":"id1","event":{"type":"tool_result","toolUseId":"child","content":"individual\nagentId: abcd1234"}},
        {"action":"finish","turnId":"id1"},
        {"action":"request","message":{"id":3,"method":"turn/start","params":{"threadId":"id0","input":[{"type":"text","text":"/workflows"}]}}},
        {"action":"request","message":{"id":4,"method":"thread/read","params":{"threadId":"id0","includeTurns":true}}}
    ]}"#,
        ),
    );
    for (index,schema) in [
        r#"{"type":"string"}"#,
        r#"{"type":"array","items":{"type":"string"}}"#,
        r#"{"type":"object","properties":{"title":{"type":"string"},"details":{"type":"array","items":{"type":"string"}},"count":{"type":"number"}},"required":["title","missing","count","details"]}"#,
        r#"{"type":"object","properties":{"meta":{"type":"object"},"1":{"type":"string"},"nested":{"type":"object","properties":{"value":{"type":"string"}}}},"required":["meta","nested"]}"#,
        r#"{"type":"array","items":{"type":"object","properties":{"title":{"type":"string"}}}}"#,
        r#"{"type":"json_schema","schema":{"type":"string"}}"#,
        "[]",
        "false",
        r#"{"type":"object","properties":{"only":{"type":"string"}},"required":[]}"#,
    ].iter().enumerate() {
        let schema=v(schema);
        let mut params=v(r#"{"threadId":"id0","input":[{"type":"text","text":"Context\n- 1. First\n2) Title：  Final\ud800"}]}"#);
        params.insert("outputSchema",schema);
        if index==0 {params.insert("planMode",true.into());}
        let spec=J::object([
            ("ids",J::Array((0..15).map(|i|format!("id{i}").into()).collect())),
            ("steps",J::Array(vec![
                v(r#"{"action":"request","message":{"id":1,"method":"thread/start","params":{"cwd":"C:/fixture"}}}"#),
                J::object([("action","request".into()),("message",J::object([("id",J::Number(2.0)),("method","turn/start".into()),("params",params)]))]),
                v(r#"{"action":"event","turnId":"id1","event":{"type":"metrics","apiDurationMs":7,"numTurns":3,"costUsd":0.25}}"#),
                v(r#"{"action":"event","turnId":"id1","event":{"type":"metrics","apiDurationMs":null,"numTurns":1,"costUsd":0.125}}"#),
                v(r#"{"action":"finish","turnId":"id1"}"#),
                v(r#"{"action":"request","message":{"id":3,"method":"thread/read","params":{"threadId":"id0","includeTurns":true}}}"#),
            ])),
        ]);
        paired(&format!("empty structured fallback and transient metrics {index}"),spec);
    }
}

#[test]
fn effective_turn_model_effort_and_sdk_configuration_match_frozen_server() {
    for (index, params, environment) in [
        (
            0,
            r#"{"model":"claude-opus","effort":"burst"}"#,
            r#"{"CLAUDE_CODEX_EFFORT_ALIASES":"{\"burst\":\"xhigh\"}","CLAUDE_CODEX_ALLOWED_TOOLS":" Bash, Read ,, ","CLAUDE_CODEX_ADD_DIRS":"[\"one\",\"two\"]","CLAUDE_CODEX_ENABLE_FILE_CHECKPOINTING":"1"}"#,
        ),
        (
            1,
            r#"{"model":"DEFAULT","effort":" low "}"#,
            r#"{"CLAUDE_CODEX_MODEL_ALIASES":"{\"DEFAULT\":\"router-\\ud800\"}","CLAUDE_CODEX_ADD_DIRS":" first,second , ","CLAUDE_CODEX_ALLOWED_TOOLS":"default"}"#,
        ),
        (
            2,
            r#"{"model":"GLM-5.3","config":{"model_reasoning_effort":"max"}}"#,
            r#"{"CLAUDE_CODEX_MODELS":"sonnet,glm-5.3","CLAUDE_CODEX_MODEL_ALIASES":"{invalid","CLAUDE_CODEX_ALLOWED_TOOLS":"*","CLAUDE_CODEX_ADD_DIRS":"[\"extra\"]"}"#,
        ),
        (
            3,
            r#"{"model":"claude-haiku","collaborationMode":{"mode":"default","settings":{"model":"claude-opus","reasoning_effort":"minimal"}}}"#,
            "{}",
        ),
        (
            4,
            r#"{"model":"default","outputSchema":{"type":"string"}}"#,
            r#"{"CLAUDE_CODEX_SUMMARY_MODEL":"title-model"}"#,
        ),
        (5, r#"{"model":"custom","effort":"unknown"}"#, "{}"),
    ] {
        let mut params = v(params);
        params.insert("threadId", "id0".into());
        params.insert("input", v(r#"[{"type":"text","text":"Prompt"}]"#));
        paired(
            &format!("effective runtime configuration {index}"),
            J::object([
                (
                    "ids",
                    J::Array((0..10).map(|i| format!("id{i}").into()).collect()),
                ),
                ("environment", v(environment)),
                (
                    "steps",
                    J::Array(vec![
                        v(
                            r#"{"action":"request","message":{"id":1,"method":"thread/start","params":{"cwd":"C:/fixture"}}}"#,
                        ),
                        J::object([
                            ("action", "request".into()),
                            (
                                "message",
                                J::object([
                                    ("id", J::Number(2.0)),
                                    ("method", "turn/start".into()),
                                    ("params", params),
                                ]),
                            ),
                        ]),
                        v(
                            r#"{"action":"event","turnId":"id1","event":{"type":"text_delta","delta":"answer"}}"#,
                        ),
                        v(r#"{"action":"finish","turnId":"id1"}"#),
                    ]),
                ),
            ]),
        );
    }
}

#[test]
fn workflow_watchdog_pending_launch_and_child_gaps_match_frozen_server() {
    for stage in ["launch", "between-children", "launch-returned"] {
        let mut steps = vec![
            v(
                r#"{"action":"request","message":{"id":1,"method":"thread/start","params":{"cwd":"C:/fixture"}}}"#,
            ),
            v(
                r#"{"action":"request","message":{"id":2,"method":"turn/start","params":{"threadId":"id0","input":[{"type":"text","text":"workflow"}]}}}"#,
            ),
            v(
                r#"{"action":"event","turnId":"id1","event":{"type":"tool_use","toolUseId":"workflow","toolName":"Workflow","input":{}}}"#,
            ),
        ];
        if stage == "between-children" {
            steps.push(v(r#"{"action":"event","turnId":"id1","event":{"type":"tool_use","toolUseId":"child","toolName":"Agent","input":{"prompt":"child"}}}"#));
            steps.push(v(r#"{"action":"event","turnId":"id1","event":{"type":"tool_result","toolUseId":"child","content":"done"}}"#));
        }
        if stage == "launch-returned" {
            steps.push(v(r#"{"action":"event","turnId":"id1","event":{"type":"tool_result","toolUseId":"workflow","content":"async launched"}}"#));
        }
        steps.push(v(r#"{"action":"wait","millis":1200}"#));
        if stage != "launch-returned" {
            steps.push(v(r#"{"action":"event","turnId":"id1","event":{"type":"tool_use","toolUseId":"late-child","toolName":"Agent","input":{"prompt":"stale"}}}"#));
        }
        steps.push(v(r#"{"action":"request","message":{"id":3,"method":"thread/read","params":{"threadId":"id0","includeTurns":true}}}"#));
        paired(
            &format!("workflow watchdog phase {stage}"),
            J::object([
                (
                    "ids",
                    J::Array((0..40).map(|i| format!("id{i}").into()).collect()),
                ),
                (
                    "environment",
                    v(r#"{"CLAUDE_CODEX_SUBAGENT_TIMEOUT_MS":"1000"}"#),
                ),
                ("steps", J::Array(steps)),
            ]),
        );
    }
}
