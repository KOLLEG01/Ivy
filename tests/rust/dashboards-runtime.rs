use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{IvyJson as J, hash::tool_definition_hash, json};
use ivy_dashboards::runtime;
use ivy_host_runtime::config::{self, InstanceConfig};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

fn native_config(root: &std::path::Path, base: &str) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("hostId", "fixture-host".into()),
        ("instanceId", "native-dashboard".into()),
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
        ("settings", J::Object(vec![])),
    ])
}

#[tokio::test]
async fn native_configuration_keeps_artifact_and_filesystem_checks() {
    let root = std::env::temp_dir().join(format!("ivy-native-config-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("artifact/dist")).unwrap();
    let path = root.join("instance.json");
    let mut value = native_config(&root, "http://127.0.0.1:1/ivy/");
    config::atomic_json(
        root.join("artifact/dist/build-info.json"),
        J::object([
            ("version", value.required("version").unwrap().clone()),
            ("buildId", value.required("buildId").unwrap().clone()),
        ]),
    )
    .await
    .unwrap();
    config::atomic_json(&path, value.clone()).await.unwrap();
    let loaded = config::native_instance_config(&path).await.unwrap();
    assert_eq!(
        json::canonical(loaded.json(), 1024 * 1024).unwrap(),
        json::canonical(&value, 1024 * 1024).unwrap()
    );
    assert!(root.join("data").is_dir());
    let old_node = ivy_host_runtime::node_runtime::NodeRuntime {
        executable: root.join("not-node"),
        version: "22.0.0".into(),
    };
    assert_eq!(
        config::instance_config(&path, &old_node)
            .await
            .err()
            .unwrap()
            .code,
        "unsupported_runtime"
    );

    value.insert("buildId", format!("sha256:{}", "b".repeat(64)).into());
    config::atomic_json(&path, value.clone()).await.unwrap();
    assert_eq!(
        config::native_instance_config(&path)
            .await
            .err()
            .unwrap()
            .code,
        "build_mismatch"
    );
    value.insert(
        "dataRoot",
        root.join("artifact/data")
            .to_string_lossy()
            .to_string()
            .into(),
    );
    config::atomic_json(&path, value.clone()).await.unwrap();
    assert_eq!(
        config::native_instance_config(&path)
            .await
            .err()
            .unwrap()
            .code,
        "invalid_arguments"
    );
    value.insert("dataRoot", "relative-data".into());
    config::atomic_json(&path, value).await.unwrap();
    assert!(config::native_instance_config(&path).await.is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
#[ignore = "requires Chromium and the Windows job launcher"]
async fn native_executable_renders_bound_json_without_node() {
    use base64::Engine;
    let root = std::env::temp_dir().join(format!("ivy-native-dashboards-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("artifact/dist/native")).unwrap();
    #[cfg(windows)]
    std::fs::copy(
        std::env::var_os("DASHBOARDS_TEST_JOB").unwrap(),
        root.join("artifact/dist/native/ivy-job.exe"),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/ivy/", listener.local_addr().unwrap());
    let mut value = native_config(&root, &base);
    value.insert(
        "settings",
        J::object([(
            "chromiumExecutable",
            std::env::var("DASHBOARDS_TEST_BROWSER").unwrap().into(),
        )]),
    );
    config::atomic_json(
        root.join("artifact/dist/build-info.json"),
        J::object([
            ("version", value.required("version").unwrap().clone()),
            ("buildId", value.required("buildId").unwrap().clone()),
        ]),
    )
    .await
    .unwrap();
    let path = root.join("instance.json");
    config::atomic_json(&path, value).await.unwrap();

    let registry = ivy_dashboards::registry();
    let tools = registry.required("namespaces").unwrap().as_array().unwrap()[0]
        .required("tools")
        .unwrap()
        .as_array()
        .unwrap();
    let hash = tool_definition_hash(
        tools
            .iter()
            .find(|tool| tool.get("name") == Some(&J::from("render")))
            .unwrap(),
    )
    .unwrap();
    let (rendered, result) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        let mut rendered = Some(rendered);
        let mut invoked = false;
        while let Some(Ok(frame)) = socket.next().await {
            let Message::Text(text) = frame else {
                continue;
            };
            let value = json::parse(&text, 32 * 1024 * 1024).unwrap();
            let Some(method) = value.get("method").and_then(|v| v.as_str().ok()) else {
                assert_eq!(value.get("id"), Some(&J::from("fixture-render")));
                rendered.take().unwrap().send(value).unwrap();
                continue;
            };
            let params = value.required("params").unwrap();
            let reply = match method.as_str() {
                "service.connect" => {
                    assert_eq!(params.get("serviceName"), Some(&J::from("dashboards")));
                    J::object([("generation", J::Number(1.0))])
                }
                "registry.sync" => {
                    assert_eq!(params.required("namespaces").unwrap().as_array().unwrap()[0].required("tools").unwrap().as_array().unwrap().len(), 7);
                    J::Object(vec![])
                }
                "service.heartbeat" => params.clone(),
                "objects.read" if params.get("objectId") == Some(&J::from("dashboard")) => J::object([
                    ("object", J::object([("contractKey", "dashboards/dashboard".into()), ("parentId", J::Null), ("effectivelyArchived", false.into())])),
                    ("revision", J::object([("revision", J::Number(1.0)), ("contractVersion", "1.1.0".into())])),
                    ("content", J::object([("encoding", "json".into()), ("value", J::object([
                        ("title", "Native rendering".into()),
                        ("html", r#"<style>html,body{margin:0}</style><script>
dashboard.onUpdate(async ({data,output})=>{
  await new Promise(resolve=>setTimeout(resolve,25));
  const saved=data.saved;
  if(output.mode!=='image'||!Object.hasOwn(saved,'__proto__')||saved.__proto__.value!==42||
     !Object.hasOwn(saved.nested,'__proto__')||saved.nested.__proto__!==null||
     saved.list[0].__proto__!=='text'||saved.text.charCodeAt(0)!==0xd800)throw Error('JSON data was changed');
  document.body.style.background='rgb('+saved.color.join(',')+')';
});</script>"#.into()),
                        ("sources", J::object([("saved", J::object([("objectId", "source".into())]))])),
                        ("metadata", J::Object(vec![])), ("imageToken", J::Null), ("refreshSeconds", J::Number(5.0)),
                    ]))])),
                ]),
                "objects.read" if params.get("objectId") == Some(&J::from("source")) => J::object([
                    ("object", J::object([("contractKey", "fixture/value".into()), ("effectivelyArchived", false.into())])),
                    ("content", J::object([("encoding", "json".into()), ("value", J::object([
                        ("color", J::Array(vec![J::Number(12.0), J::Number(34.0), J::Number(56.0)])),
                        ("__proto__", J::object([("value", J::Number(42.0))])),
                        ("nested", J::object([("__proto__", J::Null)])),
                        ("list", J::Array(vec![J::object([("__proto__", "text".into())])])),
                        ("text", J::String(ivy_contracts::JsString(vec![0xd800]))),
                    ]))])),
                ]),
                other => panic!("unexpected RPC {other}"),
            };
            socket
                .send(Message::Text(
                    json::stringify(
                        &J::object([
                            ("jsonrpc", "2.0".into()),
                            ("id", value.required("id").unwrap().clone()),
                            ("result", reply),
                        ]),
                        32 * 1024 * 1024,
                    )
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            if method == "service.heartbeat"
                && params.get("ready") == Some(&J::Bool(true))
                && !invoked
            {
                invoked = true;
                socket
                    .send(Message::Text(
                        json::stringify(
                            &J::object([
                                ("jsonrpc", "2.0".into()),
                                ("id", "fixture-render".into()),
                                ("method", "provider.invoke".into()),
                                (
                                    "params",
                                    J::object([
                                        ("qualifiedName", "dashboards.render".into()),
                                        (
                                            "arguments",
                                            J::object([
                                                ("id", "dashboard".into()),
                                                ("width", J::Number(120.0)),
                                                ("height", J::Number(100.0)),
                                                ("colorMode", "color".into()),
                                            ]),
                                        ),
                                        ("definitionHash", hash.clone().into()),
                                        ("generation", J::Number(1.0)),
                                        ("callerPrincipalId", "fixture-caller".into()),
                                    ]),
                                ),
                            ]),
                            32 * 1024 * 1024,
                        )
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
        }
    });
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ivy-dashboards"))
        .args([std::ffi::OsStr::new("--config"), path.as_os_str()])
        .env("IVY_NODE_EXECUTABLE", root.join("node-is-not-installed"))
        .env("IVY_LAUNCH_ID", uuid::Uuid::new_v4().to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(45), result)
        .await
        .unwrap()
        .unwrap();
    let image = reply
        .required("result")
        .unwrap_or_else(|_| panic!("render failed: {reply:?}"));
    let bytes = base64::prelude::BASE64_STANDARD
        .decode(image.required("data").unwrap().as_str().unwrap())
        .unwrap();
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut buffer = vec![0; decoder.output_buffer_size().unwrap()];
    let info = decoder.next_frame(&mut buffer).unwrap();
    assert_eq!((info.width, info.height), (120, 100));
    assert!(
        buffer[..info.buffer_size()]
            .chunks_exact(info.color_type.samples())
            .all(|pixel| pixel[..3] == [12, 34, 56])
    );
    let health = config::json_file(root.join("data/health.json"), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(health.get("ready"), Some(&J::Bool(true)));
    config::atomic_json(
        root.join("data/control.json"),
        J::object([
            ("schemaVersion", J::Number(1.0)),
            ("instanceId", health.required("instanceId").unwrap().clone()),
            ("bootId", health.required("bootId").unwrap().clone()),
            ("launchId", health.required("launchId").unwrap().clone()),
            ("action", "shutdown".into()),
            ("requestedAt", chrono::Utc::now().to_rfc3339().into()),
        ]),
    )
    .await
    .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    peer.await.unwrap();
    assert_eq!(
        config::json_file(root.join("data/health.json"), 1024 * 1024)
            .await
            .unwrap()
            .get("ready"),
        Some(&J::Bool(false))
    );
    std::fs::remove_dir_all(root).unwrap();
}

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
