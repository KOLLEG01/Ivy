use ivy_agent_manager::{
    claude::{
        ask,
        projects::ProjectStore,
        protocol::{Adapter, AdapterClock, Effect, Environment},
        rate_limits::RateLimitStore,
        store::SessionStore,
    },
    registry, runtime,
};
use ivy_contracts::{
    IvyJson, JsString,
    hash::digest,
    json::{parse, stringify},
};
use ivy_host_runtime::node_path;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

const BYTES: usize = ivy_contracts::limits::MANAGEMENT_FRAME_BYTES;
#[path = "agent-native-home.rs"]
mod native_home_cases;
#[path = "agent-native-home-json.rs"]
mod native_home_json_cases;
#[path = "agent-native-owner.rs"]
mod native_owner_cases;
#[path = "agent-runtime-reset.rs"]
mod runtime_reset_cases;
fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn value(path: &Path) -> IvyJson {
    parse(&fs::read_to_string(path).unwrap(), BYTES)
        .unwrap_or_else(|error| panic!("Invalid fixture JSON at {}: {error:?}", path.display()))
}
fn save(path: &Path, value: &IvyJson) {
    fs::write(path, stringify(value, BYTES).unwrap()).unwrap();
}
fn text(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
async fn health_probe(config: &Path, bin: &Path, executable: &Path) -> (bool, IvyJson) {
    let output = Command::new(executable)
        .arg("--config")
        .arg(config)
        .env("PATH", bin)
        .env("IVY_NODE_EXECUTABLE", bin.join("absent-node.exe"))
        .output()
        .await
        .unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        output.status.success(),
        json(std::str::from_utf8(&output.stdout).unwrap().trim()),
    )
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-agent-runtime-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn json(source: &str) -> IvyJson {
    parse(source, BYTES).unwrap()
}

fn adapter(root: &Root, environment: IvyJson) -> Adapter {
    let clock: ivy_agent_manager::claude::store::Clock = Arc::new(|| 1_700_000_000.0);
    let ids: Arc<dyn Fn() -> JsString + Send + Sync> =
        Arc::new(|| uuid::Uuid::new_v4().to_string().into());
    let projects = ProjectStore::open(
        &root.0,
        node_path::Style::native(),
        Arc::new(|a, b| runtime::compare_names(a, b, false)),
        clock.clone(),
        ids.clone(),
    )
    .unwrap();
    let environment = Environment {
        environment,
        discovered_router_models: vec![],
        cwd: node_path::from_native(&root.0),
        codex_home: node_path::from_native(&root.0),
        platform_os: std::env::consts::OS.into(),
        cpu: std::env::consts::ARCH.into(),
        compat_version: "0.154.0".into(),
        version_suffix: "claude-codex".into(),
        default_model: "sonnet".into(),
        default_effort: "medium".into(),
        mcp_servers: IvyJson::Null,
        startup_statuses: vec![],
        config_overrides: IvyJson::object([] as [(&str, IvyJson); 0]),
        config_path: root.0.join("config.json"),
        prepare_worktree: Arc::new(|_, cwd| Ok(cwd.clone())),
        read_base64: Arc::new(|_| panic!("fixture does not read images")),
        subagent_completed: None,
        subagent_watchdog_ms: None,
    };
    Adapter::new(
        SessionStore::with_clock(&root.0, &root.0.join("sessions.sqlite"), clock.clone()).unwrap(),
        projects,
        RateLimitStore::open(&root.0),
        environment,
        AdapterClock {
            seconds: clock,
            millis: Arc::new(|| 1_700_000_000_000.0),
        },
        ids,
    )
}

fn request(adapter: &mut Adapter, method: &str, params: IvyJson) -> IvyJson {
    let effects = adapter
        .receive(
            &"fixture".into(),
            &IvyJson::object([
                ("id", "request".into()),
                ("method", method.into()),
                ("params", params),
            ]),
        )
        .unwrap();
    effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Send { message, .. } if message.get("id") == Some(&"request".into()) => {
                assert!(message.get("error").is_none(), "{message:?}");
                Some(message.get("result").unwrap().clone())
            }
            _ => None,
        })
        .expect("native response")
}

#[test]
fn question_answers_preserve_literal_user_keys_and_reject_invalid_options() {
    let input = json(r#"{"questions":[{"question":"__proto__","options":[{"label":"Keep"}]}]}"#);
    let questions = ask::questions(&input).unwrap();
    let answer = ask::format_answers(
        &input,
        &questions,
        &json(r#"{"answers":{"q0":{"answers":["Keep"],"notes":"literal key"}}}"#),
    )
    .unwrap();
    let answer = json(&answer.to_utf8().unwrap());
    assert_eq!(
        answer.get("answers").unwrap().get("__proto__"),
        Some(&"Keep".into())
    );
    assert_eq!(
        answer
            .get("annotations")
            .unwrap()
            .get("__proto__")
            .unwrap()
            .get("notes"),
        Some(&"literal key".into())
    );
    let error = ask::questions(&json(r#"{"questions":[{"options":true}]}"#)).unwrap_err();
    assert_eq!(error.name, "invalid_arguments");
}

#[test]
fn managed_mcp_rejects_invalid_ownership_instead_of_treating_it_as_absent() {
    use ivy_agent_manager::mcp_output::{McpOutputInput, plan_mcp_output};
    for state in [IvyJson::Bool(false), IvyJson::Number(0.0), "".into()] {
        let error = plan_mcp_output(McpOutputInput {
            source: "",
            state: Some(&state),
            authority: "fixture",
            sections: None,
            server_names: &[],
            app_id: None,
            desired_version: "1",
        })
        .err()
        .expect("invalid ownership must fail");
        assert_eq!(error.code, "target_conflict");
    }
    assert!(
        plan_mcp_output(McpOutputInput {
            source: "",
            state: None,
            authority: "fixture",
            sections: None,
            server_names: &[],
            app_id: None,
            desired_version: "1",
        })
        .is_ok()
    );
}

#[test]
fn session_restart_preserves_json_data_and_rejects_wrong_storage_types() {
    let root = Root::new();
    let mut adapter = adapter(&root, json("{}"));
    let started = request(&mut adapter, "thread/start", json("{}"));
    let id = started
        .get("thread")
        .unwrap()
        .get("id")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    drop(adapter);
    let path = root.0.join("sessions.sqlite");
    let store = SessionStore::open(&root.0, &path).unwrap();
    let original = store.get_thread(&id).unwrap().unwrap();
    let mut invalid = original.clone();
    invalid.insert("createdAt", "0x20".into());
    assert_eq!(
        store.upsert_thread(&invalid).unwrap_err().name,
        "invalid_arguments"
    );
    assert_eq!(store.get_thread(&id).unwrap().unwrap(), original);
    let mut turn = json(
        r#"{"id":"turn","status":"inProgress","startedAt":1700000000,"completedAt":null,"durationMs":null,"items":[],"diff":"","error":null}"#,
    );
    turn.insert("threadId", IvyJson::String(id.clone()));
    store.upsert_turn(&turn).unwrap();
    let item = json(
        r#"{"id":"item","type":"agentMessage","text":"Grüße 😀","data":{"__proto__":{"literal":true},"constructor":"value"}}"#,
    );
    store.append_item(&"turn".into(), &item).unwrap();
    drop(store);
    let store = SessionStore::open(&root.0, &path).unwrap();
    assert_eq!(
        store
            .get_turn(&"turn".into())
            .unwrap()
            .unwrap()
            .get("items"),
        Some(&IvyJson::Array(vec![item]))
    );
    drop(store);
    let db = ivy_store::rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE threads SET preview = X'0102'", [])
        .unwrap();
    drop(db);
    let store = SessionStore::open(&root.0, &path).unwrap();
    assert_eq!(store.get_thread(&id).unwrap_err().name, "invalid_data");
}

#[test]
fn native_registration_accepts_literal_catalog_method_names() {
    let mut catalog = value(&repository().join("specs/native/codex-0.154.0/catalog.json"));
    let template = catalog.get("clientRequests").unwrap().as_array().unwrap()[0].clone();
    let methods = ["__proto__", "constructor"]
        .into_iter()
        .map(|method| {
            let mut entry = template.clone();
            entry.insert("method", method.into());
            entry
        })
        .collect();
    catalog.insert("clientRequests", IvyJson::Array(methods));
    let registry = registry::agent_registry(&catalog).unwrap();
    let namespace = &registry.get("namespaces").unwrap().as_array().unwrap()[0];
    let tools = namespace.get("tools").unwrap().as_array().unwrap();
    for method in ["__proto__", "constructor"] {
        assert!(
            tools
                .iter()
                .any(|tool| tool.get("nativeMethod") == Some(&method.into()))
        );
    }
}

#[test]
fn native_adapter_redacts_selection_diagnostics_and_completes_structured_output() {
    let root = Root::new();
    let mut adapter = adapter(&root, IvyJson::object([("CLAUDE_CODEX_PROVIDER",
        "API_KEY=fixture12345 Bearer fixture22345 sk-ant-fixture32345 ghp_fixture42345 AKIA1234567890ABCDEF -----BEGIN PRIVATE KEY-----\nfixture52345\n-----END PRIVATE KEY-----".into())]));
    let response = request(&mut adapter, "config/read", json("{}"));
    let issues = response
        .get("config")
        .unwrap()
        .get("provider_loop_config")
        .unwrap()
        .get("issues")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(issues.len(), 1);
    let diagnostic = issues[0].get("message").unwrap().as_str().unwrap();
    assert_eq!(diagnostic.matches("[redacted]").count(), 6);
    assert!(!diagnostic.contains("fixture"));
    let started = request(&mut adapter, "thread/start", json("{}"));
    let thread = started.get("thread").unwrap().get("id").unwrap().clone();
    let mut params = json(
        r#"{"input":[{"type":"text","text":"Keep this response"}],"outputSchema":{"type":"object","properties":{"__proto__":{"type":"string"},"constructor":{"type":"string"}},"required":["__proto__","constructor"]}}"#,
    );
    params.insert("threadId", thread.clone());
    let started = request(&mut adapter, "turn/start", params);
    let turn = started
        .get("turn")
        .unwrap()
        .get("id")
        .unwrap()
        .as_js_string()
        .unwrap();
    assert!(
        adapter
            .begin_turn(turn)
            .unwrap()
            .iter()
            .any(|effect| matches!(effect, Effect::Run { .. }))
    );
    adapter.runtime_finished(turn, Ok(())).unwrap();
    let response = request(
        &mut adapter,
        "thread/read",
        IvyJson::object([("threadId", thread)]),
    );
    let turn = &response
        .get("thread")
        .unwrap()
        .get("turns")
        .unwrap()
        .as_array()
        .unwrap()[0];
    assert_eq!(turn.get("status"), Some(&"completed".into()));
    let item = turn
        .get("items")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item.get("type") == Some(&"agentMessage".into()))
        .unwrap();
    let output = json(&item.get("text").unwrap().as_str().unwrap());
    for name in ["__proto__", "constructor"] {
        assert_eq!(output.get(name), Some(&"Keep this response".into()));
    }
}

#[test]
fn runtime_codecs_preserve_staged_bytes_and_browser_query_values() {
    assert_eq!(
        runtime::canonical_base64(&"AP+Aw6Q=".into()),
        Some(vec![0, 255, 128, 195, 164])
    );
    for bad in ["AP+Aw6Q", "AP+Aw6Q=\n", "AB==", "_w=="] {
        assert!(runtime::canonical_base64(&bad.into()).is_none());
    }
    let query = runtime::form_query(&[
        ("threadId".into(), "space + Grüße😀".into()),
        ("threadId".into(), "second".into()),
    ])
    .unwrap();
    let decoded: Vec<_> = url::form_urlencoded::parse(query.to_utf8().unwrap().as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(
        decoded,
        [
            ("threadId".into(), "space + Grüße😀".into()),
            ("threadId".into(), "second".into())
        ]
    );
    assert!(runtime::compare_names(&"Alpha".into(), &"Zulu".into(), false).is_lt());
    let home = runtime::home_inputs().unwrap();
    assert!(node_path::to_native(&home.account_home).is_absolute());
    assert!(node_path::to_native(&home.cwd).is_absolute());
}

#[tokio::test]
async fn executable_rejects_missing_configuration_without_starting_a_native_owner() {
    let output = Command::new(env!("CARGO_BIN_EXE_ivy-agent-manager"))
        .env_remove("IVY_INSTANCE_CONFIG")
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let error = parse(std::str::from_utf8(&output.stderr).unwrap().trim(), BYTES).unwrap();
    assert_eq!(error.get("code"), Some(&"invalid_arguments".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executable_registers_dispatches_status_and_drains_on_control_shutdown() {
    let root = Root::new();
    let node_output = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .await
        .unwrap();
    assert!(node_output.status.success());
    let node = PathBuf::from(String::from_utf8(node_output.stdout).unwrap().trim());
    let mut peer = Command::new(&node)
        .arg(repository().join("tests/fixtures/agent-runtime-hive.mjs"))
        .arg(&root.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut peer_lines = BufReader::new(peer.stdout.take().unwrap()).lines();
    let first = tokio::time::timeout(Duration::from_secs(15), peer_lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let endpoint = parse(&first, BYTES)
        .unwrap()
        .required("url")
        .unwrap()
        .clone();

    let bin = root.0.join("bin");
    let artifact = root.0.join("artifact");
    let data = root.0.join("data");
    let home = data.join("native");
    for directory in [
        &bin,
        &data,
        &artifact.join("dist/native"),
        &artifact.join("dist/specs/native/codex-0.154.0"),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    let executable = bin.join(if cfg!(windows) {
        "native.exe"
    } else {
        "native"
    });
    let compile = std::process::Command::new("rustc")
        .args(["--edition=2024", "-C", "debuginfo=0"])
        .arg(repository().join("tests/fixtures/native-launch-child.rs"))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let shell = bin.join("pwsh.exe");
    fs::copy(&executable, &shell).unwrap();
    let marker = root.0.join("native-child");
    fs::write(
        bin.join("launch-fixture.txt"),
        format!(
            "{}\n{}\nlaunch-runtime\n{}\n{}\n{{}}\n0.154.0\n",
            node.display(),
            repository()
                .join("tests/fixtures/native-connection-child.mjs")
                .display(),
            home.display(),
            marker.display()
        ),
    )
    .unwrap();

    let native_hash = digest(fs::read(&executable).unwrap());
    let mut catalog = value(&repository().join("specs/native/codex-0.154.0/catalog.json"));
    catalog.insert("nativeExecutableHash", native_hash.clone().into());
    save(
        &artifact.join("dist/specs/native/codex-0.154.0/catalog.json"),
        &catalog,
    );
    let prepared = Command::new(&node)
        .arg(repository().join("tests/fixtures/agent-runtime-prepare.mjs"))
        .arg(&root.0)
        .arg(repository())
        .output()
        .await
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let prepared = json(std::str::from_utf8(&prepared.stdout).unwrap().trim());
    let artifact = node_path::filesystem_path(
        prepared
            .required("artifactRoot")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    let build_id = prepared.required("buildId").unwrap().clone();
    let version = prepared.required("version").unwrap().clone();
    let service_executable = node_path::filesystem_path(
        prepared
            .required("executable")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    let health_executable =
        node_path::filesystem_path(prepared.required("health").unwrap().as_js_string().unwrap());
    let mut settings = IvyJson::object([
        ("nativeExecutable", text(&executable)),
        ("nativeVersion", "0.154.0".into()),
        ("nativeExecutableHash", native_hash.into()),
        ("codexHome", text(&home)),
        (
            "appServer",
            IvyJson::object([("mode", "owned-stdio".into())]),
        ),
        ("patcherEnabled", false.into()),
        (
            "limits",
            IvyJson::object([
                ("maxOperations", IvyJson::Number(100.0)),
                ("maxJournalBytes", IvyJson::Number(64.0 * 1024.0 * 1024.0)),
                ("maxPendingInputs", IvyJson::Number(8.0)),
                ("maxNotificationBytes", IvyJson::Number(1024.0 * 1024.0)),
            ]),
        ),
    ]);
    if cfg!(windows) {
        settings.insert(
            "windowsShell",
            IvyJson::object([
                ("executable", text(&shell)),
                ("executableHash", digest(fs::read(&shell).unwrap()).into()),
            ]),
        );
    }
    let config = root.0.join("instance.json");
    save(
        &config,
        &IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("componentId", "agent-manager".into()),
            ("hostId", "runtime-fixture".into()),
            ("instanceId", "runtime-agent".into()),
            ("serviceNodeId", "runtime-fixture.agent".into()),
            ("publicBaseUrl", endpoint),
            ("dataRoot", text(&data)),
            ("artifactRoot", text(&artifact)),
            ("buildId", build_id),
            ("version", version),
            ("credential", "runtime-fixture-token".into()),
            ("settings", settings),
        ]),
    );
    let mut service = Command::new(&service_executable)
        .arg("--config")
        .arg(&config)
        .env("PATH", &bin)
        .env("IVY_NODE_EXECUTABLE", bin.join("absent-node.exe"))
        .env("IVY_LAUNCH_ID", "runtime-fixture-launch")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stderr = service.stderr.take().unwrap();
    let stderr = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut s = String::new();
        let mut r = stderr;
        r.read_to_string(&mut s).await.unwrap();
        s
    });
    // Match the component's readiness window, including catalog validation.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    while !root.0.join("status-reply.json").is_file() {
        if let Some(status) = service.try_wait().unwrap() {
            panic!("Service exited {status}: {}", stderr.await.unwrap());
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = service.kill().await;
            let _ = service.wait().await;
            panic!(
                "Service did not become ready; health: {:?}; launches: {:?}; native frames: {:?}; stderr: {}",
                fs::read_to_string(data.join("health.json")),
                fs::read_to_string(root.0.join("native-child.launches")),
                fs::read_to_string(root.0.join("native-child.frames")),
                stderr.await.unwrap(),
            );
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let status = value(&root.0.join("status-reply.json"));
    assert!(status.get("error").is_none(), "{status:?}");
    assert_eq!(
        status.required("result").unwrap().get("serviceNodeId"),
        Some(&"runtime-fixture.agent".into())
    );
    let identity = value(&root.0.join("identity.json"));
    assert_eq!(identity.get("serviceName"), Some(&"agent-manager".into()));
    let connection = value(&root.0.join("connection.json"));
    assert_eq!(
        connection.get("authorization"),
        Some(&"Bearer runtime-fixture-token".into())
    );
    let registry = value(&root.0.join("registry.json"));
    assert!(
        registry
            .required("namespaces")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|ns| ns
                .get("tools")
                .and_then(IvyJson::as_array)
                .is_some_and(|tools| tools.len() == 22))
    );
    assert!(data.join("journal/agent-runtime.sqlite").is_file());
    let health = value(&data.join("health.json"));
    assert_eq!(health.get("ready"), Some(&true.into()));
    let (ok, probe) = health_probe(&config, &bin, &health_executable).await;
    assert!(ok, "{probe:?}");
    assert_eq!(probe.get("ok"), Some(&true.into()));
    assert_eq!(probe.get("buildId"), health.get("buildId"));
    assert_eq!(probe.get("generation"), health.get("generation"));
    let direct = Command::new(&service_executable)
        .arg("--health")
        .arg("--config")
        .arg(&config)
        .env("PATH", &bin)
        .env("IVY_NODE_EXECUTABLE", bin.join("absent-node.exe"))
        .output()
        .await
        .unwrap();
    assert!(direct.status.success());
    assert!(direct.stderr.is_empty());
    assert_eq!(
        json(std::str::from_utf8(&direct.stdout).unwrap().trim()),
        probe
    );

    for rejected in [
        IvyJson::object([("ready", false.into())]),
        IvyJson::object([("synced", false.into())]),
        IvyJson::object([("buildId", digest("other build").into())]),
    ] {
        save(&root.0.join("registration-override.json"), &rejected);
        let (ok, probe) = health_probe(&config, &bin, &health_executable).await;
        assert!(!ok);
        assert_eq!(probe.get("code"), Some(&"service_not_ready".into()));
    }
    fs::remove_file(root.0.join("registration-override.json")).unwrap();

    let probe_data = root.0.join("probe-data");
    fs::create_dir(&probe_data).unwrap();
    let mut probe_config = value(&config);
    probe_config.insert("dataRoot", text(&probe_data));
    let probe_config_path = root.0.join("probe-instance.json");
    save(&probe_config_path, &probe_config);
    for (key, rejected) in [
        ("ready", false.into()),
        ("instanceId", "other-instance".into()),
        ("buildId", digest("other build").into()),
        ("observedAt", "2020-01-01T00:00:00Z".into()),
        ("observedAt", "2100-01-01T00:00:00Z".into()),
        ("observedAt", "invalid timestamp".into()),
    ] {
        let mut rejected_health = health.clone();
        rejected_health.insert(key, rejected);
        save(&probe_data.join("health.json"), &rejected_health);
        let (ok, probe) = health_probe(&probe_config_path, &bin, &health_executable).await;
        assert!(!ok, "{key}");
        assert_eq!(probe.get("code"), Some(&"service_not_ready".into()));
    }
    let launches = fs::read(root.0.join("native-child.launches")).unwrap();
    save(
        &data.join("control.json"),
        &IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("instanceId", "runtime-agent".into()),
            ("launchId", "runtime-fixture-launch".into()),
            ("bootId", health.required("bootId").unwrap().clone()),
            ("action", "shutdown".into()),
            ("requestedAt", "2026-10-22T12:00:00.000Z".into()),
        ]),
    );
    let exit = tokio::time::timeout(Duration::from_secs(20), service.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(exit.success(), "{}", stderr.await.unwrap());
    let stopped = value(&data.join("health.json"));
    assert_eq!(stopped.get("ready"), Some(&false.into()));
    assert!(!data.join("process-stop-fence.json").exists());
    let (ok, probe) = health_probe(&config, &bin, &health_executable).await;
    assert!(!ok);
    assert_eq!(probe.get("code"), Some(&"service_not_ready".into()));
    assert_eq!(
        fs::read(root.0.join("native-child.launches")).unwrap(),
        launches
    );
    peer.kill().await.unwrap();
    let _ = peer.wait().await;
}

#[test]
fn claude_resume_reuses_the_native_canonical_directory_project_key() {
    use ivy_agent_manager::claude::sdk_options::SdkEnvironment;
    let root = Root::new();
    let directory = std::env::temp_dir().join(root.0.file_name().unwrap());
    let key = node_path::from_native(&directory)
        .0
        .into_iter()
        .map(|unit| {
            if matches!(unit, 48..=57 | 65..=90 | 97..=122 | 45) {
                unit
            } else {
                45
            }
        })
        .collect();
    let key = node_path::to_native(&JsString(key));
    let projects = root.0.join(".claude/projects");
    let transcript = projects.join(&key).join("session.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, b"retained transcript").unwrap();
    let environment = SdkEnvironment {
        home: node_path::from_native(&root.0),
        environment: IvyJson::object([] as [(&str, IvyJson); 0]),
    };
    assert_eq!(
        environment.resume(&"session".into(), Some(&node_path::from_native(&root.0))),
        Some("session".into())
    );
    assert_eq!(fs::read(&transcript).unwrap(), b"retained transcript");
    assert_eq!(
        fs::read_dir(projects)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        vec![key.into_os_string()]
    );
}
