use super::protocol_cases::normalize;
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_agent_manager::claude::{
    ask::{self, Callback},
    sdk_options::SdkEnvironment,
};
pub(super) fn replace(v: &J, root: &Path) -> J {
    match v {
        J::String(s) => {
            let mut out = vec![];
            let needle = JsString::from("__ROOT__");
            let mut i = 0;
            while i < s.0.len() {
                if s.0[i..].starts_with(&needle.0) {
                    out.extend(node_path::from_native(root).0);
                    i += needle.0.len()
                } else {
                    out.push(s.0[i]);
                    i += 1
                }
            }
            J::String(JsString(out))
        }
        J::Array(a) => J::Array(a.iter().map(|v| replace(v, root)).collect()),
        J::Object(o) => J::object(o.iter().map(|(k, v)| (k.clone(), replace(v, root)))),
        _ => v.clone(),
    }
}
fn normalized(v: &J, root: &Path) -> J {
    let v = normalize(v, root);
    let needle = node_path::from_native(root)
        .0
        .into_iter()
        .map(|u| {
            if (48..=57).contains(&u)
                || (65..=90).contains(&u)
                || (97..=122).contains(&u)
                || u == 45
            {
                u
            } else {
                45
            }
        })
        .collect::<Vec<_>>();
    fn visit(v: &J, needle: &[u16]) -> J {
        match v {
            J::String(s) => {
                let mut out = vec![];
                let mut i = 0;
                while i < s.0.len() {
                    if s.0[i..].starts_with(needle) {
                        out.extend("__ROOT__".encode_utf16());
                        i += needle.len()
                    } else {
                        out.push(s.0[i]);
                        i += 1
                    }
                }
                J::String(JsString(out))
            }
            J::Array(a) => J::Array(a.iter().map(|v| visit(v, needle)).collect()),
            J::Object(o) => J::object(o.iter().map(|(k, v)| (k.clone(), visit(v, needle)))),
            _ => v.clone(),
        }
    }
    visit(&v, &needle)
}
fn native(root: &Path, spec: &J) -> J {
    for file in f(spec, "files").as_array().unwrap_or(&[]) {
        let path = root.join(f(file, "path").as_str().unwrap());
        assert!(path.starts_with(root));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, f(file, "text").as_str().unwrap()).unwrap()
    }
    let count = Arc::new(Mutex::new(0usize));
    let next = count.clone();
    let ids: Arc<dyn Fn() -> JsString + Send + Sync> = Arc::new(move || {
        let mut n = next.lock().unwrap();
        let id = format!("fixture-{}", *n);
        *n += 1;
        id.into()
    });
    let rows = f(spec, "rows")
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let row = replace(row, root);
            let env = SdkEnvironment {
                home: node_path::from_native(root),
                environment: f(&row, "environment").clone(),
            };
            let context = f(&row, "context");
            match env.query(context) {
                Err(e) => J::object([(
                    "error",
                    J::object([("name", e.name.into()), ("message", J::String(e.message))]),
                )]),
                Ok(options) => {
                    let mut calls = vec![];
                    let mut responses = vec![];
                    let enabled = f(&options.params, "permissionCallback").as_bool().unwrap();
                    for callback in f(&row, "callbacks").as_array().unwrap_or(&[]) {
                        if !enabled {
                            responses.push(J::Null);
                            continue;
                        }
                        let value = ask::callback(
                            context,
                            f(callback, "name"),
                            f(callback, "input"),
                            f(callback, "id"),
                            options.auto_allow,
                            &ids,
                        );
                        responses.push(match value {
                            Callback::Immediate(result) => result,
                            Callback::Permission { event, input } => {
                                calls.push(event);
                                ask::permission_result(
                                    &input,
                                    row.get("decision")
                                        .unwrap_or(&v(r#"{"decision":"accept"}"#)),
                                    false,
                                )
                            }
                            Callback::UserInput {
                                event,
                                input,
                                questions,
                            } => {
                                calls.push(event);
                                ask::user_input_result(
                                    &input,
                                    &questions,
                                    row.get("answers").unwrap_or(&v(r#"{"answers":{}}"#)),
                                )
                                .unwrap()
                            }
                        });
                    }
                    J::object([
                        ("options", f(&options.params, "options").clone()),
                        ("messages", f(&options.params, "messages").clone()),
                        ("permissionCallback", enabled.into()),
                        ("calls", J::Array(calls)),
                        ("responses", J::Array(responses)),
                    ])
                }
            }
        })
        .collect();
    fn tree(root: &Path, dir: &Path, files: &mut Vec<J>) {
        let mut entries = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap())
            .collect::<Vec<_>>();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            assert!(!entry.file_type().unwrap().is_symlink());
            if entry.file_type().unwrap().is_dir() {
                tree(root, &entry.path(), files)
            } else {
                files.push(J::object([
                    (
                        "path",
                        entry
                            .path()
                            .strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/")
                            .into(),
                    ),
                    (
                        "base64",
                        STANDARD.encode(fs::read(entry.path()).unwrap()).into(),
                    ),
                ]))
            }
        }
    }
    let mut files = vec![];
    tree(root, root, &mut files);
    J::object([("rows", J::Array(rows)), ("files", J::Array(files))])
}
#[test]
fn sdk_options_multimodal_resume_permissions_and_questions_match_frozen_runtime() {
    let base = v(
        r#"{"threadId":"thread","turnId":"turn","purpose":"normal","prompt":"hello","cwd":"__ROOT__/work","runtimeType":null,"model":"sonnet","effort":"high","claudeSessionId":null,"forkSession":false,"mcpServers":null,"allowedTools":null,"addDirs":[],"enableFileCheckpointing":false,"outputFormat":null,"approvalPolicy":"on-request","sandboxMode":"workspace-write","systemPromptAddendum":"extra instructions\ud800","planMode":false,"imageInputs":[]}"#,
    );
    let callbacks = v(
        r#"[{"name":"Bash","input":{"command":"fixture"},"id":"bash"},{"name":"AskUserQuestion","id":"question","input":{"questions":[{"header":"Long header 123456","question":"Which?","options":[{"label":"A","description":"first"}]}]}}]"#,
    );
    let mut rows = vec![];
    for mode in [
        "",
        "default",
        "acceptEdits",
        "bypassPermissions",
        "plan",
        "dontAsk",
        "auto",
    ] {
        let mut environment = v(r#"{"CLAUDE_CODEX_CLI":"__ROOT__/claude-fixture.exe"}"#);
        if !mode.is_empty() {
            environment.insert("CLAUDE_CODEX_PERMISSION_MODE", mode.into())
        }
        let mut context = base.clone();
        context.insert("approvalPolicy", "never".into());
        context.insert("sandboxMode", "danger-full-access".into());
        rows.push(J::object([
            ("context", context),
            ("environment", environment),
            ("callbacks", callbacks.clone()),
            (
                "answers",
                v(r#"{"answers":{"q0":{"answers":["Other"],"notes":"custom"}}}"#),
            ),
        ]));
    }
    let mut context = base.clone();
    context.insert("prompt", " /workflows perform the task ".into());
    context.insert("imageInputs",v(r#"[{"kind":"base64","mediaType":"image/png","data":"aGVsbG8=","displayPath":"__ROOT__/image.png"},{"kind":"url","data":"https://example.invalid/image","mediaType":"image/*","displayPath":"https://example.invalid/image"}]"#));
    context.insert("mcpServers", v(r#"{"fixture":{"command":"unused"}}"#));
    context.insert("allowedTools", v(r#"["Read"]"#));
    context.insert("addDirs", v(r#"["__ROOT__/extra"]"#));
    context.insert(
        "outputFormat",
        v(r#"{"type":"json_schema","schema":{"type":"object"}}"#),
    );
    context.insert("claudeSessionId", "session1".into());
    context.insert("forkSession", true.into());
    rows.push(J::object([("context",context),("environment",v(r#"{"CLAUDE_CODEX_CLI":"__ROOT__/claude-fixture.exe","CLAUDE_CODEX_INSTRUCTIONS_FILE":"__ROOT__/managed.txt"}"#)),("callbacks",callbacks.clone()),("decision",v(r#"{"decision":"acceptForSession","updatedInput":{"command":"changed"}}"#))]));
    rows.push(J::object([("context",base.clone()),("environment",v(r#"{"CLAUDE_CODEX_CLI":"__ROOT__/claude-fixture.exe","CLAUDE_CODEX_SUBSCRIPTION_ONLY":"1","CLAUDE_CONFIG_DIR":"__ROOT__/config","ANTHROPIC_API_KEY":"fixture-only"}"#)),("callbacks",callbacks.clone())]));
    rows.push(J::object([("context",base.clone()),("environment",v(r#"{"CLAUDE_CODEX_CLI":"__ROOT__/claude-fixture.exe","CLAUDE_CODEX_SUBSCRIPTION_ONLY":"1","CLAUDE_CONFIG_DIR":"__ROOT__/config"}"#)),("callbacks",callbacks.clone()),("decision",v(r#"{"decision":"decline"}"#))]));
    let mut context = base.clone();
    context.insert("planMode", true.into());
    rows.push(J::object([
        ("context", context),
        (
            "environment",
            v(r#"{"CLAUDE_CODEX_CLI":"__ROOT__/claude-fixture.exe"}"#),
        ),
        ("callbacks", callbacks),
    ]));
    let spec = J::object([
        ("rows", J::Array(rows)),
        (
            "files",
            v(
                r#"[{"path":"work/AGENTS.md","text":"local instructions"},{"path":"managed.txt","text":"managed instructions\n@AGENTS.md\n"},{"path":"config/settings.json","text":"{}"},{"path":".claude/sessions/session1.jsonl","text":"fixture transcript\n"}]"#,
            ),
        ),
    ]);
    let root = Root::new();
    let old = root.0.join("original");
    let new = root.0.join("native");
    fs::create_dir(&old).unwrap();
    fs::create_dir(&new).unwrap();
    let mut original_spec = spec.clone();
    original_spec.insert("root", J::String(node_path::from_native(&old)));
    let mut child = Command::new("node")
        .arg("tests/fixtures/agent-claude-sdk-oracle.mjs")
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
        .write_all(stringify(&original_spec, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let expected = parse(std::str::from_utf8(&result.stdout).unwrap(), usize::MAX).unwrap();
    compare(
        "SDK options and callbacks",
        normalized(&native(&new, &spec), &new),
        normalized(&expected, &old),
    );
}
#[tokio::test]
async fn owned_bridge_multiplexes_callbacks_and_stops_with_backpressure() {
    use ivy_agent_manager::claude::bridge::{Bridge, BridgeLaunch};
    use std::{collections::BTreeMap, time::Duration};
    use tokio::time::timeout;
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            "process.stdout.write(process.execPath)",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let node = PathBuf::from(String::from_utf8(output.stdout).unwrap());
    // The test transport entry supplies the fake SDK directly to the actual
    // production session function. It never imports the official SDK or CLI.
    let launch = BridgeLaunch {
        node_executable: node,
        entry: repository.join("tests/fixtures/agent-claude-bridge-transport.mjs"),
        cwd: repository.clone(),
        environment: std::env::vars_os()
            .filter(|(k, _)| k.to_str().is_some_and(|s|matches!(s.to_ascii_lowercase().as_str(), "systemroot" | "windir" | "temp" | "tmp")))
            .collect::<BTreeMap<_, _>>(),
    };
    let (bridge, mut events) = Bridge::spawn(launch, Arc::new(|_| {})).unwrap();
    let call = |method: &str, params: J| {
        let bridge = bridge.clone();
        let method = method.to_owned();
        async move {
            timeout(Duration::from_secs(10), bridge.call(&method, params))
                .await
                .unwrap()
        }
    };
    let models = call(
        "models",
        v(r#"{"options":{"cwd":"fixture"},"timeoutMs":30000}"#),
    )
    .await
    .unwrap();
    assert_eq!(
        f(&models.as_array().unwrap()[0], "value"),
        &J::from("sonnet")
    );
    call("query/start",v(r#"{"turnId":"turn-a","options":{"fixtureTag":"turn-a"},"messages":[],"permissionCallback":true}"#)).await.unwrap();
    let pending = tokio::spawn(call("fixture/permission", v(r#"{"turnId":"turn-a"}"#)));
    let event = timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f(&event, "type"), &J::from("permission"));
    // A separate SDK control call completes while the permission is pending.
    let models = call(
        "models",
        v(r#"{"options":{"cwd":"fixture"},"timeoutMs":30000}"#),
    )
    .await
    .unwrap();
    assert!(!models.as_array().unwrap().is_empty());
    call(
        "permission/reply",
        J::object([
            ("callbackId", f(&event, "callbackId").clone()),
            (
                "result",
                v(r#"{"behavior":"allow","updatedInput":{"command":"changed"}}"#),
            ),
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        pending.await.unwrap().unwrap(),
        v(r#"{"behavior":"allow","updatedInput":{"command":"changed"}}"#)
    );
    call("query/steer",v(r#"{"turnId":"turn-a","messages":[{"type":"user","message":{"role":"user","content":"steer"},"parent_tool_use_id":null,"origin":{"kind":"human"}}]}"#)).await.unwrap();
    let snapshot = call("fixture/inspect", J::object([] as [(&str, J); 0]))
        .await
        .unwrap();
    assert_eq!(
        f(&snapshot.as_array().unwrap()[1], "steers"),
        &J::Number(1.0)
    );
    call("fixture/burst", v(r#"{"turnId":"turn-a","count":256}"#))
        .await
        .unwrap();
    // Leave the bounded event receiver full. Stop must still reach the child
    // owner and reap it, without requiring a consumer to drain the queue.
    tokio::time::sleep(Duration::from_millis(30)).await;
    timeout(Duration::from_secs(10), bridge.stop())
        .await
        .unwrap()
        .unwrap();
    assert!(
        call("models", v(r#"{"options":{},"timeoutMs":30000}"#))
            .await
            .is_err()
    );
    let mut received = 0;
    while events.recv().await.is_some() {
        received += 1
    }
    assert!(received > 0 && received <= 128);
}
