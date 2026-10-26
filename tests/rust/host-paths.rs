use std::{fs, path::PathBuf};

#[allow(dead_code)]
#[path = "host-service-fixture.rs"]
mod service_fixture;
use service_fixture::{ServiceFixture, fixture_node, host_configuration};

#[path = "host-backup-files-cases.rs"]
mod backup_files_cases;
#[path = "host-bootstrap-maintenance-cases.rs"]
mod bootstrap_maintenance_cases;
#[path = "host-build-input-cases.rs"]
mod build_input_cases;
#[path = "host-codex-cases.rs"]
mod codex_cases;
#[path = "host-configuration-updater-cases.rs"]
mod configuration_updater_cases;
#[path = "host-filesystem-cases.rs"]
mod filesystem_cases;
#[path = "host-startup-cases.rs"]
mod host_startup_cases;
#[path = "host-layout-cases.rs"]
mod layout_cases;
#[path = "host-native-helper-cases.rs"]
mod native_helper_cases;
#[path = "host-package-archive-cases.rs"]
mod package_archive_cases;
#[path = "host-package-updater-cases.rs"]
mod package_updater_cases;
#[path = "host-preparation-cases.rs"]
mod preparation_cases;
#[path = "host-reset-stop-cases.rs"]
mod reset_stop_cases;

use ivy_contracts::{IvyJson, JsString};
use ivy_host_runtime::{config, health::HealthFile};

fn assert_json_eq(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        ivy_contracts::json::canonical(actual, usize::MAX).unwrap(),
        ivy_contracts::json::canonical(expected, usize::MAX).unwrap()
    );
}
fn assert_json_eq_with_context(actual: &IvyJson, expected: &IvyJson, context: &IvyJson) {
    assert_eq!(
        ivy_contracts::json::canonical(actual, usize::MAX).unwrap(),
        ivy_contracts::json::canonical(expected, usize::MAX).unwrap(),
        "{context:?}"
    );
}
fn assert_optional_json_eq(actual: Option<IvyJson>, expected: Option<IvyJson>) {
    assert_eq!(
        actual
            .as_ref()
            .map(|value| ivy_contracts::json::canonical(value, usize::MAX).unwrap()),
        expected
            .as_ref()
            .map(|value| ivy_contracts::json::canonical(value, usize::MAX).unwrap())
    );
}

fn stop_fence() -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("owner", "native".into()),
        ("ownerId", "native-owner".into()),
        ("processId", IvyJson::Number(123.0)),
        ("observedAt", "2026-01-01T00:00:00.000Z".into()),
        ("code", "outcome_unknown".into()),
    ])
}

struct FixedHealthClock(i64);
impl ivy_sdk::clock::Clock for FixedHealthClock {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
fn hive_settings(host: &str, port: u16) -> IvyJson {
    IvyJson::object([
        ("listenHost", host.into()),
        ("listenPort", IvyJson::Number(port as f64)),
        (
            "credentials",
            IvyJson::Array(vec![IvyJson::object([
                ("principalId", "fixture".into()),
                ("token", "fixture-credential".into()),
            ])]),
        ),
        (
            "backup",
            IvyJson::object([
                ("directory", "unused".into()),
                ("intervalHours", IvyJson::Number(24.0)),
                ("retain", IvyJson::Number(7.0)),
            ]),
        ),
    ])
}

fn health_rpc_fixture(
    result: IvyJson,
    expected_method: &'static str,
) -> (String, std::thread::JoinHandle<()>) {
    use std::{
        io::{Read, Write},
        time::{Duration, Instant},
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/base", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        for _ in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("health fixture accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let header_end;
            loop {
                let mut b = [0];
                stream.read_exact(&mut b).unwrap();
                bytes.push(b[0]);
                if bytes.ends_with(b"\r\n\r\n") {
                    header_end = bytes.len();
                    break;
                }
                assert!(bytes.len() < 16384);
            }
            let headers = String::from_utf8(bytes.clone())
                .unwrap()
                .to_ascii_lowercase();
            assert!(headers.starts_with("post /base/api/v1/rpc "));
            assert!(headers.contains("authorization: bearer fixture-credential\r\n"));
            let length: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(length < 16384);
            bytes.resize(header_end + length, 0);
            stream.read_exact(&mut bytes[header_end..]).unwrap();
            let request = ivy_contracts::json::parse(
                std::str::from_utf8(&bytes[header_end..]).unwrap(),
                16384,
            )
            .unwrap();
            assert_eq!(request.get("method"), Some(&IvyJson::from(expected_method)));
            if expected_method == "serviceNodes.get" {
                assert_eq!(
                    request.required("params").unwrap().get("serviceNodeId"),
                    Some(&IvyJson::from("host.service"))
                );
            }
            let body = ivy_contracts::json::stringify(
                &IvyJson::object([
                    ("jsonrpc", "2.0".into()),
                    ("id", request.required("id").unwrap().clone()),
                    ("result", result.clone()),
                ]),
                16384,
            )
            .unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (endpoint, thread)
}

#[tokio::test]
async fn native_health_cli_preserves_config_precedence_and_json_exit_contract() {
    let fixture = ServiceFixture::new();
    let mut source = fixture.value();
    source.remove("credential");
    fixture.save(source.clone()).await;
    fixture.build(&source).await;
    let config = config::InstanceConfig::from_json(source.clone()).unwrap();
    HealthFile::new(config)
        .write(true, "ready".into(), Some(7))
        .await
        .unwrap();
    let node = std::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    let node = String::from_utf8(node.stdout).unwrap().trim().to_string();
    for mode in ["environment", "argument", "invalid", "build"] {
        if mode == "build" {
            let mut value = source.clone();
            value.insert("buildId", format!("sha256:{}", "b".repeat(64)).into());
            fixture.build(&value).await;
        }
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_ivy-health"));
        command
            .env("IVY_NODE_EXECUTABLE", &node)
            .env_remove("IVY_HIVE_HEALTH_URL")
            .env("IVY_INSTANCE_CONFIG", fixture.0.join("config.json"));
        if mode == "argument" {
            command
                .env("IVY_INSTANCE_CONFIG", "relative")
                .arg("--config")
                .arg(fixture.0.join("config.json"));
        }
        if mode == "invalid" {
            command.arg("--config").arg("relative");
        }
        let result = command.output().unwrap();
        assert!(result.stderr.is_empty());
        let output =
            ivy_contracts::json::parse(std::str::from_utf8(&result.stdout).unwrap(), 1024).unwrap();
        if ["environment", "argument"].contains(&mode) {
            assert!(result.status.success(), "{output:?}");
            assert_eq!(output.get("buildId"), source.get("buildId"));
            assert_eq!(output.get("generation"), Some(&IvyJson::Number(7.0)));
        } else {
            assert_eq!(result.status.code(), Some(1));
            assert_eq!(output.get("ok"), Some(&IvyJson::Bool(false)));
            assert_eq!(
                output.get("code"),
                Some(&IvyJson::from(if mode == "build" {
                    "build_mismatch"
                } else {
                    "invalid_arguments"
                }))
            );
        }
    }
}

#[tokio::test]
async fn stop_fence_native_observation_creation_and_exact_clear() {
    use ivy_host_runtime::process_fence::*;
    let fixture = ServiceFixture::new();
    let canonical = ivy_contracts::json::canonical(&stop_fence(), usize::MAX).unwrap();
    let mut other = stop_fence();
    other.insert("ownerId", "different-owner".into());
    let states = [
        ("missing", None, None, true),
        (
            "canonical",
            Some(canonical.clone()),
            Some(stop_fence()),
            true,
        ),
        (
            "json-space",
            Some(format!("\t {canonical}\r\n")),
            Some(stop_fence()),
            true,
        ),
        (
            "bom",
            Some(format!("\u{feff}\t{canonical}\r\n")),
            None,
            false,
        ),
        (
            "native-space",
            Some(format!("\u{85}{canonical}\u{85}")),
            None,
            true,
        ),
        ("null", Some("null".into()), None, false),
        ("malformed", Some("{".into()), None, false),
        ("oversize", Some(" ".repeat(4097)), None, false),
        (
            "different-owner",
            Some(ivy_contracts::json::canonical(&other, usize::MAX).unwrap()),
            Some(other),
            false,
        ),
    ];
    for (name, content, observed, clears) in states {
        let root = fixture.0.join(name);
        fs::create_dir(&root).unwrap();
        let path = root.join("process-stop-fence.json");
        if let Some(content) = &content {
            fs::write(&path, content).unwrap();
        }
        let observation = process_stop_fence(&root).await;
        match observed {
            Some(expected) => assert_json_eq(&observation.unwrap().unwrap(), &expected),
            None if content.is_none() => assert!(observation.unwrap().is_none()),
            None => {
                let error = observation.unwrap_err();
                assert_eq!(error.code, "outcome_unknown", "{name}");
                assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
            }
        }
        let admission = require_clear_process_stop_fence(&root).await;
        if content.is_none() {
            admission.unwrap();
            assert!(!clear_process_stop_fence(&root, stop_fence()).await.unwrap());
        } else {
            let error = admission.unwrap_err();
            assert_eq!(error.code, "outcome_unknown", "{name}");
            assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
        }
        assert_eq!(
            record_process_stop_fence(&root, stop_fence())
                .await
                .unwrap(),
            content.is_none(),
            "{name}"
        );
        let retained = content
            .unwrap_or_else(|| format!("{canonical}\n"))
            .into_bytes();
        assert_eq!(fs::read(&path).unwrap(), retained, "{name}");
        // Every root is disposable and has no running owner; release is established here.
        assert_eq!(
            clear_process_stop_fence(&root, stop_fence()).await.unwrap(),
            clears,
            "{name}"
        );
        if clears {
            assert!(!path.exists(), "{name}");
            require_clear_process_stop_fence(&root).await.unwrap();
        } else {
            assert_eq!(fs::read(&path).unwrap(), retained, "{name}");
            assert_eq!(
                require_clear_process_stop_fence(&root)
                    .await
                    .unwrap_err()
                    .code,
                "outcome_unknown"
            );
        }
    }
}

#[tokio::test]
async fn stop_fence_clear_requires_valid_utf8_and_canonical_expected_identity() {
    use ivy_host_runtime::process_fence::*;
    let fixture = ServiceFixture::new();
    let path = fixture.0.join("process-stop-fence.json");
    let mut expected = stop_fence();
    expected.insert("ownerId", "native-\u{fffd}".into());
    let canonical = ivy_contracts::json::canonical(&expected, usize::MAX).unwrap();
    let mut corrupt = canonical.as_bytes().to_vec();
    let replacement = "\u{fffd}".as_bytes();
    let index = corrupt
        .windows(replacement.len())
        .position(|bytes| bytes == replacement)
        .unwrap();
    corrupt.splice(index..index + replacement.len(), [0xff]);
    let noncanonical = canonical.replacen("\":", "\" : ", 1);
    assert_json_eq(
        &ivy_contracts::json::parse(&noncanonical, usize::MAX).unwrap(),
        &expected,
    );
    for bytes in [
        corrupt,
        noncanonical.into_bytes(),
        format!("{canonical}\0").into_bytes(),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(
            !clear_process_stop_fence(&fixture.0, expected.clone())
                .await
                .unwrap()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(
            !record_process_stop_fence(&fixture.0, expected.clone())
                .await
                .unwrap()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            require_clear_process_stop_fence(&fixture.0)
                .await
                .unwrap_err()
                .code,
            "outcome_unknown"
        );
    }
    fs::write(&path, format!("{canonical}\n")).unwrap();
    assert!(
        clear_process_stop_fence(&fixture.0, expected)
            .await
            .unwrap()
    );
    let mut opaque = stop_fence();
    opaque.insert("ownerId", IvyJson::String(JsString(vec![78, 0xd800])));
    assert!(
        record_process_stop_fence(&fixture.0, opaque.clone())
            .await
            .unwrap()
    );
    assert_json_eq(
        &process_stop_fence(&fixture.0).await.unwrap().unwrap(),
        &opaque,
    );
    let before = fs::read(&path).unwrap();
    assert_eq!(
        clear_process_stop_fence(&fixture.0, IvyJson::Null)
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(clear_process_stop_fence(&fixture.0, opaque).await.unwrap());
    assert!(
        !clear_process_stop_fence(&fixture.0, stop_fence())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn stop_fence_exclusive_writer_and_invalid_marker_fail_closed() {
    use ivy_host_runtime::process_fence::*;
    let fixture = ServiceFixture::new();
    let root = fixture.0.join("fenced");
    let mut writers = Vec::new();
    for _ in 0..16 {
        let root = root.clone();
        writers.push(tokio::spawn(async move {
            record_process_stop_fence(root, stop_fence()).await.unwrap()
        }));
    }
    let mut winners = 0;
    for writer in writers {
        if writer.await.unwrap() {
            winners += 1;
        }
    }
    assert_eq!(winners, 1);
    let error = require_clear_process_stop_fence(&root).await.unwrap_err();
    assert_eq!(error.code, "outcome_unknown");
    assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
    assert!(clear_process_stop_fence(&root, stop_fence()).await.unwrap());
    let path = root.join("process-stop-fence.json");
    fs::create_dir(&path).unwrap();
    assert_eq!(
        process_stop_fence(&root).await.unwrap_err().code,
        "outcome_unknown"
    );
    assert!(
        !record_process_stop_fence(&root, stop_fence())
            .await
            .unwrap()
    );
    assert!(path.is_dir());
    assert!(clear_process_stop_fence(&root, stop_fence()).await.is_err());
    fs::remove_dir(&path).unwrap();
    assert!(
        record_process_stop_fence(&root, IvyJson::Null)
            .await
            .is_err()
    );
    assert!(!path.exists());
}

#[tokio::test]
async fn instance_configuration_checks_build_before_creating_runtime_roots() {
    let fixture = ServiceFixture::new();
    let value = fixture.value();
    fixture.save(value.clone()).await;
    let mut wrong_build = value.clone();
    wrong_build.insert("version", "2.0.0".into());
    fixture.build(&wrong_build).await;
    let path = fixture.0.join("config.json");
    let result = config::instance_config(&path, &fixture_node("24.18.0")).await;
    assert_eq!(result.err().unwrap().code, "build_mismatch");
    assert!(!fixture.0.join("data").exists());
    fixture.build(&value).await;
    for version in ["22.0.0", "24.17.9", "25.0.0"] {
        assert_eq!(
            config::instance_config(&path, &fixture_node(version))
                .await
                .err()
                .unwrap()
                .code,
            "unsupported_runtime"
        );
        assert!(!fixture.0.join("data").exists());
    }
    let actual = config::instance_config(&path, &fixture_node("24.18.0"))
        .await
        .unwrap();
    assert_eq!(
        ivy_contracts::json::canonical(actual.json(), usize::MAX).unwrap(),
        ivy_contracts::json::canonical(&value, usize::MAX).unwrap()
    );
    for root in ["data", "work", "logs"] {
        assert!(fixture.0.join(root).is_dir());
    }
    for root in [
        fixture.0.join("artifact"),
        fixture.0.join("artifact/nested"),
        fixture.0.clone(),
        PathBuf::from("relative"),
    ] {
        let mut invalid = value.clone();
        invalid.insert("dataRoot", root.to_string_lossy().to_string().into());
        fixture.save(invalid).await;
        assert_eq!(
            config::instance_config(&path, &fixture_node("24.18.0"))
                .await
                .err()
                .unwrap()
                .code,
            "invalid_arguments"
        );
    }
    for key in ["workRoot", "logsRoot"] {
        let mut invalid = value.clone();
        invalid.insert(
            key,
            fixture
                .0
                .join("artifact/nested")
                .to_string_lossy()
                .to_string()
                .into(),
        );
        fixture.save(invalid).await;
        assert_eq!(
            config::instance_config(&path, &fixture_node("24.18.0"))
                .await
                .err()
                .unwrap()
                .code,
            "invalid_arguments"
        );
    }
}

#[tokio::test]
async fn configuration_selection_and_json_files_preserve_native_text_and_limits() {
    let fixture = ServiceFixture::new();
    let path = fixture.0.join("config.json");
    let env = Some(path.clone().into_os_string());
    assert_eq!(config::configuration_path(&[], env.clone()).unwrap(), path);
    assert!(config::configuration_path(&["--config".into()], env.clone()).is_err());
    assert!(
        config::configuration_path(&["--config".into(), "relative".into()], env.clone()).is_err()
    );
    assert_eq!(
        config::configuration_path(
            &[
                "--config".into(),
                path.clone().into_os_string(),
                "--config".into(),
                "relative".into()
            ],
            None
        )
        .unwrap(),
        path
    );
    let valid = "{\"text\":\"ü 空 😀 �\",\"escaped\":\"\\ud800\"}";
    let expected = IvyJson::object([
        ("text", "ü 空 😀 �".into()),
        ("escaped", IvyJson::String(JsString(vec![0xd800]))),
    ]);
    fs::write(&path, valid.as_bytes()).unwrap();
    assert_json_eq(
        &config::json_file(&path, valid.len()).await.unwrap(),
        &expected,
    );
    assert_eq!(
        config::json_file_if_present(&path, valid.len())
            .await
            .unwrap(),
        Some(expected)
    );
    assert_eq!(
        config::json_file(&path, valid.len() - 1)
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let invalid = [b'"', 0xff, b'"'];
    fs::write(&path, invalid).unwrap();
    assert_eq!(
        config::json_file(&path, 3).await.unwrap_err().code,
        "internal_error"
    );
    assert_eq!(
        config::json_file_if_present(&path, 3)
            .await
            .unwrap_err()
            .code,
        "internal_error"
    );
    assert_eq!(fs::read(&path).unwrap(), invalid);
    assert_eq!(
        config::json_file(&path, 2).await.unwrap_err().code,
        "invalid_arguments"
    );
    fs::write(&path, "{bad").unwrap();
    assert_eq!(
        config::json_file(&path, 100).await.unwrap_err().code,
        "internal_error"
    );
    assert_eq!(
        config::json_file(&fixture.0, 100).await.unwrap_err().code,
        "invalid_arguments"
    );
    fs::remove_file(&path).unwrap();
    assert_eq!(
        config::json_file_if_present(&path, 100).await.unwrap(),
        None
    );
    fs::write(&path, b"null").unwrap();
    assert_eq!(
        config::json_file_if_present(&path, 4).await.unwrap(),
        Some(IvyJson::Null)
    );
    let mut value = fixture.value();
    value.insert("unexpected", true.into());
    assert!(config::InstanceConfig::from_json(value).is_err());
}

#[tokio::test]
async fn health_control_requires_all_three_identities_and_preserves_utf16_slice() {
    let fixture = ServiceFixture::new();
    let config = config::InstanceConfig::from_json(fixture.value()).unwrap();
    let health = HealthFile::with_launch_id(config.clone(), Some("launch-1".into()));
    assert!(health.control().await.unwrap().is_none());
    let details = JsString(vec![0xd800; 4097]);
    health.write(true, details, Some(7)).await.unwrap();
    let value = config::json_file(fixture.0.join("data/health.json"), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        value.required("details").unwrap().as_js_string().unwrap().0,
        vec![0xd800; 4096]
    );
    assert_eq!(value.get("generation"), Some(&IvyJson::Number(7.0)));
    let before = value.clone();
    health.write(false, "draining".into(), None).await.unwrap();
    let value = config::json_file(fixture.0.join("data/health.json"), 1024 * 1024)
        .await
        .unwrap();
    for key in ["bootId", "launchId", "startedAt", "pid"] {
        assert_eq!(value.get(key), before.get(key));
    }
    let control = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("instanceId", "renamed-service".into()),
        ("launchId", "launch-1".into()),
        ("bootId", health.boot_id().into()),
        ("action", "shutdown".into()),
        ("requestedAt", "2026-10-18T00:00:00.000Z".into()),
    ]);
    let path = fixture.0.join("data/control.json");
    for key in ["instanceId", "launchId", "bootId"] {
        let mut wrong = control.clone();
        wrong.insert(key, "wrong".into());
        config::atomic_json(&path, wrong).await.unwrap();
        assert!(health.control().await.unwrap().is_none());
    }
    config::atomic_json(&path, control.clone()).await.unwrap();
    assert_eq!(
        ivy_contracts::json::canonical(&health.control().await.unwrap().unwrap(), usize::MAX)
            .unwrap(),
        ivy_contracts::json::canonical(&control, usize::MAX).unwrap()
    );
    let replacement = HealthFile::with_launch_id(config, Some("launch-1".into()));
    assert!(replacement.control().await.unwrap().is_none());
    fs::write(&path, "{}").unwrap();
    assert!(health.control().await.is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn cancelled_health_waiter_cannot_release_the_active_file_writer() {
    use std::{os::windows::fs::OpenOptionsExt, sync::Arc, time::Duration};
    let fixture = ServiceFixture::new();
    let health = Arc::new(HealthFile::with_launch_id(
        config::InstanceConfig::from_json(fixture.value()).unwrap(),
        Some("launch".into()),
    ));
    health.write(false, "initial".into(), None).await.unwrap();
    let path = fixture.0.join("data/health.json");
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    let writer = health.clone();
    let first =
        tokio::spawn(async move { writer.write(true, "cancelled waiter".into(), None).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if fs::read_dir(path.parent().unwrap()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("first write entered durable replacement");
    first.abort();
    let _ = first.await;
    let writer = health.clone();
    let mut second = tokio::spawn(async move { writer.write(false, "last".into(), None).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err()
    );
    drop(held);
    second.await.unwrap().unwrap();
    let value = config::json_file(&path, config::CONFIGURATION_BYTES)
        .await
        .unwrap();
    assert_eq!(value.get("details"), Some(&IvyJson::from("last")));
    assert_eq!(value.get("ready"), Some(&IvyJson::Bool(false)));
    assert!(!fs::read_dir(path.parent().unwrap()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[path = "host-artifact-inventory-cases.rs"]
mod artifact_inventory_cases;
