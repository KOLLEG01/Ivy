// HostExecutor owns the module index; exercise this owned module until exported.
pub use ivy_host_runtime::process;
#[cfg(windows)]
#[path = "service-manager-owner-cases.rs"]
mod owner_cases;
#[path = "../../packages/host-runtime/rust/src/runtime_owner/stop.rs"]
mod stop;

use ivy_contracts::{IvyError, IvyJson, json::parse, limits::MANAGEMENT_FRAME_BYTES};
use process::{Command, LaunchContext, OwnedProcess};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

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
    let result = stop::stop_child(&child, Duration::from_secs(61), async {
        tokio::fs::write(fixture.0.join("control.json"), "shutdown")
            .await
            .unwrap();
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.error_code, None);
    // Repeated stop on an already completed child must not write stale control.
    stop::stop_child(&child, Duration::from_secs(3), async {
        panic!("late control write")
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unresponsive_child_is_forced_only_after_the_grace_interval() {
    let fixture = Fixture::new();
    let child = fixture.spawn("ignore").await;
    let start = Instant::now();
    let grace = Duration::from_millis(250);
    let result = stop::stop_child(&child, grace, async {
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
        stop::stop_child(&child, Duration::from_millis(250), std::future::pending()),
    )
    .await
    .expect("bounded stop")
    .unwrap();
    assert_ne!(result.error_code.as_deref(), Some("outcome_unknown"));
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
        if cfg!(windows) { 8 } else { 6 }
    );
}

/// The unchanged Hive fixture drives a real Rust RuntimeOwner in this process.
/// Its child is the retained Node dispatcher and actual Manager binary, not a
/// second service implementation in the fixture.
#[cfg(windows)]
#[tokio::test]
async fn dispatcher_owner_fixture() {
    use ivy_host_runtime::{
        config::json_file, journal::HostJournal, node_runtime::NodeRuntime,
        runtime_owner::RuntimeOwner,
    };
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
    let node_path = path("nodeExecutable");
    let node = NodeRuntime::discover(Some(&node_path)).await.unwrap();
    let owner = RuntimeOwner::for_host(host, "manager".into(), path("bootstrapRoot"), node)
        .await
        .unwrap();
    owner.start().unwrap();
    let close = path("closePath");
    let requested = tokio::time::timeout(Duration::from_secs(60), async {
        while !close.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let result = owner.close().await;
    journal.close().await.unwrap();
    result.unwrap();
    requested.expect("Dispatcher owner fixture exceeded its deadline");
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
