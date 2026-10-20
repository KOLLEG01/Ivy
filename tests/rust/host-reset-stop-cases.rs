use super::{ServiceFixture, assert_json_eq, host_configuration};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyJson as J, IvyResult, JsString, json};
use ivy_host_runtime::{
    config, exec_file, layout, node_path, process, runtime_maintenance,
    runtime_reset::{self, ResetHost},
};
use ivy_sdk::clock::{Clock, parse_date};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};

const DATE: &str = "2026-10-20T12:00:00.000Z";
static CORPUS: Mutex<()> = Mutex::new(());
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
fn input(fixture: &ServiceFixture, config: &J, operation: &str, deadline: bool) -> J {
    let marker = runtime_maintenance::runtime_reset_marker(config).unwrap();
    J::object([
        ("fixtureRoot", J::String(node_path::from_native(&fixture.0))),
        ("config", config.clone()),
        ("instance", instance(config)),
        ("operation", operation.into()),
        ("at", DATE.into()),
        ("deadlineClock", deadline.into()),
        ("healthBefore", contents(&data(config).join("health.json"))),
        ("markerBefore", contents(&marker)),
        (
            "recordBefore",
            contents(&marker.parent().unwrap().join("current.json")),
        ),
    ])
}
fn oracle(input: &J) -> J {
    let mut child = Command::new(std::env::var_os("IVY_RESTORE_NODE").expect("pinned Node"))
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
        ])
        .arg(root().join("tests/fixtures/host-reset-stop-oracle.mjs"))
        // Inherit exactly the same cwd as the native dispatch under test.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn observed(input: &J) -> J {
    let mut input = input.clone();
    input.insert("kind", "observe".into());
    oracle(&input)
}
fn wrap(result: IvyResult<J>) -> J {
    match result {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn bundle(outcome: J, input: &J) -> J {
    J::object([("outcome", outcome), ("state", observed(input))])
}
fn compare(case: &str, setup: J, actual: J, expected: J) {
    if let Some(file) = std::env::var_os("IVY_HOST_RESET_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .unwrap();
        writeln!(
            file,
            "{}",
            json::canonical(
                &J::object([
                    ("group", "host-stop".into()),
                    ("case", case.into()),
                    ("setup", setup),
                    ("actual", actual.clone()),
                    ("expected", expected.clone())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_json_eq(
        actual.get("outcome").unwrap(),
        expected.get("outcome").unwrap(),
    );
    assert_json_eq(actual.get("state").unwrap(), expected.get("state").unwrap());
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
fn clear_control(config: &J) {
    let control = data(config).join("control.json");
    if control.is_dir() {
        fs::remove_dir(control).unwrap();
    } else if control.exists() {
        fs::remove_file(control).unwrap();
    }
}

#[tokio::test]
async fn actual_administrator_and_full_entrypoint_denials_preserve_all_preimages() {
    let fixture = ServiceFixture::new();
    let config = host_configuration(&fixture);
    let native = host(false);
    let admin = input(&fixture, &config, "admin", false);
    let expected = oracle(&admin);
    let actual = wrap(native.require_administrator().await.map(|_| J::Null));
    let denied = actual.get("ok") == Some(&false.into());
    compare(
        "actual-administrator",
        admin.clone(),
        bundle(actual, &admin),
        expected,
    );
    for (operation, digest) in [
        ("prepare", "invalid"),
        (
            "prepare",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        ("complete", "unused"),
        ("confirm", "unused"),
    ] {
        // A genuinely elevated host may run the positive stages in a separate
        // admitted fixture suite. Never replace admission to force this branch.
        if !denied && digest != "invalid" {
            continue;
        }
        let marker = runtime_maintenance::runtime_reset_marker(&config).unwrap();
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "retained hold").unwrap();
        fs::write(
            marker.parent().unwrap().join("current.json"),
            "retained record",
        )
        .unwrap();
        let mut setup = input(&fixture, &config, operation, false);
        setup.insert("resetId", "fixture-reset".into());
        setup.insert("runtimeEpoch", "fixture-epoch".into());
        setup.insert("publisherDigest", digest.into());
        setup.insert("plan", J::Null);
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
        let actual = bundle(wrap(result), &setup);
        let expected = oracle(&setup);
        assert_eq!(
            actual.get("state").unwrap().get("recordUnchanged"),
            Some(&true.into())
        );
        assert_eq!(
            actual.get("state").unwrap().get("markerUnchanged"),
            Some(&true.into())
        );
        compare(&format!("{operation}-{digest}"), setup, actual, expected);
    }
}

#[tokio::test]
async fn health_validation_and_control_publication_match_the_unmodified_original() {
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
    ] {
        let fixture = ServiceFixture::new();
        let config = host_configuration(&fixture);
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
            save_health(&config, value).await;
        }
        if case == "malformed" {
            fs::write(data(&config).join("health.json"), "{").unwrap();
        }
        if case == "control-directory" {
            fs::create_dir(data(&config).join("control.json")).unwrap();
        }
        let setup = input(&fixture, &config, "send", false);
        let actual = bundle(
            wrap(
                native
                    .send_stop(config.clone(), instance(&config))
                    .await
                    .map(|v| v.map(J::Number).unwrap_or(J::Null)),
            ),
            &setup,
        );
        let expected_pid = match case {
            "modern" | "not-ready" | "stale-build" => J::Number(123.0),
            "large-modern-pid" => J::Number(9007199254740991.0),
            _ => J::Null,
        };
        assert_eq!(
            actual.get("outcome").unwrap().get("value"),
            Some(&expected_pid),
            "{case}"
        );
        if case != "control-directory" {
            clear_control(&config);
        }
        let expected = oracle(&setup);
        compare(case, setup, actual, expected);
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
                "--experimental-transform-types".into(),
                "--disable-warning=ExperimentalWarning".into(),
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
fn normalize_owned_pid(result: &mut J, pid: f64) {
    let mut outcome = result.remove("outcome").unwrap();
    assert_eq!(outcome.get("value"), Some(&J::Number(pid)));
    outcome.insert("value", "fixture-owned-pid".into());
    result.insert("outcome", outcome);
}

#[tokio::test]
async fn legacy_signal_uses_only_the_exact_owned_process_identity() {
    let fixture = ServiceFixture::new();
    let config = host_configuration(&fixture);
    let native = host(false);
    let mut results = Vec::new();
    let mut setups = Vec::new();
    let mut receipts = Vec::new();
    for label in ["native", "reference"] {
        let child = actor(&fixture, &config, label, "legacy", false).await;
        let mut value = health(child.pid);
        value.remove("launchId");
        save_health(&config, value).await;
        let setup = input(&fixture, &config, "send", false);
        let mut result = if label == "native" {
            bundle(
                wrap(
                    native
                        .send_stop(config.clone(), instance(&config))
                        .await
                        .map(|p| p.map(J::Number).unwrap_or(J::Null)),
                ),
                &setup,
            )
        } else {
            oracle(&setup)
        };
        let pid = child.pid;
        receipts.push(retire(child).await);
        normalize_owned_pid(&mut result, pid);
        results.push(result);
        setups.push(setup);
    }
    compare(
        "owned-legacy-sigterm",
        J::object([
            ("inputs", J::Array(setups)),
            ("retirements", J::Array(receipts)),
        ]),
        results.remove(0),
        results.remove(0),
    );
}

#[tokio::test]
async fn native_process_probe_and_deadline_wait_match_owned_kernel_state() {
    let fixture = ServiceFixture::new();
    let config = host_configuration(&fixture);
    let child = actor(&fixture, &config, "probe", "held", false).await;
    for (case, pid) in [
        ("live", child.pid),
        ("current-process", 0.0),
        ("fractional", 1.5),
        ("too-large", 9007199254740991.0),
    ] {
        let mut setup = input(&fixture, &config, "alive", false);
        setup.insert("pid", J::Number(pid));
        let actual = bundle(
            wrap(Ok(runtime_reset::reset_process_alive(pid).into())),
            &setup,
        );
        let expected = oracle(&setup);
        compare(case, setup, actual, expected);
    }
    let mut setup = input(&fixture, &config, "wait", true);
    setup.insert("pids", J::Array(vec![J::Number(child.pid)]));
    let actual = bundle(
        wrap(
            runtime_reset::wait_reset_processes_stopped(
                vec![child.pid],
                Arc::new(DeadlineClock(AtomicI64::new(0))),
            )
            .await
            .map(|_| J::Null),
        ),
        &setup,
    );
    let expected = oracle(&setup);
    let pid = child.pid;
    let receipt = retire(child).await;
    setup.insert("retirement", receipt);
    compare("live-wait-rejected", setup, actual, expected);
    let mut setup = input(&fixture, &config, "alive", false);
    setup.insert("pid", J::Number(pid));
    compare(
        "retired",
        setup.clone(),
        bundle(
            wrap(Ok(runtime_reset::reset_process_alive(pid).into())),
            &setup,
        ),
        oracle(&setup),
    );
    let mut setup = input(&fixture, &config, "wait", false);
    setup.insert("pids", J::Array(vec![]));
    compare(
        "empty-wait",
        setup.clone(),
        bundle(
            wrap(
                runtime_reset::wait_reset_processes_stopped(vec![], Arc::new(FixedClock))
                    .await
                    .map(|_| J::Null),
            ),
            &setup,
        ),
        oracle(&setup),
    );
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
    assert!(!retained.is_null());
    child.owner.completion.wait().await;
    let mut code = 0;
    let queried = unsafe { GetExitCodeProcess(retained, &mut code) };
    let mut setup = input(&fixture, &config, "alive", false);
    setup.insert("pid", J::Number(child.pid));
    setup.insert("retainedExitCode", J::Number(code as f64));
    let actual = bundle(
        wrap(Ok(runtime_reset::reset_process_alive(child.pid).into())),
        &setup,
    );
    let expected = oracle(&setup);
    unsafe {
        CloseHandle(retained);
    }
    setup.insert("retirement", retire(child).await);
    assert_ne!(queried, 0);
    assert_eq!(code, 259);
    compare("exited-259-retained-handle", setup, actual, expected);
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn real_control_stop_wait_and_owner_lock_proof_match_the_original() {
    for mode in ["cooperative", "unresponsive"] {
        let fixture = ServiceFixture::new();
        let config = host_configuration(&fixture);
        let deadline = mode == "unresponsive";
        let native = host(deadline);
        let mut results = Vec::new();
        let mut setups = Vec::new();
        let mut receipts = Vec::new();
        for label in ["native", "reference"] {
            clear_control(&config);
            let child = actor(&fixture, &config, label, mode, true).await;
            save_health(&config, health(child.pid)).await;
            let setup = input(&fixture, &config, "stop", deadline);
            let result = if label == "native" {
                bundle(
                    wrap(native.stop(config.clone()).await.map(|_| J::Null)),
                    &setup,
                )
            } else {
                oracle(&setup)
            };
            if mode == "cooperative" {
                child.owner.completion.wait().await;
            }
            let receipt = retire(child).await;
            if mode == "cooperative" {
                assert_eq!(
                    receipt.get("exitCode"),
                    Some(&J::Number(0.0)),
                    "cooperative fixture must exit normally after control"
                );
            }
            receipts.push(receipt);
            results.push(result);
            setups.push(setup);
        }
        compare(
            &format!("real-owner-{mode}"),
            J::object([
                ("inputs", J::Array(setups)),
                ("retirements", J::Array(receipts)),
            ]),
            results.remove(0),
            results.remove(0),
        );
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
        executables.insert(
            "ssh",
            if case == "missing-executable" {
                fixture
                    .0
                    .join("missing-ssh.exe")
                    .to_string_lossy()
                    .to_string()
                    .into()
            } else {
                J::String(node_path::from_native(Path::new(
                    &std::env::var_os("IVY_RESTORE_NODE").unwrap(),
                )))
            },
        );
        config.insert("executables", executables);
        let remote = J::object([
            ("ssh", "--no-warnings".into()),
            (
                "cliPath",
                J::String(node_path::from_native(
                    &root().join("tests/fixtures/host-reset-ssh-fixture.mjs"),
                )),
            ),
            (
                "configPath",
                J::String(node_path::from_native(&fixture.0.join("host config.json"))),
            ),
        ]);
        let args: Vec<JsString> = vec![
            "--fixture-case".into(),
            case.into(),
            "--reset-id".into(),
            "space ' quote \" $(literal) é 🚀".into(),
            "--runtime-epoch".into(),
            "exact-epoch".into(),
        ];
        let mut setup = input(&fixture, &config, "remote", false);
        setup.insert("host", remote.clone());
        setup.insert(
            "args",
            J::Array(args.iter().cloned().map(J::String).collect()),
        );
        let actual = bundle(
            wrap(
                native
                    .remote(config.clone(), remote, args)
                    .await
                    .map(|v| v.unwrap_or_else(|| "fixture-undefined-data".into())),
            ),
            &setup,
        );
        let expected = oracle(&setup);
        compare(&format!("ssh-{case}"), setup, actual, expected);
    }
}
