pub use ivy_host_runtime::process;
#[cfg(windows)]
#[path = "service-manager-owner-cases.rs"]
mod owner_cases;
use ivy_host_runtime::runtime_owner::stop;

use ivy_contracts::JsString;
use ivy_contracts::{IvyError, IvyJson, json::parse, limits::MANAGEMENT_FRAME_BYTES};
use ivy_host_runtime::{
    IvyResult,
    linux_host::FileMetadata,
    systemd::{SystemdAdapter, SystemdHost, UnitFile},
};
#[allow(dead_code)]
#[path = "host-service-fixture.rs"]
mod service_fixture;
use process::{Command, LaunchContext, OwnedProcess};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use stop::StopPolicy;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ivy-manager-stop-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    async fn spawn(&self, mode: &str) -> OwnedProcess {
        let context = LaunchContext {
            environment: BTreeMap::from([
                ("IVY_MANAGER_STOP_FIXTURE".into(), mode.into()),
                ("IVY_MANAGER_STOP_ROOT".into(), self.0.as_os_str().into()),
            ]),
            #[cfg(windows)]
            job_launcher: Some(
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("ivy-job.exe"),
            ),
            ..Default::default()
        };
        let command = Command {
            executable: "fixture".into(),
            args: vec![
                "--exact".into(),
                "child_fixture".into(),
                "--nocapture".into(),
            ],
            cwd: None,
            timeout_ms: 10000,
        };
        let child = process::spawn(
            &command,
            &self.0,
            &BTreeMap::from([("fixture".into(), std::env::current_exe().unwrap())]),
            context,
        )
        .await
        .unwrap();
        let ready = tokio::time::timeout(Duration::from_secs(10), async {
            while !self.0.join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if ready.is_err() {
            let _ = child.stop(Duration::ZERO).await;
            panic!("fixture did not start");
        }
        child
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let path = self.0.canonicalize().unwrap();
        assert!(path.starts_with(std::env::temp_dir().canonicalize().unwrap()));
        if let Err(error) = std::fs::remove_dir_all(&path) {
            if !std::thread::panicking() {
                panic!("Fixture cleanup failed: {error}");
            }
        }
    }
}

#[test]
fn child_fixture() {
    let Ok(mode) = std::env::var("IVY_MANAGER_STOP_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("IVY_MANAGER_STOP_ROOT").unwrap());
    std::fs::write(root.join("ready"), "ready").unwrap();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(20) {
        if mode == "cooperate" && root.join("control.json").exists() {
            return;
        }
        if mode == "drain" && root.join("control.json").exists() {
            std::fs::write(root.join("draining"), "waiting for model").unwrap();
            if root.join("release").exists() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("fixture watchdog expired");
}

#[test]
fn control_is_bound_to_instance_launch_and_boot() {
    let mut health = parse(
        r#"{"instanceId":"one","launchId":"launch","bootId":"boot"}"#,
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let control = stop::shutdown_control(&health, &"one".into(), &"launch".into(), "now").unwrap();
    assert_eq!(control, parse(r#"{"schemaVersion":1,"instanceId":"one","launchId":"launch","bootId":"boot","action":"shutdown","requestedAt":"now"}"#, MANAGEMENT_FRAME_BYTES).unwrap());
    assert!(stop::shutdown_control(&health, &"two".into(), &"launch".into(), "now").is_none());
    assert!(stop::shutdown_control(&health, &"one".into(), &"old".into(), "now").is_none());
    health.insert("bootId", "".into());
    assert!(stop::shutdown_control(&health, &"one".into(), &"launch".into(), "now").is_none());
}

#[tokio::test]
async fn cooperative_exit_completes_before_any_forced_stop() {
    let fixture = Fixture::new();
    let child = fixture.spawn("cooperate").await;
    let result = stop::stop_child(
        &child,
        StopPolicy::Deadline(Duration::from_secs(61)),
        async {
            tokio::fs::write(fixture.0.join("control.json"), "shutdown")
                .await
                .unwrap();
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.error_code, None);
    // Repeated stop on an already completed child must not write stale control.
    stop::stop_child(
        &child,
        StopPolicy::Deadline(Duration::from_secs(3)),
        async { panic!("late control write") },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn unresponsive_child_is_forced_only_after_the_grace_interval() {
    let fixture = Fixture::new();
    let child = fixture.spawn("ignore").await;
    let start = Instant::now();
    let grace = Duration::from_millis(250);
    let result = stop::stop_child(&child, StopPolicy::Deadline(grace), async {
        Err(IvyError::new(
            "internal_error",
            "Injected control write failure.",
        ))
    })
    .await
    .unwrap();
    assert!(start.elapsed() >= grace);
    assert_ne!(result.exit_code, Some(0));
    assert_ne!(result.error_code.as_deref(), Some("outcome_unknown"));
    assert!(child.completion.current().is_some());
}

#[tokio::test]
async fn stalled_control_write_cannot_extend_the_stop_deadline() {
    let fixture = Fixture::new();
    let child = fixture.spawn("ignore").await;
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        stop::stop_child(
            &child,
            StopPolicy::Deadline(Duration::from_millis(250)),
            std::future::pending(),
        ),
    )
    .await
    .expect("bounded stop")
    .unwrap();
    assert_ne!(result.error_code.as_deref(), Some("outcome_unknown"));
}

#[tokio::test]
async fn cooperative_owner_keeps_a_draining_process_until_its_own_completion() {
    let fixture = Fixture::new();
    let child = fixture.spawn("drain").await;
    let mut stopping = Box::pin(stop::stop_child(&child, StopPolicy::Cooperative, async {
        tokio::fs::write(fixture.0.join("control.json"), "shutdown")
            .await
            .unwrap();
        Ok(())
    }));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut stopping)
            .await
            .is_err()
    );
    assert!(fixture.0.join("draining").exists());
    assert!(child.completion.current().is_none());
    tokio::fs::write(fixture.0.join("release"), "model completed")
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.error_code, None);
}

type OsFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;
struct StopHost {
    states: Mutex<VecDeque<&'static str>>,
    commands: Mutex<Vec<Vec<JsString>>>,
    elapsed: AtomicI64,
    advance: i64,
}
impl StopHost {
    fn new(states: &[&'static str], advance: i64) -> Arc<Self> {
        Arc::new(Self {
            states: Mutex::new(states.iter().copied().collect()),
            commands: Mutex::new(vec![]),
            elapsed: AtomicI64::new(0),
            advance,
        })
    }
}
impl SystemdHost for StopHost {
    fn platform(&self) -> &str {
        "linux"
    }
    fn user_id(&self) -> Option<u32> {
        Some(0)
    }
    fn metadata(&self, _: JsString) -> OsFuture<std::io::Result<FileMetadata>> {
        panic!("No file operation during stop")
    }
    fn read(&self, _: JsString) -> OsFuture<std::io::Result<Vec<u8>>> {
        panic!("No file operation during stop")
    }
    fn open(&self, _: JsString, _: bool) -> OsFuture<std::io::Result<Box<dyn UnitFile>>> {
        panic!("No file operation during stop")
    }
    fn rename(&self, _: JsString, _: JsString) -> OsFuture<std::io::Result<()>> {
        panic!("No file operation during stop")
    }
    fn unlink(&self, _: JsString) -> OsFuture<std::io::Result<()>> {
        panic!("No file operation during stop")
    }
    fn execute(&self, _: JsString, args: Vec<JsString>) -> OsFuture<IvyResult<JsString>> {
        self.commands.lock().unwrap().push(args.clone());
        let output = if args[0].eq_str("show") {
            let mut states = self.states.lock().unwrap();
            let state = if states.len() > 1 {
                states.pop_front().unwrap()
            } else {
                states[0]
            };
            format!(
                "LoadState=loaded\nActiveState={state}\nSubState={}\nMainPID={}\n",
                if state == "inactive" {
                    "dead"
                } else {
                    "stop-sigterm"
                },
                if state == "inactive" { 0 } else { 123 }
            )
            .into()
        } else {
            assert!(args[0].eq_str("stop"));
            assert!(args[1].eq_str("--no-block"));
            "".into()
        };
        Box::pin(async move { Ok(output) })
    }
    fn uuid(&self) -> String {
        panic!("No file operation during stop")
    }
    fn now_ms(&self) -> f64 {
        self.elapsed.load(Ordering::SeqCst) as f64
    }
    fn sleep(&self, ms: u64) -> OsFuture<()> {
        self.elapsed.fetch_add(
            if self.advance == 0 {
                ms as i64
            } else {
                self.advance
            },
            Ordering::SeqCst,
        );
        Box::pin(tokio::task::yield_now())
    }
}
fn stop_config() -> IvyJson {
    IvyJson::object([(
        "executables",
        IvyJson::object([("systemctl", "/fixture/systemctl".into())]),
    )])
}
fn stop_unit() -> JsString {
    "ivy-next-0123456789ab-abcdef012345.service".into()
}

#[tokio::test]
async fn systemd_stop_observes_async_completion_and_accepts_manifest_sized_budgets() {
    for milliseconds in [150_000, 600_000] {
        let host = StopHost::new(&["active", "deactivating", "inactive"], 100);
        let state = SystemdAdapter::with_host(host.clone())
            .stop_unit(
                stop_config(),
                stop_unit(),
                StopPolicy::Deadline(Duration::from_millis(milliseconds)),
            )
            .await
            .unwrap();
        assert!(state.active_state.eq_str("inactive"));
        let commands = host.commands.lock().unwrap();
        assert_eq!(
            commands
                .iter()
                .filter(|args| args[0].eq_str("stop"))
                .count(),
            1
        );
        assert_eq!(
            commands[1],
            vec!["stop".into(), "--no-block".into(), stop_unit()]
        );
    }
    let host = StopHost::new(&["active", "deactivating"], 100);
    let error = SystemdAdapter::with_host(host)
        .stop_unit(
            stop_config(),
            stop_unit(),
            StopPolicy::Deadline(Duration::from_millis(150)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "outcome_unknown");
    let host = StopHost::new(&["active"], 0);
    let error = SystemdAdapter::with_host(host.clone())
        .stop_unit(
            stop_config(),
            stop_unit(),
            StopPolicy::Deadline(Duration::from_secs(601)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(host.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cooperative_systemd_stop_waits_beyond_a_manifest_deadline_without_another_stop() {
    let host = StopHost::new(
        &["active", "deactivating", "deactivating", "inactive"],
        700_000,
    );
    let policy = StopPolicy::for_manifest(&IvyJson::object([
        ("componentId", "agent-manager".into()),
        (
            "shutdown",
            IvyJson::object([("timeoutMs", 150_000.0.into())]),
        ),
    ]));
    assert_eq!(policy, StopPolicy::Cooperative);
    let state = SystemdAdapter::with_host(host.clone())
        .stop_unit(stop_config(), stop_unit(), policy)
        .await
        .unwrap();
    assert!(state.active_state.eq_str("inactive"));
    assert!(host.elapsed.load(Ordering::SeqCst) > 600_000);
    assert_eq!(
        host.commands
            .lock()
            .unwrap()
            .iter()
            .filter(|args| args[0].eq_str("stop"))
            .count(),
        1
    );
}

#[test]
fn agent_manager_unit_signals_only_its_owner_and_keeps_native_work_outside_a_stop_deadline() {
    let root = service_fixture::ServiceFixture::new();
    let hash = || IvyJson::from(format!("sha256:{}", "a".repeat(64)));
    for component in ["agent-manager", "fixture"] {
        let mut config = service_fixture::host_configuration(&root);
        let mut instance = config.required("instances").unwrap().as_array().unwrap()[0].clone();
        instance.insert("componentId", component.into());
        config.insert("instances", IvyJson::Array(vec![instance]));
        let candidate = IvyJson::object([
            ("candidateId", hash()),
            ("componentId", component.into()),
            (
                "artifactRoot",
                root.0.join("artifact").to_string_lossy().to_string().into(),
            ),
            (
                "platform",
                IvyJson::object([("os", "linux".into()), ("arch", "x64".into())]),
            ),
        ]);
        let target = IvyJson::object([
            ("instanceId", "fixture".into()),
            ("candidateId", hash()),
            ("revision", "launch".into()),
            (
                "configPath",
                root.0
                    .join("instance.json")
                    .to_string_lossy()
                    .to_string()
                    .into(),
            ),
        ]);
        let manifest = IvyJson::object([
            ("componentId", component.into()),
            (
                "entrypoint",
                IvyJson::object([
                    ("executable", "dist/native/linux-x64/fixture".into()),
                    ("args", IvyJson::Array(vec![])),
                ]),
            ),
            (
                "shutdown",
                IvyJson::object([("timeoutMs", 150_000.0.into())]),
            ),
        ]);
        let unit = ivy_host_runtime::systemd::render_systemd_unit(
            &config,
            &"fixture".into(),
            &target,
            &candidate,
            &manifest,
            None,
        )
        .unwrap()
        .content
        .to_utf8()
        .unwrap();
        if component == "agent-manager" {
            assert!(unit.contains("\nKillMode=mixed\nTimeoutStopSec=infinity\n"));
        } else {
            assert!(unit.contains("\nKillMode=control-group\nTimeoutStopSec=630\n"));
        }
    }
}

struct DrainClock(AtomicI64);
impl ivy_sdk::clock::Clock for DrainClock {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct DrainObservation {
    clock: Arc<DrainClock>,
    calls: AtomicUsize,
    cancel: bool,
}
impl ivy_host_runtime::executor::ActivationRuntime for DrainObservation {
    fn prepare<'a>(
        &'a self,
        _: &'a ivy_host_runtime::executor::OperationContext,
        _: &'a IvyJson,
        _: &'a str,
    ) -> ivy_host_runtime::executor::ExecutorFuture<'a, IvyJson> {
        Box::pin(async { panic!("No build during drain") })
    }
    fn compatibility<'a>(
        &'a self,
        _: &'a ivy_host_runtime::executor::OperationContext,
        _: &'a str,
        _: Option<IvyJson>,
    ) -> ivy_host_runtime::executor::ExecutorFuture<'a, ()> {
        Box::pin(async { panic!("No new deployment during drain") })
    }
    fn reconcile<'a>(
        &'a self,
        context: &'a ivy_host_runtime::executor::OperationContext,
        target: Option<IvyJson>,
        allow_start: bool,
    ) -> ivy_host_runtime::executor::ExecutorFuture<'a, ()> {
        assert!(!allow_start);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.clock.0.fetch_add(1000, Ordering::SeqCst);
        let at = ivy_sdk::clock::Clock::utc_millis(self.clock.as_ref());
        if self.cancel {
            context.cancel.cancel();
        }
        Box::pin(async move {
            let target = target.unwrap();
            context
                .journal
                .record_observation(
                    "fixture",
                    IvyJson::object([
                        ("schemaVersion", 1.0.into()),
                        ("instanceId", "fixture".into()),
                        ("ownerPid", (std::process::id() as f64).into()),
                        ("ownerBootId", "owner".into()),
                        (
                            "observedAt",
                            chrono::DateTime::from_timestamp_millis(at)
                                .unwrap()
                                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                                .into(),
                        ),
                        ("targetRevision", target.required("revision")?.clone()),
                        ("candidateId", target.required("candidateId")?.clone()),
                        ("buildId", target.required("candidateId")?.clone()),
                        (
                            "state",
                            if call == 0 { "draining" } else { "stopped" }.into(),
                        ),
                        ("health", IvyJson::Null),
                        ("restartCount", 0.0.into()),
                        ("nextRestartAt", IvyJson::Null),
                        ("code", IvyJson::Null),
                        ("message", "".into()),
                    ]),
                )
                .await
        })
    }
    fn dispose(&self) -> ivy_host_runtime::executor::ExecutorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn deployment_drain_keeps_its_original_owner_past_deadline_and_respects_cancellation() {
    use ivy_host_runtime::{
        executor::{ExecutorOptions, ObservationGoal, OperationContext, wait_for},
        journal::HostJournal,
    };
    for (component, cancel, cancel_before) in [
        ("agent-manager", false, false),
        ("fixture", false, false),
        ("agent-manager", true, false),
        ("agent-manager", false, true),
    ] {
        let root = service_fixture::ServiceFixture::new();
        let config = service_fixture::host_configuration(&root);
        let clock = Arc::new(DrainClock(AtomicI64::new(1760000000000)));
        let journal = HostJournal::open_with_clock(config.clone(), clock.clone())
            .await
            .unwrap();
        let hash = || IvyJson::from(format!("sha256:{}", "a".repeat(64)));
        let candidate = IvyJson::object([
            ("candidateId", hash()),
            ("buildId", hash()),
            ("componentId", component.into()),
            (
                "artifactRoot",
                root.0.join("artifact").to_string_lossy().to_string().into(),
            ),
            (
                "manifestPath",
                root.0
                    .join("manifest.json")
                    .to_string_lossy()
                    .to_string()
                    .into(),
            ),
            ("createdAt", "2026-01-01T00:00:00.000Z".into()),
            (
                "platform",
                IvyJson::object([
                    ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                    ("arch", "x64".into()),
                    ("node", "24.18.0".into()),
                ]),
            ),
        ]);
        let manifest = IvyJson::object([
            ("schemaVersion", 1.0.into()),
            ("componentId", component.into()),
            ("kind", "service".into()),
            ("version", "1.0.0".into()),
            ("buildId", hash()),
            ("connectsToHive", false.into()),
            (
                "requirements",
                IvyJson::object([
                    ("node", ">=24.18.0 <25.0.0".into()),
                    ("hiveProtocol", IvyJson::Null),
                    ("contracts", IvyJson::Array(vec![])),
                ]),
            ),
            (
                "entrypoint",
                IvyJson::object([
                    ("executable", "fixture".into()),
                    ("args", IvyJson::Array(vec![])),
                    ("timeoutMs", 30_000.0.into()),
                ]),
            ),
            (
                "readiness",
                IvyJson::object([
                    ("timeoutMs", 30_000.0.into()),
                    (
                        "command",
                        IvyJson::object([
                            ("executable", "fixture".into()),
                            ("args", IvyJson::Array(vec![])),
                            ("timeoutMs", 5000.0.into()),
                        ]),
                    ),
                ]),
            ),
            (
                "shutdown",
                IvyJson::object([("timeoutMs", 150_000.0.into())]),
            ),
            (
                "restart",
                IvyJson::object([
                    ("policy", "always".into()),
                    ("minimumDelayMs", 1000.0.into()),
                    ("maximumDelayMs", 60_000.0.into()),
                ]),
            ),
        ]);
        journal.save_candidate(candidate, manifest).await.unwrap();
        let context = OperationContext {
            journal,
            config,
            instance_id: "fixture".into(),
            cancel: Default::default(),
            clock: clock.clone(),
            options: ExecutorOptions {
                own_instance_id: None,
                account_home: root.0.to_string_lossy().to_string().into(),
                build_id: None,
            },
        };
        let target = IvyJson::object([
            ("instanceId", "fixture".into()),
            ("candidateId", hash()),
            ("revision", "original-launch".into()),
            ("desired", "stopped".into()),
            (
                "configPath",
                root.0
                    .join("instance.json")
                    .to_string_lossy()
                    .to_string()
                    .into(),
            ),
            ("requestedAt", "2026-01-01T00:00:00.000Z".into()),
        ]);
        let runtime = DrainObservation {
            clock,
            calls: AtomicUsize::new(0),
            cancel,
        };
        if cancel_before {
            context.cancel.cancel();
        }
        let result = wait_for(
            &context,
            &runtime,
            Some(target),
            ObservationGoal::Drained,
            1.0,
            false,
        )
        .await;
        if cancel || cancel_before {
            assert_eq!(result.unwrap_err().code, "cancelled");
            assert_eq!(
                runtime.calls.load(Ordering::SeqCst),
                if cancel_before { 0 } else { 1 }
            );
        } else if component == "agent-manager" {
            assert_eq!(result.unwrap().get("state"), Some(&"stopped".into()));
            assert_eq!(runtime.calls.load(Ordering::SeqCst), 2);
        } else {
            assert_eq!(result.unwrap_err().code, "observation_deadline");
            assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        }
        context.journal.close().await.unwrap();
    }
}

#[test]
fn native_executable_interoperates_with_unchanged_hive_and_public_sdk() {
    let root = Fixture::new();
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_ivy-service-manager"));
    let helper = binary.parent().unwrap().join(if cfg!(windows) {
        "ivy-job.exe"
    } else {
        "ivy-job"
    });
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "--import",
            "./tests/fixtures/service-manager-native-loader.mjs",
            "tests/fixtures/service-manager-native-hive.mjs",
        ])
        .arg(&binary)
        .arg(&root.0)
        .arg(helper)
        .arg(std::env::current_exe().unwrap())
        .arg(binary.parent().unwrap().join(if cfg!(windows) {
            "ivy-health.exe"
        } else {
            "ivy-health"
        }))
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(path) = std::env::var_os("IVY_MANAGER_INTEGRATION_TRACE") {
        std::fs::write(path, &output.stdout).unwrap();
    }
    let result = parse(
        std::str::from_utf8(&output.stdout).unwrap().trim(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    assert_eq!(result.get("ok"), Some(&IvyJson::Bool(true)));
    assert_eq!(
        result
            .required("results")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        if cfg!(windows) { 9 } else { 6 }
    );
}

#[cfg(windows)]
#[test]
fn native_bootstrap_launch_without_environment_accepts_fenced_graceful_control() {
    let root = Fixture::new();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_ivy-service-manager"));
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "--import",
            "./tests/fixtures/service-manager-native-loader.mjs",
            "tests/fixtures/service-manager-native-hive.mjs",
        ])
        .arg(&binary)
        .arg(&root.0)
        .arg(binary.parent().unwrap().join("ivy-job.exe"))
        .arg(std::env::current_exe().unwrap())
        .arg(binary.parent().unwrap().join("ivy-health.exe"))
        .arg("bootstrap-only")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result = parse(
        std::str::from_utf8(&output.stdout).unwrap().trim(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    assert_eq!(result.get("ok"), Some(&IvyJson::Bool(true)));
    assert_eq!(
        result
            .required("results")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

/// The unchanged Hive fixture drives a real Rust RuntimeOwner in this process.
/// Its child is the actual native Manager binary under the Windows Job.
#[cfg(windows)]
#[tokio::test]
async fn dispatcher_owner_fixture() {
    use ivy_host_runtime::{config::json_file, journal::HostJournal, runtime_owner::RuntimeOwner};
    let Some(request) = std::env::var_os("IVY_MANAGER_DISPATCH_OWNER") else {
        return;
    };
    let request = json_file(PathBuf::from(request), MANAGEMENT_FRAME_BYTES)
        .await
        .unwrap();
    let path = |key: &str| PathBuf::from(request.required(key).unwrap().as_str().unwrap());
    let host = json_file(path("hostConfigPath"), MANAGEMENT_FRAME_BYTES)
        .await
        .unwrap();
    let journal = HostJournal::open(host.clone()).await.unwrap();
    let target = request.required("target").unwrap().clone();
    publish_dispatch_target(&journal, target).await;
    let owner = RuntimeOwner::for_host(host, "manager".into(), path("bootstrapRoot"), None)
        .await
        .unwrap();
    owner.start().unwrap();
    let close = path("closePath");
    let replacement = path("replacementPath");
    let requested = tokio::time::timeout(Duration::from_secs(60), async {
        let mut published = false;
        while !close.exists() {
            if !published && replacement.exists() {
                publish_dispatch_target(
                    &journal,
                    json_file(replacement.clone(), MANAGEMENT_FRAME_BYTES)
                        .await
                        .unwrap(),
                )
                .await;
                published = true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let result = owner.close().await;
    journal.close().await.unwrap();
    result.unwrap();
    requested.expect("Dispatcher owner fixture exceeded its deadline");
}

#[cfg(windows)]
async fn publish_dispatch_target(
    journal: &ivy_host_runtime::journal::HostJournal,
    target: IvyJson,
) {
    let entry = journal
        .accept(
            IvyJson::object([
                ("action", "deploy".into()),
                ("instanceId", "manager".into()),
                (
                    "candidateId",
                    target.required("candidateId").unwrap().clone(),
                ),
                ("operationId", target.required("revision").unwrap().clone()),
            ]),
            None,
        )
        .await
        .unwrap();
    let id = entry
        .required("record")
        .unwrap()
        .required("deploymentId")
        .unwrap()
        .as_str()
        .unwrap();
    let empty = || IvyJson::object(Vec::<(String, IvyJson)>::new());
    journal
        .advance(&id, "prepared", "checking", empty())
        .await
        .unwrap();
    journal
        .advance(&id, "checking", "draining", empty())
        .await
        .unwrap();
    journal
        .advance(
            &id,
            "draining",
            "activating",
            IvyJson::object([("target", target.clone())]),
        )
        .await
        .unwrap();
    assert!(
        journal
            .accepted_target_policy(target)
            .await
            .unwrap()
            .is_some()
    );
}

#[test]
fn invalid_explicit_configuration_never_falls_back_to_environment() {
    let root = Fixture::new();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ivy-service-manager"))
        .args(["--config", "relative.json"])
        .env("IVY_INSTANCE_CONFIG", root.0.join("private-config.json"))
        .env_remove("IVY_NODE_EXECUTABLE")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = parse(
        std::str::from_utf8(&output.stderr).unwrap().trim(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    assert_eq!(error.get("code"), Some(&"invalid_arguments".into()));
    assert_eq!(
        error.get("message"),
        Some(&"ServiceManager failed to start.".into())
    );
    assert_eq!(error.as_object().unwrap().len(), 2);
}
