use super::{ServiceFixture, assert_json_eq, host_configuration};
use ivy_contracts::{
    IvyError, IvyJson as J, IvyResult, JsString, core_schema::validate_host, hash::hash_json, json,
};
use ivy_host_runtime::{
    artifact::FileCompare,
    backup_files, config,
    executor_lock::ExecutorLock,
    host_config,
    layout::{self, StorageArea, StorageContext},
    node_path::{self, Context, Style},
    private_backup::{self, BackupBindings, NativeBackupPaths},
    project_storage, streaming_hash,
};
use ivy_sdk::clock::SystemClock;
use ivy_store::rusqlite::Connection;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
static CORPUS: Mutex<()> = Mutex::new(());
fn jp(p: &Path) -> J {
    J::String(node_path::from_native(p))
}
fn v(s: &str) -> J {
    json::parse(s, usize::MAX).unwrap()
}
fn wrapped(value: IvyResult<J>) -> J {
    match value {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn retain(case: &str, input: J, actual: J, expected: J) {
    if let Some(p) = std::env::var_os("IVY_HOST_BACKUP_CORPUS") {
        let _g = CORPUS.lock().unwrap();
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap();
        writeln!(
            f,
            "{}",
            json::canonical(
                &J::object([
                    ("case", case.into()),
                    ("input", input),
                    ("actual", actual),
                    ("expected", expected)
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
}
fn oracle(input: &J) -> J {
    let mut c = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-private-backup-oracle.mjs",
        ])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin
        .take()
        .unwrap()
        .write_all(json::stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    json::parse(std::str::from_utf8(&o.stdout).unwrap(), usize::MAX).unwrap()
}
fn compare(case: &str, input: J, result: IvyResult<J>) {
    let actual = wrapped(result);
    let expected = oracle(&input);
    retain(case, input, actual.clone(), expected.clone());
    assert_json_eq(&actual, &expected);
}
fn comparator(order: &[J]) -> FileCompare {
    let order = order
        .iter()
        .map(|v| v.as_js_string().unwrap().clone())
        .collect::<Vec<_>>();
    Arc::new(move |a, b| {
        order
            .iter()
            .position(|v| v == a)
            .unwrap()
            .cmp(&order.iter().position(|v| v == b).unwrap())
    })
}
fn context(root: &Path) -> StorageContext {
    StorageContext {
        paths: Context::current().unwrap(),
        account_home: node_path::from_native(root),
        compare: Arc::new(|a, b| a.cmp(b)),
    }
}
fn request(f: &ServiceFixture, kind: &str) -> J {
    J::object([("fixtureRoot", jp(&f.0)), ("kind", kind.into())])
}
#[test]
fn storage_areas_and_members_preserve_original_locale_union_and_path_errors() {
    for (style, label, root) in [
        (Style::Windows, "win32", "C:\\fixture"),
        (Style::Posix, "posix", "/fixture"),
    ] {
        let names = vec!["z", "A", "a", "ä", "é", "😀"]
            .into_iter()
            .map(J::from)
            .collect::<Vec<_>>();
        let base = JsString::from(root);
        let home = node_path::join(style, &[base.clone(), "account".into()]);
        let state = node_path::join(style, &[base.clone(), "state".into()]);
        let mut instances = Vec::new();
        for id in &names {
            instances.push(J::object([
                ("instanceId", id.clone()),
                ("componentId", "extension".into()),
                ("settings", v("{}")),
                (
                    "paths",
                    J::object([
                        (
                            "data",
                            J::String(node_path::join(
                                style,
                                &[base.clone(), id.as_js_string().unwrap().clone()],
                            )),
                        ),
                        ("work", J::String(state.clone())),
                        ("logs", J::String(state.clone())),
                    ]),
                ),
            ]));
        }
        let config = J::object([
            ("runtimeRoot", J::String(state)),
            ("instances", J::Array(instances)),
        ]);
        let mut cases = vec![J::object([
            ("operation", "hostStorageAreas".into()),
            ("config", config.clone()),
        ])];
        for project in [
            J::Null,
            "relative".into(),
            J::String(node_path::join(style, &[home.clone(), "internal".into()])),
        ] {
            let agent = J::object([
                ("instanceId", "a".into()),
                ("componentId", "agent-manager".into()),
                ("settings", J::object([("internalProjectRoot", project)])),
            ]);
            let c = J::object([
                (
                    "runtimeRoot",
                    J::String(node_path::join(style, &[base.clone(), "state".into()])),
                ),
                (
                    "servicesRoot",
                    J::String(node_path::join(style, &[base.clone(), "services".into()])),
                ),
                ("instances", J::Array(vec![agent])),
            ]);
            cases.push(J::object([
                ("operation", "hostStorageAreas".into()),
                ("config", c),
            ]));
        }
        for member in [
            "state/",
            "state/child",
            "state/../escape",
            "state/a/../../escape",
            "state/a\\..\\escape",
            "state/..hidden",
            "state/./ok",
            "unknown/x",
            "services/a/data/child",
        ] {
            cases.push(J::object([
                ("operation", "storageFile".into()),
                ("config", config.clone()),
                ("args", J::Array(vec![member.into()])),
            ]));
        }
        for path in [
            root,
            &format!("{root}/state/child"),
            &format!("{root}/state/..hidden"),
            &format!("{root}/a/file"),
            &format!("{root}/outside"),
        ] {
            cases.push(J::object([
                ("operation", "storagePath".into()),
                ("config", config.clone()),
                ("args", J::Array(vec![path.into()])),
            ]));
        }
        let input = J::object([
            ("kind", "layout".into()),
            ("style", label.into()),
            ("home", J::String(home.clone())),
            ("cwd", J::String(base.clone())),
            ("names", J::Array(names)),
            ("cases", J::Array(cases.clone())),
        ]);
        let expected = oracle(&input);
        let context = StorageContext {
            paths: Context {
                style,
                cwd: base,
                drive_cwds: Default::default(),
            },
            account_home: home,
            compare: comparator(expected.get("localeOrder").unwrap().as_array().unwrap()),
        };
        for (i, c) in cases.iter().enumerate() {
            let config = c.get("config").unwrap();
            let result = match c.get("operation").unwrap().as_str().unwrap().as_str() {
                "hostStorageAreas" => layout::host_storage_areas(config, &context)
                    .map(|a| J::Array(a.iter().map(StorageArea::json).collect())),
                "storagePath" => layout::storage_path(
                    config,
                    c.get("args").unwrap().as_array().unwrap()[0]
                        .as_js_string()
                        .unwrap(),
                    &context,
                )
                .map(J::String),
                _ => layout::storage_file(
                    config,
                    c.get("args").unwrap().as_array().unwrap()[0]
                        .as_js_string()
                        .unwrap(),
                    None,
                    &context,
                )
                .map(J::String),
            };
            let actual = wrapped(result);
            let exp = expected.get("cases").unwrap().as_array().unwrap()[i].clone();
            retain(
                "storage lexical union",
                c.clone(),
                actual.clone(),
                exp.clone(),
            );
            assert_json_eq(&actual, &exp);
        }
    }
}
#[tokio::test]
async fn retained_projects_follow_cached_owned_inventory_and_restored_identity() {
    let f = ServiceFixture::new();
    let mut c = host_configuration(&f);
    let mut instances = c.get("instances").unwrap().as_array().unwrap().to_vec();
    instances[0].insert("componentId", "agent-manager".into());
    instances[0].insert(
        "settings",
        J::object([("internalProjectRoot", jp(&f.0.join("internal")))]),
    );
    c.insert("instances", J::Array(instances));
    let data = layout::service_paths(&c, &"fixture".into())
        .unwrap()
        .get("data")
        .unwrap()
        .as_str()
        .unwrap();
    fs::create_dir_all(&data).unwrap();
    let cache = PathBuf::from(data).join("project-backup-paths.json");
    let ctx = context(&f.0);
    for paths in [
        J::Null,
        v("[\"relative\"]"),
        J::Array(vec![
            jp(&f.0.join("state/owned")),
            jp(&f.0.join("old-internal")),
            jp(&f.0.join("old-internal/child")),
        ]),
        J::Array(vec![]),
    ] {
        config::atomic_json(&cache, paths).await.unwrap();
        let mut input = request(&f, "project");
        input.insert("config", c.clone());
        compare(
            "cached projects",
            input,
            project_storage::project_storage_areas(&c, &ctx)
                .await
                .map(|a| J::Array(a.iter().map(StorageArea::json).collect())),
        );
    }
    let mut areas = layout::host_storage_areas(&c, &ctx).unwrap();
    let old = node_path::from_native(&f.0.join("old"));
    areas.push(StorageArea {
        key: format!(
            "retained-projects/fixture/{}",
            &hash_json(&J::String(old.clone())).unwrap()[7..]
        )
        .into(),
        path: old,
    });
    let captured = J::Array(areas.iter().map(StorageArea::json).collect());
    let input = J::object([
        ("kind", "layout".into()),
        ("names", J::Array(vec!["fixture".into()])),
        (
            "cases",
            J::Array(vec![J::object([
                ("operation", "restoredStorageAreas".into()),
                ("config", c.clone()),
                ("captured", captured),
            ])]),
        ),
    ]);
    let expected = oracle(&input).get("cases").unwrap().as_array().unwrap()[0].clone();
    let actual = wrapped(
        project_storage::restored_storage_areas(&c, &areas, &ctx)
            .map(|a| J::Array(a.iter().map(StorageArea::json).collect())),
    );
    retain("restored projects", input, actual.clone(), expected.clone());
    assert_json_eq(&actual, &expected);
}
#[tokio::test]
async fn snapshot_uses_sqlite_read_transaction_and_rejects_corruption_before_destination() {
    for (case, sql) in [
        (
            "wal",
            "PRAGMA journal_mode=WAL;PRAGMA wal_autocheckpoint=0;CREATE TABLE data(id INTEGER PRIMARY KEY,value TEXT);INSERT INTO data VALUES(1,'before'),(2,'😀');PRAGMA user_version=1;",
        ),
        ("empty", ""),
        (
            "ignored-check-constraint",
            "CREATE TABLE data(value INTEGER CHECK(value>0));PRAGMA ignore_check_constraints=ON;INSERT INTO data VALUES(-1);",
        ),
        (
            "blob",
            "CREATE TABLE data(value BLOB);INSERT INTO data VALUES(zeroblob(900000));",
        ),
        ("malformed", ""),
    ] {
        let f = ServiceFixture::new();
        let source = f.0.join("source.sqlite");
        let db = if case == "malformed" {
            fs::write(&source, [0u8; 4096]).unwrap();
            None
        } else {
            let db = Connection::open(&source).unwrap();
            db.execute_batch(sql).unwrap();
            Some(db)
        };
        let actual_path = f.0.join("rust.sqlite");
        let expected_path = f.0.join("node.sqlite");
        let result = private_backup::snapshot_journal(source.clone(), actual_path.clone()).await;
        let mut input = request(&f, "snapshot");
        input.insert("source", jp(&source));
        input.insert("destination", jp(&expected_path));
        compare(case, input, result.map(|_| J::Null));
        if actual_path.exists() {
            let actual = fs::read(&actual_path).unwrap();
            let expected = fs::read(&expected_path).unwrap();
            // Different SQLite releases write their own version header. Keep
            // both byte identities; compare the actual contents independently.
            let inspect = |path: &Path| {
                let mut input = request(&f, "inspectSnapshot");
                input.insert("source", jp(path));
                oracle(&input)
            };
            let contents = inspect(&actual_path);
            let expected_contents = inspect(&expected_path);
            assert_json_eq(&contents, &expected_contents);
            assert_json_eq(&contents, &inspect(&source));
            retain(
                "snapshot SQL contents",
                case.into(),
                contents,
                expected_contents,
            );
            let identity = |bytes: &[u8]| {
                J::object([
                    ("bytes", J::Number(bytes.len() as f64)),
                    ("hash", ivy_contracts::hash::digest(bytes).into()),
                    (
                        "writerVersion",
                        if bytes.len() >= 100 {
                            J::Number(u32::from_be_bytes(bytes[96..100].try_into().unwrap()) as f64)
                        } else {
                            J::Null
                        },
                    ),
                ])
            };
            retain(
                "snapshot file identities (observed, no byte-equality requirement)",
                case.into(),
                identity(&actual),
                identity(&expected),
            );
        } else {
            assert!(!expected_path.exists());
        }
        if let Some(db) = db {
            assert!(db.is_autocommit());
            db.execute_batch("PRAGMA user_version=2").unwrap();
        }
    }
}
#[tokio::test]
async fn offline_owner_failure_releases_prior_locks_and_preserves_the_fence() {
    let f = ServiceFixture::new();
    let c = host_configuration(&f);
    let root = f.0.join("state");
    let docker = ivy_host_runtime::docker::DockerAdapter::new(f.0.clone()).unwrap();
    let compare: FileCompare = Arc::new(|a, b| a.cmp(b));
    let busy = ExecutorLock::acquire(root.join("owners/fixture"), false)
        .await
        .unwrap();
    let mut input = request(&f, "owners");
    input.insert("config", c.clone());
    let actual = private_backup::offline_owners(&c, &compare, &docker).await;
    compare_result(&input, actual.err().unwrap());
    busy.close().await.unwrap();
    let executor = ExecutorLock::acquire(root.join("executor"), false)
        .await
        .unwrap();
    executor.close().await.unwrap();
    let data = PathBuf::from(
        layout::service_paths(&c, &"fixture".into())
            .unwrap()
            .get("data")
            .unwrap()
            .as_str()
            .unwrap(),
    );
    let fence = super::stop_fence();
    ivy_host_runtime::process_fence::record_process_stop_fence(&data, fence.clone())
        .await
        .unwrap();
    let actual = private_backup::offline_owners(&c, &compare, &docker).await;
    compare_result(&input, actual.err().unwrap());
    assert!(
        ivy_host_runtime::process_fence::process_stop_fence(&data)
            .await
            .unwrap()
            .is_some()
    );
    let executor = ExecutorLock::acquire(root.join("executor"), false)
        .await
        .unwrap();
    executor.close().await.unwrap();
}
fn compare_result(input: &J, error: IvyError) {
    let expected = oracle(input);
    let actual = wrapped(Err(error));
    retain(
        "offline ownership failure",
        input.clone(),
        actual.clone(),
        expected.clone(),
    );
    assert_json_eq(&actual, &expected);
}

struct NoNative;
impl NativeBackupPaths for NoNative {
    fn exclusions(&self, c: &J) -> IvyResult<Vec<J>> {
        assert!(
            c.get("instances")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i.get("componentId") != Some(&"agent-manager".into()))
        );
        Ok(vec![])
    }
    fn shared_homes(&self, c: &J) -> IvyResult<Vec<J>> {
        self.exclusions(c)
    }
}
async fn setup(f: &ServiceFixture) -> (J, Arc<BackupBindings>) {
    let mut c = host_configuration(f);
    let node = std::env::var_os("IVY_BACKUP_NODE").unwrap();
    c.insert("executables", J::object([("node", jp(Path::new(&node)))]));
    config::atomic_json(f.0.join("host.json"), c.clone())
        .await
        .unwrap();
    let c = host_config::host_config(f.0.join("host.json"))
        .await
        .unwrap();
    let data = PathBuf::from(
        layout::service_paths(&c, &"fixture".into())
            .unwrap()
            .get("data")
            .unwrap()
            .as_str()
            .unwrap(),
    );
    fs::write(data.join("owned.txt"), "current owned state").unwrap();
    let distribution = f.0.join("distribution");
    fs::create_dir_all(distribution.join("dist/native")).unwrap();
    #[cfg(windows)]
    fs::copy(
        std::env::var_os("IVY_BACKUP_JOB").unwrap(),
        distribution.join("dist/native/ivy-job.exe"),
    )
    .unwrap();
    let b = Arc::new(BackupBindings {
        compare: Arc::new(|a, b| a.cmp(b)),
        native_paths: Arc::new(NoNative),
        node: ivy_host_runtime::node_runtime::NodeRuntime {
            executable: node.into(),
            version: "24.18.0".into(),
        },
        distribution_root: distribution,
        clock: Arc::new(SystemClock),
    });
    (c, b)
}
#[tokio::test]
async fn complete_backup_publishes_actual_current_state_and_releases_ownership() {
    let f = ServiceFixture::new();
    let (c, b) = setup(&f).await;
    let result = private_backup::backup_host(f.0.join("host.json"), f.0.join("backup"), b.clone())
        .await
        .unwrap();
    validate_host("BackupResult", &result).unwrap();
    let manifest = config::json_file(f.0.join("backup/backup.json"), 12 * 1024 * 1024)
        .await
        .unwrap();
    validate_host("BackupManifest", &manifest).unwrap();
    assert_eq!(manifest.get("backupId"), result.get("backupId"));
    assert_eq!(
        result.get("manifestHash").unwrap().as_str().unwrap(),
        streaming_hash::file_hash(f.0.join("backup/backup.json"))
            .await
            .unwrap()
    );
    assert_eq!(
        manifest.get("configurationHash").unwrap().as_str().unwrap(),
        hash_json(&c).unwrap()
    );
    let files = manifest.get("files").unwrap().as_array().unwrap();
    assert!(
        files
            .iter()
            .any(|v| v.get("path") == Some(&"state/instances/fixture/data/owned.txt".into()))
    );
    assert!(!files.iter().any(|v| {
        v.get("path")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("executor-lock.sqlite")
    }));
    for file in files {
        let path = backup_files::relative_file(
            &node_path::from_native(&f.0.join("backup")),
            file.get("path").unwrap().as_js_string().unwrap(),
        )
        .unwrap();
        assert_eq!(
            streaming_hash::file_hash(node_path::filesystem_path(&path))
                .await
                .unwrap(),
            file.get("hash").unwrap().as_str().unwrap()
        );
    }
    let mut verify = request(&f, "verify");
    verify.insert("destination", jp(&f.0.join("backup")));
    verify.insert("manifestHash", result.get("manifestHash").unwrap().clone());
    compare(
        "unchanged original verifies Rust backup",
        verify,
        Ok(manifest.clone()),
    );
    let lock = ExecutorLock::acquire(f.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
    retain(
        "actual backup publication",
        jp(&f.0),
        J::object([("result", result), ("manifest", manifest)]),
        J::Null,
    );
    let error = private_backup::backup_host(f.0.join("host.json"), f.0.join("state/overlap"), b)
        .await
        .unwrap_err();
    assert_eq!(error.code, "target_conflict");
    assert!(!f.0.join("state/overlap").exists());
}

struct PolicyGate {
    entered: tokio::sync::Notify,
    released: Mutex<bool>,
    wake: std::sync::Condvar,
    calls: Mutex<Vec<&'static str>>,
}
impl PolicyGate {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}
impl NativeBackupPaths for PolicyGate {
    fn exclusions(&self, _: &J) -> IvyResult<Vec<J>> {
        self.calls.lock().unwrap().push("exclusions");
        self.entered.notify_one();
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.wake.wait(released).unwrap();
        }
        Ok(vec![])
    }
    fn shared_homes(&self, _: &J) -> IvyResult<Vec<J>> {
        self.calls.lock().unwrap().push("shared_homes");
        Ok(vec![])
    }
}
struct ReleaseGate(Arc<PolicyGate>);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_backup_keeps_locks_after_waiter_drop_and_rechecks_master_configuration() {
    for change in [false, true] {
        let f = ServiceFixture::new();
        let (config, b) = setup(&f).await;
        let gate = Arc::new(PolicyGate {
            entered: Default::default(),
            released: Mutex::new(false),
            wake: Default::default(),
            calls: Default::default(),
        });
        let _release = ReleaseGate(gate.clone());
        let bindings = Arc::new(BackupBindings {
            compare: b.compare.clone(),
            native_paths: gate.clone(),
            node: b.node.clone(),
            distribution_root: b.distribution_root.clone(),
            clock: b.clock.clone(),
        });
        let source = f.0.join("host.json");
        let destination = f.0.join("admitted-backup");
        let source2 = source.clone();
        let destination2 = destination.clone();
        let waiter = tokio::spawn(async move {
            private_backup::backup_host(source2, destination2, bindings).await
        });
        tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
            .await
            .unwrap();
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(
            ExecutorLock::acquire(f.0.join("state/executor"), false)
                .await
                .err()
                .unwrap()
                .code,
            "executor_already_running"
        );
        if change {
            let mut edited = config.clone();
            edited.insert("publicBaseUrl", "http://127.0.0.1:39082/ivy".into());
            config::atomic_json(&source, edited).await.unwrap();
        }
        gate.release();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if destination
                    .join(if change {
                        "backup-failure.json"
                    } else {
                        "backup.json"
                    })
                    .exists()
                {
                    if let Ok(lock) = ExecutorLock::acquire(f.0.join("state/executor"), false).await
                    {
                        lock.close().await.unwrap();
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            *gate.calls.lock().unwrap(),
            vec!["exclusions", "shared_homes"]
        );
        if change {
            assert!(!destination.join("backup.json").exists());
            let failure = config::json_file(destination.join("backup-failure.json"), 1024 * 1024)
                .await
                .unwrap();
            assert_eq!(failure.get("code"), Some(&"backup_changed".into()));
            retain(
                "changed master prevents publication after waiter drop",
                jp(&destination),
                failure,
                J::Null,
            );
        } else {
            let manifest = config::json_file(destination.join("backup.json"), 12 * 1024 * 1024)
                .await
                .unwrap();
            validate_host("BackupManifest", &manifest).unwrap();
            retain(
                "admitted publication survives waiter drop",
                jp(&destination),
                manifest,
                J::Null,
            );
        }
    }
}
