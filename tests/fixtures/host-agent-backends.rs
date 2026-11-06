//! Host admission and maintenance retain each configured native account.
use super::{ServiceFixture, host_configuration};
use ivy_contracts::{
    IvyJson as J, JsString,
    hash::hash_json,
    native_backend::{BackendIdentity, NativeProvider},
};
use ivy_host_runtime::{
    agent_backends::configured_backends,
    config, default_settings, filesystem,
    host_config::{self, HostContext},
    layout::{self, StorageContext},
    native_backup_paths::NativeBackupPolicy,
    node_path::{self, Context},
    project_storage,
    runtime_reset::local_runtime_reset_plan,
};
use std::{collections::BTreeMap, fs, path::Path, sync::Arc};

fn path(path: &Path) -> J {
    J::String(node_path::from_native(path))
}
fn backend(id: &str, provider: NativeProvider, account: &str) -> BackendIdentity {
    BackendIdentity::new(id.into(), provider, account.into()).unwrap()
}
fn input(f: &ServiceFixture) -> J {
    let mut host = host_configuration(f);
    host.insert(
        "executables",
        J::object([("native", path(&std::env::current_exe().unwrap()))]),
    );
    let backends = [
        ("alpha", NativeProvider::Codex, "personal"),
        ("beta", NativeProvider::Claude, "work"),
    ]
    .map(|(id, provider, account)| {
        let data = f.0.join(format!("backend-{id}"));
        let mut server = J::object([(
            "mode",
            if provider == NativeProvider::Claude {
                "claude-adapter"
            } else {
                "external-proxy"
            }
            .into(),
        )]);
        if provider == NativeProvider::Codex {
            server.insert("expectedCodexHome", path(&f.0.join("shared")));
        } else {
            for key in ["adapterRoot", "claudeExecutable"] {
                server.insert(key, path(&f.0.join(key)));
            }
            for key in ["nodeExecutableHash", "claudeExecutableHash"] {
                server.insert(key, format!("sha256:{}", "a".repeat(64)).into());
            }
            server.insert("defaultModel", "shared".into());
        }
        J::object([
            ("dataRoot", path(&data)),
            (
                "settings",
                J::object([
                    ("backend", backend(id, provider, account).json()),
                    ("appServer", server),
                    ("nativeExecutable", path(&std::env::current_exe().unwrap())),
                    (
                        "nativeVersion",
                        if provider == NativeProvider::Claude {
                            "0.142.3"
                        } else {
                            "0.163.0"
                        }
                        .into(),
                    ),
                    (
                        "nativeExecutableHash",
                        format!("sha256:{}", "b".repeat(64)).into(),
                    ),
                    ("nativeHome", path(&data.join("home"))),
                ]),
            ),
        ])
    });
    let mut instance = host.required("instances").unwrap().as_array().unwrap()[0].clone();
    instance.insert("componentId", "agent-manager".into());
    instance.insert(
        "paths",
        J::object([
            ("data", path(&f.0.join("manager/data"))),
            ("work", path(&f.0.join("manager/work"))),
            ("logs", path(&f.0.join("manager/logs"))),
        ]),
    );
    instance.insert(
        "settings",
        J::object([("backends", J::Array(backends.to_vec()))]),
    );
    host.insert("instances", J::Array(vec![instance]));
    host
}
fn entries(host: &J) -> Vec<J> {
    host.required("instances").unwrap().as_array().unwrap()[0]
        .required("settings")
        .unwrap()
        .required("backends")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec()
}
fn replace(host: &mut J, entries: Vec<J>) {
    let mut instances = host
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    instances[0].insert("settings", J::object([("backends", J::Array(entries))]));
    host.insert("instances", J::Array(instances));
}
fn context(f: &ServiceFixture) -> HostContext {
    HostContext {
        paths: Context::current().unwrap(),
        environment: BTreeMap::new(),
        account_home: node_path::from_native(&f.0.join("account")),
    }
}
fn storage(f: &ServiceFixture) -> Arc<StorageContext> {
    Arc::new(StorageContext {
        paths: Context::current().unwrap(),
        account_home: node_path::from_native(&f.0.join("account")),
        compare: Arc::new(|a, b| a.cmp(b)),
    })
}
async fn checked(f: &ServiceFixture, input: J) -> ivy_contracts::IvyResult<J> {
    host_config::checked_host_config_with_context(input, f.0.join("host.json"), context(f)).await
}

#[test]
fn scoped_defaults_preserve_provider_choices_and_refuse_ambiguous_owners() {
    let f = ServiceFixture::new();
    let host = input(&f);
    let settings = host.required("instances").unwrap().as_array().unwrap()[0]
        .required("settings")
        .unwrap();
    let before = settings.clone();
    let resolved = default_settings::default_service_settings(
        "agent-manager",
        settings,
        &f.0,
        &f.0.join("host.json"),
    )
    .unwrap();
    assert_eq!(settings, &before);
    assert_eq!(resolved.as_object().unwrap().len(), 1);
    let resolved = layout::agent_manager_account_settings(
        &resolved,
        &node_path::from_native(&f.0.join("account")),
        Context::current().unwrap().style,
    )
    .unwrap();
    ivy_contracts::agent_schema::validate_agent("ServiceSettings", &resolved).unwrap();
    let owners = configured_backends(&resolved, None).unwrap();
    assert_eq!(owners.len(), 2);
    for (owner, expected) in owners.iter().zip(["general-codex", "general-claude"]) {
        assert!(
            owner
                .settings
                .required("capabilities")
                .unwrap()
                .as_array()
                .unwrap()[0]
                .required("key")
                .unwrap()
                .is_str(expected)
        );
        assert!(owner.settings.get("backends").is_none());
    }
    assert!(owners[0].settings.get("skillsRoot").is_some());
    assert!(owners[1].settings.get("skillsRoot").is_none());
    assert_ne!(
        owners[0].settings.get("internalProjectRoot"),
        owners[1].settings.get("internalProjectRoot")
    );
    let mut changed = entries(&host);
    let mut setting = changed[0].required("settings").unwrap().clone();
    setting.insert(
        "backend",
        backend("alpha", NativeProvider::Codex, "other").json(),
    );
    changed[0].insert("settings", setting);
    let changed = J::object([("backends", J::Array(changed))]);
    assert_ne!(
        configured_backends(settings, None).unwrap()[0]
            .storage_key()
            .unwrap(),
        configured_backends(&changed, None).unwrap()[0]
            .storage_key()
            .unwrap()
    );
    for provider in [NativeProvider::Codex, NativeProvider::Claude] {
        let mut rows = entries(&host);
        let mut setting = rows[1].required("settings").unwrap().clone();
        setting.insert("backend", backend("alpha", provider, "work").json());
        rows[1].insert("settings", setting);
        assert!(configured_backends(&J::object([("backends", J::Array(rows))]), None).is_err());
    }
}

#[tokio::test]
async fn host_admission_checks_every_backend_path_and_filesystem_alias() {
    for variant in 0..8 {
        let f = ServiceFixture::new();
        let mut host = input(&f);
        let mut rows = entries(&host);
        match variant {
            1 => {
                let root = rows[0].required("dataRoot").unwrap().clone();
                rows[1].insert("dataRoot", root);
            }
            2 => {
                let mut instances = host
                    .required("instances")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .to_vec();
                let mut other = instances[0].clone();
                other.insert("instanceId", "other".into());
                other.insert("serviceNodeId", "other-node".into());
                other.insert("componentId", "fixture".into());
                other.insert("settings", J::Object(vec![]));
                other.insert(
                    "paths",
                    J::object([
                        ("data", path(&f.0.join("other/data"))),
                        ("work", path(&f.0.join("other/work"))),
                        ("logs", path(&f.0.join("other/logs"))),
                    ]),
                );
                instances.push(other);
                host.insert("instances", J::Array(instances));
                rows[1].insert("dataRoot", path(&f.0.join("other/data")));
            }
            3 => {
                rows[1].insert("dataRoot", path(&f.0.join("artifacts/foreign")));
            }
            4 => {
                fs::create_dir_all(f.0.join("backend-alpha")).unwrap();
                filesystem::directory_link(&f.0.join("backend-alpha"), &f.0.join("alias")).unwrap();
                rows[1].insert("dataRoot", path(&f.0.join("alias")));
            }
            5 | 6 => {
                let index = if variant == 5 { 0 } else { 1 };
                let mut settings = rows[index].required("settings").unwrap().clone();
                settings.insert(
                    if variant == 5 {
                        "projectRoot"
                    } else {
                        "internalProjectRoot"
                    },
                    path(&f.0.join(if variant == 5 {
                        "backend-beta/projects"
                    } else {
                        "shared/private"
                    })),
                );
                rows[index].insert("settings", settings);
            }
            7 => {
                let mut settings = rows[0].required("settings").unwrap().clone();
                settings.insert("nativeHome", path(&f.0.join("backend-beta/foreign-home")));
                rows[0].insert("settings", settings);
            }
            _ => {}
        }
        replace(&mut host, rows);
        let result = checked(&f, host).await;
        if variant == 0 {
            let host = result.unwrap();
            assert_eq!(entries(&host).len(), 2);
            assert!(f.0.join("backend-alpha").is_dir() && f.0.join("backend-beta").is_dir());
        } else {
            assert_eq!(
                result.unwrap_err().code,
                if variant == 7 {
                    "native_backend_mismatch"
                } else {
                    "target_conflict"
                },
                "variant {variant}"
            );
        }
    }
}

#[tokio::test]
async fn backup_roots_and_retained_projects_keep_complete_account_ownership() {
    let f = ServiceFixture::new();
    let host = checked(&f, input(&f)).await.unwrap();
    let storage = storage(&f);
    let policy = NativeBackupPolicy::new(
        StorageContext {
            paths: storage.paths.clone(),
            account_home: storage.account_home.clone(),
            compare: storage.compare.clone(),
        },
        BTreeMap::new(),
        storage.paths.cwd.clone(),
    );
    let shared = policy.shared_homes(&host).unwrap();
    let excluded = policy.exclusions(&host).unwrap();
    assert_eq!(shared.len(), 1);
    assert_eq!(excluded.len(), 1);
    assert!(BackendIdentity::matches(
        &shared[0],
        Some(&backend("alpha", NativeProvider::Codex, "personal"))
    ));
    assert!(BackendIdentity::matches(
        &excluded[0],
        Some(&backend("beta", NativeProvider::Claude, "work"))
    ));
    let manifest = J::object([
        ("schemaVersion", 1.0.into()),
        ("backupId", "fixture".into()),
        ("hostId", "fixture-host".into()),
        ("createdAt", "2026-01-02T03:04:05.000Z".into()),
        ("completedAt", "2026-01-02T03:04:05.000Z".into()),
        ("consistency", "offline-owned-state".into()),
        (
            "platform",
            J::object([
                ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                ("arch", "x64".into()),
                ("node", "24.18.0".into()),
            ]),
        ),
        ("configurationHash", hash_json(&host).unwrap().into()),
        ("files", J::Array(vec![])),
        ("externalFiles", J::Array(vec![])),
        ("hiveSnapshots", J::Array(vec![])),
        ("excluded", J::Array(vec![])),
        ("ephemeralNativePaths", J::Array(excluded.clone())),
        ("sharedNativeHomes", J::Array(shared.clone())),
    ]);
    ivy_contracts::core_schema::validate_host("BackupManifest", &manifest).unwrap();
    let base = layout::host_storage_areas(&host, &storage).unwrap();
    for id in ["alpha", "beta"] {
        assert!(base.iter().any(|area| area.path == node_path::from_native(&f.0.join(format!("backend-{id}")))));
        config::atomic_json(
            f.0.join(format!("backend-{id}/project-backup-paths.json")),
            J::Array(vec![path(&f.0.join(format!("retired-{id}/same-project")))]),
        )
        .await
        .unwrap();
    }
    assert!(!base.iter().any(|area| {
        storage
            .paths
            .inside(&area.path, &node_path::from_native(&f.0.join("shared")))
    }));
    let captured = project_storage::project_storage_areas(&host, &storage)
        .await
        .unwrap();
    let retained = captured
        .iter()
        .filter(|area| {
            area.key
                .0
                .starts_with(&JsString::from("retained-projects/").0)
        })
        .collect::<Vec<_>>();
    assert_eq!(retained.len(), 2);
    assert_ne!(retained[0].key, retained[1].key);
    let restored = project_storage::restored_storage_areas(&host, &captured, &storage).unwrap();
    for (area, row) in retained.iter().zip(entries(&host)) {
        let target = restored
            .iter()
            .find(|target| target.key == area.key)
            .unwrap();
        assert!(
            storage.paths.inside(
                row.required("settings")
                    .unwrap()
                    .required("internalProjectRoot")
                    .unwrap()
                    .as_js_string()
                    .unwrap(),
                &target.path
            )
        );
    }
    let mut changed = host.clone();
    let mut rows = entries(&changed);
    let mut settings = rows[1].required("settings").unwrap().clone();
    settings.insert(
        "backend",
        backend("beta", NativeProvider::Claude, "other").json(),
    );
    rows[1].insert("settings", settings);
    replace(&mut changed, rows);
    assert_eq!(
        project_storage::restored_storage_areas(&changed, &captured, &storage)
            .unwrap_err()
            .code,
        "backup_invalid"
    );
}

#[tokio::test]
async fn reset_preview_covers_each_native_owner_and_common_host_paths() {
    let f = ServiceFixture::new();
    let mut host = checked(&f, input(&f)).await.unwrap();
    host.insert(
        "deployment",
        J::object([
            ("identity", "fixture".into()),
            (
                "hosts",
                J::Array(vec![J::object([
                    ("hostId", "fixture-host".into()),
                    ("configPath", path(&f.0.join("host.json"))),
                ])]),
            ),
        ]),
    );
    let before = hash_json(&host).unwrap();
    let plan = local_runtime_reset_plan(
        host.clone(),
        node_path::from_native(&f.0.join("host.json")),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        storage(&f),
    )
    .await
    .unwrap();
    assert_eq!(hash_json(&host).unwrap(), before);
    let markers = plan
        .required("completionMarkers")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(markers.len(), 2);
    for (marker, row) in markers.iter().zip(entries(&host)) {
        let owner = BackendIdentity::from_json(
            row.required("settings")
                .unwrap()
                .required("backend")
                .unwrap(),
        )
        .unwrap();
        assert!(BackendIdentity::matches(marker, Some(&owner)));
        assert_eq!(
            marker.required("absolutePath").unwrap(),
            &path(
                &node_path::filesystem_path(
                    row.required("dataRoot").unwrap().as_js_string().unwrap()
                )
                .join("runtime-reset.json")
            )
        );
        assert!(
            plan.required("delete")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| BackendIdentity::matches(entry, Some(&owner)))
        );
    }
    assert!(
        plan.required("delete")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.get("backend").is_none()
                && entry.required("area").unwrap().is_str("data"))
    );
}
