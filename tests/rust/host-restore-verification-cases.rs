use super::target_cases::{instance, instances, jp, property, shared, state};
use super::*;
use ivy_contracts::{core_schema::validate_host, hash::hash_json};
use ivy_host_runtime::{backup_files, config, layout, streaming_hash};

#[tokio::test]
async fn immutable_backup_and_captured_ownership_checks_match_original() {
    let mut cases = vec![
        "valid",
        "hash-mismatch",
        "failure-marker",
        "duplicate",
        "case-collision",
        "traversal",
        "payload-changed",
        "payload-missing",
        "payload-extra",
        "configuration-hash",
        "configuration-host",
        "captured-layout",
        "retained-valid",
        "retained-overlap",
        "shared-valid",
        "shared-mismatch",
        "shared-missing",
        "ephemeral-mismatch",
        "ephemeral-contained",
        "unicode-dot-paths",
        "root-link",
        "manifest-link",
        "payload-link",
        "manifest-directory",
        "missing-manifest",
        "marker-before-hash",
    ];
    #[cfg(windows)]
    cases.extend(["opaque-high-root", "opaque-low-root"]);
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
        let mut original = super::super::host_configuration(&f);
        original.insert(
            "executables",
            J::object([("node", jp(&root.join("native-executable")))]),
        );
        let mut exclusions = Vec::new();
        let mut shared_homes = Vec::new();
        if case.starts_with("shared")
            || case.starts_with("retained")
            || case.starts_with("ephemeral")
        {
            let runtime = node_path::to_native(&field(&original, "runtimeRoot"));
            let home = runtime.join("instances/agent/data/native");
            let mut settings = J::object([
                ("appServer", J::object([("mode", "owned-stdio".into())])),
                ("codexHome", jp(&home)),
                ("internalProjectRoot", jp(&root.join("source-projects"))),
            ]);
            if case.starts_with("shared") {
                let home = root.join("shared-home");
                settings.insert("codexHome", jp(&home));
                settings.insert(
                    "appServer",
                    J::object([
                        ("mode", "external-proxy".into()),
                        ("expectedCodexHome", jp(&home)),
                    ]),
                );
                shared_homes = shared(&home);
            } else {
                exclusions.push(J::object([
                    ("instanceId", "agent".into()),
                    ("path", "instances/agent/data/native/tmp/arg0".into()),
                    ("kind", "codex-arg0-helper-cache".into()),
                ]));
            }
            instances(&mut original).push(instance("agent", "agent-manager", settings));
        }
        validate_host("HostConfig", &original).unwrap();
        let mut backup = root.join(if case == "unicode-dot-paths" {
            "backup Ω"
        } else {
            "backup"
        });
        #[cfg(windows)]
        if case.starts_with("opaque-") {
            use std::os::windows::ffi::OsStringExt;
            let unit = if case == "opaque-high-root" {
                0xd800
            } else {
                0xdfff
            };
            let name = std::ffi::OsString::from_wide(&[98, unit]);
            backup = root.join(&name);
            let replacement = root.join(name.to_string_lossy().as_ref());
            fs::create_dir(&replacement).unwrap();
            fs::write(
                replacement.join("backup.json"),
                b"distinct replacement alias",
            )
            .unwrap();
        }
        fs::create_dir_all(backup.join("state")).unwrap();
        config::atomic_json(backup.join("configuration.json"), original.clone())
            .await
            .unwrap();
        fs::write(backup.join("state/payload"), b"immutable payload").unwrap();
        let mut captured = layout::host_storage_areas(&original, &storage).unwrap();
        if case.starts_with("retained") {
            let path = if case == "retained-overlap" {
                original.required("runtimeRoot").unwrap().clone()
            } else {
                jp(&root.join("retained-project"))
            };
            captured.push(StorageArea {
                path: text(&path),
                key: format!(
                    "retained-projects/agent/{}",
                    &hash_json(&path).unwrap()[7..]
                )
                .into(),
            });
        }
        if case == "ephemeral-contained" {
            let path = backup.join("state/instances/agent/data/native/tmp/arg0");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"excluded native cache").unwrap();
        }
        let records = backup_files::inventory_files(backup.clone(), None)
            .await
            .unwrap();
        let mut files = records.iter().map(|file| file.json()).collect::<Vec<_>>();
        match case {
            "duplicate" => files.push(files[0].clone()),
            "case-collision" => {
                let mut file = files[0].clone();
                let name = file
                    .required("path")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_uppercase();
                file.insert("path", name.into());
                files.push(file);
            }
            "traversal" => {
                files[0].insert("path", "../unsafe".into());
            }
            _ => (),
        }
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
            ("files", J::Array(files)),
            ("externalFiles", J::Array(vec![])),
            ("hiveSnapshots", J::Array(vec![])),
            ("ephemeralNativePaths", J::Array(exclusions.clone())),
            ("sharedNativeHomes", J::Array(shared_homes.clone())),
            (
                "storageAreas",
                J::Array(captured.iter().map(StorageArea::json).collect()),
            ),
            ("excluded", J::Array(vec![])),
        ]);
        match case {
            "configuration-hash" => {
                manifest.insert(
                    "configurationHash",
                    format!("sha256:{}", "f".repeat(64)).into(),
                );
            }
            "configuration-host" => {
                manifest.insert("hostId", "other-host".into());
            }
            "captured-layout" => {
                manifest.insert(
                    "storageAreas",
                    J::Array(vec![J::object([
                        ("key", "state".into()),
                        ("path", jp(&root.join("wrong-state"))),
                    ])]),
                );
            }
            "shared-mismatch" => {
                let J::Array(homes) = property(&mut manifest, "sharedNativeHomes") else {
                    unreachable!()
                };
                homes[0].insert("path", jp(&root.join("other-home")));
            }
            "shared-missing" => {
                manifest.insert("sharedNativeHomes", J::Array(vec![]));
            }
            "ephemeral-mismatch" => {
                manifest.insert("ephemeralNativePaths", J::Array(vec![]));
            }
            _ => (),
        }
        validate_host("BackupManifest", &manifest).unwrap();
        let manifest_path = backup.join("backup.json");
        config::atomic_json(manifest_path.clone(), manifest.clone())
            .await
            .unwrap();
        let mut manifest_hash: JsString = streaming_hash::file_hash(manifest_path.clone())
            .await
            .unwrap()
            .into();
        let mut directory = backup.clone();
        match case {
            "hash-mismatch" => {
                manifest_hash = format!("sha256:{}", "e".repeat(64)).into();
            }
            "failure-marker" | "marker-before-hash" => {
                fs::write(backup.join("backup-failure.json"), b"failure").unwrap();
                if case == "marker-before-hash" {
                    manifest_hash = format!("sha256:{}", "e".repeat(64)).into();
                }
            }
            "payload-changed" => {
                fs::write(backup.join("state/payload"), b"changed payload").unwrap();
            }
            "payload-missing" => {
                fs::remove_file(backup.join("state/payload")).unwrap();
            }
            "payload-extra" => {
                fs::write(backup.join("extra"), b"unexpected").unwrap();
            }
            "unicode-dot-paths" => {
                fs::create_dir(root.join("route")).unwrap();
                directory = root.join("route").join("..").join("backup Ω");
            }
            "root-link" => {
                directory = root.join("backup-alias");
                #[cfg(windows)]
                std::os::windows::fs::symlink_dir(&backup, &directory).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(&backup, &directory).unwrap();
            }
            "manifest-link" => {
                let source = root.join("sealed-manifest.json");
                fs::rename(&manifest_path, &source).unwrap();
                #[cfg(windows)]
                std::os::windows::fs::symlink_file(&source, &manifest_path).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(&source, &manifest_path).unwrap();
            }
            "payload-link" => {
                let source = root.join("sealed-payload");
                let path = backup.join("state/payload");
                fs::rename(&path, &source).unwrap();
                #[cfg(windows)]
                std::os::windows::fs::symlink_file(&source, &path).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(&source, &path).unwrap();
            }
            "manifest-directory" => {
                fs::remove_file(&manifest_path).unwrap();
                fs::create_dir(&manifest_path).unwrap();
            }
            "missing-manifest" => {
                fs::remove_file(&manifest_path).unwrap();
            }
            _ => (),
        }
        let b = RestoreBindings {
            storage,
            environment: host.environment,
            native_paths: Arc::new(FixturePolicy {
                exclusions,
                shared: shared_homes,
            }),
        };
        let before = state(&f.0);
        let result =
            restore::verify_backup(node_path::from_native(&directory), manifest_hash, &b).await;
        assert_eq!(
            state(&f.0),
            before,
            "{case}: verification changed filesystem state"
        );
        if [
            "valid",
            "retained-valid",
            "shared-valid",
            "unicode-dot-paths",
            "opaque-high-root",
            "opaque-low-root",
        ]
        .contains(&case)
        {
            assert_json_eq(
                &result.unwrap_or_else(|error| panic!("{case}: {error:?}")),
                &manifest,
            );
        } else {
            let error = result.unwrap_err();
            let code = match case {
                "hash-mismatch" | "payload-changed" | "payload-missing" | "payload-extra" => {
                    "backup_changed"
                }
                "case-collision" if !cfg!(windows) => "backup_changed",
                "root-link" | "payload-link" => "target_conflict",
                "missing-manifest" => "internal_error",
                _ => "backup_invalid",
            };
            assert_eq!(error.code, code, "{case}");
        }
    }
}
