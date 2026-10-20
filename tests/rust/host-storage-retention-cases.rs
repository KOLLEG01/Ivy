use super::{ServiceFixture, assert_json_eq};
use ivy_contracts::{IvyJson as J, json};
use ivy_host_runtime::{
    artifact::FileCompare,
    executor_lock::ExecutorLock,
    node_path,
    node_runtime::NodeRuntime,
    storage_retention::{self, RetentionBindings},
};
use ivy_sdk::clock::Clock;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
const NOW: i64 = 1_792_483_200_000;
static CORPUS: Mutex<()> = Mutex::new(());
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
fn oracle(f: &ServiceFixture, root: &Path, case: &str, kind: &str) -> J {
    let request = J::object([
        ("fixtureRoot", text(&f.0)),
        ("root", text(root)),
        ("case", case.into()),
        ("kind", kind.into()),
        ("clock", J::Number(NOW as f64)),
    ]);
    let mut child =
        Command::new(std::env::var_os("IVY_RETENTION_NODE").unwrap_or_else(|| "node".into()))
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/host-storage-retention-oracle.mjs",
            ])
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(&request, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "oracle {case}/{kind}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    json::parse(std::str::from_utf8(&result.stdout).unwrap(), usize::MAX).unwrap()
}
fn retain(case: &str, actual: J, expected: J, raw: J) {
    if let Some(path) = std::env::var_os("IVY_HOST_RETENTION_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(
            file,
            "{}",
            json::canonical(
                &J::object([
                    ("case", case.into()),
                    ("actual", actual),
                    ("expected", expected),
                    ("raw", raw)
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
}
fn normalized(value: &J, root: &Path) -> J {
    match value {
        J::String(s) => J::from(
            s.to_utf8()
                .unwrap()
                .replace(&root.to_string_lossy().to_string(), "$FIXTURE"),
        ),
        J::Array(values) => J::Array(values.iter().map(|v| normalized(v, root)).collect()),
        J::Object(entries) => J::Object(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), normalized(v, root)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn normalized_observation(observation: &J, root: &Path) -> J {
    let mut result = normalized(observation, root);
    let mut status = result.required("status").unwrap().clone();
    let spaces = status
        .required("availableBytes")
        .unwrap()
        .as_object()
        .unwrap();
    let measured = J::Object(
        spaces
            .iter()
            .map(|(k, v)| {
                let n = v.as_f64().unwrap();
                assert!(n.is_finite() && n >= 0.0 && n <= 9_007_199_254_740_991.0);
                (k.clone(), true.into())
            })
            .collect(),
    );
    status.insert("availableBytes", measured);
    result.insert("status", status);
    result
}
fn bindings(setup: &J, clock: Arc<dyn Clock>) -> Arc<RetentionBindings> {
    let order = setup
        .required("localeOrder")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_js_string().unwrap().clone())
        .collect::<Vec<_>>();
    let compare: FileCompare = Arc::new(move |a, b| {
        let a = order
            .iter()
            .position(|v| v == a)
            .expect("unqualified fixture date");
        let b = order
            .iter()
            .position(|v| v == b)
            .expect("unqualified fixture date");
        a.cmp(&b)
    });
    Arc::new(RetentionBindings {
        node: NodeRuntime {
            executable: std::env::var_os("IVY_RETENTION_NODE").unwrap().into(),
            version: "24.18.0".into(),
        },
        compare,
        clock,
    })
}
async fn compare_case(case: &str) -> J {
    let f = ServiceFixture::new();
    let native = f.0.join("native");
    let original = f.0.join("original");
    let setup = oracle(&f, &native, case, "setup");
    let original_setup = oracle(&f, &original, case, "setup");
    assert_eq!(
        setup.get("fixtureSourceHash"),
        original_setup.get("fixtureSourceHash")
    );
    let held = match setup.get("held").filter(|v| !v.is_null()) {
        Some(path) => Some(
            ExecutorLock::acquire(PathBuf::from(path.as_str().unwrap()), false)
                .await
                .unwrap(),
        ),
        None => None,
    };
    let result = storage_retention::collect_host_storage(
        setup.required("config").unwrap().clone(),
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
    let expected = oracle(&f, &original, case, "run");
    let actual_observation = oracle(&f, &native, case, "observe");
    let expected_observation = oracle(&f, &original, case, "observe");
    let actual_compare = J::object([
        ("outcome", normalized(&actual, &native)),
        (
            "state",
            normalized_observation(&actual_observation, &native),
        ),
    ]);
    let expected_compare = J::object([
        ("outcome", normalized(&expected, &original)),
        (
            "state",
            normalized_observation(&expected_observation, &original),
        ),
    ]);
    retain(
        case,
        actual_compare.clone(),
        expected_compare.clone(),
        J::object([
            ("native", actual_observation.clone()),
            ("original", expected_observation),
            (
                "fixtureSourceHash",
                setup.required("fixtureSourceHash").unwrap().clone(),
            ),
        ]),
    );
    assert_json_eq(&actual_compare, &expected_compare);
    // Both collection locks are available after every terminal outcome.
    for name in ["artifact-retention-lock", "bootstrap-maintenance-lock"] {
        let runtime = PathBuf::from(
            setup
                .required("config")
                .unwrap()
                .required("runtimeRoot")
                .unwrap()
                .as_str()
                .unwrap(),
        );
        let lock = ExecutorLock::acquire(runtime.join(name), false)
            .await
            .unwrap();
        lock.close().await.unwrap();
    }
    J::object([("outcome", actual), ("state", actual_observation)])
}
#[tokio::test]
async fn live_references_and_completed_preparation_cleanup_match_original() {
    let observed = compare_case("basic").await;
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
    let result = compare_case("unknown-preparation").await;
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
        compare_case(case).await;
    }
}
#[tokio::test]
async fn linked_retirement_directories_preserve_their_targets() {
    let observed = compare_case("retired-link").await;
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
        compare_case(case).await;
    }
}
#[tokio::test]
async fn unknown_version_and_history_evidence_preserve_the_original_failure_boundaries() {
    for case in ["unknown-version", "completed-unknown", "unresolved-unknown"] {
        compare_case(case).await;
    }
}
#[tokio::test]
async fn current_rollback_and_unresolved_bootstrap_plans_are_retained() {
    for case in ["bootstrap-current", "bootstrap-prior"] {
        compare_case(case).await;
    }
}
#[cfg(windows)]
#[tokio::test]
async fn bootstrap_plan_cover_selection_preserves_journal_insertion_order() {
    let observed = compare_case("bootstrap-order").await;
    let plans = observed
        .required("state")
        .unwrap()
        .required("plans")
        .unwrap();
    assert!(plans.as_array().unwrap().contains(&"plan-7".into()));
    assert!(!plans.as_array().unwrap().contains(&"plan-8".into()));
    let f = ServiceFixture::new();
    let root = f.0.join("sorted-control");
    oracle(&f, &root, "bootstrap-order", "setup");
    let outcome = oracle(&f, &root, "bootstrap-order", "run-sorted-control");
    assert_eq!(outcome.get("ok"), Some(&true.into()));
    let sorted = oracle(&f, &root, "bootstrap-order", "observe");
    let sorted_plans = sorted.required("plans").unwrap();
    assert!(sorted_plans.as_array().unwrap().contains(&"plan-8".into()));
    assert_ne!(plans, sorted_plans);
    retain("sorted-set negative control",J::object([("originalOrder",plans.clone()),("sortedOrder",sorted_plans.clone())]),J::Null,J::object([("control","Original collector with only its journal Set iteration sorted, emulating the prior Rust boundary".into())]));
}
#[tokio::test]
async fn maintenance_details_are_pruned_without_changing_their_replay_records() {
    let observed = compare_case("maintenance-ring").await;
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
    assert!(
        details
            .iter()
            .any(|d| d.required("files").unwrap().as_array().unwrap().len() == 1)
    );
}
#[tokio::test]
async fn distinct_skip_reasons_are_coalesced_at_the_original_status_bound() {
    let observed = compare_case("reason-cap").await;
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
    let setup = oracle(&f, &root, "retired-staging", "setup");
    let gate = Arc::new(GateClock {
        calls: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        released: Mutex::new(false),
        wake: Condvar::new(),
    });
    let _release_on_exit = ReleaseGate(gate.clone());
    let config = setup.required("config").unwrap().clone();
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
    let observed = oracle(&f, &root, "retired-staging", "observe");
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
    retain(
        "admitted lifetime",
        observed,
        J::Null,
        J::object([
            ("observerCancelled", true.into()),
            ("ownerHeldUntilPublication", true.into()),
        ]),
    );
}
