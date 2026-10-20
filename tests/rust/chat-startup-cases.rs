use super::*;
use ivy_chat_bridge::startup::ChatStartup;
use ivy_contracts::core_schema::validate_host;
use ivy_host_runtime::config::atomic_json;
use std::{ffi::OsString, fs, path::PathBuf};

fn disk_json(path: &Path) -> Json {
    parse(&fs::read_to_string(path).unwrap(), usize::MAX).unwrap()
}

fn copy_catalog(source: &Path, artifact: &Path) {
    let relative = "specs/native/codex-0.154.0";
    let target = artifact.join(relative);
    fs::create_dir_all(&target).unwrap();
    for item in fs::read_dir(source.join(relative)).unwrap() {
        let item = item.unwrap();
        assert!(item.file_type().unwrap().is_file());
        fs::copy(item.path(), target.join(item.file_name())).unwrap();
    }
}

#[tokio::test]
async fn host_configuration_assets_health_and_control_reach_the_real_service() {
    let seed = read_oracle("startup-seed", "IVY_CHAT_STARTUP_SEED_CAPTURE");
    let root = std::env::temp_dir().join(format!("ivy-rust-chat-startup-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let artifact = root.join("artifact");
    let data = root.join("data");
    fs::create_dir_all(artifact.join("dist")).unwrap();
    fs::create_dir(artifact.join("instructions")).unwrap();
    for (name, key) in [
        ("chat-main-instructions.json", "instructions"),
        ("chat-notice-prompt.json", "noticeInstructions"),
    ] {
        fs::write(
            artifact.join("instructions").join(name),
            STANDARD.decode(text(&seed, key)).unwrap(),
        )
        .unwrap();
    }
    copy_catalog(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        &artifact,
    );
    let node = PathBuf::from(std::env::var_os("IVY_CHAT_STARTUP_NODE").unwrap());
    let mut peer = Peer::start().await;
    peer.command(Json::object([
        ("action", "begin".into()),
        ("spec", Json::object([("label", "host-startup".into())])),
    ]))
    .await;
    let config = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("instanceId", "startup-instance".into()),
        ("componentId", "chat-bridge".into()),
        ("credential", "fixture-token".into()),
        ("serviceNodeId", "chat-provider".into()),
        ("hostId", "fixture-host".into()),
        ("version", "1.0.0".into()),
        ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
        ("publicBaseUrl", peer.url.clone().into()),
        ("dataRoot", data.to_str().unwrap().into()),
        ("artifactRoot", artifact.to_str().unwrap().into()),
        (
            "settings",
            Json::object([
                ("pollMs", Json::Number(60000.0)),
                ("definition", field(&seed, "definition").clone()),
            ]),
        ),
    ]);
    let path = root.join("instance.json");
    atomic_json(&path, config.clone()).await.unwrap();
    let args: Vec<OsString> = vec!["--config".into(), path.clone().into()];
    assert_eq!(
        ChatStartup::load(&["--config".into()], Some(path.clone().into()), &node)
            .await
            .err()
            .unwrap()
            .code,
        "invalid_arguments"
    );
    assert_eq!(
        ChatStartup::load(&args, None, Path::new("node"))
            .await
            .err()
            .unwrap()
            .code,
        "unsupported_runtime"
    );
    let build = Json::object([
        ("buildId", field(&config, "buildId").clone()),
        ("version", field(&config, "version").clone()),
    ]);
    let mut wrong = build.clone();
    wrong.insert("version", "0.0.0".into());
    atomic_json(artifact.join("dist/build-info.json"), wrong)
        .await
        .unwrap();
    assert_eq!(
        ChatStartup::load(&args, None, &node)
            .await
            .err()
            .unwrap()
            .code,
        "build_mismatch"
    );
    assert!(!data.exists(), "Build admission precedes data creation");
    atomic_json(artifact.join("dist/build-info.json"), build)
        .await
        .unwrap();

    // Assets belong to the admitted distribution, never the source checkout.
    let instructions = artifact.join("instructions/chat-main-instructions.json");
    let original = fs::read(&instructions).unwrap();
    fs::remove_file(&instructions).unwrap();
    assert!(ChatStartup::load(&args, None, &node).await.is_err());
    assert!(!data.join("health.json").exists());
    fs::write(&instructions, original).unwrap();

    let startup = ChatStartup::load(&[], Some(path.clone().into()), &node)
        .await
        .unwrap();
    equal(
        startup.config().json(),
        &config,
        "Original Host configuration",
    );
    assert_eq!(startup.config().data_root(), data);
    assert_eq!(startup.config().artifact_root(), artifact);
    assert_eq!(
        startup.node_runtime().executable,
        node.canonicalize().unwrap()
    );
    let node_version = startup.node_runtime().version.clone();
    let before = peer
        .command(Json::object([("action", "summary".into())]))
        .await;
    assert!(field(&before, "calls").as_array().unwrap().is_empty());
    assert!(!data.join("health.json").exists());
    fs::write(
        data.join("chat-bridge.sqlite"),
        STANDARD.decode(text(&seed, "database")).unwrap(),
    )
    .unwrap();
    let app = startup.start(None).await.unwrap();
    until(|| app.service().ready(), "File-backed startup ready").await;
    let ready = disk_json(&data.join("health.json"));
    validate_host("Health", &ready).unwrap();
    assert!(flag(&ready, "ready"));
    assert_eq!(field(&ready, "generation"), &Json::Number(7.0));
    for key in ["instanceId", "componentId", "buildId"] {
        equal(field(&ready, key), field(&config, key), key);
    }
    assert_eq!(
        field(&ready, "launchId"),
        &Json::from("startup-fixture-launch")
    );
    {
        let generation = app.runtime().unwrap();
        let lease = generation.accept().unwrap();
        assert_eq!(lease.config().data_root.as_ref(), Some(&data));
        assert_eq!(lease.config().language, JsString::from("en"));
        assert_eq!(
            lease.config().instructions,
            parse(
                &String::from_utf8(STANDARD.decode(text(&seed, "instructions")).unwrap()).unwrap(),
                usize::MAX
            )
            .unwrap()
            .into_js_string()
            .unwrap()
        );
    }
    let mut control = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("instanceId", field(&ready, "instanceId").clone()),
        ("launchId", field(&ready, "launchId").clone()),
        ("bootId", "another-boot".into()),
        ("action", "drain".into()),
        ("requestedAt", "2026-01-01T00:00:00.000Z".into()),
    ]);
    atomic_json(data.join("control.json"), control.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        app.service().ready(),
        "A different boot cannot stop the service"
    );
    let ignored = disk_json(&data.join("health.json"));
    assert!(flag(&ignored, "ready"));
    control.insert("bootId", field(&ready, "bootId").clone());
    atomic_json(data.join("control.json"), control.clone())
        .await
        .unwrap();
    until(
        || {
            disk_json(&data.join("health.json")).get("details")
                == Some(&Json::from("ChatBridge stopped."))
        },
        "Identity-matched drain closes the real service",
    )
    .await;
    app.close().await.unwrap();
    assert!(!app.service().ready());
    let stopped = disk_json(&data.join("health.json"));
    validate_host("Health", &stopped).unwrap();
    assert!(!flag(&stopped, "ready"));
    assert!(field(&stopped, "generation").is_null());
    for key in [
        "instanceId",
        "buildId",
        "bootId",
        "launchId",
        "startedAt",
        "pid",
    ] {
        equal(field(&stopped, key), field(&ready, key), key);
    }
    let report = peer
        .command(Json::object([("action", "summary".into())]))
        .await;
    assert!(field(&report, "mismatches").as_array().unwrap().is_empty());
    let connect = field(&report, "handshakes")
        .as_array()
        .unwrap()
        .first()
        .unwrap();
    for key in ["hostId", "serviceNodeId", "buildId", "version"] {
        equal(
            field(field(connect, "params"), key),
            field(&config, key),
            key,
        );
    }
    let capture = Json::object([
        ("nodeVersion", node_version.into()),
        ("configuration", config.clone()),
        ("ready", ready),
        ("ignoredControl", ignored),
        ("control", control),
        ("stopped", stopped),
        ("peer", report),
        ("database", database(&data.join("chat-bridge.sqlite"))),
    ]);
    if let Some(path) = std::env::var_os("IVY_CHAT_STARTUP_CAPTURE") {
        fs::write(path, canonical(&capture, usize::MAX).unwrap()).unwrap();
    }
    peer.finish().await;
    assert_eq!(
        root.canonicalize().unwrap().parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    assert!(
        !fs::symlink_metadata(&root)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_dir_all(&root).unwrap();
}
