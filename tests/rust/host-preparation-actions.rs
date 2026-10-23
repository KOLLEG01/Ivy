use super::{ServiceFixture, actions, assert_json, fixture_node, journal, node, save};
use ivy_contracts::{IvyJson as J, json};
use ivy_host_runtime::{
    config, executor_lock::ExecutorLock, node_path, portable_http::PortableHttp,
    prepare::PreparationRuntime, storage_retention::RetentionBindings,
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

fn retention(clock: Arc<dyn Clock>) -> Arc<RetentionBindings> {
    Arc::new(RetentionBindings {
        node: None,
        clock,
    })
}
fn closed(f: &ServiceFixture) {
    let db = ivy_store::rusqlite::Connection::open(f.0.join("state/deployments.sqlite")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA journal_mode=DELETE", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "delete"
    );
}
fn unrecognized(f: &ServiceFixture) -> PathBuf {
    let root =
        f.0.join("staging/build-00000000-0000-5000-8000-000000000001");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("payload"), b"retained-data").unwrap();
    root
}
#[tokio::test]
async fn inventory_and_compaction_keep_unrecognized_payload_and_close_request_journals() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    let root = unrecognized(&f);
    let inventory = actions.preparation_inventory(path.clone()).await.unwrap();
    closed(&f);
    let entries = inventory.required("entries").unwrap().as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].get("phase"), Some(&"unrecognized".into()));
    assert_eq!(entries[0].get("bytes"), Some(&J::Number(13.0)));
    let compacted = actions
        .compact_preparations(path, fixture_node("24.18.0"))
        .await
        .unwrap();
    closed(&f);
    let entry = &compacted.required("entries").unwrap().as_array().unwrap()[0];
    assert_eq!(entry.get("action"), Some(&"retained".into()));
    assert_eq!(entry.get("code"), Some(&"preparation_unrecognized".into()));
    assert_eq!(
        std::fs::read(root.join("payload")).unwrap(),
        b"retained-data"
    );
}
#[tokio::test]
async fn manual_collection_returns_durable_status_and_releases_owned_locks() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let mut host = super::host(&f);
    host.insert(
        "executables",
        J::object([(
            "native-host",
            J::String(node_path::from_native(&std::env::current_exe().unwrap())),
        )]),
    );
    config::atomic_json(&path, host).await.unwrap();
    let actions = actions(&f);
    let result = actions
        .collect_storage(path, retention(Arc::new(SystemClock)))
        .await
        .unwrap();
    closed(&f);
    let status = result.required("status").unwrap();
    assert_eq!(status.get("state"), Some(&"succeeded".into()));
    assert_eq!(status.get("trigger"), Some(&"manual".into()));
    let journal = journal(&f).await;
    assert_json(
        &journal.storage_retention_status().await.unwrap().unwrap(),
        status,
    );
    journal.close().await.unwrap();
    for name in ["artifact-retention-lock", "bootstrap-maintenance-lock"] {
        let lock = ExecutorLock::acquire(f.0.join("state").join(name), false)
            .await
            .unwrap();
        lock.close().await.unwrap();
    }
}
struct GateClock {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    first: AtomicBool,
}
impl Clock for GateClock {
    fn utc_millis(&self) -> i64 {
        if !self.first.swap(true, Ordering::SeqCst) {
            self.entered.store(true, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
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
async fn dropped_collection_observer_retains_publication_and_final_cleanup() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let guard = Release(release.clone());
    let bindings = retention(Arc::new(GateClock {
        entered: entered.clone(),
        release: release.clone(),
        first: AtomicBool::new(false),
    }));
    let observer = tokio::spawn(async move { actions.collect_storage(path, bindings).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    observer.abort();
    drop(guard);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let journal = journal(&f).await;
            let status = journal.storage_retention_status().await.unwrap();
            journal.close().await.unwrap();
            if status.is_some_and(|value| value.get("state") == Some(&"succeeded".into())) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        loop {
            let db = ivy_store::rusqlite::Connection::open(f.0.join("state/deployments.sqlite"))
                .unwrap();
            if db
                .query_row("PRAGMA journal_mode=DELETE", [], |row| {
                    row.get::<_, String>(0)
                })
                .is_ok_and(|v| v == "delete")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn prepare_captures_one_snapshot_then_preserves_missing_plan_error_and_cleanup() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    let checkout = f.0.join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    std::fs::write(
        checkout.join("package.json"),
        b"{\"name\":\"fixture\",\"version\":\"1.0.0\",\"private\":true}\n",
    )
    .unwrap();
    std::fs::write(
        checkout.join("package-lock.json"),
        b"{\"name\":\"fixture\",\"lockfileVersion\":3,\"packages\":{}}\n",
    )
    .unwrap();
    let git = std::env::var_os("IVY_TEST_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut command =
                std::process::Command::new(if cfg!(windows) { "where.exe" } else { "which" });
            command.arg("git");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let output = command.output().unwrap();
            assert!(output.status.success());
            PathBuf::from(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap(),
            )
        });
    let mut host = super::host(&f);
    let mut executables = host.required("executables").unwrap().clone();
    executables.insert("git", J::String(node_path::from_native(&git)));
    host.insert("executables", executables);
    config::atomic_json(&path, host).await.unwrap();
    for arguments in [
        &["init", "--quiet"][..],
        &["add", "package.json", "package-lock.json"][..],
    ] {
        let mut command = std::process::Command::new(&git);
        command.args(arguments).current_dir(&checkout);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        assert!(command.output().unwrap().status.success());
    }
    #[cfg(windows)]
    {
        let helpers = f.0.join("unused-current-package/dist/native");
        std::fs::create_dir_all(&helpers).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_ivy-job"), helpers.join("ivy-job.exe")).unwrap();
    }
    let runtime = PreparationRuntime {
        node: Some(ivy_host_runtime::node_runtime::NodeRuntime {
            executable: node(),
            version: "24.18.0".into(),
        }),
        compare: Arc::new(|_, _| panic!("No artifact inventory admitted")),
        http: PortableHttp::new(Default::default()).unwrap(),
    };
    let error = actions
        .prepare(
            path,
            "missing-component".into(),
            node_path::from_native(&checkout),
            runtime,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "not_found", "{error:?}");
    assert_eq!(error.message, "Source component has no deploy plan.");
    closed(&f);
    let snapshots = std::fs::read_dir(f.0.join("staging/sources"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(snapshots.len(), 1);
    let snapshot = config::json_file(snapshots[0].path().join("snapshot.json"), 1024 * 1024)
        .await
        .unwrap();
    let source = node_path::to_native(
        snapshot
            .required("snapshot")
            .unwrap()
            .required("sourceRoot")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    assert_eq!(
        std::fs::read(source.join("package.json")).unwrap(),
        std::fs::read(checkout.join("package.json")).unwrap()
    );
    assert!(
        !std::fs::read_dir(f.0.join("staging"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("incoming-"))
    );
    let journal = journal(&f).await;
    assert!(journal.list(100.0).await.unwrap().is_empty());
    journal.close().await.unwrap();
}
#[tokio::test]
#[ignore = "Requires an explicitly pinned original CLI and its dependency bounds"]
async fn original_inventory_and_compaction() {
    let f = ServiceFixture::new();
    let path = save(&f).await;
    let actions = actions(&f);
    let root = unrecognized(&f);
    let original = std::env::var_os("IVY_TEST_ORIGINAL_CLI")
        .map(PathBuf::from)
        .expect("Explicit original CLI required");
    for command in ["preparations", "compact-preparations"] {
        let output = tokio::process::Command::new(node())
            .arg(&original)
            .args([command, "--config"])
            .arg(&path)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = json::parse(&String::from_utf8(output.stdout).unwrap(), 1024 * 1024).unwrap();
        let mut expected = output.required("data").unwrap().clone();
        let mut actual = if command == "preparations" {
            actions.preparation_inventory(path.clone()).await.unwrap()
        } else {
            actions
                .compact_preparations(path.clone(), fixture_node("24.18.0"))
                .await
                .unwrap()
        };
        // Observation times and measured volume free space differ by read time.
        for value in [&mut actual, &mut expected] {
            assert!(
                value
                    .required("observedAt")
                    .unwrap()
                    .as_js_string()
                    .is_some()
            );
            value.remove("observedAt");
            if command == "preparations" {
                assert!(
                    value
                        .required("availableBytes")
                        .unwrap()
                        .as_f64()
                        .is_some_and(|v| v.is_finite() && v >= 0.0)
                );
                value.remove("availableBytes");
            }
        }
        assert_json(&actual, &expected);
        closed(&f);
    }
    assert_eq!(
        std::fs::read(root.join("payload")).unwrap(),
        b"retained-data"
    );
}
