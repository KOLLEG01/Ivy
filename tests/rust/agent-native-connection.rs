use ivy_host_runtime::{codex_rpc, node_path, paths, process, process_fence};
#[path = "../../packages/host-runtime/rust/src/codex_handshake.rs"]
mod codex_handshake;
#[path = "../../packages/host-runtime/rust/src/codex_proxy_transport.rs"]
mod codex_proxy_transport;
#[path = "../../packages/host-runtime/rust/src/codex_connection.rs"]
mod connection;
use connection::{Identity, Mode};
use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-native-connection-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(std::fs::canonicalize(root).unwrap())
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn catalog() -> IvyJson {
    parse(
        include_str!("../../specs/native/codex-0.154.0/catalog.json"),
        2 * 1024 * 1024,
    )
    .unwrap()
}
fn identity(root: &Root, mode: Mode) -> Identity {
    Identity {
        data_root: root.0.clone(),
        home: root.0.clone(),
        epoch: uuid::Uuid::new_v4().to_string(),
        native_version: "0.154.0".into(),
        client_name: "IvyFixture".into(),
        client_version: "1.0.0".into(),
        mode,
    }
}
async fn spawn(root: &Root, mode: &str, home: String) -> process::OwnedProcess {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let node = std::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success());
    let node = PathBuf::from(String::from_utf8(node.stdout).unwrap().trim());
    let mut context = process::LaunchContext {
        raw_output: true,
        ..Default::default()
    };
    if cfg!(windows) {
        let launcher = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ivy-job.exe");
        assert!(
            launcher.is_file(),
            "Build the host's ivy-job binary before native connection tests"
        );
        context.job_launcher = Some(launcher);
    }
    process::spawn(
        &process::Command {
            executable: "fixture".into(),
            cwd: None,
            timeout_ms: 10_000,
            args: vec![
                repo.join("tests/fixtures/native-connection-child.mjs")
                    .into_os_string(),
                mode.into(),
                home.into(),
                root.0.join("started").into_os_string(),
            ],
        },
        &root.0,
        &BTreeMap::from([("fixture".into(), node)]),
        context,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn owned_connection_initializes_requests_and_joins_one_close_result() {
    let root = Root::new();
    let child = spawn(&root, "owned", root.0.to_string_lossy().into_owned()).await;
    let closed = Arc::new(Mutex::new(Vec::new()));
    let observed = closed.clone();
    let events = codex_rpc::Events {
        on_close: Arc::new(move |code| {
            observed.lock().unwrap().push(code);
            Box::pin(async { Ok(()) })
        }),
        ..Default::default()
    };
    let native = connection::connect(child, identity(&root, Mode::OwnedStdio), catalog(), events)
        .await
        .unwrap();
    assert_eq!(native.home, root.0);
    assert_eq!(native.mode, Mode::OwnedStdio);
    assert!(!native.epoch.is_empty());
    let result = native
        .rpc
        .request(
            "model/list".into(),
            IvyJson::object([("limit", IvyJson::Number(1.0))]),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.get("result").unwrap().get("data"),
        Some(&IvyJson::Array(vec![]))
    );
    assert!(root.0.join("started.initialized").is_file());
    let (first, second) = tokio::join!(native.close(), native.close());
    first.unwrap();
    second.unwrap();
    assert!(!native.rpc.connected());
    assert_eq!(
        closed.lock().unwrap().as_slice(),
        &["native_owner_stopping"]
    );
    assert_ne!(
        native
            .completion
            .wait()
            .await
            .unwrap()
            .error_code
            .as_deref(),
        Some("outcome_unknown")
    );
    assert!(
        process_fence::process_stop_fence(&root.0)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn failed_handshake_retires_child_and_preserves_original_error() {
    let root = Root::new();
    let child = spawn(
        &root,
        "wrong-version",
        root.0.to_string_lossy().into_owned(),
    )
    .await;
    let completion = child.completion.clone();
    let result = connection::connect(
        child,
        identity(&root, Mode::OwnedStdio),
        catalog(),
        Default::default(),
    )
    .await;
    assert_eq!(result.err().unwrap().code, "native_server_version_mismatch");
    assert_ne!(
        completion.current().unwrap().error_code.as_deref(),
        Some("outcome_unknown")
    );
    assert!(!root.0.join("started.initialized").exists());
}

#[tokio::test]
async fn cancelled_startup_and_dropped_connection_retire_with_retained_rpc_clone() {
    let root = Root::new();
    let child = spawn(&root, "silent", root.0.to_string_lossy().into_owned()).await;
    let completion = child.completion.clone();
    let pending = connection::connect(
        child,
        identity(&root, Mode::OwnedStdio),
        catalog(),
        Default::default(),
    );
    // Even a never-polled startup future has transferred process ownership.
    drop(pending);
    let result = tokio::time::timeout(Duration::from_secs(8), completion.wait())
        .await
        .unwrap();
    assert_ne!(result.error_code.as_deref(), Some("outcome_unknown"));
    let child = spawn(&root, "owned", root.0.to_string_lossy().into_owned()).await;
    let native = connection::connect(
        child,
        identity(&root, Mode::OwnedStdio),
        catalog(),
        Default::default(),
    )
    .await
    .unwrap();
    let rpc = native.rpc.clone();
    let completion = native.completion.clone();
    drop(native);
    let result = tokio::time::timeout(Duration::from_secs(8), completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(result.error_code.as_deref(), Some("outcome_unknown"));
    assert!(!rpc.connected());
}

#[tokio::test]
async fn natural_exit_closes_rpc_once_and_preserves_exit_evidence() {
    let root = Root::new();
    let child = spawn(&root, "exit", root.0.to_string_lossy().into_owned()).await;
    let native = connection::connect(
        child,
        identity(&root, Mode::OwnedStdio),
        catalog(),
        Default::default(),
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(8), native.completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.exit_code, Some(7));
    assert!(!native.rpc.connected());
    native.close().await.unwrap();
}

#[tokio::test]
async fn closing_proxy_retires_only_tunnel_and_leaves_external_listener_available() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let root = Root::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = root.0.to_string_lossy().into_owned();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            match message {
                Message::Text(text) => {
                    let request = parse(text.as_str(), 65536).unwrap();
                    if request.get("method").and_then(IvyJson::as_js_string)
                        == Some(&"initialize".into())
                    {
                        let reply = IvyJson::object([
                            ("id", request.get("id").unwrap().clone()),
                            (
                                "result",
                                IvyJson::object([
                                    ("codexHome", home.clone().into()),
                                    ("userAgent", "codex/0.154.0".into()),
                                ]),
                            ),
                        ]);
                        ws.send(Message::Text(stringify(&reply, 65536).unwrap().into()))
                            .await
                            .unwrap();
                    }
                }
                Message::Close(frame) => {
                    assert_eq!(frame.unwrap().reason.as_str(), "ivy_agent_stopping");
                    let _ = ws.flush().await;
                    break;
                }
                _ => {}
            }
        }
        // This listener belongs to the external server, outside the tunnel Job.
        listener.accept().await.unwrap();
    });
    let child = spawn(&root, "proxy", port.to_string()).await;
    let native = connection::connect(
        child,
        identity(&root, Mode::ExternalProxy),
        catalog(),
        Default::default(),
    )
    .await
    .unwrap();
    native.close().await.unwrap();
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
