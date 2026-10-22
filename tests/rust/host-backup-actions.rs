use super::{ServiceFixture, assert_json, host, journal, node};
use ivy_contracts::{IvyJson as J, JsString, core_schema::validate_host, json};
use ivy_host_runtime::{
    artifact::FileCompare,
    config,
    executor_lock::ExecutorLock,
    host_actions::{HostActions, RestoreRuntime},
    host_config::HostContext,
    layout::StorageContext,
    native_backup_paths::NativeBackupPolicy,
    node_path,
    node_runtime::NodeRuntime,
    private_backup::BackupBindings,
    streaming_hash,
};
use ivy_sdk::clock::{Clock, SystemClock};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn compare() -> FileCompare {
    Arc::new(|a, b| a.cmp(b))
}
fn runtime_node() -> NodeRuntime {
    NodeRuntime {
        executable: node(),
        version: "24.18.0".into(),
    }
}
fn job(f: &ServiceFixture) -> PathBuf {
    f.0.join("distribution/dist/native/ivy-job.exe")
}
fn actions(f: &ServiceFixture, clock: Arc<dyn Clock>) -> HostActions {
    HostActions::with_clock(
        runtime_node(),
        compare(),
        f.0.join("distribution"),
        job(f),
        clock,
    )
    .unwrap()
}
async fn setup(f: &ServiceFixture) -> (PathBuf, Arc<BackupBindings>) {
    let mut input = host(f);
    input.insert("instances", J::Array(vec![]));
    let path = f.0.join("host.json");
    config::atomic_json(&path, input).await.unwrap();
    std::fs::create_dir_all(job(f).parent().unwrap()).unwrap();
    #[cfg(windows)]
    std::fs::copy(env!("CARGO_BIN_EXE_ivy-job"), job(f)).unwrap();
    let account = HostContext::current().unwrap();
    let cwd = account.paths.cwd.clone();
    let policy = NativeBackupPolicy::new(
        StorageContext {
            paths: account.paths,
            account_home: account.account_home,
            compare: compare(),
        },
        account.environment,
        cwd,
    );
    (
        path,
        Arc::new(BackupBindings {
            compare: compare(),
            native_paths: Arc::new(policy),
            node: runtime_node(),
            distribution_root: f.0.join("distribution"),
            clock: Arc::new(SystemClock),
        }),
    )
}
fn closed(f: &ServiceFixture) {
    let db = ivy_store::rusqlite::Connection::open(f.0.join("state/deployments.sqlite")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
}
async fn target(f: &ServiceFixture, suffix: &str) -> PathBuf {
    let mut input = host(f);
    input.insert("instances", J::Array(vec![]));
    for (key, base) in [
        ("runtimeRoot", "restored-state"),
        ("artifactRoot", "restored-artifacts"),
        ("stagingRoot", "restored-staging"),
    ] {
        input.insert(
            key,
            J::String(node_path::from_native(
                &f.0.join(format!("{base}-{suffix}")),
            )),
        );
    }
    input.insert("publicBaseUrl", "http://127.0.0.1:39082/ivy".into());
    let path = f.0.join(format!("template-{suffix}.json"));
    config::atomic_json(&path, input).await.unwrap();
    path
}
fn restore_runtime() -> RestoreRuntime {
    RestoreRuntime::native(runtime_node(), compare()).unwrap()
}
fn selected_hash(result: &J) -> JsString {
    result
        .required("manifestHash")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone()
}
async fn assert_backup_unchanged(f: &ServiceFixture, manifest: &J) {
    for file in manifest.required("files").unwrap().as_array().unwrap() {
        let local = file.required("path").unwrap().as_str().unwrap();
        assert_eq!(
            streaming_hash::file_hash(f.0.join("backup").join(local))
                .await
                .unwrap(),
            file.required("hash").unwrap().as_str().unwrap()
        );
    }
    assert_eq!(
        std::fs::read(f.0.join("state/retained.txt")).unwrap(),
        b"source authority"
    );
}
#[tokio::test]
async fn backup_refuses_overlap_and_busy_owner_then_closes_outer_journal() {
    let f = ServiceFixture::new();
    let (path, b) = setup(&f).await;
    let actions = actions(&f, Arc::new(SystemClock));
    let error = actions
        .backup(path.clone(), f.0.join("state/overlap"), b.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, "target_conflict");
    assert!(!f.0.join("state/overlap").exists());
    closed(&f);
    let busy = ExecutorLock::acquire(f.0.join("state/executor"), false)
        .await
        .unwrap();
    let error = actions
        .backup(path, f.0.join("busy-backup"), b)
        .await
        .unwrap_err();
    assert_eq!(error.code, "backup_busy");
    assert!(!f.0.join("busy-backup").exists());
    busy.close().await.unwrap();
    closed(&f);
}
#[tokio::test]
async fn backup_then_restore_preserves_source_authority_and_publishes_disabled_target() {
    let f = ServiceFixture::new();
    let (path, b) = setup(&f).await;
    let actions = actions(&f, Arc::new(SystemClock));
    let journal = journal(&f).await;
    journal.close().await.unwrap();
    std::fs::write(f.0.join("state/retained.txt"), b"source authority").unwrap();
    let backup = actions.backup(path, f.0.join("backup"), b).await.unwrap();
    validate_host("BackupResult", &backup).unwrap();
    closed(&f);
    let manifest = config::json_file(f.0.join("backup/backup.json"), 12 * 1024 * 1024)
        .await
        .unwrap();
    let result = actions
        .restore(
            target(&f, "complete").await,
            f.0.join("backup"),
            selected_hash(&backup),
            restore_runtime(),
        )
        .await
        .unwrap();
    validate_host("RestoreResult", &result).unwrap();
    assert_eq!(result.get("state"), Some(&"disabled".into()));
    let restored = f.0.join("restored-state-complete");
    assert_eq!(
        std::fs::read(restored.join("retained.txt")).unwrap(),
        b"source authority"
    );
    assert_eq!(
        std::fs::read(restored.join("recovery-original/deployments.sqlite")).unwrap(),
        std::fs::read(f.0.join("backup/state/deployments.sqlite")).unwrap()
    );
    assert_eq!(
        std::fs::read(restored.join("recovery-original/configuration.json")).unwrap(),
        std::fs::read(f.0.join("backup/configuration.json")).unwrap()
    );
    let saved = config::json_file(restored.join("host.json"), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        saved.required("restoredFrom").unwrap().get("manifestHash"),
        backup.get("manifestHash")
    );
    let native = config::json_file(restored.join("native-recovery.json"), 1024 * 1024)
        .await
        .unwrap();
    validate_host("NativeRecoveryReport", &native).unwrap();
    assert!(
        native
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_backup_unchanged(&f, &manifest).await;
    closed(&f);
}
#[tokio::test]
async fn restore_checks_backup_identity_before_target_configuration_or_state() {
    let f = ServiceFixture::new();
    let (_path, _b) = setup(&f).await;
    let actions = actions(&f, Arc::new(SystemClock));
    let backup = f.0.join("invalid-backup");
    std::fs::create_dir(&backup).unwrap();
    std::fs::write(backup.join("backup.json"), b"{}").unwrap();
    let template = f.0.join("absent-template.json");
    for (hash, expected) in [
        (JsString::from("invalid"), "invalid_arguments"),
        (
            format!("sha256:{}", "a".repeat(64)).into(),
            "backup_changed",
        ),
    ] {
        let error = actions
            .restore(template.clone(), backup.clone(), hash, restore_runtime())
            .await
            .unwrap_err();
        assert_eq!(error.code, expected, "{error:?}");
    }
    assert!(!template.exists());
    assert!(!f.0.join("state").exists());
    assert!(!f.0.join("restored-state").exists());
    assert_eq!(std::fs::read(backup.join("backup.json")).unwrap(), b"{}");
}
struct GateClock {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}
impl Clock for GateClock {
    fn utc_millis(&self) -> i64 {
        self.entered.store(true, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        SystemClock.utc_millis()
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct Release(Arc<AtomicBool>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_restore_observer_keeps_admitted_copy_recovery_and_publication() {
    let f = ServiceFixture::new();
    let (path, b) = setup(&f).await;
    let journal = journal(&f).await;
    journal.close().await.unwrap();
    std::fs::write(f.0.join("state/retained.txt"), b"source authority").unwrap();
    let backup = actions(&f, Arc::new(SystemClock))
        .backup(path, f.0.join("backup"), b)
        .await
        .unwrap();
    let manifest = config::json_file(f.0.join("backup/backup.json"), 12 * 1024 * 1024)
        .await
        .unwrap();
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let guard = Release(release.clone());
    let actions = actions(
        &f,
        Arc::new(GateClock {
            entered: entered.clone(),
            release,
        }),
    );
    let template = target(&f, "dropped").await;
    let root = f.0.join("backup");
    let observer = tokio::spawn(async move {
        actions
            .restore(template, root, selected_hash(&backup), restore_runtime())
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    assert!(!f.0.join("restored-state-dropped").exists());
    observer.abort();
    drop(guard);
    tokio::time::timeout(Duration::from_secs(15), async {
        while !f.0.join("restored-state-dropped/host.json").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let saved = config::json_file(f.0.join("restored-state-dropped/host.json"), 1024 * 1024)
        .await
        .unwrap();
    assert!(saved.get("restoredFrom").is_some());
    assert_backup_unchanged(&f, &manifest).await;
    closed(&f);
}
#[tokio::test]
#[ignore = "Requires an explicitly pinned original CLI and its dependency bounds"]
async fn original_restore_identity_refusals() {
    let f = ServiceFixture::new();
    let (_path, _b) = setup(&f).await;
    let actions = actions(&f, Arc::new(SystemClock));
    let backup = f.0.join("invalid-backup");
    std::fs::create_dir(&backup).unwrap();
    std::fs::write(backup.join("backup.json"), b"{}").unwrap();
    let original = std::env::var_os("IVY_TEST_ORIGINAL_CLI")
        .map(PathBuf::from)
        .expect("Explicit original CLI required");
    for hash in ["invalid".to_owned(), format!("sha256:{}", "a".repeat(64))] {
        let output = tokio::process::Command::new(node())
            .arg(&original)
            .args(["restore", "--config"])
            .arg(f.0.join("absent-template.json"))
            .args(["--backup"])
            .arg(&backup)
            .args(["--backup-hash", &hash])
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        let wire = json::parse(&String::from_utf8(output.stdout).unwrap(), 1024 * 1024).unwrap();
        let error = actions
            .restore(
                f.0.join("absent-template.json"),
                backup.clone(),
                hash.into(),
                restore_runtime(),
            )
            .await
            .unwrap_err();
        // The CLI contract projects a domain error into code/data. The shared
        // RPC error envelope returned by to_wire is a different public shape.
        let mut data = J::object([
            ("message", J::String(error.message_js())),
            ("outcome", error.outcome_json()),
        ]);
        if let Some(details) = error.details.clone() {
            data.insert("details", details);
        }
        assert_json(
            &wire,
            &J::object([
                ("schemaVersion", J::Number(1.0)),
                ("ok", false.into()),
                ("code", J::String(error.code_js())),
                ("data", data),
            ]),
        );
        assert_eq!(
            output.status.code(),
            Some(if error.code == "invalid_arguments" {
                2
            } else {
                1
            })
        );
    }
    assert!(!f.0.join("state").exists());
}
