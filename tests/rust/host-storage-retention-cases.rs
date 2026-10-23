use super::ServiceFixture;
use ivy_contracts::{IvyJson as J, core_schema::validate_host, hash::hash_json};
use ivy_host_runtime::{
    executor_lock::ExecutorLock,
    node_path,
    node_runtime::NodeRuntime,
    storage_retention::{self, RetentionBindings},
};
use ivy_sdk::clock::Clock;
use std::{
    path::{Path, PathBuf},
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
    J::String(node_path::from_native(path))
}
fn bindings(setup: &fixture::Setup, clock: Arc<dyn Clock>) -> Arc<RetentionBindings> {
    Arc::new(RetentionBindings {
        node: setup
            .config
            .required("executables")
            .unwrap()
            .get("node")
            .map(|executable| NodeRuntime {
                executable: node_path::to_native(executable.as_js_string().unwrap()),
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
            (None, 3, "partial")
        }
        "bootstrap-scalar-time" => (Some("internal_error"), 6, "failed"),
        "bootstrap-order" => (None, 4, "succeeded"),
        "maintenance-ring" => (None, 3, "succeeded"),
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
        | "bootstrap-null-time"
        | "maintenance-ring" => &[1, 3, 4],
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
        let runtime = node_path::to_native(
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
async fn current_rollback_and_unresolved_bootstrap_plans_are_retained() {
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
async fn bootstrap_plan_cover_preserves_referenced_releases() {
    let observed = run_case("bootstrap-order").await;
    let plans = observed
        .required("state")
        .unwrap()
        .required("plans")
        .unwrap();
    assert!(plans.as_array().unwrap().contains(&"plan-7".into()));
    assert!(!plans.as_array().unwrap().contains(&"plan-8".into()));
}
#[tokio::test]
async fn maintenance_details_are_pruned_without_changing_their_replay_records() {
    let observed = run_case("maintenance-ring").await;
    let details = observed
        .required("state")
        .unwrap()
        .required("details")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(details.len(), 25);
    assert!(
        details
            .iter()
            .all(|d| d.get("recordUnchanged") == Some(&true.into()))
    );
    for number in 0..25 {
        let directory = hash_json(&J::from(format!("old-maintenance-{number}"))).unwrap();
        let detail = details
            .iter()
            .find(|detail| detail.get("name") == Some(&directory[7..].into()))
            .unwrap();
        let files: &[&str] = if (1..5).contains(&number) {
            &["record.json"]
        } else {
            &[
                "host-executor.desired.json",
                "host-executor.prior.json",
                "record.json",
            ]
        };
        assert_eq!(
            detail.required("files").unwrap(),
            &J::Array(files.iter().map(|file| J::from(*file)).collect()),
            "Maintenance {number}: retain the newest 20 and the rollback record"
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
    let runtime = PathBuf::from(config.required("runtimeRoot").unwrap().as_str().unwrap());
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
