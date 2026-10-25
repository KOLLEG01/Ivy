use super::{ServiceFixture, host_configuration};
use ivy_contracts::{IvyJson as J, core_schema::validate_host};
use ivy_hive::{storage_read::inspect_stopped_storage, store::Store};
use ivy_host_executor::backup::native_backup_bindings;
use ivy_host_runtime::{
    config,
    executor_lock::ExecutorLock,
    host_actions::{HostActions, RestoreRuntime},
    host_config::{self, HostContext},
    layout::{self, StorageContext},
    node_path,
    restore::{self, RestoreBindings},
};
use ivy_sdk::clock::SystemClock;
use std::{fs, path::Path, sync::Arc};

fn path(path: &Path) -> J {
    J::String(node_path::from_native(path))
}

#[tokio::test]
async fn additional_storage_cannot_claim_another_instance_or_nest_in_runtime_data() {
    for cross_owner in [false, true] {
        let f = ServiceFixture::new();
        let mut host = host_configuration(&f);
        host.insert(
            "executables",
            J::object([("native-host", path(&std::env::current_exe().unwrap()))]),
        );
        let mut instances = host
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        for (index, root) in ["first", "second"].iter().enumerate() {
            if index == 1 {
                let mut other = instances[0].clone();
                other.insert("instanceId", "other".into());
                other.insert("serviceNodeId", "other-node".into());
                instances.push(other);
            }
            instances[index].insert(
                "paths",
                J::object([
                    ("data", path(&f.0.join(root).join("data"))),
                    ("work", path(&f.0.join(root).join("work"))),
                    ("logs", path(&f.0.join(root).join("logs"))),
                ]),
            );
        }
        let root = if cross_owner {
            f.0.join("second")
        } else {
            f.0.join("first/data/nested")
        };
        instances[0].insert("storageRoots", J::object([("configuration", path(&root))]));
        host.insert("instances", J::Array(instances));
        let error = host_config::checked_host_config(host, f.0.join("host.json"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "target_conflict");
        assert!(!root.exists());
    }
}

#[tokio::test]
async fn native_hive_backup_preserves_source_and_propagates_owner_refusals_without_node() {
    for invalid in [false, true] {
        let f = ServiceFixture::new();
        let mut host = host_configuration(&f);
        host.insert(
            "executables",
            J::object([("native-host", path(&std::env::current_exe().unwrap()))]),
        );
        let mut instances = host
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        instances[0].insert("componentId", "hive".into());
        let owned = f.0.join("owned-service");
        let credentials = f.0.join("owned-credentials");
        instances[0].insert(
            "paths",
            J::object([
                ("data", path(&owned.join("data"))),
                ("work", path(&owned.join("work"))),
                ("logs", path(&owned.join("logs"))),
            ]),
        );
        instances[0].insert(
            "storageRoots",
            J::object([
                ("configuration", path(&owned)),
                ("credentials", path(&credentials)),
            ]),
        );
        instances[0].insert(
            "settings",
            J::object([
                ("listenHost", "127.0.0.1".into()),
                ("listenPort", J::Number(39081.0)),
                ("credentials", J::Array(vec![])),
                (
                    "backup",
                    J::object([
                        ("directory", path(&f.0.join("rotating"))),
                        ("intervalHours", J::Number(24.0)),
                        ("retain", J::Number(7.0)),
                    ]),
                ),
            ]),
        );
        host.insert("instances", J::Array(instances));
        let config_path = f.0.join("host.json");
        config::atomic_json(&config_path, host).await.unwrap();
        let host = host_config::host_config(config_path.clone()).await.unwrap();
        let protected = br#"{"retained":"configuration outside data"}"#;
        fs::write(owned.join("config.json"), protected).unwrap();
        fs::write(credentials.join("account.bin"), [0, 255, 1, 128]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(owned.join("config.json"), fs::Permissions::from_mode(0o600))
                .unwrap();
            fs::set_permissions(
                credentials.join("account.bin"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let data = node_path::to_native(
            layout::service_paths(&host, &"fixture".into())
                .unwrap()
                .required("data")
                .unwrap()
                .as_js_string()
                .unwrap(),
        );
        let source = data.join("hive.sqlite");
        let store = Store::open(&source).unwrap();
        store.connection().execute_batch("CREATE TABLE backup_probe(value BLOB); INSERT INTO backup_probe VALUES(X'0001FF');").unwrap();
        if invalid {
            store
                .connection()
                .execute_batch("PRAGMA user_version=99;")
                .unwrap();
        }
        store.close().unwrap();
        let original = fs::read(&source).unwrap();
        let distribution = f.0.join("distribution");
        let launcher = ivy_host_runtime::native_plan::native_binary_path(&distribution, "ivy-job");
        #[cfg(windows)]
        {
            let debug = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_owned();
            let helper = std::env::var_os("IVY_BACKUP_JOB")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| debug.join("ivy-job.exe"));
            fs::create_dir_all(launcher.parent().unwrap()).unwrap();
            fs::copy(helper, &launcher).unwrap();
        }
        let compare = Arc::new(|a: &ivy_contracts::JsString, b: &ivy_contracts::JsString| a.cmp(b));
        let bindings = Arc::new(
            native_backup_bindings(
                None,
                compare.clone(),
                distribution.clone(),
                Arc::new(SystemClock),
            )
            .unwrap(),
        );
        assert!(bindings.node.is_none());
        let actions =
            HostActions::native(None, compare.clone(), distribution.clone(), launcher).unwrap();
        let destination = f.0.join("backup");
        let result = actions
            .backup(config_path, destination.clone(), bindings.clone())
            .await;
        if invalid {
            assert_eq!(result.unwrap_err().code, "unsupported_storage_format");
            assert!(!destination.join("backup.json").exists());
            let failure = config::json_file(destination.join("backup-failure.json"), 4096)
                .await
                .unwrap();
            assert_eq!(
                failure.get("code"),
                Some(&"unsupported_storage_format".into())
            );
        } else {
            let result = result.unwrap();
            validate_host("BackupResult", &result).unwrap();
            let manifest = config::json_file(destination.join("backup.json"), 12 * 1024 * 1024)
                .await
                .unwrap();
            validate_host("BackupManifest", &manifest).unwrap();
            assert_eq!(
                manifest.required("platform").unwrap().get("node"),
                Some(&"unavailable".into())
            );
            let snapshots = manifest
                .required("hiveSnapshots")
                .unwrap()
                .as_array()
                .unwrap();
            assert_eq!(snapshots.len(), 1);
            assert_eq!(snapshots[0].get("instanceId"), Some(&"fixture".into()));
            assert_eq!(snapshots[0].get("format"), Some(&J::Number(4.0)));
            assert!(snapshots[0].required("pages").unwrap().as_f64().unwrap() > 0.0);
            let local = snapshots[0].required("path").unwrap().as_str().unwrap();
            let snapshot = destination.join(local);
            let account = HostContext::current().unwrap();
            let restore = RestoreBindings {
                storage: StorageContext {
                    paths: account.paths,
                    account_home: account.account_home,
                    compare: compare.clone(),
                },
                environment: account.environment,
                native_paths: bindings.native_paths.clone(),
            };
            restore::verify_backup(
                node_path::from_native(&destination),
                result
                    .required("manifestHash")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone(),
                &restore,
            )
            .await
            .unwrap();
            assert_eq!(
                fs::read(destination.join("services/fixture/storage/configuration/config.json"))
                    .unwrap(),
                protected
            );
            assert_eq!(
                fs::read(destination.join("services/fixture/storage/credentials/account.bin"))
                    .unwrap(),
                [0, 255, 1, 128]
            );
            assert_eq!(
                manifest
                    .required("storageAreas")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                3
            );

            let mut target = host.clone();
            target.insert("publicBaseUrl", "http://127.0.0.1:39082/ivy".into());
            for (name, suffix) in [
                ("runtimeRoot", "restored-state"),
                ("artifactRoot", "restored-artifacts"),
                ("stagingRoot", "restored-staging"),
            ] {
                target.insert(name, path(&f.0.join(suffix)));
            }
            let target_owned = f.0.join("restored-service");
            let target_credentials = f.0.join("restored-credentials");
            let mut instances = target
                .required("instances")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec();
            instances[0].insert("enabled", false.into());
            instances[0].insert(
                "paths",
                J::object([
                    ("data", path(&target_owned.join("data"))),
                    ("work", path(&target_owned.join("work"))),
                    ("logs", path(&target_owned.join("logs"))),
                ]),
            );
            instances[0].insert(
                "storageRoots",
                J::object([
                    ("configuration", path(&target_owned)),
                    ("credentials", path(&target_credentials)),
                ]),
            );
            let mut settings = instances[0].required("settings").unwrap().clone();
            settings.insert("listenPort", J::Number(39082.0));
            let mut rotating = settings.required("backup").unwrap().clone();
            rotating.insert("directory", path(&f.0.join("restored-state/rotating")));
            settings.insert("backup", rotating);
            instances[0].insert("settings", settings);
            target.insert("instances", J::Array(instances));
            let target_path = f.0.join("restore-template.json");
            config::atomic_json(&target_path, target).await.unwrap();
            let restored = actions
                .restore(
                    target_path,
                    destination.clone(),
                    result
                        .required("manifestHash")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .clone(),
                    RestoreRuntime::native(None, compare).unwrap(),
                )
                .await
                .unwrap();
            validate_host("RestoreResult", &restored).unwrap();
            assert_eq!(
                fs::read(target_owned.join("config.json")).unwrap(),
                protected
            );
            assert_eq!(
                fs::read(target_credentials.join("account.bin")).unwrap(),
                [0, 255, 1, 128]
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(target_owned.join("config.json"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
            // SQLite inspection may create WAL bookkeeping. Read a disposable
            // copy, keeping the selected published backup immutable.
            let inspection = f.0.join("inspect.sqlite");
            fs::copy(&snapshot, &inspection).unwrap();
            inspect_stopped_storage(&inspection, &[]).unwrap();
        }
        assert_eq!(fs::read(&source).unwrap(), original);
        assert_eq!(fs::read(owned.join("config.json")).unwrap(), protected);
        for owner in [f.0.join("state/executor"), f.0.join("state/owners/fixture")] {
            ExecutorLock::acquire(owner, false)
                .await
                .unwrap()
                .close()
                .await
                .unwrap();
        }
        assert!(
            !distribution
                .join("dist/services/hive/src/offline-backup.js")
                .exists()
        );
    }
}
