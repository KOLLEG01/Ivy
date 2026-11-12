use super::ServiceFixture;
use ivy_contracts::{IvyJson as J, core_schema::validate_host, hash::hash_json};
use ivy_host_runtime::{
    executor_lock::ExecutorLock,
    native_path,
    node_runtime::NodeRuntime,
    storage_retention::{self, RetentionBindings},
};
use ivy_sdk::clock::Clock;
use std::{
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
#[path = "host-storage-retention-fixture.rs"]
mod fixture;
const NOW: i64 = 1_792_483_200_000;
struct FixedClock;
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        NOW
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
fn text(path: &Path) -> J {
    J::String(native_path::from_native(path))
}
fn bindings(setup: &fixture::Setup, clock: Arc<dyn Clock>) -> Arc<RetentionBindings> {
    Arc::new(RetentionBindings {
        node: setup
            .config
            .required("executables")
            .unwrap()
            .get("node")
            .map(|executable| NodeRuntime {
                executable: native_path::to_native(executable.as_js_string().unwrap()),
                version: "24.18.0".into(),
            }),
        clock,
    })
}
async fn run_case(case: &str) -> J {
    let f = ServiceFixture::new();
    let setup = fixture::setup(&f.0.join("native"), case).await;
    let held = match &setup.held {
        Some(path) => Some(ExecutorLock::acquire(path.clone(), false).await.unwrap()),
        None => None,
    };
    let result = storage_retention::collect_host_storage(
        setup.config.clone(),
        Some(NOW),
        None,
        bindings(&setup, Arc::new(FixedClock)),
    )
    .await;
    let actual = match result {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    };
    if let Some(lock) = held {
        lock.close().await.unwrap();
    }
    let state = fixture::observe(&setup).await;
    let (error, remaining, status) = match case {
        "basic" => (None, 3, "succeeded"),
        "unknown-preparation" => (None, 1, "partial"),
        "retired-staging" | "pending-retirement" => (None, 0, "succeeded"),
        "pending-unowned" => (Some("target_conflict"), 0, "failed"),
        "retired-link" | "completed-unknown" | "completed-scalar-request" => (None, 0, "partial"),
        "busy-candidate" => (None, 1, "partial"),
        "busy-bootstrap" => (Some("executor_already_running"), 6, "failed"),
        "unknown-version" => (None, 1, "succeeded"),
        "unresolved-unknown" | "unresolved-scalar-request" => {
            (Some("storage_invalid"), 6, "failed")
        }
        "bootstrap-current" => (None, 2, "succeeded"),
        "bootstrap-prior" | "bootstrap-missing-time" | "bootstrap-null-time" => {
            (None, 3, "succeeded")
        }
        "bootstrap-scalar-time" => (Some("internal_error"), 6, "failed"),
        "bootstrap-order" => (None, 4, "succeeded"),
        "maintenance-ring" => (None, 2, "succeeded"),
        "reason-cap" => (None, 4, "partial"),
        "active-snapshot" => (None, 0, "succeeded"),
        "scalar-snapshot" => (Some("invalid_arguments"), 6, "failed"),
        _ => panic!("Unknown native retention case: {case}"),
    };
    assert_eq!(
        actual.get("ok"),
        Some(&error.is_none().into()),
        "{case}: {actual:?}"
    );
    if let Some(code) = error {
        assert_eq!(
            actual
                .required("error")
                .unwrap()
                .required("data")
                .unwrap()
                .get("code"),
            Some(&code.into()),
            "{case}"
        );
    }
    assert_eq!(
        state
            .required("registered")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        remaining,
        "{case}: {state:?}"
    );
    let protected: &[u8] = match case {
        "basic" => &[1, 2, 3],
        "unknown-preparation" | "unknown-version" => &[2],
        "busy-candidate" => &[4],
        "busy-bootstrap"
        | "unresolved-unknown"
        | "unresolved-scalar-request"
        | "bootstrap-scalar-time"
        | "scalar-snapshot" => &[1, 2, 3, 4, 5, 6],
        "bootstrap-current" => &[2, 3],
        "bootstrap-prior"
        | "bootstrap-missing-time"
        | "bootstrap-null-time" => &[1, 3, 4],
        "maintenance-ring" => &[3, 4],
        "bootstrap-order" => &[0, 4, 5, 6],
        "reason-cap" => &[1, 2, 3, 4],
        _ => &[],
    };
    let registered = protected
        .iter()
        .map(|number| J::from(format!("sha256:{}", format!("{number:x}").repeat(64))))
        .collect::<Vec<_>>();
    assert_eq!(
        state.required("registered").unwrap(),
        &J::Array(registered),
        "{case}"
    );
    let mut directories = protected
        .iter()
        .map(|number| format!("{number:x}").repeat(64))
        .collect::<Vec<_>>();
    if case == "pending-unowned" {
        directories.push("2".repeat(64));
    }
    directories.sort();
    assert_eq!(
        state.required("candidateDirectories").unwrap(),
        &J::Array(directories.into_iter().map(J::from).collect()),
        "{case}"
    );
    let preparations = match case {
        "unknown-preparation" | "retired-link" => 1,
        "reason-cap" => 80,
        _ => 0,
    };
    let snapshots = usize::from(
        [
            "unknown-preparation",
            "retired-link",
            "active-snapshot",
            "scalar-snapshot",
        ]
        .contains(&case),
    );
    assert_eq!(
        state
            .required("preparations")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        preparations,
        "{case}"
    );
    assert_eq!(
        state
            .required("snapshots")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        snapshots,
        "{case}"
    );
    if ["unknown-preparation", "active-snapshot", "scalar-snapshot"].contains(&case) {
        assert_eq!(state.get("snapshotUnchanged"), Some(&true.into()), "{case}");
    }
    let observed = state.required("status").unwrap();
    validate_host("StorageRetentionStatus", observed).unwrap();
    assert_eq!(
        observed.get("state"),
        Some(&status.into()),
        "{case}: {observed:?}"
    );
    assert_eq!(
        observed.get("startedAt"),
        observed.get("completedAt"),
        "{case}"
    );
    if status == "succeeded" {
        assert_eq!(observed.get("lastSuccessAt"), observed.get("completedAt"));
    } else {
        assert_eq!(
            observed.get("lastSuccessAt"),
            Some(&"2000-01-01T00:00:00.000Z".into())
        );
    }
    for (_, bytes) in observed
        .required("availableBytes")
        .unwrap()
        .as_object()
        .unwrap()
    {
        let bytes = bytes.as_f64().unwrap();
        assert!(bytes.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&bytes));
    }
    if case == "pending-unowned" {
        assert_eq!(
            state.required("pending").unwrap().as_array().unwrap().len(),
            1
        );
    } else {
        assert!(
            state
                .required("pending")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    for name in ["artifact-retention-lock", "bootstrap-maintenance-lock"] {
        let runtime = native_path::to_native(
            setup
                .config
                .required("runtimeRoot")
                .unwrap()
                .as_js_string()
                .unwrap(),
        );
        let lock = ExecutorLock::acquire(runtime.join(name), false)
            .await
            .unwrap();
        lock.close().await.unwrap();
    }
    J::object([("outcome", actual), ("state", state)])
}
#[tokio::test]
async fn live_references_and_completed_preparation_cleanup_remain_protected() {
    let observed = run_case("basic").await;
    assert_eq!(
        observed
            .required("outcome")
            .unwrap()
            .required("value")
            .unwrap(),
        &J::object([
            ("candidates", J::Number(3.0)),
            ("preparations", J::Number(1.0)),
            ("snapshots", J::Number(1.0))
        ])
    );
}
#[tokio::test]
async fn uncertain_preparation_ownership_blocks_only_snapshot_collection() {
    let result = run_case("unknown-preparation").await;
    let state = result.required("state").unwrap();
    assert_eq!(
        state
            .required("registered")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        state
            .required("snapshots")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        state.required("status").unwrap().get("state"),
        Some(&"partial".into())
    );
}
#[tokio::test]
async fn retirement_intents_resume_and_unowned_paths_keep_the_pending_record() {
    for case in ["retired-staging", "pending-retirement", "pending-unowned"] {
        run_case(case).await;
    }
}
#[tokio::test]
async fn linked_retirement_directories_preserve_their_targets() {
    let observed = run_case("retired-link").await;
    let state = observed.required("state").unwrap();
    assert_eq!(state.get("kept"), Some(&"must remain".into()));
    let status = state.required("status").unwrap();
    assert_eq!(status.get("state"), Some(&"partial".into()));
    assert_eq!(
        status.required("removed").unwrap().get("preparations"),
        Some(&J::Number(0.0))
    );
    assert_eq!(
        status.required("removed").unwrap().get("snapshots"),
        Some(&J::Number(0.0))
    );
}
#[tokio::test]
async fn busy_build_and_bootstrap_locks_preserve_status_and_release_collection_ownership() {
    for case in ["busy-candidate", "busy-bootstrap"] {
        run_case(case).await;
    }
}
#[tokio::test]
async fn unknown_version_and_history_evidence_preserve_failure_boundaries() {
    for case in [
        "unknown-version",
        "completed-unknown",
        "unresolved-unknown",
        "completed-scalar-request",
        "unresolved-scalar-request",
    ] {
        run_case(case).await;
    }
}
#[tokio::test]
async fn selected_and_unresolved_bootstrap_plans_are_retained() {
    for case in [
        "bootstrap-current",
        "bootstrap-prior",
        "bootstrap-missing-time",
        "bootstrap-null-time",
        "bootstrap-scalar-time",
    ] {
        let observed = run_case(case).await;
        if case != "bootstrap-current" {
            let details = observed
                .required("state")
                .unwrap()
                .required("details")
                .unwrap()
                .as_array()
                .unwrap();
            assert_eq!(details.len(), 1, "{case}");
            assert_eq!(
                details[0].get("recordUnchanged"),
                Some(&true.into()),
                "{case}"
            );
        }
    }
}
#[cfg(windows)]
#[tokio::test]
async fn superseded_bootstrap_plans_do_not_archive_referenced_releases() {
    let observed = run_case("bootstrap-order").await;
    let plans = observed
        .required("state")
        .unwrap()
        .required("plans")
        .unwrap();
    assert!(!plans.as_array().unwrap().contains(&"plan-7".into()));
    assert!(!plans.as_array().unwrap().contains(&"plan-8".into()));
}
#[tokio::test]
async fn superseded_maintenance_is_collected_while_selected_evidence_is_unchanged() {
    let observed = run_case("maintenance-ring").await;
    let details = observed
        .required("state")
        .unwrap()
        .required("details")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(details.len(), 21);
    assert!(
        details
            .iter()
            .all(|d| d.get("recordUnchanged") == Some(&true.into()))
    );
    for number in [0] {
        let directory = hash_json(&J::from(format!("old-maintenance-{number}"))).unwrap();
        let detail = details
            .iter()
            .find(|detail| detail.get("name") == Some(&directory[7..].into()))
            .unwrap();
        let files: &[&str] = &[
                "host-executor.desired.json",
                "host-executor.prior.json",
                "record.json",
            ];
        assert_eq!(
            detail.required("files").unwrap(),
            &J::Array(files.iter().map(|file| J::from(*file)).collect()),
            "Selected maintenance {number} retains its exact definition evidence"
        );
    }
}
#[tokio::test]
async fn distinct_skip_reasons_are_coalesced_at_the_status_bound() {
    let observed = run_case("reason-cap").await;
    let skipped = observed
        .required("state")
        .unwrap()
        .required("status")
        .unwrap()
        .required("skipped")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(skipped.len(), 64);
    assert!(
        skipped
            .iter()
            .any(|r| r.get("reason") == Some(&"additional_retained_entries".into()))
    );
}
struct GateClock {
    calls: AtomicUsize,
    entered: tokio::sync::Notify,
    released: Mutex<bool>,
    wake: Condvar,
}
struct ReleaseGate(Arc<GateClock>);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        *self.0.released.lock().unwrap() = true;
        self.0.wake.notify_all();
    }
}
impl Clock for GateClock {
    fn utc_millis(&self) -> i64 {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            self.entered.notify_one();
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.wake.wait(released).unwrap();
            }
        }
        NOW
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_collection_keeps_locks_after_its_observer_drops() {
    let f = ServiceFixture::new();
    let root = f.0.join("native");
    let setup = fixture::setup(&root, "retired-staging").await;
    let gate = Arc::new(GateClock {
        calls: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        released: Mutex::new(false),
        wake: Condvar::new(),
    });
    let _release_on_exit = ReleaseGate(gate.clone());
    let config = setup.config.clone();
    let runtime = native_path::to_native(
        config
            .required("runtimeRoot")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    let b = bindings(&setup, gate.clone());
    let observer = tokio::spawn(async move {
        storage_retention::collect_host_storage(config, None, None, b).await
    });
    tokio::time::timeout(Duration::from_secs(15), gate.entered.notified())
        .await
        .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    let busy = ExecutorLock::acquire(runtime.join("artifact-retention-lock"), false)
        .await
        .err()
        .unwrap();
    assert_eq!(busy.code, "executor_already_running");
    *gate.released.lock().unwrap() = true;
    gate.wake.notify_all();
    let mut released = false;
    for _ in 0..100 {
        if let Ok(lock) =
            ExecutorLock::acquire(runtime.join("artifact-retention-lock"), false).await
        {
            lock.close().await.unwrap();
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(released);
    let observed = fixture::observe(&setup).await;
    assert_eq!(
        observed.required("status").unwrap().get("state"),
        Some(&"succeeded".into())
    );
    assert!(
        observed
            .required("registered")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn unfinished_source_snapshot_fields_are_typed_before_snapshot_collection() {
    for case in ["active-snapshot", "scalar-snapshot"] {
        let observed = run_case(case).await;
        assert_eq!(
            observed
                .required("state")
                .unwrap()
                .required("snapshots")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

struct MovingClock(std::sync::atomic::AtomicI64);
impl Clock for MovingClock {
    fn utc_millis(&self) -> i64 { self.0.load(Ordering::SeqCst) }
    fn monotonic(&self) -> Instant { Instant::now() }
}
struct IdleActivation;
impl ivy_host_runtime::executor::ActivationRuntime for IdleActivation {
    fn prepare<'a>(&'a self, _: &'a ivy_host_runtime::executor::OperationContext, _: &'a J, _: &'a str) -> ivy_host_runtime::executor::ExecutorFuture<'a, J> { Box::pin(async { panic!("No deployment") }) }
    fn compatibility<'a>(&'a self, _: &'a ivy_host_runtime::executor::OperationContext, _: &'a str, _: Option<J>) -> ivy_host_runtime::executor::ExecutorFuture<'a, ()> { Box::pin(async { panic!("No deployment") }) }
    fn reconcile<'a>(&'a self, _: &'a ivy_host_runtime::executor::OperationContext, _: Option<J>, _: bool) -> ivy_host_runtime::executor::ExecutorFuture<'a, ()> { Box::pin(async { Ok(()) }) }
    fn dispose(&self) -> ivy_host_runtime::executor::ExecutorFuture<'_, ()> { Box::pin(async { Ok(()) }) }
}
async fn wait_collection(journal: &ivy_host_runtime::journal::HostJournal, previous: Option<J>) -> J {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(status) = journal.storage_retention_status().await.unwrap()
                && status.get("state") == Some(&"succeeded".into())
                && status.get("startedAt") != previous.as_ref() { return status; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.unwrap()
}
#[tokio::test]
async fn executor_automatically_collects_real_storage_and_resumes_its_daily_schedule() {
    use ivy_host_runtime::{executor::{HostExecutor, ExecutorOptions}, journal::HostJournal, storage_space};
    let f = ServiceFixture::new(); let setup = fixture::setup(&f.0.join("automatic"), "basic").await;
    let runtime = native_path::to_native(setup.config.required("runtimeRoot").unwrap().as_js_string().unwrap());
    // An isolated fresh installation: no CLI collection or persisted schedule.
    { let db = ivy_store::rusqlite::Connection::open(runtime.join("deployments.sqlite")).unwrap(); db.execute("DELETE FROM meta WHERE key='storage-retention'", []).unwrap(); }
    let clock = Arc::new(MovingClock(std::sync::atomic::AtomicI64::new(NOW)));
    let journal = HostJournal::open_with_clock(setup.config.clone(), clock.clone()).await.unwrap();
    let options = ExecutorOptions { own_instance_id: None, account_home: f.0.to_string_lossy().to_string().into(), build_id: None };
    let artifacts = native_path::to_native(setup.config.required("artifactRoot").unwrap().as_js_string().unwrap());
    let obsolete = artifacts.join("candidates").join("5".repeat(64));
    std::fs::write(obsolete.join("artifact/retention-probe"), vec![23; 4 * 1024 * 1024]).unwrap();
    let space_before = storage_space::available_bytes(&artifacts).unwrap();
    let low = storage_space::host_storage_space(setup.config.clone()).await.unwrap().get("lowSpace") == Some(&true.into());
    let owner = HostExecutor::open_with_clock(setup.config.clone(), options.clone(), Arc::new(IdleActivation), clock.clone()).await.unwrap();
    owner.start().unwrap();
    if !low {
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(journal.storage_retention_status().await.unwrap().is_none());
        clock.0.fetch_add(60_000, Ordering::SeqCst);
        // Advancing the injected wall clock does not wake the native watcher.
        // Trigger its ordinary reconciliation hint instead of waiting 30 real seconds.
        ivy_host_runtime::config::atomic_json(runtime.join("runtime-change.json"), J::object([
            ("schemaVersion", 1.0.into()), ("generation", uuid::Uuid::new_v4().to_string().into()),
        ])).await.unwrap();
    }
    let first = wait_collection(&journal, None).await;
    assert_eq!(first.get("trigger"), Some(&if low { "storage_pressure" } else { "scheduled" }.into()));
    assert!(first.required("removed").unwrap().required("candidates").unwrap().as_f64().unwrap() > 0.0);
    assert!(!obsolete.exists());
    let space_after = storage_space::available_bytes(&artifacts).unwrap();
    eprintln!("automatic retention: trigger={}, native available before={space_before}, after={space_after}", if low { "storage_pressure" } else { "scheduled" });
    let first_started = first.required("startedAt").unwrap().clone();
    owner.close().await.unwrap();
    let owner = HostExecutor::open_with_clock(setup.config.clone(), options, Arc::new(IdleActivation), clock.clone()).await.unwrap(); owner.start().unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(journal.storage_retention_status().await.unwrap().unwrap().get("startedAt"), Some(&first_started));
    clock.0.fetch_add(86_400_000, Ordering::SeqCst);
    ivy_host_runtime::config::atomic_json(runtime.join("runtime-change.json"), J::object([
        ("schemaVersion", 1.0.into()), ("generation", uuid::Uuid::new_v4().to_string().into()),
    ])).await.unwrap();
    let second = wait_collection(&journal, Some(first_started)).await;
    assert_eq!(second.get("trigger"), Some(&"scheduled".into()));
    owner.close().await.unwrap(); journal.close().await.unwrap();
}

#[tokio::test]
async fn held_source_lease_preserves_snapshots_until_build_work_has_drained() {
    let f = ServiceFixture::new(); let setup = fixture::setup(&f.0.join("source-lease"), "basic").await;
    let staging = native_path::to_native(setup.config.required("stagingRoot").unwrap().as_js_string().unwrap());
    let lease = ExecutorLock::acquire(staging.join("source-storage-lock"), true).await.unwrap();
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap();
    let observed = fixture::observe(&setup).await;
    assert_eq!(observed.required("snapshots").unwrap().as_array().unwrap().len(), 1);
    lease.close().await.unwrap();
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap();
    assert!(fixture::observe(&setup).await.required("snapshots").unwrap().as_array().unwrap().is_empty());
}

#[tokio::test]
async fn source_root_link_cannot_admit_an_external_retirement_directory() {
    let f = ServiceFixture::new(); let setup = fixture::setup(&f.0.join("source-root-link"), "basic").await;
    let staging = native_path::to_native(setup.config.required("stagingRoot").unwrap().as_js_string().unwrap());
    let sources = staging.join("sources"); let outside = f.0.join("outside");
    std::fs::rename(&sources, &outside).unwrap();
    let sentinel = outside.join(".retiring-00000000-0000-5000-8000-000000000004/sentinel");
    std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap(); std::fs::write(&sentinel, b"retain").unwrap();
    ivy_host_runtime::filesystem::directory_link(&outside, &sources).unwrap();
    let failure = storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap_err();
    assert_eq!(failure.code, "target_conflict"); assert_eq!(std::fs::read(sentinel).unwrap(), b"retain");
}

#[tokio::test]
async fn completed_transfers_resume_cleanup_while_unknown_and_leased_imports_survive() {
    use std::fs;
    let f = ServiceFixture::new(); let setup = fixture::setup(&f.0.join("transfers"), "basic").await;
    let staging = native_path::to_native(setup.config.required("stagingRoot").unwrap().as_js_string().unwrap());
    let transfers = staging.join("package-transfers");
    for (name, completed) in [('7', true), ('8', false), ('9', true)] {
        let key = name.to_string().repeat(64); let directory = transfers.join(&key);
        fs::create_dir_all(directory.join("extracted")).unwrap();
        fs::write(directory.join("archive.tar.gz"), vec![13; 1024 * 1024]).unwrap();
        fs::write(directory.join("extracted/payload"), b"owned intermediate").unwrap();
        ivy_host_runtime::config::atomic_json(directory.join("transfer.json"), J::object([
            ("schemaVersion", J::Number(1.0)), ("archiveHash", format!("sha256:{key}").into()), ("buildId", format!("sha256:{key}").into()), ("completed", completed.into())
        ])).await.unwrap();
    }
    let outside = f.0.join("retained-backup"); fs::create_dir_all(&outside).unwrap(); fs::write(outside.join("sentinel"), b"keep").unwrap();
    ivy_host_runtime::filesystem::directory_link(&outside, &transfers.join("a".repeat(64))).unwrap();
    let lease = ExecutorLock::acquire(staging.join("build-locks").join("9".repeat(64)), false).await.unwrap();
    let space_before = ivy_host_runtime::storage_space::available_bytes(&staging).unwrap();
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap();
    let space_after = ivy_host_runtime::storage_space::available_bytes(&staging).unwrap();
    assert!(!transfers.join("7".repeat(64)).exists());
    assert!(transfers.join("8".repeat(64)).join("archive.tar.gz").is_file());
    assert!(transfers.join("9".repeat(64)).join("archive.tar.gz").is_file());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    let observed = fixture::observe(&setup).await; let status = observed.required("status").unwrap();
    assert_eq!(status.get("state"), Some(&"partial".into()));
    eprintln!("transfer retention: native available before={space_before}, after={space_after}");
    lease.close().await.unwrap();
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap();
    assert!(!transfers.join("9".repeat(64)).exists());
    assert!(transfers.join("8".repeat(64)).join("archive.tar.gz").is_file());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
}

#[tokio::test]
async fn compiler_cache_ownership_and_live_build_lease_bound_reclamation() {
    use std::fs;
    let f = ServiceFixture::new(); let setup = fixture::setup(&f.0.join("compiler-cache"), "basic").await;
    let staging = native_path::to_native(setup.config.required("stagingRoot").unwrap().as_js_string().unwrap());
    let cache = staging.join("compiler-cache"); fs::create_dir_all(cache.join("cargo")).unwrap(); fs::write(cache.join("cargo/payload"), vec![42; 1024 * 1024]).unwrap();
    ivy_host_runtime::config::atomic_json(cache.join("owner.json"), J::object([
        ("schemaVersion", J::Number(1.0)), ("owner", "ivy-compiler-cache".into()), ("lastUsedAt", J::Number((NOW - 8 * 86_400_000) as f64))
    ])).await.unwrap();
    let legacy = staging.join("build-cache/cargo/unknown"); fs::create_dir_all(legacy.parent().unwrap()).unwrap(); fs::write(&legacy, b"historical").unwrap();
    let lease = ExecutorLock::acquire(staging.join("source-storage-lock"), true).await.unwrap();
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap(); assert!(cache.join("cargo/payload").is_file());
    lease.close().await.unwrap();
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap();
    assert!(!cache.exists()); assert_eq!(fs::read(legacy).unwrap(), b"historical");
    let retired = staging.join(".retiring-compiler-cache"); fs::create_dir(&retired).unwrap(); fs::write(retired.join("unknown"), b"retain").unwrap();
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap();
    assert_eq!(fs::read(retired.join("unknown")).unwrap(), b"retain");
    fs::remove_dir_all(&retired).unwrap(); fs::create_dir_all(retired.join("cargo")).unwrap(); fs::write(retired.join("cargo/payload"), b"admitted retirement").unwrap();
    ivy_host_runtime::config::atomic_json(retired.join("owner.json"), J::object([
        ("schemaVersion", J::Number(1.0)), ("owner", "ivy-compiler-cache".into()), ("lastUsedAt", J::Number((NOW - 8 * 86_400_000) as f64))
    ])).await.unwrap();
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap(); assert!(!retired.exists());
}

#[tokio::test]
async fn rollback_and_grace_protection_survive_a_newer_release() {
    use ivy_host_runtime::{journal::HostJournal, filesystem};
    let f = ServiceFixture::new(); let setup = fixture::setup(&f.0.join("rollback-grace"), "basic").await;
    let journal = HostJournal::open(setup.config.clone()).await.unwrap();
    let rollback = format!("sha256:{}", "2".repeat(64));
    assert!(journal.retention_candidate_ids().await.unwrap().contains(&rollback));
    assert!(!journal.retire_candidate(&rollback).await.unwrap());
    let artifacts = native_path::to_native(setup.config.required("artifactRoot").unwrap().as_js_string().unwrap());
    let fresh = artifacts.join("candidates").join("4".repeat(64)); filesystem::set_times_ms(&fresh, NOW, NOW).unwrap();
    let staging = native_path::to_native(setup.config.required("stagingRoot").unwrap().as_js_string().unwrap());
    let snapshot = staging.join("sources/00000000-0000-5000-8000-000000000002"); filesystem::set_times_ms(&snapshot, NOW, NOW).unwrap();
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap();
    assert!(fresh.is_dir()); assert!(snapshot.is_dir()); assert!(journal.candidate(&rollback).await.is_ok());
    journal.close().await.unwrap();
}

#[tokio::test]
async fn current_space_is_sampled_separately_from_historical_collection_capacity() {
    use ivy_host_runtime::journal::HostJournal;
    let f = ServiceFixture::new(); let setup = fixture::setup(&f.0.join("space-status"), "basic").await;
    let clock = Arc::new(MovingClock(std::sync::atomic::AtomicI64::new(NOW)));
    let journal = HostJournal::open_with_clock(setup.config.clone(), clock.clone()).await.unwrap();
    let mut old = journal.storage_retention_status().await.unwrap().unwrap();
    old.insert("availableBytes", J::object([("runtime", J::Number(7.0)), ("artifacts", J::Number(7.0)), ("staging", J::Number(7.0))]));
    journal.save_storage_retention_status(old.clone()).await.unwrap();
    let first = journal.status(None).await.unwrap();
    assert_eq!(hash_json(first.required("storageRetention").unwrap()).unwrap(), hash_json(&old).unwrap());
    assert_ne!(first.required("storageSpace").unwrap().get("availableBytes"), old.get("availableBytes"));
    clock.0.fetch_add(1000, Ordering::SeqCst);
    assert_eq!(journal.status(None).await.unwrap().get("storageSpace"), first.get("storageSpace"));
    clock.0.fetch_add(60_000, Ordering::SeqCst);
    assert_ne!(journal.status(None).await.unwrap().required("storageSpace").unwrap().get("observedAt"), first.required("storageSpace").unwrap().get("observedAt"));
    journal.close().await.unwrap();
}

#[tokio::test]
async fn collection_budget_is_reported_and_remaining_retirements_resume() {
    use std::fs;
    let f = ServiceFixture::new(); let setup = fixture::setup(&f.0.join("budget"), "basic").await;
    let staging = native_path::to_native(setup.config.required("stagingRoot").unwrap().as_js_string().unwrap());
    for id in 1..=130 { let directory = staging.join(format!(".retiring-build-{}", uuid::Uuid::from_u128(id))); fs::create_dir(&directory).unwrap(); fs::write(directory.join("payload"), b"retired").unwrap(); }
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap();
    let observed = fixture::observe(&setup).await;
    let status = observed.required("status").unwrap(); assert_eq!(status.get("state"), Some(&"partial".into()));
    assert_eq!(status.required("removed").unwrap().required("preparations").unwrap(), &J::Number(128.0));
    storage_retention::collect_host_storage(setup.config.clone(), None, None, bindings(&setup, Arc::new(FixedClock))).await.unwrap();
    assert!(!fs::read_dir(staging).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".retiring-build-")));
}

#[tokio::test]
async fn known_successful_predecessor_replaces_the_nearest_version_fallback() {
    use ivy_host_runtime::journal::HostJournal;
    let f = ServiceFixture::new(); let setup = fixture::setup(&f.0.join("known-predecessor"), "basic").await;
    let journal = HostJournal::open(setup.config.clone()).await.unwrap();
    let mut entry = journal.accept(J::object([
        ("action", "deploy".into()), ("operationId", "successful-selection".into()), ("instanceId", "fixture".into()), ("candidateId", format!("sha256:{}", "3".repeat(64)).into())
    ]), None).await.unwrap();
    entry.insert("previousCandidateId", format!("sha256:{}", "1".repeat(64)).into());
    let mut record = entry.required("record").unwrap().clone(); record.insert("phase", "succeeded".into()); entry.insert("record", record);
    let runtime = native_path::to_native(setup.config.required("runtimeRoot").unwrap().as_js_string().unwrap());
    let db = ivy_store::rusqlite::Connection::open(runtime.join("deployments.sqlite")).unwrap();
    db.execute("UPDATE operations SET phase='succeeded',entry_json=? WHERE operation_id='successful-selection'", [ivy_contracts::json::stringify(&entry, usize::MAX).unwrap()]).unwrap(); drop(db);
    let protected = journal.retention_candidate_ids().await.unwrap();
    assert!(protected.contains(&format!("sha256:{}", "1".repeat(64))));
    assert!(!protected.contains(&format!("sha256:{}", "2".repeat(64))));
    journal.close().await.unwrap();
}
