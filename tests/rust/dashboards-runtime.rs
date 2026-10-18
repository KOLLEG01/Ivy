use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{IvyJson as J, hash::tool_definition_hash, json};
use ivy_dashboards::runtime;
use ivy_host_runtime::config::{self, InstanceConfig};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
#[ignore = "requires pinned isolated Chromium and the built Windows job launcher"]
async fn provider_reconnect_health_and_identity_bound_shutdown() {
    assert!(
        std::env::var_os("IVY_LAUNCH_ID").is_some(),
        "the control fixture requires an explicit launch identity"
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/ivy/", listener.local_addr().unwrap());
    let (events, mut seen) = mpsc::unbounded_channel();
    let registry = ivy_dashboards::registry();
    let list = registry.required("namespaces").unwrap().as_array().unwrap()[0]
        .required("tools")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool.get("name") == Some(&J::from("list")))
        .unwrap();
    let hash = tool_definition_hash(list).unwrap();
    let peer = tokio::spawn(async move {
        for generation in [1, 2] {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            let mut invoked = false;
            while let Some(Ok(frame)) = socket.next().await {
                let Message::Text(text) = frame else {
                    continue;
                };
                let value = json::parse(&text, 32 * 1024 * 1024).unwrap();
                let method = value.get("method").and_then(|v| v.as_str().ok());
                if method.is_none() {
                    assert_eq!(
                        value.required("result").unwrap(),
                        &J::object([("items", J::Array(vec![])), ("nextCursor", J::Null)])
                    );
                    events.send(generation).unwrap();
                    if generation == 1 {
                        socket.close(None).await.unwrap();
                        break;
                    }
                    continue;
                }
                let method = method.unwrap();
                let params = value.required("params").unwrap();
                let result = match method.as_str() {
                    "service.connect" => {
                        assert_eq!(params.get("serviceName"), Some(&J::from("dashboards")));
                        J::object([("generation", J::Number(generation as f64))])
                    }
                    "registry.sync" => {
                        assert_eq!(
                            params.required("namespaces").unwrap().as_array().unwrap()[0]
                                .required("tools")
                                .unwrap()
                                .as_array()
                                .unwrap()
                                .len(),
                            7
                        );
                        J::object([] as [(&str, J); 0])
                    }
                    "objects.stat" => {
                        assert_eq!(params.get("objectId"), Some(&J::from("root")));
                        J::object([] as [(&str, J); 0])
                    }
                    "objects.query" => {
                        assert_eq!(
                            params.required("where").unwrap().get("value"),
                            Some(&J::from("root"))
                        );
                        J::object([("items", J::Array(vec![])), ("nextCursor", J::Null)])
                    }
                    "service.heartbeat" => params.clone(),
                    _ => panic!("unexpected method: {method}"),
                };
                let reply = J::object([
                    ("jsonrpc", "2.0".into()),
                    ("id", value.required("id").unwrap().clone()),
                    ("result", result),
                ]);
                socket
                    .send(Message::Text(
                        json::stringify(&reply, 32 * 1024 * 1024).unwrap().into(),
                    ))
                    .await
                    .unwrap();
                if method == "service.heartbeat"
                    && params.get("ready") == Some(&J::Bool(true))
                    && !invoked
                {
                    invoked = true;
                    let invocation = J::object([
                        ("jsonrpc", "2.0".into()),
                        ("id", "fixture-invoke".into()),
                        ("method", "provider.invoke".into()),
                        (
                            "params",
                            J::object([
                                ("qualifiedName", "dashboards.list".into()),
                                ("arguments", J::object([] as [(&str, J); 0])),
                                ("definitionHash", hash.clone().into()),
                                ("generation", J::Number(generation as f64)),
                                ("callerPrincipalId", "fixture-caller".into()),
                            ]),
                        ),
                    ]);
                    socket
                        .send(Message::Text(
                            json::stringify(&invocation, 32 * 1024 * 1024)
                                .unwrap()
                                .into(),
                        ))
                        .await
                        .unwrap();
                }
            }
        }
    });
    let root =
        std::env::temp_dir().join(format!("ivy-dashboards-runtime-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("artifact/dist/native")).unwrap();
    std::fs::create_dir(root.join("data")).unwrap();
    #[cfg(windows)]
    std::fs::copy(
        std::env::var_os("DASHBOARDS_TEST_JOB").unwrap(),
        root.join("artifact/dist/native/ivy-job.exe"),
    )
    .unwrap();
    let value = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("hostId", "fixture-host".into()),
        ("instanceId", "renamed-dashboard".into()),
        ("serviceNodeId", "fixture-host.dashboards".into()),
        ("componentId", "dashboards".into()),
        ("publicBaseUrl", base.into()),
        (
            "dataRoot",
            root.join("data").to_string_lossy().to_string().into(),
        ),
        (
            "artifactRoot",
            root.join("artifact").to_string_lossy().to_string().into(),
        ),
        ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
        ("version", "1.0.0".into()),
        ("credential", "isolated-test-token".into()),
        (
            "settings",
            J::object([
                ("rootObjectId", "root".into()),
                (
                    "chromiumExecutable",
                    std::env::var("DASHBOARDS_TEST_BROWSER").unwrap().into(),
                ),
            ]),
        ),
    ]);
    let instance = InstanceConfig::from_json(value).unwrap();
    let runtime = runtime::start(instance.clone()).await.unwrap();
    for generation in [1, 2] {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(30), seen.recv())
                .await
                .unwrap(),
            Some(generation)
        );
    }
    let health = config::json_file(root.join("data/health.json"), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(health.get("ready"), Some(&J::Bool(true)));
    assert_eq!(health.get("generation"), Some(&J::Number(2.0)));
    // Exercise the production health reader against the real runtime's file.
    // Registration RPC is covered separately by the Host fixture; this peer
    // models the provider WebSocket only.
    let checked = ivy_host_runtime::health::check_health(&instance, None, false)
        .await
        .unwrap();
    assert_eq!(checked.get("generation"), Some(&J::Number(2.0)));
    assert_eq!(checked.get("bootId"), health.get("bootId"));
    let mut control = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("instanceId", "renamed-dashboard".into()),
        ("bootId", "wrong".into()),
        ("action", "shutdown".into()),
        ("requestedAt", "2026-10-18T00:00:00.000Z".into()),
    ]);
    if let Some(launch) = health.get("launchId") {
        control.insert("launchId", launch.clone());
    }
    config::atomic_json(root.join("data/control.json"), control.clone())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(650), runtime.stopped())
            .await
            .is_err()
    );
    control.insert("bootId", health.required("bootId").unwrap().clone());
    config::atomic_json(root.join("data/control.json"), control)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), runtime.stopped())
        .await
        .unwrap();
    runtime.close().await.unwrap();
    let final_health = config::json_file(root.join("data/health.json"), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(final_health.get("ready"), Some(&J::Bool(false)));
    let stopped = ivy_host_runtime::health::check_health(&instance, None, false)
        .await
        .unwrap_err();
    assert_eq!(stopped.code_js(), "service_not_ready".into());
    assert_eq!(
        final_health.get("details"),
        Some(&J::from("Dashboards stopped."))
    );
    peer.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
