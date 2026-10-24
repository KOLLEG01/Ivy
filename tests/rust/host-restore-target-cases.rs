use super::*;
use ivy_contracts::{core_schema::validate_host, hash::hash_json};
use ivy_host_runtime::{config, default_settings, layout, streaming_hash};
use std::collections::BTreeMap;

pub(super) fn jp(path: &Path) -> J {
    J::String(node_path::from_native(path))
}
pub(super) fn property<'a>(value: &'a mut J, name: &str) -> &'a mut J {
    let J::Object(fields) = value else {
        panic!("Expected fixture object")
    };
    &mut fields
        .iter_mut()
        .find(|(key, _)| *key == JsString::from(name))
        .unwrap()
        .1
}
pub(super) fn instances(config: &mut J) -> &mut Vec<J> {
    let J::Array(values) = property(config, "instances") else {
        panic!("Expected instances")
    };
    values
}
pub(super) fn instance(id: &str, component: &str, settings: J) -> J {
    J::object([
        ("instanceId", id.into()),
        ("componentId", component.into()),
        ("serviceNodeId", format!("{id}-node").into()),
        ("engine", "process".into()),
        ("enabled", true.into()),
        ("settings", settings),
    ])
}
pub(super) fn shared(path: &Path) -> Vec<J> {
    vec![J::object([
        ("instanceId", "agent".into()),
        ("path", jp(path)),
        ("kind", "external-user-state".into()),
    ])]
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Entry {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}
pub(super) fn state(root: &Path) -> BTreeMap<PathBuf, Entry> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Entry>) {
        let info = fs::symlink_metadata(path).unwrap();
        let entry = if info.file_type().is_symlink() {
            Entry::Link(fs::read_link(path).unwrap())
        } else if info.is_file() {
            Entry::File(fs::read(path).unwrap())
        } else {
            Entry::Directory
        };
        entries.insert(path.strip_prefix(root).unwrap().to_owned(), entry);
        if info.is_dir() && !info.file_type().is_symlink() {
            for child in fs::read_dir(path).unwrap() {
                visit(root, &child.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

#[tokio::test]
async fn fresh_disabled_restore_targets_preserve_source_and_layout_boundaries() {
    let mut cases = vec![
        "valid",
        "enabled",
        "host-identity",
        "instance-identity",
        "same-endpoint",
        "executables",
        "prerequisite-valid",
        "prerequisite-changed",
        "existing-root",
        "missing-parent",
        "source-overlap",
        "linked-parent-overlap",
        "root-overlap",
        "original-config",
        "config-in-artifacts",
        "config-exists",
        "template-is-config",
        "owned-native",
        "shared-valid",
        "shared-changed",
        "retained-valid",
        "manager-valid",
        "manager-wrong-config",
        "hive-valid",
        "hive-same-port",
        "hive-backup-in-data",
        "unicode-dot-paths",
        "file-parent",
        "backup-overlap",
        "retained-config-overlap",
    ];
    #[cfg(windows)]
    cases.extend([
        "opaque-target-high",
        "opaque-target-low",
        "opaque-template-high",
        "opaque-template-low",
    ]);
    for case in cases {
        let f = ServiceFixture::new();
        let root = f.0.join("restore");
        fs::create_dir(&root).unwrap();
        let host = HostContext::current().unwrap();
        let storage = StorageContext {
            paths: host.paths,
            account_home: host.account_home,
            compare: Arc::new(|a, b| a.cmp(b)),
        };
        let mut original = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("hostId", "restore-fixture".into()),
            ("runtimeRoot", jp(&root.join("source-state"))),
            ("artifactRoot", jp(&root.join("source-artifacts"))),
            ("stagingRoot", jp(&root.join("source-staging"))),
            ("publicBaseUrl", "http://127.0.0.1:38081/ivy".into()),
            (
                "executables",
                J::object([("node", jp(&root.join("native-executable")))]),
            ),
            (
                "instances",
                J::Array(vec![instance(
                    "fixture",
                    "fixture",
                    J::object(Vec::<(String, J)>::new()),
                )]),
            ),
        ]);
        let agent =
            case.starts_with("shared") || case.starts_with("retained") || case == "owned-native";
        if agent {
            instances(&mut original).push(instance(
                "agent",
                "agent-manager",
                J::object([
                    ("appServer", J::object([("mode", "owned-stdio".into())])),
                    (
                        "codexHome",
                        jp(&root.join("source-state/instances/agent/data/native")),
                    ),
                    ("internalProjectRoot", jp(&root.join("source-projects"))),
                ]),
            ));
            if case.starts_with("shared") {
                let settings = property(&mut instances(&mut original)[1], "settings");
                settings.insert("codexHome", jp(&root.join("shared-home")));
                settings.insert(
                    "appServer",
                    J::object([
                        ("mode", "external-proxy".into()),
                        ("expectedCodexHome", jp(&root.join("shared-home"))),
                    ]),
                );
            }
        }
        if case.starts_with("manager") {
            let source_config = layout::host_configuration_path(&original).unwrap();
            instances(&mut original).push(instance(
                "manager",
                "host-executor",
                J::object([("hostConfigPath", jp(&source_config))]),
            ));
        }
        if case.starts_with("hive") {
            let settings = default_settings::default_service_settings(
                "hive",
                &J::object([("credentials", J::Array(vec![]))]),
                &root,
                &root.join("original.json"),
            )
            .unwrap();
            instances(&mut original).push(instance("hive", "hive", settings));
        }
        validate_host("HostConfig", &original).unwrap();
        let backup = root.join("backup");
        fs::create_dir_all(backup.join("state")).unwrap();
        config::atomic_json(backup.join("configuration.json"), original.clone())
            .await
            .unwrap();
        fs::write(backup.join("state/payload"), b"immutable payload").unwrap();
        let mut captured = layout::host_storage_areas(&original, &storage).unwrap();
        if case.starts_with("retained") {
            let path = jp(&root.join("retained-project"));
            captured.push(StorageArea {
                path: text(&path),
                key: format!(
                    "retained-projects/agent/{}",
                    &hash_json(&path).unwrap()[7..]
                )
                .into(),
            });
        }
        let captured_shared = if case.starts_with("shared") {
            shared(&root.join("shared-home"))
        } else {
            vec![]
        };
        let mut manifest = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("backupId", "00000000-0000-4000-8000-000000000086".into()),
            ("hostId", original.required("hostId").unwrap().clone()),
            ("createdAt", "2026-10-20T00:00:00.000Z".into()),
            ("completedAt", "2026-10-20T00:00:00.000Z".into()),
            ("consistency", "offline-owned-state".into()),
            (
                "platform",
                J::object([
                    ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                    (
                        "arch",
                        if cfg!(target_arch = "x86_64") {
                            "x64"
                        } else {
                            "arm64"
                        }
                        .into(),
                    ),
                    ("node", "unavailable".into()),
                ]),
            ),
            ("configurationHash", hash_json(&original).unwrap().into()),
            ("files", J::Array(vec![])),
            ("externalFiles", J::Array(vec![])),
            ("hiveSnapshots", J::Array(vec![])),
            ("ephemeralNativePaths", J::Array(vec![])),
            ("sharedNativeHomes", J::Array(captured_shared)),
            (
                "storageAreas",
                J::Array(captured.iter().map(StorageArea::json).collect()),
            ),
            ("excluded", J::Array(vec![])),
        ]);
        let target = root.join("target");
        fs::create_dir(&target).unwrap();
        let mut template = root.join("template.json");
        let mut raw = original.clone();
        for (key, suffix) in [
            ("runtimeRoot", "state"),
            ("artifactRoot", "artifacts"),
            ("stagingRoot", "staging"),
        ] {
            raw.insert(key, jp(&target.join(suffix)));
        }
        raw.insert("publicBaseUrl", "http://127.0.0.1:38082/ivy".into());
        let target_config = layout::host_configuration_path(&raw).unwrap();
        let target_runtime = node_path::to_native(&field(&raw, "runtimeRoot"));
        for item in instances(&mut raw) {
            item.insert("enabled", false.into());
            let component = item.required("componentId").unwrap().as_str().unwrap();
            let id = item.required("instanceId").unwrap().as_str().unwrap();
            let settings = property(item, "settings");
            if component == "agent-manager" {
                if !case.starts_with("shared") {
                    settings.insert(
                        "codexHome",
                        jp(&target_runtime
                            .join("instances")
                            .join(id)
                            .join("data/native")),
                    );
                }
                settings.insert("internalProjectRoot", jp(&target.join("projects")));
            }
            if component == "host-executor" {
                settings.insert("hostConfigPath", jp(&target_config));
            }
            if component == "hive" {
                settings.insert("listenPort", J::Number(38082.0));
                property(settings, "backup")
                    .insert("directory", jp(&target_runtime.join("hive-backups")));
            }
        }
        match case {
            "enabled" => {
                instances(&mut raw)[0].insert("enabled", true.into());
            }
            "host-identity" => {
                raw.insert("hostId", "other-host".into());
            }
            "instance-identity" => {
                instances(&mut raw)[0].insert("serviceNodeId", "other-node".into());
            }
            "same-endpoint" => {
                raw.insert(
                    "publicBaseUrl",
                    original.required("publicBaseUrl").unwrap().clone(),
                );
            }
            "executables" => {
                property(&mut raw, "executables").insert("node", jp(&root.join("other-node")));
            }
            "prerequisite-valid" | "prerequisite-changed" => {
                let path = root.join("runtime-prerequisite");
                fs::write(&path, b"recorded executable").unwrap();
                manifest.insert(
                    "externalFiles",
                    J::Array(vec![J::object([
                        ("path", jp(&path)),
                        (
                            "hash",
                            if case == "prerequisite-valid" {
                                streaming_hash::file_hash(path).await.unwrap()
                            } else {
                                format!("sha256:{}", "d".repeat(64))
                            }
                            .into(),
                        ),
                        ("bytes", J::Number(19.0)),
                        ("mode", J::Number(384.0)),
                    ])]),
                );
            }
            "existing-root" => {
                fs::create_dir(&target_runtime).unwrap();
            }
            "missing-parent" => {
                raw.insert("stagingRoot", jp(&root.join("absent/staging")));
            }
            "source-overlap" => {
                fs::create_dir(root.join("source-state")).unwrap();
                raw.insert("runtimeRoot", jp(&root.join("source-state/nested")));
            }
            "linked-parent-overlap" => {
                fs::create_dir(root.join("source-state")).unwrap();
                let link = target.join("linked");
                #[cfg(windows)]
                std::os::windows::fs::symlink_dir(root.join("source-state"), &link).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(root.join("source-state"), &link).unwrap();
                raw.insert("runtimeRoot", jp(&link.join("nested")));
            }
            "root-overlap" => {
                raw.insert("stagingRoot", jp(&target.join("artifacts/staging")));
            }
            "original-config" => {
                raw.insert(
                    "configPath",
                    jp(&layout::host_configuration_path(&original).unwrap()),
                );
            }
            "config-in-artifacts" => {
                raw.insert("configPath", jp(&target.join("artifacts/host.json")));
            }
            "config-exists" => {
                raw.insert("configPath", jp(&target.join("host.json")));
                fs::write(target.join("host.json"), b"retained").unwrap();
            }
            "template-is-config" => {
                raw.insert("configPath", jp(&template));
            }
            "manager-wrong-config" => {
                property(instances(&mut raw).last_mut().unwrap(), "settings").insert(
                    "hostConfigPath",
                    jp(&layout::host_configuration_path(&original).unwrap()),
                );
            }
            "hive-same-port" => {
                let port = original
                    .required("instances")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()
                    .required("settings")
                    .unwrap()
                    .required("listenPort")
                    .unwrap()
                    .clone();
                property(instances(&mut raw).last_mut().unwrap(), "settings")
                    .insert("listenPort", port);
            }
            "hive-backup-in-data" => {
                let item = raw
                    .required("instances")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap();
                let data = layout::service_paths(&raw, item).unwrap();
                property(
                    property(instances(&mut raw).last_mut().unwrap(), "settings"),
                    "backup",
                )
                .insert("directory", data.required("data").unwrap().clone());
            }
            "shared-changed" => {
                let settings = property(&mut instances(&mut raw)[1], "settings");
                settings.insert("codexHome", jp(&root.join("changed-shared")));
                property(settings, "appServer")
                    .insert("expectedCodexHome", jp(&root.join("changed-shared")));
            }
            "unicode-dot-paths" => {
                fs::create_dir(target.join("route")).unwrap();
                for (key, suffix) in [
                    ("runtimeRoot", "state Ω"),
                    ("artifactRoot", "artifacts Ω"),
                    ("stagingRoot", "staging Ω"),
                ] {
                    raw.insert(key, jp(&target.join("route").join("..").join(suffix)));
                }
                template = root.join("template Ω.json");
                raw.insert("configPath", jp(&target.join("host Ω.json")));
            }
            "file-parent" => {
                fs::write(target.join("file"), b"regular parent").unwrap();
                raw.insert("stagingRoot", jp(&target.join("file/staging")));
            }
            "backup-overlap" => {
                raw.insert("runtimeRoot", jp(&backup.join("nested")));
            }
            "retained-config-overlap" => {
                raw.insert("configPath", jp(&root.join("retained-project/host.json")));
            }
            #[cfg(windows)]
            case if case.starts_with("opaque-") => {
                use std::os::windows::ffi::OsStringExt;
                let unit = if case.ends_with("high") {
                    0xd800
                } else {
                    0xdfff
                };
                let name = std::ffi::OsString::from_wide(&[104, unit, 46, 106, 115, 111, 110]);
                if case.starts_with("opaque-template") {
                    template = root.join(name);
                    raw.insert("configPath", jp(&template));
                } else {
                    let path = target.join(&name);
                    fs::write(
                        target.join(name.to_string_lossy().as_ref()),
                        b"distinct replacement alias",
                    )
                    .unwrap();
                    raw.insert("configPath", jp(&path));
                }
            }
            _ => (),
        }
        config::atomic_json(template.clone(), raw.clone())
            .await
            .unwrap();
        let current_shared = if case.starts_with("shared") {
            shared(&root.join(if case == "shared-changed" {
                "changed-shared"
            } else {
                "shared-home"
            }))
        } else {
            vec![]
        };
        let b = RestoreBindings {
            storage,
            environment: host.environment,
            native_paths: Arc::new(FixturePolicy {
                exclusions: vec![],
                shared: current_shared,
            }),
        };
        let expected = layout::resolve_host_configuration(
            &raw,
            &node_path::from_native(&template),
            &b.environment,
            b.storage.paths.style,
            &b.storage.account_home,
        )
        .unwrap();
        let before = state(&f.0);
        let result = restore::target_configuration(
            node_path::from_native(&template),
            node_path::from_native(&backup),
            &manifest,
            format!("sha256:{}", "a".repeat(64)).into(),
            "00000000-0000-4000-8000-000000000087".into(),
            &b,
        )
        .await;
        assert_eq!(
            state(&f.0),
            before,
            "{case}: admission changed filesystem state"
        );
        let valid = [
            "valid",
            "prerequisite-valid",
            "template-is-config",
            "owned-native",
            "shared-valid",
            "retained-valid",
            "manager-valid",
            "hive-valid",
            "unicode-dot-paths",
            "opaque-target-high",
            "opaque-target-low",
            "opaque-template-high",
            "opaque-template-low",
        ]
        .contains(&case);
        if valid {
            let mut expected = expected;
            expected.insert(
                "restoredFrom",
                J::object([
                    ("backupId", manifest.required("backupId").unwrap().clone()),
                    ("manifestHash", format!("sha256:{}", "a".repeat(64)).into()),
                    ("restoreId", "00000000-0000-4000-8000-000000000087".into()),
                ]),
            );
            assert_json_eq(
                &result.unwrap_or_else(|error| panic!("{case}: {error:?}")),
                &expected,
            );
        } else {
            let error = result.unwrap_err();
            let code = match case {
                "prerequisite-changed" => "runtime_changed",
                "file-parent" => "internal_error",
                _ => "target_conflict",
            };
            assert_eq!(error.code, code, "{case}");
        }
    }
}
