use ivy_agent_manager::{
    native_handshake::{AgentHandshake, NativeMode, initialize_agent},
    native_launch::{NativeHomeInputs, NativeProcessOptions, start_native_process},
};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    hash::digest,
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

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-agent-owner-Grüße😀-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(paths::realpath(&path).unwrap())
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
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap()
    );
}
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn path(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn original(input: IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .arg("tests/fixtures/agent-native-owner-oracle.mjs")
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
    original(IvyJson::object([
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
        // Only small pinned fixture files reach this hasher. Production requires the
        // Host streaming primitive; this corpus does not qualify that dependency.
        file_hash: Arc::new(|path| {
            Box::pin(async move { Ok(digest(tokio::fs::read(path).await.unwrap())) })
        }),
        record_stop: Arc::new(|_, _, _, _| panic!("No native process belongs to this fixture")),
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
        let mut prepared = prepared(&root, variant);
        prepared.insert("action", "preflight".into());
        let expected = original(prepared.clone());
        let before = Arc::new(Mutex::new(None));
        let observed = before.clone();
        let base = node_path::filesystem_path(&text(&prepared, "root"));
        let normalize = base.clone();
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
            canonical(&expected, usize::MAX).unwrap(),
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
    let (release, mut wait) = watch::channel(false);
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
    let oracle = original(IvyJson::object([
        ("action", "handshake".into()),
        ("rows", IvyJson::Array(rows.clone())),
    ]));
    let catalog = parse(
        include_str!("../../specs/native/codex-0.142.3/catalog.json"),
        usize::MAX,
    )
    .unwrap();
    for (row, expected_row) in rows.iter().zip(field(&oracle, "rows").as_array().unwrap()) {
        let mode = match text(row, "mode").to_utf8().unwrap().as_str() {
            "external" => NativeMode::ExternalProxy,
            "claude" => NativeMode::ClaudeAdapter,
            _ => NativeMode::OwnedStdio,
        };
        let (input, read) = tokio::io::duplex(65536);
        let (mut write, output) = tokio::io::duplex(65536);
        let rpc = NativeRpc::start(&catalog, input, output, Default::default()).unwrap();
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
        read.read_line(&mut line).await.unwrap();
        let request = parse(&line, 65536).unwrap();
        let mut frame = request.clone();
        frame.remove("id");
        let mut frames = vec![frame];
        let mut reply = field(row, "reply").clone();
        reply.insert("id", field(&request, "id").clone());
        write
            .write_all(format!("{}\n", stringify(&reply, 65536).unwrap()).as_bytes())
            .await
            .unwrap();
        let actual = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        let (value, error) = match actual {
            Ok(value) => {
                line.clear();
                read.read_line(&mut line).await.unwrap();
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
            Err(error) => (
                IvyJson::Null,
                IvyJson::object([
                    ("code", IvyJson::String(error.code_js())),
                    ("message", IvyJson::String(error.message_js())),
                ]),
            ),
        };
        same(
            &IvyJson::object([
                ("frames", IvyJson::Array(frames)),
                ("value", value),
                ("error", error),
            ]),
            expected_row,
        );
        rpc.close("fixture_complete").await;
    }
}
