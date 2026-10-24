use super::{ServiceFixture, assert_json_eq, host_configuration};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyJson as J, JsString, json};
use ivy_host_runtime::{
    config, exec_file, layout, node_path, process, runtime_maintenance,
    runtime_reset::{self, ResetHost},
};
use ivy_sdk::clock::{Clock, parse_date};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};

const DATE: &str = "2026-10-20T12:00:00.000Z";
struct FixedClock;
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        parse_date(&DATE.into()) as i64
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct DeadlineClock(AtomicI64);
impl Clock for DeadlineClock {
    fn utc_millis(&self) -> i64 {
        self.0.fetch_add(100_000, Ordering::SeqCst)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn host(deadline: bool) -> ResetHost {
    let clock: Arc<dyn Clock> = if deadline {
        Arc::new(DeadlineClock(AtomicI64::new(0)))
    } else {
        Arc::new(FixedClock)
    };
    ResetHost::new(
        root(),
        PathBuf::from(std::env::var_os("IVY_BACKUP_JOB").expect("actual Job helper")),
        clock,
    )
    .unwrap()
}
fn path(value: &J) -> PathBuf {
    node_path::node_filesystem_path(value.as_js_string().unwrap()).unwrap()
}
fn instance(config: &J) -> J {
    config.get("instances").unwrap().as_array().unwrap()[0].clone()
}
fn data(config: &J) -> PathBuf {
    path(
        layout::service_paths(config, &instance(config))
            .unwrap()
            .get("data")
            .unwrap(),
    )
}
fn contents(file: &Path) -> J {
    match fs::metadata(file) {
        Ok(meta) if meta.is_dir() => J::object([("directory", true.into())]),
        Ok(_) => J::object([("base64", STANDARD.encode(fs::read(file).unwrap()).into())]),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => J::Null,
        Err(e) => panic!("fixture observation: {e}"),
    }
}
fn health(pid: f64) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("instanceId", "fixture".into()),
        ("componentId", "fixture".into()),
        ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
        ("pid", J::Number(pid)),
        ("bootId", "fixture-boot".into()),
        ("launchId", "fixture-launch".into()),
        ("startedAt", DATE.into()),
        ("observedAt", DATE.into()),
        ("ready", true.into()),
        ("generation", J::Null),
        ("details", "fixture".into()),
    ])
}
async fn save_health(config: &J, value: J) {
    config::atomic_json(data(config).join("health.json"), value)
        .await
        .unwrap();
}

fn preimages(config: &J) -> [J; 4] {
    let marker = runtime_maintenance::runtime_reset_marker(config).unwrap();
    [
        contents(&data(config).join("health.json")),
        contents(&data(config).join("control.json")),
        contents(&marker),
        contents(&marker.parent().unwrap().join("current.json")),
    ]
}
fn control() -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("instanceId", "fixture".into()),
        ("launchId", "fixture-launch".into()),
        ("bootId", "fixture-boot".into()),
        ("action", "shutdown".into()),
        ("requestedAt", DATE.into()),
    ])
}
fn path_json(path: &Path) -> J {
    J::String(node_path::from_native(path))
}

#[tokio::test]
async fn actual_administrator_and_full_entrypoint_denials_preserve_all_preimages() {
    let fixture = ServiceFixture::new();
    let config = host_configuration(&fixture);
    let native = host(false);
    let before = preimages(&config);
    let denied = match native.require_administrator().await {
        Ok(()) => false,
        Err(error) => {
            assert_eq!(error.code, "access_denied");
            true
        }
    };
    assert_eq!(preimages(&config), before);
    for (operation, digest) in [
        ("prepare", "invalid"),
        (
            "prepare",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        ("complete", "unused"),
        ("confirm", "unused"),
    ] {
        if !denied && digest != "invalid" {
            continue;
        }
        let marker = runtime_maintenance::runtime_reset_marker(&config).unwrap();
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, b"retained hold").unwrap();
        fs::write(
            marker.parent().unwrap().join("current.json"),
            b"retained record",
        )
        .unwrap();
        let before = preimages(&config);
        let result = match operation {
            "prepare" => {
                native
                    .prepare(
                        config.clone(),
                        J::Null,
                        "fixture-reset".into(),
                        "fixture-epoch".into(),
                        digest.into(),
                    )
                    .await
            }
            "complete" => {
                native
                    .complete(
                        config.clone(),
                        "fixture-reset".into(),
                        "fixture-epoch".into(),
                    )
                    .await
            }
            _ => {
                native
                    .confirm(
                        config.clone(),
                        "fixture-reset".into(),
                        "fixture-epoch".into(),
                    )
                    .await
            }
        };
        assert_eq!(
            result.unwrap_err().code,
            if digest == "invalid" {
                "invalid_arguments"
            } else {
                "access_denied"
            }
        );
        assert_eq!(
            preimages(&config),
            before,
            "{operation}: admission changed preimages"
        );
    }
}

#[tokio::test]
async fn native_health_validation_and_control_publication_preserve_paths_and_preimages() {
    let native = host(false);
    for case in [
        "modern",
        "not-ready",
        "stale-build",
        "wrong-instance",
        "missing",
        "malformed",
        "missing-boot",
        "empty-launch",
        "control-directory",
        "large-modern-pid",
        "large-legacy-pid",
        "unicode-data",
        "opaque-data",
        "dot-alias",
    ] {
        let fixture = ServiceFixture::new();
        let mut config = host_configuration(&fixture);
        let mut expected_data = data(&config);
        if ["unicode-data", "opaque-data", "dot-alias"].contains(&case) {
            expected_data = fixture.0.join("服务-é-🚀");
            if case == "opaque-data" {
                #[cfg(windows)]
                {
                    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
                    expected_data.push(OsString::from_wide(&[0xd800]));
                }
            }
            let configured_data = if case == "dot-alias" {
                expected_data.join("absent").join("..").join(".")
            } else {
                expected_data.clone()
            };
            let mut item = instance(&config);
            item.insert(
                "paths",
                J::object([
                    ("data", path_json(&configured_data)),
                    ("work", path_json(&fixture.0.join("work"))),
                    ("logs", path_json(&fixture.0.join("logs"))),
                ]),
            );
            config.insert("instances", J::Array(vec![item]));
        }
        let mut value = health(123.0);
        match case {
            "not-ready" => {
                value.insert("ready", false.into());
            }
            "stale-build" => {
                value.insert("buildId", format!("sha256:{}", "b".repeat(64)).into());
                value.insert("observedAt", "2020-01-01T00:00:00.000Z".into());
            }
            "wrong-instance" => {
                value.insert("instanceId", "different".into());
            }
            "missing-boot" => {
                value.remove("bootId");
            }
            "empty-launch" => {
                value.insert("launchId", "".into());
                value.insert("pid", J::Number(9007199254740991.0));
            }
            "large-modern-pid" | "large-legacy-pid" => {
                value.insert("pid", J::Number(9007199254740991.0));
                if case == "large-legacy-pid" {
                    value.remove("launchId");
                }
            }
            _ => {}
        }
        if case != "missing" {
            config::atomic_json(expected_data.join("health.json"), value)
                .await
                .unwrap();
        }
        if case == "malformed" {
            fs::write(expected_data.join("health.json"), b"{").unwrap();
        }
        if case == "control-directory" {
            fs::create_dir(expected_data.join("control.json")).unwrap();
        }
        let health_before = contents(&expected_data.join("health.json"));
        let marker = runtime_maintenance::runtime_reset_marker(&config).unwrap();
        let retained = (
            contents(&marker),
            contents(&marker.parent().unwrap().join("current.json")),
        );
        let actual = native
            .send_stop(config.clone(), instance(&config))
            .await
            .unwrap();
        let expected = match case {
            "modern" | "not-ready" | "stale-build" | "unicode-data" | "opaque-data"
            | "dot-alias" => Some(123.0),
            "large-modern-pid" => Some(9007199254740991.0),
            _ => None,
        };
        assert_eq!(actual, expected, "{case}");
        assert_eq!(
            contents(&expected_data.join("health.json")),
            health_before,
            "{case}"
        );
        assert_eq!(
            (
                contents(&marker),
                contents(&marker.parent().unwrap().join("current.json"))
            ),
            retained,
            "{case}"
        );
        if expected.is_some() {
            assert_json_eq(
                &config::json_file(
                    expected_data.join("control.json"),
                    config::CONFIGURATION_BYTES,
                )
                .await
                .unwrap(),
                &control(),
            );
            let expected_bytes =
                format!("{}\n", json::canonical(&control(), usize::MAX).unwrap()).into_bytes();
            assert_eq!(
                fs::read(expected_data.join("control.json")).unwrap(),
                expected_bytes
            );
        } else if case == "control-directory" {
            assert!(expected_data.join("control.json").is_dir());
        } else {
            assert!(!expected_data.join("control.json").exists(), "{case}");
        }
        assert!(
            !expected_data.join("absent").exists(),
            "{case}: path normalization created an alias"
        );
    }
}

struct Actor {
    owner: process::OwnedProcess,
    pid: f64,
    identity: J,
}
async fn actor(fixture: &ServiceFixture, config: &J, label: &str, mode: &str, hold: bool) -> Actor {
    let actor_root = fixture.0.join(label);
    fs::create_dir(&actor_root).unwrap();
    let nonce = uuid::Uuid::new_v4().to_string();
    let owner_root = if hold {
        path(config.get("runtimeRoot").unwrap()).join("owners/fixture")
    } else {
        PathBuf::new()
    };
    let owner = exec_file::spawn_ignored(
        exec_file::SpawnCommand {
            executable: std::env::var_os("IVY_RESTORE_NODE").unwrap(),
            args: vec![
                root()
                    .join("tests/fixtures/host-reset-owned-process-fixture.mjs")
                    .into_os_string(),
                actor_root.clone().into_os_string(),
                nonce.clone().into(),
                mode.into(),
                owner_root.into_os_string(),
                data(config).join("control.json").into_os_string(),
            ],
            cwd: None,
        },
        root(),
        exec_file::SpawnOptions {
            job_launcher: Some(std::env::var_os("IVY_BACKUP_JOB").unwrap().into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let identity = loop {
        if let Ok(bytes) = fs::read(actor_root.join("identity.json")) {
            if let Ok(value) = json::parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX) {
                break value;
            }
        }
        if Instant::now() >= until {
            owner.stop(Duration::ZERO).await.unwrap();
            panic!("owned fixture never identified itself")
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(identity.get("nonce"), Some(&nonce.into()));
    Actor {
        pid: identity.get("pid").unwrap().as_f64().unwrap(),
        owner,
        identity,
    }
}
async fn retire(actor: Actor) -> J {
    let result = actor.owner.stop(Duration::ZERO).await.unwrap();
    let stopped = process::owned_process_tree_stopped(&actor.owner)
        .await
        .unwrap();
    assert!(stopped, "exact owned process tree did not retire");
    J::object([
        ("identity", actor.identity),
        ("launcherPid", J::Number(actor.owner.pid as f64)),
        ("treeStopped", stopped.into()),
        (
            "exitCode",
            result
                .exit_code
                .map(|v| J::Number(v as f64))
                .unwrap_or(J::Null),
        ),
    ])
}

#[tokio::test]
async fn legacy_signal_uses_only_the_exact_owned_process_identity() {
    let fixture = ServiceFixture::new();
    let config = host_configuration(&fixture);
    let native = host(false);
    let child = actor(&fixture, &config, "legacy-native", "legacy", false).await;
    let pid = child.pid;
    let mut value = health(pid);
    value.remove("launchId");
    save_health(&config, value).await;
    let before = preimages(&config);
    let result = native.send_stop(config.clone(), instance(&config)).await;
    let receipt = retire(child).await;
    assert_eq!(result.unwrap(), Some(pid));
    assert_eq!(receipt.get("treeStopped"), Some(&true.into()));
    assert_eq!(preimages(&config), before);
    assert!(!runtime_reset::reset_process_alive(pid));
}

#[tokio::test]
async fn native_process_probe_and_deadline_wait_match_owned_kernel_state() {
    let fixture = ServiceFixture::new();
    let config = host_configuration(&fixture);
    let child = actor(&fixture, &config, "probe", "held", false).await;
    let pid = child.pid;
    let probes = [
        ("live", pid, true),
        ("current-process", 0.0, true),
        ("fractional", 1.5, false),
        ("too-large", 9007199254740991.0, false),
    ]
    .map(|(case, pid, expected)| (case, runtime_reset::reset_process_alive(pid), expected));
    let wait = runtime_reset::wait_reset_processes_stopped(
        vec![pid],
        Arc::new(DeadlineClock(AtomicI64::new(0))),
    )
    .await;
    let receipt = retire(child).await;
    for (case, actual, expected) in probes {
        assert_eq!(actual, expected, "{case}");
    }
    assert_eq!(wait.unwrap_err().code, "outcome_unknown");
    assert_eq!(receipt.get("treeStopped"), Some(&true.into()));
    assert!(!runtime_reset::reset_process_alive(pid));
    runtime_reset::wait_reset_processes_stopped(vec![], Arc::new(FixedClock))
        .await
        .unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn exited_259_is_gone_even_when_the_numeric_status_is_still_active() {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE,
        },
    };
    let fixture = ServiceFixture::new();
    let config = host_configuration(&fixture);
    let child = actor(&fixture, &config, "exit259", "exit259", false).await;
    let retained = unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            child.pid as u32,
        )
    };
    child.owner.completion.wait().await;
    let mut code = 0;
    let queried = unsafe { GetExitCodeProcess(retained, &mut code) };
    let alive = runtime_reset::reset_process_alive(child.pid);
    unsafe {
        CloseHandle(retained);
    }
    let receipt = retire(child).await;
    assert!(!retained.is_null());
    assert_ne!(queried, 0);
    assert_eq!(code, 259);
    assert!(!alive);
    assert_eq!(receipt.get("treeStopped"), Some(&true.into()));
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn real_control_stop_wait_and_owner_lock_proof_preserve_shutdown_fences() {
    for mode in ["cooperative", "unresponsive"] {
        let fixture = ServiceFixture::new();
        let config = host_configuration(&fixture);
        let native = host(mode == "unresponsive");
        let child = actor(&fixture, &config, "native-owner", mode, true).await;
        save_health(&config, health(child.pid)).await;
        let before = preimages(&config);
        let result = native.stop(config.clone()).await;
        let alive = runtime_reset::reset_process_alive(child.pid);
        let owner_root = path(config.required("runtimeRoot").unwrap()).join("owners/fixture");
        let lock = ivy_host_runtime::journal::ExecutorLock::acquire(owner_root, false).await;
        let lock_held = lock.is_err();
        if let Ok(lock) = lock {
            lock.close().await.unwrap();
        }
        let receipt = retire(child).await;
        let after = preimages(&config);
        assert_eq!(after[0], before[0], "{mode}: health changed");
        assert_eq!(after[2..], before[2..], "{mode}: reset authority changed");
        let mut expected_control = control();
        if mode == "unresponsive" {
            expected_control.insert("requestedAt", "1970-01-01T00:00:00.000Z".into());
        }
        assert_json_eq(
            &config::json_file(
                data(&config).join("control.json"),
                config::CONFIGURATION_BYTES,
            )
            .await
            .unwrap(),
            &expected_control,
        );
        assert_eq!(receipt.get("treeStopped"), Some(&true.into()));
        if mode == "cooperative" {
            result.unwrap();
            assert!(!alive);
            assert!(!lock_held);
            assert_eq!(receipt.get("exitCode"), Some(&J::Number(0.0)));
        } else {
            assert_eq!(result.unwrap_err().code, "outcome_unknown");
            assert!(alive);
            assert!(lock_held, "live unresponsive writer lost its owner fence");
        }
        for member in ["executor", "owners/fixture"] {
            let lock = ivy_host_runtime::journal::ExecutorLock::acquire(
                path(config.required("runtimeRoot").unwrap()).join(member),
                false,
            )
            .await
            .unwrap();
            lock.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn configured_ssh_dispatch_preserves_literal_argv_and_response_envelopes() {
    let native = host(false);
    for case in [
        "success",
        "truthy-ok",
        "undefined-data",
        "null-data",
        "denied",
        "missing-code",
        "empty-code",
        "null-envelope",
        "malformed",
        "exit",
        "max-buffer",
        "missing-executable",
    ] {
        let fixture = ServiceFixture::new();
        let mut config = host_configuration(&fixture);
        let mut executables = config.remove("executables").unwrap();
        let executable = if case == "missing-executable" {
            fixture.0.join("missing-ssh.exe")
        } else {
            PathBuf::from(std::env::var_os("IVY_RESTORE_NODE").unwrap())
        };
        executables.insert("ssh", path_json(&executable));
        config.insert("executables", executables);
        let config_path = fixture.0.join("host config.json");
        let receipt_path = fixture.0.join("ssh-receipt.json");
        let remote = J::object([
            ("ssh", "--no-warnings".into()),
            (
                "cliPath",
                path_json(&root().join("tests/fixtures/host-reset-ssh-fixture.mjs")),
            ),
            ("configPath", path_json(&config_path)),
        ]);
        let args: Vec<JsString> = vec![
            "--fixture-case".into(),
            case.into(),
            "--fixture-receipt".into(),
            node_path::from_native(&receipt_path),
            "--reset-id".into(),
            r#"space ' quote " $(literal) é 🚀"#.into(),
            "--runtime-epoch".into(),
            "exact-epoch".into(),
        ];
        let mut expected_args = vec![
            J::from("runtime-reset-host"),
            "--config".into(),
            path_json(&config_path),
        ];
        expected_args.extend(args.iter().cloned().map(J::String));
        expected_args.push("--json".into());
        let before = preimages(&config);
        let result = native.remote(config.clone(), remote, args).await;
        match case {
            "success" => {
                let value = result.unwrap().unwrap();
                assert_json_eq(
                    value.required("args").unwrap(),
                    &J::Array(expected_args.clone()),
                );
                assert_eq!(
                    path(value.required("cwd").unwrap()),
                    std::env::current_dir().unwrap()
                );
            }
            "undefined-data" => assert!(result.unwrap().is_none()),
            "null-data" => assert_eq!(result.unwrap(), Some(J::Null)),
            "denied" => assert_eq!(result.unwrap_err().code, "access_denied"),
            "truthy-ok" | "missing-code" | "null-envelope" => {
                assert_eq!(result.unwrap_err().code, "remote_reset_failed", "{case}")
            }
            "empty-code" => assert_eq!(result.unwrap_err().code, ""),
            _ => assert_eq!(result.unwrap_err().code, "internal_error", "{case}"),
        }
        assert_eq!(
            preimages(&config),
            before,
            "{case}: remote dispatch changed local reset state"
        );
        if case != "missing-executable" {
            let receipt = config::json_file(&receipt_path, config::CONFIGURATION_BYTES)
                .await
                .unwrap();
            assert_json_eq(receipt.required("args").unwrap(), &J::Array(expected_args));
        } else {
            assert!(!receipt_path.exists());
        }
    }
}
