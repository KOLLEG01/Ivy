#[allow(dead_code)]
#[path = "host-service-fixture.rs"]
mod service_fixture;
#[path = "host-preparation-actions.rs"]
mod preparation_cases;
#[path = "host-backup-actions.rs"]
mod backup_cases;
#[path = "host-reset-actions.rs"]
mod reset_cases;
#[path = "host-bootstrap-actions.rs"]
mod bootstrap_cases;
use ivy_contracts::{IvyJson as J, JsString, core_schema::validate_host, json};
use ivy_host_runtime::{
    actions::{Action, LocalAction},
    config,
    host_actions::{HostActions, ProgressSink},
    journal::HostJournal,
    node_path,
};
use ivy_sdk::clock::{Clock, SystemClock};
use service_fixture::{ServiceFixture, fixture_node, host_configuration};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn host(f: &ServiceFixture) -> J {
    let temp = std::env::temp_dir().canonicalize().unwrap();
    assert!(f.0.canonicalize().unwrap().starts_with(temp));
    let mut host = host_configuration(f);
    host.insert(
        "executables",
        J::object([("node", J::String(node_path::from_native(&node())))]),
    );
    host
}
fn node() -> PathBuf {
    std::env::var_os("IVY_TEST_NODE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let output = std::process::Command::new("node")
                .args(["-p", "process.execPath"])
                .output()
                .unwrap();
            assert!(output.status.success());
            PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        })
}
fn actions(f: &ServiceFixture) -> HostActions {
    HostActions::native(
        fixture_node("24.18.0"),
        Arc::new(|_, _| panic!("No artifact comparison admitted")),
        f.0.join("unused-current-package"),
        f.0.join("unused-ivy-job"),
    )
    .unwrap()
}
fn action(kind: Action, operation: &str) -> LocalAction {
    LocalAction {
        action: kind,
        instance_id: "fixture".into(),
        operation_id: operation.into(),
        candidate_id: None,
        source: None,
        target_build: None,
    }
}
async fn save(f: &ServiceFixture) -> PathBuf {
    let path = f.0.join("host.json");
    config::atomic_json(&path, host(f)).await.unwrap();
    path
}
async fn journal(f: &ServiceFixture) -> HostJournal {
    HostJournal::open(
        ivy_host_runtime::host_config::host_config(f.0.join("host.json"))
            .await
            .unwrap(),
    )
    .await
    .unwrap()
}
async fn seed(f: &ServiceFixture) {
    // Reuse the candidate/manifest shape of the existing journal/action cases.
    // No artifact verification or activation is admitted by these local calls.
    let hash = || J::from(format!("sha256:{}", "a".repeat(64)));
    let candidate = J::object([
        ("candidateId", hash()),
        ("componentId", "fixture".into()),
        (
            "artifactRoot",
            J::String(node_path::from_native(&f.0.join("unused-artifact"))),
        ),
        (
            "manifestPath",
            J::String(node_path::from_native(&f.0.join("unused-manifest.json"))),
        ),
        ("createdAt", "2026-01-01T00:00:00.000Z".into()),
        (
            "platform",
            J::object([
                ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                ("arch", "x64".into()),
                ("node", "24.18.0".into()),
            ]),
        ),
        ("buildId", hash()),
    ]);
    let manifest = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("componentId", "fixture".into()),
        ("kind", "app".into()),
        ("version", "1.0.0".into()),
        ("buildId", hash()),
        ("connectsToHive", false.into()),
        (
            "requirements",
            J::object([
                ("node", ">=24.18.0 <25.0.0".into()),
                ("hiveProtocol", J::Null),
                ("contracts", J::Array(vec![])),
            ]),
        ),
        (
            "app",
            J::object([
                ("appId", "fixture".into()),
                ("dist", "dist/apps/fixture".into()),
                ("entryPath", "index.html".into()),
            ]),
        ),
    ]);
    let journal = journal(f).await;
    journal.save_candidate(candidate, manifest).await.unwrap();
    journal
        .set_installed(J::object([
            ("instanceId", "fixture".into()),
            ("candidateId", hash()),
            ("buildId", hash()),
            ("enabled", true.into()),
            ("installedAt", "2026-01-01T00:00:00.000Z".into()),
        ]))
        .await
        .unwrap();
    journal.close().await.unwrap();
}
fn id(entry: &J) -> JsString {
    entry
        .required("record")
        .unwrap()
        .required("deploymentId")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone()
}
fn assert_json(actual: &J, expected: &J) {
    assert_eq!(
        json::canonical(actual, usize::MAX).unwrap(),
        json::canonical(expected, usize::MAX).unwrap()
    );
}

#[tokio::test]
async fn native_lifecycle_replays_one_acceptance_and_reloads_configuration() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    seed(&f).await;
    let actions = actions(&f);
    let first = actions
        .lifecycle(
            path.clone(),
            action(Action::Disable, "facade-replay"),
            0.0,
            None,
        )
        .await;
    let first_id = first.deployment_id.unwrap();
    let entry = first.entry.unwrap();
    assert_eq!(first_id, id(&entry));
    let replay = actions
        .lifecycle(
            path.clone(),
            action(Action::Disable, "facade-replay"),
            0.0,
            None,
        )
        .await;
    assert_eq!(replay.deployment_id, Some(first_id.clone()));
    assert_json(&replay.entry.unwrap(), &entry);
    assert_json(
        &actions
            .deployment(path.clone(), first_id.clone())
            .await
            .unwrap(),
        &entry,
    );
    let conflict = actions
        .lifecycle(
            path.clone(),
            action(Action::Enable, "facade-replay"),
            0.0,
            None,
        )
        .await;
    assert_eq!(conflict.entry.unwrap_err().code, "mutation_conflict");
    assert!(conflict.deployment_id.is_none());
    let journal = journal(&f).await;
    assert_eq!(journal.list(100.0).await.unwrap().len(), 1);
    journal.close().await.unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(actions.status(path).await.is_err());
    // No request journal is retained by the reusable facade after completion.
    let db = ivy_store::rusqlite::Connection::open(f.0.join("state/deployments.sqlite")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA journal_mode=DELETE", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "delete"
    );
}

#[tokio::test]
async fn native_status_keeps_cli_shape_and_rejects_invalid_or_stale_executor_evidence() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    let status = actions.status(path.clone()).await.unwrap();
    assert_eq!(
        status,
        J::object([
            ("hostId", "fixture-host".into()),
            ("executor", J::Null),
            (
                "instances",
                J::Array(vec![J::object([
                    ("instanceId", "fixture".into()),
                    ("componentId", "fixture".into()),
                    ("enabled", true.into()),
                    ("installed", J::Null)
                ])])
            ),
            ("observations", J::object([] as [(&str, J); 0])),
            ("unfinished", J::Array(vec![])),
            ("restoredFrom", J::Null)
        ])
    );
    let at = chrono::DateTime::from_timestamp_millis(SystemClock.utc_millis())
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let executor = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("hostId", "fixture-host".into()),
        ("pid", J::Number(1.0)),
        ("bootId", "fixture-boot".into()),
        ("buildId", J::Null),
        ("observedAt", at.into()),
        ("state", "ready".into()),
        ("activeDeploymentId", J::Null),
        ("code", J::Null),
    ]);
    validate_host("ExecutorStatus", &executor).unwrap();
    for (key, value) in [
        ("hostId", "other-host".into()),
        ("observedAt", "2000-01-01T00:00:00.000Z".into()),
        ("pid", J::Number(0.0)),
    ] {
        let mut changed = executor.clone();
        changed.insert(key, value);
        config::atomic_json(f.0.join("state/executor.json"), changed)
            .await
            .unwrap();
        assert_eq!(
            actions.status(path.clone()).await.unwrap().get("executor"),
            Some(&J::Null)
        );
    }
    config::atomic_json(f.0.join("state/executor.json"), executor.clone())
        .await
        .unwrap();
    assert_json(
        actions.status(path).await.unwrap().get("executor").unwrap(),
        &executor,
    );
}

#[tokio::test]
async fn wait_read_failure_retains_accepted_identity_and_closes_its_journal() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    seed(&f).await;
    let actions = actions(&f);
    let saved = Arc::new(Mutex::new(None::<String>));
    let saved_clone = saved.clone();
    let database = f.0.join("state/deployments.sqlite");
    let database_clone = database.clone();
    let sink: ProgressSink = Arc::new(move |_| {
        let db = ivy_store::rusqlite::Connection::open(&database_clone).unwrap();
        let original = db
            .query_row(
                "SELECT entry_json FROM operations WHERE operation_id='facade-corrupt-read'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        *saved_clone.lock().unwrap() = Some(original);
        db.execute(
            "UPDATE operations SET entry_json='{}' WHERE operation_id='facade-corrupt-read'",
            [],
        )
        .unwrap();
    });
    let result = actions
        .lifecycle(
            path,
            action(Action::Disable, "facade-corrupt-read"),
            500.0,
            Some(sink),
        )
        .await;
    assert!(result.entry.is_err());
    assert!(result.deployment_id.is_some());
    let db = ivy_store::rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        db.query_row("PRAGMA journal_mode=DELETE", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    db.execute(
        "UPDATE operations SET entry_json=? WHERE operation_id='facade-corrupt-read'",
        [saved.lock().unwrap().take().unwrap()],
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_observer_retains_admitted_wait_and_display_failure_is_nonfatal() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    seed(&f).await;
    let actions = actions(&f);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let messages = Arc::new(Mutex::new(Vec::new()));
    let (entered_sink, release_sink, messages_sink) =
        (entered.clone(), release.clone(), messages.clone());
    let sink: ProgressSink = Arc::new(move |message| {
        messages_sink.lock().unwrap().push(message.clone());
        entered_sink.store(true, Ordering::SeqCst);
        while !release_sink.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("Intentional display-only failure");
    });
    let observer = tokio::spawn(async move {
        actions
            .lifecycle(
                path,
                action(Action::Disable, "facade-dropped-observer"),
                300.0,
                Some(sink),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    observer.abort();
    release.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let db = ivy_store::rusqlite::Connection::open(f.0.join("state/deployments.sqlite"))
                .unwrap();
            if db
                .query_row("PRAGMA journal_mode=DELETE", [], |row| {
                    row.get::<_, String>(0)
                })
                .is_ok_and(|mode| mode == "delete")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(messages.lock().unwrap().len(), 1);
    let journal = journal(&f).await;
    let entry = journal
        .operation("facade-dropped-observer")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        entry.required("record").unwrap().get("phase"),
        Some(&"prepared".into())
    );
    journal.close().await.unwrap();
}

#[tokio::test]
async fn native_maintenance_binding_and_invalid_wait_refuse_before_unrelated_effects() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    let error = actions
        .bootstrap_status(path, "missing-maintenance".into())
        .await
        .unwrap_err();
    assert_eq!(error.code, "not_found");
    assert!(
        actions
            .bootstrap()
            .inspect(f.0.join("absent-config.json"))
            .await
            .is_err()
    );
    for wait in [f64::NAN, -1.0, 0.5, 600_001.0] {
        let result = actions
            .lifecycle(
                f.0.join("absent-config.json"),
                action(Action::Disable, "never-admitted"),
                wait,
                None,
            )
            .await;
        assert_eq!(result.entry.unwrap_err().code, "invalid_arguments");
        assert!(result.deployment_id.is_none());
    }
    assert!(!f.0.join("state/deployments.sqlite").exists());
}

#[tokio::test]
#[ignore = "Requires an explicitly pinned original CLI and its dependency closure"]
async fn original_cli_roundtrip() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    seed(&f).await;
    let actions = actions(&f);
    let original = std::env::var_os("IVY_TEST_ORIGINAL_CLI")
        .map(PathBuf::from)
        .expect("Explicit original program required");
    let original_call = |arguments: Vec<std::ffi::OsString>| {
        let original = original.clone();
        async move {
            let output = tokio::process::Command::new(node())
                .arg(original)
                .args(arguments)
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            json::parse(&String::from_utf8(output.stdout).unwrap(), 1024 * 1024).unwrap()
        }
    };
    let original_status = original_call(vec![
        "status".into(),
        "--config".into(),
        path.clone().into_os_string(),
    ])
    .await;
    assert_json(
        &actions.status(path.clone()).await.unwrap(),
        original_status.required("data").unwrap(),
    );
    let accepted = original_call(vec![
        "disable".into(),
        "--instance".into(),
        "fixture".into(),
        "--operation-id".into(),
        "facade-original-roundtrip".into(),
        "--config".into(),
        path.clone().into_os_string(),
    ])
    .await;
    let replay = actions
        .lifecycle(
            path.clone(),
            action(Action::Disable, "facade-original-roundtrip"),
            0.0,
            None,
        )
        .await;
    assert_json(&replay.entry.unwrap(), accepted.required("data").unwrap());
    assert_eq!(
        replay.deployment_id.as_ref().map(|v| J::String(v.clone())),
        accepted.get("deploymentId").cloned()
    );
    let observed = original_call(vec![
        "status".into(),
        "--deployment".into(),
        replay
            .deployment_id
            .clone()
            .unwrap()
            .to_utf8()
            .unwrap()
            .into(),
        "--config".into(),
        path.clone().into_os_string(),
    ])
    .await;
    assert_json(
        &actions
            .deployment(path.clone(), replay.deployment_id.unwrap())
            .await
            .unwrap(),
        observed.required("data").unwrap(),
    );
    let original_after = original_call(vec![
        "status".into(),
        "--config".into(),
        path.clone().into_os_string(),
    ])
    .await;
    assert_json(
        &actions.status(path).await.unwrap(),
        original_after.required("data").unwrap(),
    );
}
