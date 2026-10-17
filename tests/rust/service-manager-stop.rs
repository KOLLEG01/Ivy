// HostExecutor owns the module index; exercise this owned module until exported.
pub use ivy_host_runtime::process;
#[path = "../../packages/host-runtime/rust/src/runtime_owner/stop.rs"]
mod stop;

use ivy_contracts::{IvyError, json::parse, limits::MANAGEMENT_FRAME_BYTES};
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
        std::fs::remove_dir_all(path).unwrap();
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
    let result = stop::stop_child(&child, Duration::from_secs(3), async {
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
