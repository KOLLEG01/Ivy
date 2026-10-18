use ivy_host_runtime::{codex_home, codex_rpc, config, node_path, paths, process, process_fence};
#[path = "../../packages/host-runtime/rust/src/codex_connection.rs"]
mod codex_connection;
#[path = "../../packages/host-runtime/rust/src/codex_daemon.rs"]
mod codex_daemon;
#[path = "../../packages/host-runtime/rust/src/codex_handshake.rs"]
mod codex_handshake;
#[path = "../../packages/host-runtime/rust/src/codex_process.rs"]
mod codex_process;
#[path = "../../packages/host-runtime/rust/src/codex_proxy_transport.rs"]
mod codex_proxy_transport;
#[path = "../../packages/host-runtime/rust/src/codex_windows_shell.rs"]
mod codex_windows_shell;
use ivy_contracts::{
    IvyJson, JsString, Outcome,
    json::{parse, stringify},
};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-launch-Grüße😀-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(paths::realpath(&root).unwrap())
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn text(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn parsed(value: &str) -> IvyJson {
    parse(value, 4 * 1024 * 1024).unwrap()
}
fn field(value: &IvyJson, key: &str) -> JsString {
    value.required(key).unwrap().as_js_string().unwrap().clone()
}
fn job() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("ivy-job.exe")
}
fn compile_fixture(root: &Root) -> PathBuf {
    let output = root.0.join(if cfg!(windows) {
        "fixture.exe"
    } else {
        "fixture"
    });
    let result = Command::new("rustc")
        .args(["--edition=2024", "-C", "debuginfo=0"])
        .arg(repo().join("tests/fixtures/native-launch-child.rs"))
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    output
}
fn oracle(input: &IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .arg(repo().join("tests/fixtures/native-launch-oracle.mjs"))
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
        .write_all(stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(&String::from_utf8(output.stdout).unwrap(), 4 * 1024 * 1024).unwrap()
}
fn input(root: &Path, fixture: &Path, variant: &str, action: &str) -> IvyJson {
    IvyJson::object([
        ("root", text(root)),
        ("fixture", text(fixture)),
        ("job", text(&job())),
        ("variant", variant.into()),
        ("action", action.into()),
    ])
}
async fn start(
    prepared: &IvyJson,
    events: codex_rpc::Events,
) -> ivy_contracts::IvyResult<codex_connection::NativeConnection> {
    let paths = node_path::Context::current().unwrap();
    let environment: BTreeMap<_, _> = std::env::vars_os()
        .map(|(key, value)| {
            (
                node_path::from_native(Path::new(&key)),
                node_path::from_native(Path::new(&value)),
            )
        })
        .collect();
    let cwd = node_path::from_native(&std::env::current_dir().unwrap());
    let home = field(prepared, "home");
    let context = codex_home::HomeContext {
        paths: &paths,
        environment: &environment,
        cwd: &cwd,
        user_home: None,
        account_home: &home,
    };
    let artifact_root = field(prepared, "artifactRoot");
    let data_root = field(prepared, "dataRoot");
    codex_process::start_codex_process(codex_process::StartOptions {
        settings: prepared.required("settings").unwrap(),
        artifact_root: &artifact_root,
        data_root: &data_root,
        home_context: &context,
        client_name: "IvyVoiceFixture".into(),
        client_version: "1.0.0".into(),
        hive_credential: Some("fixture-credential".into()),
        events,
    })
    .await
}

async fn proxy_peer(
    listener: tokio::net::TcpListener,
    home: JsString,
    frames: Arc<Mutex<Vec<IvyJson>>>,
    trace: Arc<Mutex<Vec<IvyJson>>>,
) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    while let Some(Ok(message)) = socket.next().await {
        match message {
            Message::Text(value) => {
                let frame = parsed(value.as_str());
                let mut retained = frame.clone();
                if frame.get("method").is_some() {
                    retained.remove("id");
                }
                frames.lock().unwrap().push(retained);
                let method = frame.get("method").and_then(IvyJson::as_js_string);
                if method == Some(&"initialized".into()) {
                    for value in [
                        r#"{"id":7,"method":"currentTime/read","params":{"threadId":"voice-thread"}}"#,
                        r#"{"method":"serverRequest/resolved","params":{"requestId":7,"threadId":"voice-thread"}}"#,
                    ] {
                        socket.send(Message::Text(value.into())).await.unwrap();
                    }
                } else if let Some(method) = method {
                    let result = if method == &JsString::from("initialize") {
                        IvyJson::object([
                            ("codexHome", IvyJson::String(home.clone())),
                            ("userAgent", "codex/0.154.0".into()),
                        ])
                    } else {
                        parsed(r#"{"data":[],"nextCursor":null}"#)
                    };
                    let reply = IvyJson::object([
                        ("id", frame.required("id").unwrap().clone()),
                        ("result", result),
                    ]);
                    socket
                        .send(Message::Text(stringify(&reply, usize::MAX).unwrap().into()))
                        .await
                        .unwrap();
                }
            }
            Message::Close(frame) => {
                let frame = frame.unwrap();
                trace.lock().unwrap().push(IvyJson::object([(
                    "transportClose",
                    format!("{}:{}", u16::from(frame.code), frame.reason).into(),
                )]));
                let _ = socket.flush().await;
                break;
            }
            _ => {}
        }
    }
    // Neither close nor the tunnel Job may own the external listener.
    let next = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let _ = listener.accept().await.unwrap();
    drop(next);
}

#[tokio::test]
async fn complete_native_launch_matches_original_owned_proxy_preflight_and_callbacks() {
    let root = Root::new();
    let fixture = compile_fixture(&root);
    let mut variants = vec![
        "owned",
        "proxy",
        "automatic",
        "default",
        "claude",
        "relative-executable",
        "overlap",
        "fence",
        "catalog-hash",
        "initial-items",
        "executable-hash",
        "socket-relative",
        "expected-home",
        "cli-version",
        "daemon-version",
        "handshake-home",
        "handshake-version",
    ];
    if cfg!(windows) {
        variants.extend(["shell-missing", "shell-hash"]);
    }
    for variant in variants {
        let own_root = root.0.join(format!("rust-{variant}"));
        let original_root = root.0.join(format!("original-{variant}"));
        let external =
            matches!(variant, "proxy" | "automatic") || variant == "default" && cfg!(windows);
        let listener = if external {
            Some(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap())
        } else {
            None
        };
        let mut preparation = input(&own_root, &fixture, variant, "prepare");
        if let Some(listener) = &listener {
            preparation.insert(
                "port",
                IvyJson::Number(listener.local_addr().unwrap().port() as f64),
            );
        }
        let prepared = oracle(&preparation);
        let trace = Arc::new(Mutex::new(Vec::new()));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let server = listener.map(|listener| {
            tokio::spawn(proxy_peer(
                listener,
                field(&prepared, "home"),
                frames.clone(),
                trace.clone(),
            ))
        });
        let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
        let events = codex_rpc::Events {
            on_request: Arc::new({
                let trace = trace.clone();
                move |request| {
                    trace.lock().unwrap().push(IvyJson::object([(
                        "request",
                        IvyJson::object([
                            ("id", request.id.clone()),
                            ("method", IvyJson::String(request.method.clone())),
                            ("params", request.params.clone()),
                        ]),
                    )]));
                    request_tx.send(request).unwrap();
                    Box::pin(async { Ok(()) })
                }
            }),
            on_notification: Arc::new({
                let trace = trace.clone();
                move |notice| {
                    trace.lock().unwrap().push(IvyJson::object([(
                        "notification",
                        IvyJson::object([
                            ("method", IvyJson::String(notice.method)),
                            ("params", notice.params),
                        ]),
                    )]));
                    Box::pin(async { Ok(()) })
                }
            }),
            on_close: Arc::new({
                let trace = trace.clone();
                move |close| {
                    trace
                        .lock()
                        .unwrap()
                        .push(IvyJson::object([("close", close.into())]));
                    Box::pin(async { Ok(()) })
                }
            }),
        };
        let actual = match tokio::time::timeout(Duration::from_secs(30), start(&prepared, events))
            .await
            .unwrap()
        {
            Err(error) => IvyJson::object([("success", false.into()), ("error", error.to_wire())]),
            Ok(connection) => {
                let request = tokio::time::timeout(Duration::from_secs(5), request_rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                let reply = connection
                    .rpc
                    .request(
                        "model/list".into(),
                        parsed(r#"{"limit":1}"#),
                        codex_rpc::RequestOptions {
                            request_id: Some("voice-models".into()),
                            timeout_ms: 5000,
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                connection
                    .rpc
                    .answer(
                        "currentTime/read".into(),
                        request.id,
                        parsed(r#"{"result":{}}"#),
                        Arc::new({
                            let trace = trace.clone();
                            move |_| {
                                trace
                                    .lock()
                                    .unwrap()
                                    .push(IvyJson::object([("answerIntent", true.into())]));
                                Box::pin(async { Ok(()) })
                            }
                        }),
                    )
                    .await
                    .unwrap();
                connection
                    .rpc
                    .request(
                        "model/list".into(),
                        parsed(r#"{"limit":1}"#),
                        codex_rpc::RequestOptions {
                            request_id: Some("after-answer".into()),
                            timeout_ms: 5000,
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                let catalog_equal =
                    connection.rpc.catalog() == prepared.required("catalog").unwrap();
                assert!(uuid::Uuid::parse_str(&connection.epoch).is_ok());
                let (one, two) = tokio::join!(connection.close(), connection.close());
                one.unwrap();
                two.unwrap();
                let stopped = connection.completion.wait().await.unwrap();
                assert_ne!(stopped.error_code.as_deref(), Some("outcome_unknown"));
                IvyJson::object([
                    ("success", true.into()),
                    (
                        "mode",
                        match connection.mode {
                            codex_connection::Mode::OwnedStdio => "owned-stdio",
                            codex_connection::Mode::ExternalProxy => "external-proxy",
                        }
                        .into(),
                    ),
                    ("home", text(&connection.home)),
                    ("catalogEqual", catalog_equal.into()),
                    ("reply", reply),
                    ("connected", connection.rpc.connected().into()),
                    (
                        "fence",
                        process_fence::process_stop_fence(node_path::to_native(&field(
                            &prepared, "dataRoot",
                        )))
                        .await
                        .unwrap()
                        .unwrap_or(IvyJson::Null),
                    ),
                ])
            }
        };
        if let Some(server) = server {
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
        }
        let mut observation = input(&own_root, &fixture, variant, "observe");
        let mut actual = actual;
        actual.insert("trace", IvyJson::Array(trace.lock().unwrap().clone()));
        observation.insert("result", actual);
        if external {
            observation.insert("frames", IvyJson::Array(frames.lock().unwrap().clone()));
        }
        let actual = oracle(&observation);
        let expected = oracle(&input(&original_root, &fixture, variant, "run"));
        assert_eq!(actual, expected, "native launch variant {variant}");
    }
}

#[tokio::test]
async fn unknown_stop_evidence_survives_restart_and_blocks_before_catalog_or_launch() {
    let root = Root::new();
    let fixture = compile_fixture(&root);
    let prepared = oracle(&input(&root.0, &fixture, "owned", "prepare"));
    let data = node_path::to_native(&field(&prepared, "dataRoot"));
    let stopped = Arc::new(process::ProcessResult {
        exit_code: None,
        signal: None,
        error_code: Some("outcome_unknown".into()),
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
    });
    let saved = codex_connection::retain_stop_evidence(
        data.clone(),
        "fixture-owner",
        1234,
        stopped.clone(),
    )
    .await
    .unwrap();
    assert!(Arc::ptr_eq(&saved, &stopped));
    let fence = process_fence::process_stop_fence(data.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fence.get("ownerId"), Some(&"fixture-owner".into()));
    assert_eq!(fence.get("processId"), Some(&IvyJson::Number(1234.0)));
    assert_eq!(oracle(&input(&root.0, &fixture, "owned", "fence")), fence);
    let before = std::fs::read(data.join("process-stop-fence.json")).unwrap();
    codex_connection::retain_stop_evidence(data.clone(), "another-owner", 5678, stopped)
        .await
        .unwrap();
    assert_eq!(
        before,
        std::fs::read(data.join("process-stop-fence.json")).unwrap()
    );
    std::fs::remove_file(
        node_path::to_native(&field(&prepared, "artifactRoot"))
            .join("dist/specs/native/codex-0.154.0/catalog.json"),
    )
    .unwrap();
    let error = match start(&prepared, Default::default()).await {
        Err(error) => error,
        Ok(_) => panic!("fenced owner launched"),
    };
    assert_eq!(error.code, "outcome_unknown");
    assert_eq!(error.outcome, Outcome::Unknown);
    assert_eq!(
        error.details.unwrap().get("ownerId"),
        Some(&"fixture-owner".into())
    );
    assert!(!root.0.join("started.launches").exists());
    assert_eq!(
        before,
        std::fs::read(data.join("process-stop-fence.json")).unwrap()
    );
}
