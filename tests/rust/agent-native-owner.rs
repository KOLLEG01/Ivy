use ivy_agent_manager::{
    native_handshake::{AgentHandshake, NativeMode, initialize_agent},
    native_launch::{NativeHomeInputs, NativeProcessOptions, start_native_process},
};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    json::{canonical, parse, stringify},
};
use ivy_host_runtime::{codex_rpc::NativeRpc, node_path, paths};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{Notify, watch},
};
#[path = "../fixtures/reference-data.rs"]
mod owner_reference_data;

fn reference() -> &'static IvyJson {
    static CASES: std::sync::OnceLock<IvyJson> = std::sync::OnceLock::new();
    CASES.get_or_init(|| {
        let bytes: &[u8] = if cfg!(windows) {
            include_bytes!("../fixtures/agent-native-owner-win32-cases.json.gz")
        } else {
            include_bytes!("../fixtures/agent-native-owner-linux-cases.json.gz")
        };
        parse(
            std::str::from_utf8(&owner_reference_data::read(bytes)).unwrap(),
            usize::MAX,
        )
        .unwrap()
    })
}
fn reference_root(value: &IvyJson, root: &str) -> IvyJson {
    match value {
        IvyJson::Array(values) => IvyJson::Array(
            values
                .iter()
                .map(|value| reference_root(value, root))
                .collect(),
        ),
        IvyJson::Object(values) => IvyJson::object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), reference_root(value, root))),
        ),
        IvyJson::String(value) => value.to_utf8().unwrap().replace("<ROOT>", root).into(),
        value => value.clone(),
    }
}

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-agent-owner-Grüße😀-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn text(value: &IvyJson, key: &str) -> JsString {
    field(value, key).as_js_string().unwrap().clone()
}
fn same(actual: &IvyJson, expected: &IvyJson) {
    fn native_paths(value: &IvyJson) -> IvyJson {
        match value {
            IvyJson::Array(values) => IvyJson::Array(values.iter().map(native_paths).collect()),
            IvyJson::Object(values) => {
                IvyJson::object(values.iter().map(|(k, v)| (k.clone(), native_paths(v))))
            }
            IvyJson::String(text) if cfg!(windows) && text.0.starts_with(&[92, 92, 63, 92]) => {
                IvyJson::String(if text.0[4..].starts_with(&[85, 78, 67, 92]) {
                    JsString([&[92, 92][..], &text.0[8..]].concat())
                } else {
                    JsString(text.0[4..].to_vec())
                })
            }
            value => value.clone(),
        }
    }
    let actual = native_paths(actual);
    let expected = native_paths(expected);
    assert_eq!(
        canonical(&actual, usize::MAX).unwrap(),
        canonical(&expected, usize::MAX).unwrap()
    );
}
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn path(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn prepare_fixture(input: IvyJson) -> IvyJson {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg("tests/fixtures/agent-native-owner-prepare.mjs")
            .current_dir(repo())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn prepared(root: &Root, name: &str) -> IvyJson {
    prepare_fixture(IvyJson::object([
        ("action", "prepare".into()),
        ("root", path(&root.0.join(name))),
        ("variant", name.into()),
    ]))
}
fn options(
    prepared: &IvyJson,
    before: Arc<ivy_agent_manager::native_launch::BeforeNativeLaunch>,
) -> NativeProcessOptions {
    let cwd = node_path::from_native(&std::env::current_dir().unwrap());
    NativeProcessOptions {
        artifact_root: text(prepared, "artifactRoot"),
        data_root: text(prepared, "dataRoot"),
        settings: field(prepared, "settings").clone(),
        client_version: "1.2.3".into(),
        hive_credential: Some("fixture-only".into()),
        home_inputs: NativeHomeInputs {
            paths: node_path::Context::current().unwrap(),
            environment: BTreeMap::new(),
            user_home: None,
            cwd,
            account_home: text(prepared, "root"),
        },
        before_launch: Some(before),
        begin_epoch: Arc::new(|_| panic!("No CLI probe or native launch belongs to this fixture")),
    }
}
#[tokio::test]
async fn agent_prelaunch_admission_and_callback_match_original_without_a_native_process() {
    let root = Root::new();
    for variant in [
        "owned",
        "external",
        "default",
        "no-realtime",
        "catalog-provider",
        "relative-socket",
        "relative-server-home",
        "relative-executable",
        "overlap",
        "data-home",
        "home-link",
        "wrapper",
        "catalog-hash",
        "executable-hash",
        "fence",
        "version-1.2.3",
        "version-0.x.3",
        "version-0.154.0",
        "version-0.158.0",
        "version-0.159.2",
        "version-lf",
        "version-cr",
        "version-ls",
        "version-ps",
        "claude-ok",
        "claude-not-owned",
        "claude-patcher-off",
        "claude-explicit-skills",
        "claude-adapter-hash",
        "claude-cli-hash",
        "claude-node-hash",
        "claude-billing-key",
        "claude-billing-helper",
        "claude-billing-console",
        "claude-empty-env",
        "claude-invalid-json",
        "claude-null-settings",
        "claude-config-link",
    ] {
        let prepared = prepared(&root, variant);
        let expected = field(field(reference(), "preflight"), variant);
        let before = Arc::new(Mutex::new(None));
        let observed = before.clone();
        let base = node_path::filesystem_path(&text(&prepared, "root"));
        let normalize = paths::realpath(&base).unwrap();
        let opts = options(
            &prepared,
            Arc::new(move |home| {
                *observed.lock().unwrap() = Some(
                    node_path::filesystem_path(&home)
                        .strip_prefix(&normalize)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
                Box::pin(async {
                    Err(IvyError::new(
                        "fixture_preflight_complete",
                        "Fixture stops before every CLI probe.",
                    ))
                })
            }),
        );
        let error = start_native_process(opts)
            .await
            .err()
            .map(|error| IvyJson::String(error.code_js()))
            .unwrap_or(IvyJson::Null);
        let data = node_path::filesystem_path(&text(&prepared, "dataRoot"));
        let actual = IvyJson::object([
            (
                "before",
                before
                    .lock()
                    .unwrap()
                    .clone()
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null),
            ),
            ("epoch", false.into()),
            ("error", error),
            ("homeExists", data.join("native").exists().into()),
            (
                "claudeConfigExists",
                data.join("claude-config").exists().into(),
            ),
            (
                "adapterHomeExists",
                data.join("claude-adapter").exists().into(),
            ),
        ]);
        assert_eq!(
            canonical(&actual, usize::MAX).unwrap(),
            canonical(expected, usize::MAX).unwrap(),
            "{variant}"
        );
    }
}

#[tokio::test]
async fn abandoning_launch_observation_does_not_cancel_accepted_before_launch() {
    let root = Root::new();
    let prepared = prepared(&root, "owned");
    let entered = Arc::new(Notify::new());
    let observed = entered.clone();
    let (release, wait) = watch::channel(false);
    let finished = Arc::new(AtomicBool::new(false));
    let completed = finished.clone();
    let run = start_native_process(options(
        &prepared,
        Arc::new(move |_| {
            observed.notify_one();
            let mut wait = wait.clone();
            let completed = completed.clone();
            Box::pin(async move {
                while !*wait.borrow_and_update() {
                    wait.changed().await.unwrap();
                }
                completed.store(true, Ordering::Release);
                Err(IvyError::new(
                    "fixture_preflight_complete",
                    "Fixture stops before every CLI probe.",
                ))
            })
        }),
    ));
    entered.notified().await;
    drop(run);
    assert!(!finished.load(Ordering::Acquire));
    release.send_replace(true);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !finished.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn agent_handshake_modes_and_payload_match_original_over_real_rpc_streams() {
    let root = Root::new();
    let home = root.0.join("home");
    fs::create_dir(&home).unwrap();
    let expected = paths::realpath(&home).unwrap();
    let mut rows = Vec::new();
    let result = |home: IvyJson, agent: IvyJson| {
        IvyJson::object([("codexHome", home), ("userAgent", agent)])
    };
    let variants = vec![
        IvyJson::object([("result", result(path(&home), "codex/0.142.3".into()))]),
        IvyJson::object([("result", result(path(&home), "codex/0.1.0".into()))]),
        IvyJson::object([(
            "result",
            result(path(&home), "claude-codex/0.142.3 Windows".into()),
        )]),
        IvyJson::object([("result", result(path(&home), "CLAUDE-CODEX/0.142.3".into()))]),
        IvyJson::object([("result", result(path(&home), "unversioned".into()))]),
        IvyJson::object([("result", result(path(&home), IvyJson::Null))]),
        IvyJson::object([("result", result(path(&root.0), "codex/0.142.3".into()))]),
        IvyJson::object([(
            "result",
            result(path(&root.0.join("absent")), "codex/0.142.3".into()),
        )]),
        IvyJson::object([("result", result(false.into(), "codex/0.142.3".into()))]),
        IvyJson::object([("result", IvyJson::Null)]),
        IvyJson::object([("result", IvyJson::Array(vec![]))]),
        IvyJson::object([(
            "error",
            IvyJson::object([
                ("code", IvyJson::Number(-32603.0)),
                ("message", "fixture error".into()),
            ]),
        )]),
    ];
    for mode in ["owned", "external", "claude"] {
        for daemon in [
            IvyJson::Null,
            IvyJson::object([("appServerVersion", "0.142.3".into())]),
            IvyJson::object([("appServerVersion", "0.1.0".into())]),
        ] {
            for reply in &variants {
                rows.push(IvyJson::object([
                    ("mode", mode.into()),
                    ("daemon", daemon.clone()),
                    ("version", "0.142.3".into()),
                    ("expectedHome", path(&home)),
                    ("reply", reply.clone()),
                ]));
            }
        }
    }
    let reference = reference_root(
        field(reference(), "handshake"),
        &node_path::from_native(&root.0).to_utf8().unwrap(),
    );
    same(&IvyJson::Array(rows.clone()), field(&reference, "input"));
    let oracle = field(&reference, "output");
    assert_eq!(rows.len(), field(oracle, "rows").as_array().unwrap().len());
    let mut catalog = parse(
        include_str!("../../specs/native/codex-0.142.3/catalog.json"),
        usize::MAX,
    )
    .unwrap();
    // This corpus qualifies initialization over the real RPC actor. Repeating
    // validation of every unrelated method's large schema in all108 rows hides
    // useful deadlines; complete catalogs are checked by admission cases above.
    for (key, method) in [
        ("clientRequests", "initialize"),
        ("clientNotifications", "initialized"),
    ] {
        let definitions = field(&catalog, key)
            .as_array()
            .unwrap()
            .iter()
            .filter(|definition| definition.get("method") == Some(&IvyJson::from(method)))
            .cloned()
            .collect();
        catalog.insert(key, IvyJson::Array(definitions));
    }
    catalog.insert("serverRequests", IvyJson::Array(vec![]));
    catalog.insert("serverNotifications", IvyJson::Array(vec![]));
    let catalog = Arc::new(catalog);
    for (row, expected_row) in rows.iter().zip(field(oracle, "rows").as_array().unwrap()) {
        let mode = match text(row, "mode").to_utf8().unwrap().as_str() {
            "external" => NativeMode::ExternalProxy,
            "claude" => NativeMode::ClaudeAdapter,
            _ => NativeMode::OwnedStdio,
        };
        let (input, read) = tokio::io::duplex(65536);
        let (mut write, output) = tokio::io::duplex(65536);
        let rpc = NativeRpc::start(catalog.clone(), input, output, Default::default()).unwrap();
        let connected = rpc.clone();
        let expected = expected.clone();
        let daemon = field(row, "daemon").clone();
        let task = tokio::spawn(async move {
            initialize_agent(
                &connected,
                AgentHandshake {
                    client_version: &"1.2.3".into(),
                    native_version: &"0.142.3".into(),
                    expected_home: &expected,
                    mode,
                    daemon: if daemon.is_null() {
                        None
                    } else {
                        Some(&daemon)
                    },
                },
            )
            .await
        });
        let mut read = BufReader::new(read);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), read.read_line(&mut line))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "initialize request was not written: {} (task finished: {})",
                    stringify(row, 65536).unwrap(),
                    task.is_finished()
                )
            })
            .unwrap();
        let request = parse(&line, 65536).unwrap();
        let mut frame = request.clone();
        frame.remove("id");
        let mut frames = vec![frame];
        let mut reply = field(row, "reply").clone();
        reply.insert("id", field(&request, "id").clone());
        let bytes = format!("{}\n", stringify(&reply, 65536).unwrap());
        tokio::time::timeout(Duration::from_secs(2), write.write_all(bytes.as_bytes()))
            .await
            .unwrap()
            .unwrap();
        let actual = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "initialize did not settle: {}",
                    stringify(row, 65536).unwrap()
                )
            })
            .unwrap();
        let (value, error) = match actual {
            Ok(value) => {
                line.clear();
                tokio::time::timeout(Duration::from_secs(2), read.read_line(&mut line))
                    .await
                    .unwrap_or_else(|_| {
                        panic!(
                            "initialized notification was not written: {}",
                            stringify(row, 65536).unwrap()
                        )
                    })
                    .unwrap();
                frames.push(parse(&line, 65536).unwrap());
                (
                    IvyJson::object([
                        ("initialized", value.initialized),
                        ("nativeHome", path(&value.native_home)),
                        (
                            "actualServerVersion",
                            IvyJson::String(value.actual_server_version),
                        ),
                    ]),
                    IvyJson::Null,
                )
            }
            Err(error) => {
                line.clear();
                if let Ok(Ok(count)) =
                    tokio::time::timeout(Duration::from_millis(20), read.read_line(&mut line)).await
                {
                    if count != 0 {
                        frames.push(parse(&line, 65536).unwrap());
                    }
                }
                (
                    IvyJson::Null,
                    IvyJson::object([
                        ("code", IvyJson::String(error.code_js())),
                        ("message", IvyJson::String(error.message_js())),
                    ]),
                )
            }
        };
        same(
            &IvyJson::object([
                ("frames", IvyJson::Array(frames)),
                ("value", value),
                ("error", error),
            ]),
            expected_row,
        );
        tokio::time::timeout(Duration::from_secs(2), rpc.close("fixture_complete"))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "RPC close did not settle: {}",
                    stringify(row, 65536).unwrap()
                )
            });
    }
}

async fn claude_call(rpc: &NativeRpc, method: &str, params: IvyJson) -> IvyJson {
    let reply = rpc
        .request(
            method.into(),
            params,
            ivy_host_runtime::codex_rpc::RequestOptions {
                timeout_ms: 10_000,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(reply.get("error").is_none(), "{method}: {reply:?}");
    field(&reply, "result").clone()
}
async fn claude_event(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<IvyJson>,
    method: &str,
) -> IvyJson {
    let mut observed = vec![];
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.get("method") == Some(&method.into()) {
                return event;
            }
            observed.push(event.get("method").cloned().unwrap_or(IvyJson::Null));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Missing native event: {method}; observed {observed:?}"))
}
fn launch_options(
    root: &Root,
    settings: &IvyJson,
    events: tokio::sync::mpsc::UnboundedSender<IvyJson>,
) -> NativeProcessOptions {
    let mut inputs = ivy_agent_manager::runtime::home_inputs().unwrap();
    inputs.account_home = node_path::from_native(&root.0);
    NativeProcessOptions {
        artifact_root: node_path::from_native(&root.0.join("artifact")),
        data_root: node_path::from_native(&root.0.join("data")),
        settings: settings.clone(),
        client_version: "test".into(),
        hive_credential: None,
        home_inputs: inputs,
        before_launch: None,
        begin_epoch: Arc::new(move |_| {
            let requests = events.clone();
            let notifications = events.clone();
            let closed = events.clone();
            Ok(ivy_host_runtime::codex_rpc::Events {
                on_request: Arc::new(move |r| {
                    let result = requests.send(IvyJson::object([
                        ("id", r.id),
                        ("method", IvyJson::String(r.method)),
                        ("params", r.params),
                    ]));
                    Box::pin(async move { result.map_err(|_| IvyError::internal()) })
                }),
                on_notification: Arc::new(move |r| {
                    let result = notifications.send(IvyJson::object([
                        ("method", IvyJson::String(r.method)),
                        ("params", r.params),
                    ]));
                    Box::pin(async move { result.map_err(|_| IvyError::internal()) })
                }),
                on_close: Arc::new(move |code| {
                    let _ = closed.send(IvyJson::object([
                        ("method", "fixture/closed".into()),
                        ("code", code.into()),
                    ]));
                    Box::pin(async { Ok(()) })
                }),
            })
        }),
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn owned_linux_launch_frees_idle_pages_and_preserves_explicit_allocator_choices() {
    use ivy_contracts::hash::digest;
    let root = Root::new();
    let executable = root.0.join("native");
    let compiled = Command::new("rustc")
        .args(["--edition=2024", "-C", "debuginfo=0"])
        .arg(repo().join("tests/fixtures/native-launch-child.rs"))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let node = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success());
    let node = String::from_utf8(node.stdout).unwrap();
    let hash = digest(fs::read(&executable).unwrap());
    let catalog_path = root
        .0
        .join("artifact/dist/specs/native/codex-0.154.0/catalog.json");
    fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
    let mut catalog = parse(
        &fs::read_to_string(repo().join("specs/native/codex-0.154.0/catalog.json")).unwrap(),
        usize::MAX,
    )
    .unwrap();
    catalog.insert("nativeExecutableHash", hash.clone().into());
    fs::write(catalog_path, stringify(&catalog, usize::MAX).unwrap()).unwrap();
    let parent = [
        std::env::var_os("_RJEM_MALLOC_CONF"),
        std::env::var_os("MALLOC_CONF"),
    ];
    for (index, external, rjem, malloc) in [
        (0, false, None, None),
        (1, false, Some("background_thread:false"), None),
        (2, false, None, Some("dirty_decay_ms:5000")),
        (3, false, Some(""), None),
        (4, false, None, Some("")),
        (
            5,
            false,
            Some("background_thread:false"),
            Some("dirty_decay_ms:5000"),
        ),
        (6, true, None, None),
        (
            7,
            true,
            Some("background_thread:true"),
            Some("dirty_decay_ms:5000"),
        ),
    ] {
        let home = root.0.join("data/native");
        let marker = root.0.join(format!("case-{index}"));
        fs::write(
            root.0.join("launch-fixture.txt"),
            format!(
                "{}\n{}\nlaunch-environment\n{}\n{}\n{{}}\n0.154.0\n",
                node.trim(),
                repo()
                    .join("tests/fixtures/native-connection-child.mjs")
                    .display(),
                home.display(),
                marker.display(),
            ),
        )
        .unwrap();
        let mut server = IvyJson::object([("mode", "owned-stdio".into())]);
        if external {
            server = IvyJson::object([
                ("mode", "external-proxy".into()),
                ("socketPath", path(&root.0.join("unused.sock"))),
                ("expectedCodexHome", path(&home)),
            ]);
        }
        let settings = IvyJson::object([
            ("nativeExecutable", path(&executable)),
            ("nativeVersion", "0.154.0".into()),
            ("nativeExecutableHash", hash.clone().into()),
            ("codexHome", path(&home)),
            ("appServer", server),
            (
                "limits",
                IvyJson::object([
                    ("maxOperations", 100.0.into()),
                    ("maxJournalBytes", 1048576.0.into()),
                    ("maxPendingInputs", 8.0.into()),
                    ("maxNotificationBytes", 1048576.0.into()),
                ]),
            ),
        ]);
        let (send, _events) = tokio::sync::mpsc::unbounded_channel();
        let mut options = launch_options(&root, &settings, send);
        for (key, value) in [("_RJEM_MALLOC_CONF", rjem), ("MALLOC_CONF", malloc)] {
            options.home_inputs.environment.remove(&JsString::from(key));
            if let Some(value) = value {
                options
                    .home_inputs
                    .environment
                    .insert(key.into(), value.into());
            }
        }
        let result = tokio::time::timeout(Duration::from_secs(15), start_native_process(options))
            .await
            .unwrap();
        if external {
            // The inert proxy records its environment and exits without a server.
            assert!(result.is_err());
        } else {
            let connection = result.unwrap();
            assert_eq!(connection.mode, NativeMode::OwnedStdio);
            connection.close().await.unwrap();
        }
        let launches = fs::read_to_string(format!("{}.launches", marker.display())).unwrap();
        let launches: Vec<_> = launches
            .lines()
            .map(|s| parse(s, usize::MAX).unwrap())
            .collect();
        assert_eq!(launches.len(), if external { 3 } else { 2 });
        let expected_rjem = if external {
            None
        } else {
            rjem.or_else(|| malloc.is_none().then_some("background_thread:true"))
        };
        let expected_malloc = if external { None } else { malloc };
        for (position, launch) in launches.iter().enumerate() {
            let child = position == launches.len() - 1;
            assert_eq!(
                launch.get("rjemMallocConf"),
                Some(&if child {
                    expected_rjem.map(IvyJson::from).unwrap_or(IvyJson::Null)
                } else {
                    IvyJson::Null
                }),
                "case {index}, launch {position}"
            );
            assert_eq!(
                launch.get("mallocConf"),
                Some(&if child {
                    expected_malloc.map(IvyJson::from).unwrap_or(IvyJson::Null)
                } else {
                    IvyJson::Null
                }),
                "case {index}, launch {position}"
            );
        }
    }
    assert_eq!(
        [
            std::env::var_os("_RJEM_MALLOC_CONF"),
            std::env::var_os("MALLOC_CONF")
        ],
        parent
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_claude_launch_retries_only_zero_work_preserves_inputs_and_reopens_data() {
    use ivy_contracts::hash::digest;
    let root = Root::new();
    let artifact = root.0.join("artifact");
    let native = artifact.join("dist/native");
    let sdk = root.0.join("sdk");
    let data = root.0.join("data");
    fs::create_dir_all(&native).unwrap();
    fs::create_dir_all(sdk.join("bridge")).unwrap();
    fs::create_dir_all(data.join("native")).unwrap();
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_ivy-agent-manager"));
    let target = executable.parent().unwrap();
    let binary = if cfg!(windows) {
        "ivy-agent-manager.exe"
    } else {
        "ivy-agent-manager"
    };
    fs::copy(&executable, native.join(binary)).unwrap();
    if cfg!(windows) {
        fs::copy(target.join("ivy-job.exe"), native.join("ivy-job.exe")).unwrap();
        for entry in fs::read_dir(target).unwrap().flatten() {
            if entry.path().extension().is_some_and(|v| v == "dll") {
                fs::copy(entry.path(), native.join(entry.file_name())).unwrap();
            }
        }
    }
    for name in ["main.mjs", "session.mjs"] {
        fs::copy(
            repo()
                .join("services/agent-manager/claude/bridge")
                .join(name),
            sdk.join("bridge").join(name),
        )
        .unwrap();
    }
    let module = sdk.join("node_modules/@anthropic-ai/claude-agent-sdk");
    fs::create_dir_all(&module).unwrap();
    fs::write(
        module.join("package.json"),
        r#"{"type":"module","exports":"./index.mjs"}"#,
    )
    .unwrap();
    fs::copy(
        repo().join("tests/fixtures/agent-claude-sdk-native.mjs"),
        module.join("index.mjs"),
    )
    .unwrap();
    fs::write(sdk.join("package-lock.json"), "{}").unwrap();
    let mut manifest = vec![];
    for name in ["bridge/main.mjs", "bridge/session.mjs", "package-lock.json"] {
        manifest.extend_from_slice(name.as_bytes());
        manifest.push(0);
        manifest.extend_from_slice(digest(fs::read(sdk.join(name)).unwrap())[7..].as_bytes());
        manifest.push(0);
    }
    let bundle = digest(manifest);
    let catalog_path = native
        .parent()
        .unwrap()
        .join("specs/native/codex-0.142.3/catalog.json");
    fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
    let mut catalog = parse(
        &fs::read_to_string(repo().join("specs/native/codex-0.142.3/catalog.json")).unwrap(),
        usize::MAX,
    )
    .unwrap();
    catalog.insert("nativeExecutableHash", bundle.clone().into());
    fs::write(catalog_path, stringify(&catalog, usize::MAX).unwrap()).unwrap();
    let node = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success());
    let node = PathBuf::from(String::from_utf8(node.stdout).unwrap().trim());
    let node_hash = digest(fs::read(&node).unwrap());
    fs::write(
        data.join("claude-mcp.json"),
        r#"{"inert":{"type":"http","url":"http://127.0.0.1:1/mcp"}}"#,
    )
    .unwrap();
    let mut settings = IvyJson::object([
        ("nativeExecutable", path(&node)),
        ("nativeVersion", "0.142.3".into()),
        ("nativeExecutableHash", bundle.into()),
        ("codexHome", path(&data.join("native"))),
        (
            "appServer",
            IvyJson::object([
                ("mode", "claude-adapter".into()),
                ("adapterRoot", path(&sdk)),
                ("nodeExecutableHash", node_hash.clone().into()),
                ("claudeExecutable", path(&node)),
                ("claudeExecutableHash", node_hash.into()),
                ("defaultModel", "sonnet".into()),
            ]),
        ),
        (
            "limits",
            IvyJson::object([
                ("maxOperations", 100.0.into()),
                ("maxJournalBytes", 1048576.0.into()),
                ("maxPendingInputs", 8.0.into()),
                ("maxNotificationBytes", 1048576.0.into()),
            ]),
        ),
    ]);
    if cfg!(windows) {
        let shell = root.0.join("pwsh.exe");
        let compiled = Command::new("rustc")
            .args(["--edition=2024", "-C", "debuginfo=0"])
            .arg(repo().join("tests/fixtures/native-launch-child.rs"))
            .arg("-o")
            .arg(&shell)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        fs::write(
            root.0.join("launch-fixture.txt"),
            format!(
                "unused\nunused\nunused\nunused\n{}\n{{}}\n0.142.3\n",
                root.0.join("shell").display()
            ),
        )
        .unwrap();
        settings.insert(
            "windowsShell",
            IvyJson::object([
                ("executable", path(&shell)),
                ("executableHash", digest(fs::read(&shell).unwrap()).into()),
            ]),
        );
    }
    let (send, mut events) = tokio::sync::mpsc::unbounded_channel();
    let connection = tokio::time::timeout(
        Duration::from_secs(45),
        start_native_process(launch_options(&root, &settings, send)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(connection.mode, NativeMode::ClaudeAdapter);
    assert_eq!(connection.executable, native.join(binary));
    let models = claude_call(
        &connection.rpc,
        "model/list",
        IvyJson::object([] as [(&str, IvyJson); 0]),
    )
    .await;
    assert_eq!(
        text(&field(&models, "data").as_array().unwrap()[0], "model"),
        JsString::from("sonnet")
    );
    let thread = claude_call(
        &connection.rpc,
        "thread/start",
        IvyJson::object([
            ("cwd", path(&root.0)),
            ("model", "sonnet".into()),
            ("approvalPolicy", "on-request".into()),
            ("sandbox", "workspace-write".into()),
        ]),
    )
    .await;
    let thread = text(field(&thread, "thread"), "id");
    for (mode, status) in [
        ("useful", "completed"),
        ("retry", "completed"),
        ("empty", "failed"),
        ("unknown-empty", "failed"),
        ("eof", "failed"),
        ("tool", "completed"),
        ("stream", "completed"),
        ("input", "completed"),
        ("approval", "completed"),
        ("interrupt", "interrupted"),
    ] {
        if mode == "retry" {
            fs::write(
                data.join("claude-mcp.json"),
                r#"{"refreshed":{"type":"http","url":"http://127.0.0.1:1/mcp"}}"#,
            )
            .unwrap();
        }
        let response = claude_call(
            &connection.rpc,
            "turn/start",
            IvyJson::object([
                ("threadId", IvyJson::String(thread.clone())),
                (
                    "input",
                    IvyJson::Array(vec![IvyJson::object([
                        ("type", "text".into()),
                        ("text", mode.into()),
                    ])]),
                ),
            ]),
        )
        .await;
        let turn = text(field(&response, "turn"), "id");
        if mode == "input" {
            let request = claude_event(&mut events, "item/tool/requestUserInput").await;
            let question = text(
                &field(field(&request, "params"), "questions")
                    .as_array()
                    .unwrap()[0],
                "id",
            );
            connection
                .rpc
                .answer(
                    "item/tool/requestUserInput".into(),
                    field(&request, "id").clone(),
                    IvyJson::object([(
                        "result",
                        IvyJson::object([(
                            "answers",
                            IvyJson::object([(
                                question,
                                IvyJson::object([("answers", IvyJson::Array(vec!["A".into()]))]),
                            )]),
                        )]),
                    )]),
                    Arc::new(|_| Box::pin(async { Ok(()) })),
                )
                .await
                .unwrap();
        }
        if mode == "approval" || mode == "interrupt" {
            let request = claude_event(&mut events, "item/commandExecution/requestApproval").await;
            if mode == "approval" {
                claude_call(
                    &connection.rpc,
                    "turn/steer",
                    IvyJson::object([
                        ("threadId", IvyJson::String(thread.clone())),
                        ("expectedTurnId", IvyJson::String(turn.clone())),
                        (
                            "input",
                            IvyJson::Array(vec![IvyJson::object([
                                ("type", "text".into()),
                                ("text", "follow up".into()),
                            ])]),
                        ),
                    ]),
                )
                .await;
                connection
                    .rpc
                    .answer(
                        "item/commandExecution/requestApproval".into(),
                        field(&request, "id").clone(),
                        IvyJson::object([(
                            "result",
                            IvyJson::object([("decision", "accept".into())]),
                        )]),
                        Arc::new(|_| Box::pin(async { Ok(()) })),
                    )
                    .await
                    .unwrap();
            } else {
                claude_call(
                    &connection.rpc,
                    "turn/interrupt",
                    IvyJson::object([
                        ("threadId", IvyJson::String(thread.clone())),
                        ("turnId", IvyJson::String(turn.clone())),
                    ]),
                )
                .await;
            }
        }
        let done = claude_event(&mut events, "turn/completed").await;
        assert_eq!(text(field(field(&done, "params"), "turn"), "id"), turn);
        assert_eq!(
            text(field(field(&done, "params"), "turn"), "status"),
            JsString::from(status),
            "{mode}: {done:?}"
        );
    }
    let calls = fs::read_to_string(root.0.join("sdk-fixture.jsonl")).unwrap();
    let calls: Vec<_> = calls
        .lines()
        .map(|s| parse(s, usize::MAX).unwrap())
        .collect();
    for (mode, count) in [
        ("useful", 1),
        ("retry", 2),
        ("empty", 2),
        ("unknown-empty", 1),
        ("eof", 1),
        ("tool", 1),
        ("stream", 1),
    ] {
        assert_eq!(
            calls
                .iter()
                .filter(|v| v.get("mode") == Some(&mode.into()))
                .count(),
            count,
            "{mode}"
        );
    }
    assert!(calls.iter().any(|v| v.get("type") == Some(&"steer".into())));
    assert!(calls.iter().any(|v| v.get("mode") == Some(&"retry".into())
        && field(v, "mcpServers").get("refreshed").is_some()));
    let response = claude_call(
        &connection.rpc,
        "turn/start",
        IvyJson::object([
            ("threadId", IvyJson::String(thread.clone())),
            (
                "input",
                IvyJson::Array(vec![IvyJson::object([
                    ("type", "text".into()),
                    ("text", "hold".into()),
                ])]),
            ),
        ]),
    )
    .await;
    assert!(response.get("turn").is_some());
    let held_turn = text(field(&response, "turn"), "id");
    tokio::time::timeout(Duration::from_secs(10), connection.close())
        .await
        .unwrap()
        .unwrap();
    // Keep the event receiver alive during the second handshake.
    let (send2, mut events2) = tokio::sync::mpsc::unbounded_channel();
    let reopened = start_native_process(launch_options(&root, &settings, send2))
        .await
        .unwrap();
    let saved = claude_call(
        &reopened.rpc,
        "thread/read",
        IvyJson::object([
            ("threadId", IvyJson::String(thread)),
            ("includeTurns", true.into()),
        ]),
    )
    .await;
    assert!(
        field(field(&saved, "thread"), "turns")
            .as_array()
            .unwrap()
            .len()
            >= 8
    );
    let turns = field(field(&saved, "thread"), "turns").as_array().unwrap();
    let held = turns
        .iter()
        .find(|turn| turn.get("id") == Some(&IvyJson::String(held_turn.clone())))
        .unwrap();
    assert_eq!(held.get("status"), Some(&"interrupted".into()));
    while events2.try_recv().is_ok() {}
    reopened.close().await.unwrap();
    assert!(!data.join("process-stop-fence.json").exists());
}
