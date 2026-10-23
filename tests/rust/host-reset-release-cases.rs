use super::*;
use crate::node_runtime::NodeRuntime;
use ivy_contracts::json;
use ivy_sdk::clock::parse_date;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Mutex,
    time::Instant,
};

const DATE: &str = "2026-10-20T14:00:00.000Z";
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
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn config(&self) -> J {
        J::object([
            ("schemaVersion", J::Number(1.0)),
            ("hostId", "fixture-host".into()),
            (
                "runtimeRoot",
                J::String(node_path::from_native(&self.0.join("state"))),
            ),
            (
                "artifactRoot",
                J::String(node_path::from_native(&self.0.join("artifacts"))),
            ),
            (
                "stagingRoot",
                J::String(node_path::from_native(&self.0.join("staging"))),
            ),
            ("publicBaseUrl", "http://127.0.0.1:39081/ivy".into()),
            (
                "executables",
                J::object([(
                    "node",
                    J::String(node_path::from_native(&PathBuf::from(
                        std::env::var_os("IVY_RESTORE_NODE").unwrap(),
                    ))),
                )]),
            ),
            (
                "instances",
                J::Array(vec![J::object([
                    ("instanceId", "fixture".into()),
                    ("serviceNodeId", "fixture-node".into()),
                    ("componentId", "fixture".into()),
                    ("engine", "process".into()),
                    ("enabled", true.into()),
                    ("settings", J::Object(vec![])),
                ])]),
            ),
        ])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn oracle(fixture: &Fixture, group: &str, case: &str, kind: &str, full: bool) -> J {
    let input = J::object([
        ("fixtureRoot", J::String(node_path::from_native(&fixture.0))),
        ("config", fixture.config()),
        ("group", group.into()),
        ("case", case.into()),
        ("kind", kind.into()),
        ("full", full.into()),
    ]);
    let mut child = Command::new(std::env::var_os("IVY_RESTORE_NODE").unwrap())
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
        ])
        .arg(root().join("tests/fixtures/host-reset-release-oracle.mjs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{group}/{case}/{kind}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn wrap(result: IvyResult<J>) -> J {
    match result {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
async fn native() -> (ResetHost, NodeRuntime) {
    let node = NodeRuntime::discover(Some(&PathBuf::from(
        std::env::var_os("IVY_RESTORE_NODE").expect("pinned Node workload"),
    )))
    .await
    .unwrap();
    let host = ResetHost::new(
        root(),
        PathBuf::from(std::env::var_os("IVY_BACKUP_JOB").expect("actual Job helper")),
        Arc::new(FixedClock),
    )
    .unwrap();
    (host, node)
}
fn equal(a: &J, b: &J) {
    assert_eq!(
        json::canonical(a, usize::MAX).unwrap(),
        json::canonical(b, usize::MAX).unwrap()
    );
}
fn retain(group: &str, case: &str, setup: J, actual: J, expected: J) {
    if let Some(path) = std::env::var_os("IVY_HOST_RESET_CORPUS") {
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
                    ("group", group.into()),
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
    equal(
        actual.get("outcome").unwrap(),
        expected.get("outcome").unwrap(),
    );
    equal(actual.get("state").unwrap(), expected.get("state").unwrap());
}
async fn compare(host: &ResetHost, node: &NodeRuntime, group: &str, case: &str, full: bool) -> J {
    let fixture = Fixture::new();
    let setup = oracle(&fixture, group, case, "setup", full);
    let config = setup.get("config").unwrap().clone();
    let args = setup.get("args").unwrap();
    let id = field(args, "resetId").unwrap();
    let epoch = field(args, "runtimeEpoch").unwrap();
    let result = match (group, full) {
        ("bootstrap", true) => {
            host.bootstrap(config.clone(), id, epoch, node.clone())
                .await
        }
        ("release", true) => host.release(config.clone(), id, epoch, node.clone()).await,
        ("bootstrap", false) => {
            host.0
                .clone()
                .bootstrap(config.clone(), id, epoch, Some(node.clone()))
                .await
        }
        ("release", false) => {
            host.0
                .clone()
                .release(config.clone(), id, epoch, Some(node.clone()))
                .await
        }
        ("resume", false) => {
            async {
                let first = host
                    .0
                    .clone()
                    .release(
                        config.clone(),
                        id.clone(),
                        epoch.clone(),
                        Some(node.clone()),
                    )
                    .await?;
                let held = prepare_reset_hold(
                    config.clone(),
                    setup.get("plan").unwrap().clone(),
                    id.clone(),
                    epoch.clone(),
                    field(args, "publisherDigest")?,
                    host.0.clock.clone(),
                )
                .await?;
                host.0.clone().stop(config.clone()).await?;
                let completed = complete_reset_record(
                    config.clone(),
                    id.clone(),
                    epoch.clone(),
                    host.0.clock.clone(),
                )
                .await?;
                let released = host
                    .0
                    .clone()
                    .release(
                        config.clone(),
                        id.clone(),
                        epoch.clone(),
                        Some(node.clone()),
                    )
                    .await?;
                let confirmed =
                    confirm_reset_record(config.clone(), id, epoch, host.0.clock.clone()).await?;
                Ok(J::object([
                    ("first", first),
                    ("held", held),
                    ("completed", completed),
                    ("released", released),
                    ("confirmed", confirmed),
                ]))
            }
            .await
        }
        _ => panic!("fixture scope"),
    };
    let actual = J::object([
        ("outcome", wrap(result)),
        ("state", oracle(&fixture, group, case, "observe", full)),
    ]);
    oracle(&fixture, group, case, "reset", full);
    let expected = oracle(&fixture, group, case, "run", full);
    let retained_case = if full {
        format!("{group}/{case}")
    } else {
        case.to_owned()
    };
    retain(
        if full { "full-admission" } else { group },
        &retained_case,
        setup,
        actual.clone(),
        expected,
    );
    actual
}

#[tokio::test]
async fn bootstrap_identity_hold_and_saved_selection_match_original_private_phase() {
    let (host, node) = native().await;
    for case in [
        "no-hive",
        "no-enabled",
        "disabled-hive",
        "process-hive",
        "wrong-reset",
        "wrong-epoch",
        "prepared",
        "cleared",
        "released",
        "missing-hold",
        "hold-directory",
        "missing-record",
        "null-record",
        "malformed-record",
        "saved-null",
        "saved-entry-null",
    ] {
        let value = compare(&host, &node, "bootstrap", case, false).await;
        if [
            "no-hive",
            "no-enabled",
            "disabled-hive",
            "process-hive",
            "hold-directory",
        ]
        .contains(&case)
        {
            assert_eq!(value.get("outcome").unwrap().get("ok"), Some(&true.into()));
        }
    }
}
#[tokio::test]
async fn bootstrap_uses_real_journal_configuration_and_native_docker_rejection() {
    let (host, node) = native().await;
    for case in [
        "hive-missing-target",
        "hive-stopped-target",
        "hive-missing-runtime",
        "hive-build-mismatch",
        "hive-unsupported",
    ] {
        let value = compare(&host, &node, "bootstrap", case, false).await;
        assert_eq!(value.get("outcome").unwrap().get("ok"), Some(&false.into()));
    }
}
#[cfg(windows)]
#[tokio::test]
async fn release_idempotence_and_ordinary_process_selection_preserve_original_bytes() {
    let (host, node) = native().await;
    for case in [
        "no-enabled",
        "ordinary-process",
        "disabled-config",
        "disabled-hive",
        "already-released",
        "released-bad-saved",
        "wrong-reset",
        "wrong-epoch",
        "prepared",
        "cleared",
        "missing-record",
        "null-record",
        "malformed-record",
        "saved-null",
        "saved-entry-null",
        "deployment-complete",
        "missing-hold",
    ] {
        let value = compare(&host, &node, "release", case, false).await;
        if [
            "no-enabled",
            "ordinary-process",
            "disabled-config",
            "disabled-hive",
            "already-released",
            "released-bad-saved",
            "deployment-complete",
            "missing-hold",
        ]
        .contains(&case)
        {
            assert_eq!(value.get("outcome").unwrap().get("ok"), Some(&true.into()));
        }
    }
}
#[cfg(windows)]
#[tokio::test]
async fn release_errors_recreate_the_original_hold_even_when_marker_recovery_fails() {
    let (host, node) = native().await;
    for case in [
        "marker-directory",
        "docker-missing-target",
        "docker-stopped-target",
        "docker-invalid-runtime",
        "docker-unsupported",
        "docker-truthy-enabled",
        "hive-missing-target",
        "hive-stopped-target",
        "hive-missing-runtime",
        "hive-build-mismatch",
        "hive-unsupported",
    ] {
        let value = compare(&host, &node, "release", case, false).await;
        assert_eq!(value.get("outcome").unwrap().get("ok"), Some(&false.into()));
        let state = value.get("state").unwrap();
        assert!(!state.get("record").unwrap().is_null());
        assert!(!state.get("marker").unwrap().is_null());
    }
}
#[cfg(windows)]
#[tokio::test]
async fn release_reprepare_complete_release_confirm_preserves_one_identity() {
    let (host, node) = native().await;
    let value = compare(&host, &node, "resume", "ordinary-process", false).await;
    assert_eq!(value.get("outcome").unwrap().get("ok"), Some(&true.into()));
    assert_eq!(value.get("state").unwrap().get("marker"), Some(&J::Null));
}
#[tokio::test]
async fn new_full_entrypoints_use_actual_administrator_admission_before_writes() {
    let (host, node) = native().await;
    for group in ["bootstrap", "release"] {
        compare(&host, &node, group, "no-enabled", true).await;
    }
}

#[tokio::test]
async fn journal_lookup_preserves_utf16_identity_through_the_existing_sqlite_boundary() {
    let (host, node) = native().await;
    for case in [
        "hive-unsupported",
        "hive-unicode-target",
        "hive-surrogate-target",
    ] {
        let value = compare(&host, &node, "bootstrap", case, false).await;
        assert_eq!(
            value
                .get("outcome")
                .unwrap()
                .get("error")
                .unwrap()
                .get("data")
                .unwrap()
                .get("code"),
            Some(&"unsupported_runtime".into())
        );
    }
}
