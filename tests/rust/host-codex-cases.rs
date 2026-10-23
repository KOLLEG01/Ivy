use super::{ServiceFixture, assert_json_eq};
use ivy_contracts::{IvyJson as J, IvyResult, JsString, json};
use ivy_host_runtime::{
    codex_connection::{self, Identity, Mode},
    codex_handshake::{self, Handshake, initialized_server_version},
    codex_home::HomeContext,
    codex_process,
    codex_rpc::{Events, NativeRpc},
    node_path, paths, process, process_fence,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn catalog() -> J {
    static CATALOG: OnceLock<J> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            let mut value = json::parse(
                include_str!("../../specs/native/codex-0.154.0/catalog.json"),
                usize::MAX,
            )
            .unwrap();
            for (field, method) in [
                ("clientRequests", Some("initialize")),
                ("clientNotifications", Some("initialized")),
                ("serverRequests", None),
                ("serverNotifications", None),
            ] {
                let selected = value
                    .required(field)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|entry| {
                        method.is_some_and(|method| entry.get("method") == Some(&J::from(method)))
                    })
                    .cloned()
                    .collect();
                value.insert(field, J::Array(selected));
            }
            value
        })
        .clone()
}
fn response(home: &Path, user_agent: J) -> J {
    J::object([
        ("codexHome", J::String(node_path::from_native(home))),
        ("userAgent", user_agent),
        ("platformFamily", "fixture".into()),
        ("platformOs", "fixture".into()),
    ])
}
fn client_name() -> JsString {
    JsString(vec![73, 118, 121, 32, 0xd800])
}
fn launch_settings(fixture: &ServiceFixture, executable: &Path, hash: &str) -> J {
    J::object([
        (
            "codexHome",
            J::String(node_path::from_native(&fixture.0.join("home"))),
        ),
        (
            "nativeExecutable",
            J::String(node_path::from_native(executable)),
        ),
        ("nativeExecutableHash", hash.into()),
        ("nativeVersion", "0.154.0".into()),
        ("appServer", J::object([("mode", "owned-stdio".into())])),
    ])
}
async fn launch(
    fixture: &ServiceFixture,
    settings: &J,
) -> IvyResult<codex_connection::NativeConnection> {
    let paths = node_path::Context::current().unwrap();
    let environment = BTreeMap::new();
    let account_home = node_path::from_native(&fixture.0.join("account"));
    let context = HomeContext {
        paths: &paths,
        environment: &environment,
        user_home: None,
        cwd: &paths.cwd,
        account_home: &account_home,
    };
    codex_process::start_codex_process(codex_process::StartOptions {
        settings,
        artifact_root: &node_path::from_native(&fixture.0.join("artifact")),
        data_root: &node_path::from_native(&fixture.0.join("data")),
        home_context: &context,
        client_name: client_name(),
        client_version: "1.0.0".into(),
        hive_credential: None,
        events: Events::default(),
    })
    .await
}

#[tokio::test]
async fn launch_fields_refuse_malformed_selected_values_before_catalog_or_directory_effects() {
    let fixture = ServiceFixture::new();
    let base = launch_settings(
        &fixture,
        &fixture.0.join("codex.exe"),
        &format!("sha256:{}", "0".repeat(64)),
    );
    let malformed = [
        J::Null,
        false.into(),
        J::Number(0.0),
        "primitive".into(),
        J::Array(vec![]),
        J::object([] as [(&str, J); 0]),
    ];
    let mut cases = Vec::new();
    for (index, value) in malformed.iter().enumerate() {
        if value.as_object().is_none() {
            cases.push((
                format!("settings-{index}"),
                value.clone(),
                "invalid_arguments",
            ));
        }
        if value.as_object().is_none() {
            let mut settings = base.clone();
            settings.insert("appServer", value.clone());
            cases.push((format!("app-server-{index}"), settings, "invalid_arguments"));
        }
        if value.as_js_string().is_none() {
            let mut settings = base.clone();
            settings.insert("appServer", J::object([("mode", value.clone())]));
            cases.push((format!("mode-{index}"), settings, "invalid_arguments"));
        }
        for (field, code) in [
            ("nativeExecutable", "invalid_arguments"),
            ("nativeExecutableHash", "invalid_arguments"),
            ("nativeVersion", "native_version_unsupported"),
        ] {
            if value.as_js_string().is_some() {
                continue;
            }
            let mut settings = base.clone();
            settings.insert(field, value.clone());
            cases.push((format!("{field}-{index}"), settings, code));
        }
        for field in ["socketPath", "expectedCodexHome"] {
            let mut settings = base.clone();
            settings.insert(
                "appServer",
                J::object([("mode", "external-proxy".into()), (field, value.clone())]),
            );
            cases.push((format!("{field}-{index}"), settings, "invalid_arguments"));
        }
    }
    for (field, code) in [
        ("nativeExecutable", "invalid_arguments"),
        ("nativeExecutableHash", "invalid_arguments"),
        ("nativeVersion", "native_version_unsupported"),
    ] {
        let mut settings = base.clone();
        settings.remove(field);
        cases.push((format!("missing-{field}"), settings, code));
    }
    for (field, value, code) in [
        ("nativeExecutable", J::from("relative"), "invalid_arguments"),
        (
            "nativeVersion",
            "../escape".into(),
            "native_version_unsupported",
        ),
        (
            "codexHome",
            J::String(node_path::from_native(
                &fixture.0.join("artifact").join("home"),
            )),
            "invalid_arguments",
        ),
        (
            "codexHome",
            J::String(node_path::from_native(&fixture.0)),
            "invalid_arguments",
        ),
    ] {
        let mut settings = base.clone();
        settings.insert(field, value);
        cases.push((field.into(), settings, code));
    }
    let mut claude = base.clone();
    claude.remove("nativeExecutable");
    claude.insert("appServer", J::object([("mode", "claude-adapter".into())]));
    cases.push((
        "claude-transport".into(),
        claude,
        "native_transport_unavailable",
    ));
    for (name, settings, code) in cases {
        let error = launch(&fixture, &settings)
            .await
            .err()
            .expect("Preflight must refuse this fixture");
        assert_eq!(error.code, code, "{name}");
        assert!(!fixture.0.join("home").exists(), "{name}");
        assert!(!fixture.0.join("data").exists(), "{name}");
    }
    let fence = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("owner", "native".into()),
        ("ownerId", "fixture-owner".into()),
        ("processId", J::Number(1.0)),
        ("observedAt", "2026-01-01T00:00:00.000Z".into()),
        ("code", "outcome_unknown".into()),
    ]);
    let data = fixture.0.join("data");
    process_fence::record_process_stop_fence(data.clone(), fence)
        .await
        .unwrap();
    let before = fs::read(data.join("process-stop-fence.json")).unwrap();
    let mut settings = base;
    settings.insert("nativeVersion", J::Null);
    assert_eq!(
        launch(&fixture, &settings).await.err().unwrap().code,
        "outcome_unknown"
    );
    assert_eq!(
        fs::read(data.join("process-stop-fence.json")).unwrap(),
        before
    );
    assert!(!fixture.0.join("home").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn launch_preserves_catalog_executable_and_shell_preflight_without_account_execution() {
    let fixture = ServiceFixture::new();
    let binary = compile_peer(&fixture);
    let hash = ivy_host_runtime::streaming_hash::file_hash(&binary)
        .await
        .unwrap();
    let base = launch_settings(&fixture, &binary, &hash);
    let mut pinned = json::parse(
        include_str!("../../specs/native/codex-0.154.0/catalog.json"),
        usize::MAX,
    )
    .unwrap();
    pinned.insert("nativeExecutableHash", hash.clone().into());
    let path = ivy_contracts::checked_native_contract::native_catalog_path(
        &node_path::from_native(&fixture.0.join("artifact").join("dist")),
        &"0.154.0".into(),
        None,
        None,
    )
    .unwrap();
    let path = node_path::to_native(&path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let write_catalog =
        |catalog: &J| fs::write(&path, json::stringify(catalog, usize::MAX).unwrap()).unwrap();
    let wrong_hash = format!("sha256:{}", "0".repeat(64));
    for kind in [
        "configured-hash",
        "actual-bytes",
        "version",
        "realtime",
        "native-socket",
    ] {
        let mut settings = base.clone();
        let mut catalog = pinned.clone();
        match kind {
            "configured-hash" => settings.insert("nativeExecutableHash", wrong_hash.clone().into()),
            "actual-bytes" => {
                settings.insert("nativeExecutableHash", wrong_hash.clone().into());
                catalog.insert("nativeExecutableHash", wrong_hash.clone().into());
            }
            "version" => catalog.insert("version", "0.153.4".into()),
            "realtime" => catalog.insert("clientRequests", J::Array(vec![])),
            "native-socket" => {
                let mut socket = node_path::from_native(&fixture.0.join("proxy.sock"));
                socket.0.push(0xd800);
                settings.insert(
                    "appServer",
                    J::object([
                        ("mode", "external-proxy".into()),
                        ("socketPath", J::String(socket)),
                    ]),
                );
                settings.insert("nativeExecutableHash", wrong_hash.clone().into());
            }
            _ => unreachable!(),
        }
        write_catalog(&catalog);
        assert_eq!(
            launch(&fixture, &settings).await.err().unwrap().code,
            "native_build_mismatch",
            "{kind}"
        );
        assert!(!fixture.0.join("home").exists());
        assert!(!fixture.0.join("data").exists());
    }
    write_catalog(&pinned);
    for server in [
        None,
        Some(J::object([] as [(&str, J); 0])),
        Some(J::object([
            ("mode", "owned-stdio".into()),
            ("socketPath", false.into()),
            ("expectedCodexHome", J::Null),
        ])),
        Some(J::object([
            ("mode", "future-mode".into()),
            ("socketPath", false.into()),
            ("expectedCodexHome", J::Null),
        ])),
        Some(J::object([("mode", "external-proxy".into())])),
    ] {
        let mut settings = base.clone();
        settings.insert("unused", J::String(JsString(vec![0xd800])));
        if let Some(server) = server {
            settings.insert("appServer", server);
        } else {
            settings.remove("appServer");
        }
        assert_eq!(
            launch(&fixture, &settings).await.err().unwrap().code,
            "native_shell_required"
        );
        assert!(fixture.0.join("home").is_dir());
        assert!(fixture.0.join("data").is_dir());
    }
    let other_home = fixture.0.join("other-home");
    fs::create_dir(&other_home).unwrap();
    let mut shared = base;
    shared.insert(
        "appServer",
        J::object([
            ("mode", "external-proxy".into()),
            (
                "expectedCodexHome",
                J::String(node_path::from_native(&other_home)),
            ),
        ]),
    );
    assert_eq!(
        launch(&fixture, &shared).await.err().unwrap().code,
        "native_home_mismatch"
    );
    assert!(!fixture.0.join("home").join("frames.jsonl").exists());
    assert!(fs::read_dir(&other_home).unwrap().next().is_none());
}
fn expected_initialize() -> J {
    J::object([
        (
            "clientInfo",
            J::object([
                ("name", J::String(client_name())),
                ("version", "1.0.0".into()),
            ]),
        ),
        (
            "capabilities",
            J::object([
                ("experimentalApi", true.into()),
                ("requestAttestation", false.into()),
            ]),
        ),
    ])
}

#[test]
fn server_version_uses_the_leading_identity_and_native_whitespace() {
    for (value, version) in [
        (
            "ivy-agent-manager/0.154.0 (Windows; x86_64) client/9.0.0",
            "0.154.0",
        ),
        (
            "Codex Desktop/0.153.4 (Linux; x86_64) client/0.154.0",
            "0.153.4",
        ),
        ("server/0.154.0", "0.154.0"),
        ("server/0.154.0\u{85}metadata", "0.154.0"),
        ("server/0.154.0\r\nmetadata", "0.154.0"),
    ] {
        assert_eq!(
            initialized_server_version(&value.into()).unwrap(),
            JsString::from(version)
        );
    }
    for value in [
        J::Null,
        false.into(),
        J::Number(7.0),
        J::object([] as [(&str, J); 0]),
        "".into(),
        "0.154.0".into(),
        "server/latest".into(),
        "/0.154.0".into(),
        "server/0.154.0-extra".into(),
        "server/0.154.0\u{feff}metadata".into(),
        "server/0.154.0client/0.155.0".into(),
        "server/0.154".into(),
        " server/0.154.0".into(),
        "server  name/0.154.0".into(),
        "server\u{85}name/0.154.0".into(),
    ] {
        assert_eq!(
            initialized_server_version(&value).unwrap_err().code,
            "native_server_version_unavailable",
            "{value:?}"
        );
    }
    let opaque = J::String(JsString(vec![0xd800, 47, 48, 46, 49, 46, 50, 32, 0xdfff]));
    assert_eq!(
        initialized_server_version(&opaque).unwrap(),
        JsString::from("0.1.2")
    );
}

async fn handshake(home: &Path, reply: J) -> (IvyResult<()>, Vec<J>) {
    let (client, server) = tokio::io::duplex(16_384);
    let (input, output) = tokio::io::split(client);
    let rpc = NativeRpc::start(&catalog(), output, input, Events::default()).unwrap();
    let peer = tokio::spawn(async move {
        let (input, mut output) = tokio::io::split(server);
        let mut lines = BufReader::new(input).lines();
        let request = json::parse(&lines.next_line().await.unwrap().unwrap(), usize::MAX).unwrap();
        assert_eq!(request.get("method"), Some(&"initialize".into()));
        assert_json_eq(request.required("params").unwrap(), &expected_initialize());
        let mut reply = reply;
        reply.insert("id", request.required("id").unwrap().clone());
        output
            .write_all(json::stringify(&reply, usize::MAX).unwrap().as_bytes())
            .await
            .unwrap();
        output.write_all(b"\n").await.unwrap();
        let mut observed = vec![request];
        while let Some(line) = lines.next_line().await.unwrap() {
            let value = json::parse(&line, usize::MAX).unwrap();
            observed.push(value);
            break;
        }
        observed
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        codex_handshake::initialize(
            &rpc,
            Handshake {
                client_name: &client_name(),
                client_version: &"1.0.0".into(),
                native_version: &"0.154.0".into(),
                expected_home: home,
            },
        ),
    )
    .await
    .unwrap();
    if result.is_err() {
        rpc.close("fixture-rejected").await;
    }
    let frames = tokio::time::timeout(Duration::from_secs(5), peer)
        .await
        .unwrap()
        .unwrap();
    rpc.close("fixture-complete").await;
    (result, frames)
}

#[tokio::test]
async fn native_handshake_admits_objects_and_authenticates_home_and_version_before_initialized() {
    let fixture = ServiceFixture::new();
    let home = fixture.0.join("home");
    fs::create_dir(&home).unwrap();
    let canonical = paths::realpath(&home).unwrap();
    let good = response(&home, "Ivy Test/0.154.0 (fixture)".into());
    let mut cases = vec![(J::object([("result", good.clone())]), None)];
    for value in [
        J::Null,
        false.into(),
        J::Number(0.0),
        "primitive".into(),
        J::Array(vec![]),
    ] {
        cases.push((
            J::object([("result", value)]),
            Some("native_initialization_failed"),
        ));
    }
    cases.push((
        J::object([(
            "error",
            J::object([("code", J::Number(-1.0)), ("message", "refused".into())]),
        )]),
        Some("native_initialization_failed"),
    ));
    for value in [
        J::Null,
        false.into(),
        "relative".into(),
        J::String(node_path::from_native(&fixture.0)),
    ] {
        let mut reply = good.clone();
        reply.insert("codexHome", value);
        cases.push((J::object([("result", reply)]), Some("native_home_mismatch")));
    }
    for (value, code) in [
        (J::Null, "native_server_version_unavailable"),
        ("client/latest".into(), "native_server_version_unavailable"),
        (
            "Ivy Test/0.153.4 client/0.154.0".into(),
            "native_server_version_mismatch",
        ),
    ] {
        let mut reply = good.clone();
        reply.insert("userAgent", value);
        cases.push((J::object([("result", reply)]), Some(code)));
    }
    for (reply, code) in cases {
        let (result, frames) = handshake(&canonical, reply).await;
        if let Some(code) = code {
            assert_eq!(result.unwrap_err().code, code);
            assert_eq!(frames.len(), 1);
        } else {
            result.unwrap();
            assert_eq!(frames.len(), 2);
            assert_json_eq(&frames[1], &J::object([("method", "initialized".into())]));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let opaque_home = fixture.0.join(OsString::from_wide(&[0xd800]));
        fs::create_dir(&opaque_home).unwrap();
        handshake(
            &paths::realpath(&opaque_home).unwrap(),
            J::object([("result", response(&opaque_home, "Ivy Test/0.154.0".into()))]),
        )
        .await
        .0
        .unwrap();
    }
}

fn compile_peer(fixture: &ServiceFixture) -> PathBuf {
    let binary = fixture.0.join(if cfg!(windows) {
        "codex-peer.exe"
    } else {
        "codex-peer"
    });
    let compiled =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2024")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tests/fixtures/host-codex-peer.rs"),
            )
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    binary
}
fn job_launcher() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(
            std::env::var_os("IVY_BACKUP_JOB")
                .unwrap_or_else(|| env!("CARGO_BIN_EXE_ivy-job").into()),
        )
    }
    #[cfg(not(windows))]
    {
        PathBuf::new()
    }
}
async fn recorded_frames(path: &Path, count: usize) -> Vec<J> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = fs::read_to_string(path) {
                let frames: Vec<_> = text
                    .lines()
                    .filter_map(|line| json::parse(line, usize::MAX).ok())
                    .collect();
                if frames.len() >= count {
                    return frames;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn native_cli_and_owned_connection_keep_exact_identity_and_confirmed_retirement() {
    let fixture = ServiceFixture::new();
    let binary = compile_peer(&fixture);
    let hash = ivy_host_runtime::streaming_hash::file_hash(&binary)
        .await
        .unwrap();
    assert_eq!(
        codex_process::verify_executable(&binary, &hash.into())
            .await
            .unwrap(),
        paths::realpath(&binary).unwrap()
    );
    assert_eq!(
        codex_process::verify_executable(&binary, &format!("sha256:{}", "0".repeat(64)).into())
            .await
            .unwrap_err()
            .code,
        "native_build_mismatch"
    );
    for (stdout, exit, expected_error) in [
        ("codex-cli 0.154.0", "0", None),
        (" \tcodex-cli 0.154.0\r\n", "0", None),
        ("\u{85}codex-cli 0.154.0\u{85}", "0", None),
        // The existing diagnostic decoder consumes an initial UTF-8 BOM.
        ("\u{feff}codex-cli 0.154.0", "0", None),
        (
            "codex-cli 0.154.0\u{feff}",
            "0",
            Some("native_build_mismatch"),
        ),
        ("codex-cli 0.153.4", "0", Some("native_build_mismatch")),
        (
            "codex-cli 0.154.0-extra",
            "0",
            Some("native_build_mismatch"),
        ),
        (
            "client codex-cli 0.154.0",
            "0",
            Some("native_build_mismatch"),
        ),
        ("codex-cli 0.154.0", "7", Some("command_failed")),
    ] {
        let environment = BTreeMap::from([
            (OsString::from("IVY_CODEX_STDOUT"), stdout.into()),
            ("IVY_CODEX_EXIT".into(), exit.into()),
        ]);
        let result = codex_process::verify_cli_version(
            &binary,
            &fixture.0,
            &"0.154.0".into(),
            environment,
            &job_launcher(),
        )
        .await;
        assert_eq!(
            result.err().map(|error| error.code),
            expected_error.map(str::to_owned),
            "stdout={stdout:?}, exit={exit}"
        );
    }
    let home = fixture.0.join("home");
    fs::create_dir(&home).unwrap();
    for (name, mode, agent, expected_error) in [
        ("success", "reply", "Ivy Test/0.154.0", None),
        (
            "mismatch",
            "reply",
            "Ivy Test/0.153.4 client/0.154.0",
            Some("native_server_version_mismatch"),
        ),
        ("cancel", "hold", "Ivy Test/0.154.0", Some("cancel")),
    ] {
        let data_root = fixture.0.join(name);
        fs::create_dir(&data_root).unwrap();
        let record = data_root.join("frames.jsonl");
        let environment = BTreeMap::from([
            ("IVY_CODEX_MODE".into(), OsString::from(mode)),
            ("IVY_CODEX_RECORD".into(), record.clone().into_os_string()),
            (
                "IVY_CODEX_REPLY".into(),
                json::stringify(&response(&home, agent.into()), usize::MAX)
                    .unwrap()
                    .into(),
            ),
        ]);
        let child = process::spawn(
            &process::Command {
                executable: "peer".into(),
                args: vec![],
                cwd: None,
                timeout_ms: 15_000,
            },
            &fixture.0,
            &BTreeMap::from([("peer".into(), binary.clone())]),
            process::LaunchContext {
                environment,
                raw_output: true,
                job_launcher: Some(job_launcher()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let completion = child.completion.clone();
        let connecting = codex_connection::connect(
            child,
            Identity {
                data_root: data_root.clone(),
                home: paths::realpath(&home).unwrap(),
                epoch: name.into(),
                native_version: "0.154.0".into(),
                client_name: client_name(),
                client_version: "1.0.0".into(),
                mode: Mode::OwnedStdio,
            },
            catalog(),
            Events::default(),
        );
        if expected_error == Some("cancel") {
            recorded_frames(&record, 1).await;
            drop(connecting);
        } else {
            let connected = tokio::time::timeout(Duration::from_secs(10), connecting)
                .await
                .unwrap();
            if let Some(code) = expected_error {
                assert_eq!(connected.err().unwrap().code, code);
                assert_eq!(recorded_frames(&record, 1).await.len(), 1);
            } else {
                let connection = connected.unwrap();
                let frames = recorded_frames(&record, 2).await;
                assert_json_eq(
                    frames[0].required("params").unwrap(),
                    &expected_initialize(),
                );
                assert_json_eq(&frames[1], &J::object([("method", "initialized".into())]));
                connection.close().await.unwrap();
                connection.close().await.unwrap();
            }
        }
        let stopped = tokio::time::timeout(Duration::from_secs(5), completion.wait())
            .await
            .unwrap();
        assert_ne!(stopped.error_code.as_deref(), Some("outcome_unknown"));
        assert!(!data_root.join("process-stop-fence.json").exists());
    }
}
