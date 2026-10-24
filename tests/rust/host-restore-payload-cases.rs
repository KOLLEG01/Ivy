use super::target_cases::{Entry, instance, instances, jp, property, state};
use super::*;
use ivy_contracts::{core_schema::validate_host, hash::hash_json};
use ivy_host_runtime::{backup_files, config, journal::HostJournal, layout};
use ivy_store::rusqlite::Connection;
use std::time::Duration;

const RESERVED_FILES: [&str; 4] = [
    "restore.json",
    "host.json",
    "native-recovery.json",
    "project-recovery.json",
];
const RESERVED_DIRECTORIES: [&str; 3] = [
    "restore-decisions",
    "recovery-original",
    "previous-recovery",
];

fn completed_recovery(path: &Path, expected: &[u8]) -> bool {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // An exclusive reader also observes the final copy's closed writer.
        options.share_mode(0);
    }
    let Ok(mut file) = options.open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).is_ok() && bytes == expected
}

#[tokio::test]
async fn restore_payload_copy_preserves_authority_and_reserves_old_recovery_names() {
    let mut cases = vec![
        "valid",
        "external-area",
        "legacy-areas",
        "changed-source",
        "missing-journal",
        "existing-root",
        "retained-area",
        "unicode-dot-paths",
        "source-link",
        "same-size-source-change",
        "dropped-observer",
        "relative-root",
        "relative-nested-root",
    ];
    #[cfg(windows)]
    cases.extend([
        "opaque-backup-high",
        "opaque-backup-low",
        "opaque-runtime-high",
        "opaque-runtime-low",
    ]);
    for case in cases {
        let f = ServiceFixture::new();
        let root = f.0.join("restore");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
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
        if case == "external-area" {
            instances(&mut original)[0].insert(
                "paths",
                J::object([
                    ("data", jp(&root.join("source-data"))),
                    (
                        "work",
                        jp(&root.join("source-state/instances/fixture/work")),
                    ),
                    (
                        "logs",
                        jp(&root.join("source-state/instances/fixture/logs")),
                    ),
                ]),
            );
        }
        if case == "retained-area" {
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
        }
        validate_host("HostConfig", &original).unwrap();
        let journal = HostJournal::open(original.clone()).await.unwrap();
        journal.close().await.unwrap();
        let source_journal = root.join("source-state/deployments.sqlite");
        {
            let db = Connection::open(&source_journal).unwrap();
            for (n, phase) in ["preparing", "succeeded"].iter().enumerate() {
                let operation = format!("restore-op-{n}");
                let deployment = format!("deployment-{n}");
                let hash = format!("sha256:{}", "a".repeat(64));
                let entry =
                    J::object([("phase", (*phase).into()), ("authority", "captured".into())]);
                db.execute(
                    "INSERT INTO operations VALUES (?,?,?,?,?,?)",
                    ivy_store::rusqlite::params![
                        operation,
                        deployment,
                        hash,
                        phase,
                        "2026-10-20T00:00:00.000Z",
                        json::canonical(&entry, usize::MAX).unwrap(),
                    ],
                )
                .unwrap();
            }
        }
        let source_journal_bytes = fs::read(&source_journal).unwrap();
        let mut backup = root.join("backup");
        let mut runtime = target.join("state");
        #[cfg(windows)]
        if case.starts_with("opaque-") {
            use std::os::windows::ffi::OsStringExt;
            let unit = if case.ends_with("high") {
                0xd800
            } else {
                0xdfff
            };
            let name = std::ffi::OsString::from_wide(&[112, unit]);
            let parent = if case.starts_with("opaque-backup") {
                &root
            } else {
                &target
            };
            let path = parent.join(&name);
            let replacement = parent.join(name.to_string_lossy().as_ref());
            fs::create_dir(&replacement).unwrap();
            fs::write(
                replacement.join("alias.txt"),
                b"distinct replacement authority",
            )
            .unwrap();
            if case.starts_with("opaque-backup") {
                backup = path;
            } else {
                runtime = path;
            }
        }
        if case == "unicode-dot-paths" {
            backup = root.join("backup Ω");
            runtime = target.join("state Ω");
        }
        fs::create_dir_all(backup.join("state")).unwrap();
        fs::copy(&source_journal, backup.join("state/deployments.sqlite")).unwrap();
        config::atomic_json(backup.join("configuration.json"), original.clone())
            .await
            .unwrap();
        for member in RESERVED_FILES.into_iter().chain([
            "restore-decisions/prior.json",
            "recovery-original/prior.txt",
            "previous-recovery/prior.txt",
            "accepted-configurations/accepted.json",
            "payload.txt",
        ]) {
            let path = backup.join("state").join(member);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("captured {member}")).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                backup.join("state/payload.txt"),
                fs::Permissions::from_mode(0o640),
            )
            .unwrap();
        }
        #[cfg(windows)]
        if case == "unicode-dot-paths" {
            use std::os::windows::ffi::OsStringExt;
            let mut names = vec![
                "name".into(),
                "name.".into(),
                "name ".into(),
                "opaque�".into(),
            ];
            for unit in [0xd800, 0xdfff] {
                names.push(std::ffi::OsString::from_wide(&[
                    111, 112, 97, 113, 117, 101, unit,
                ]));
            }
            for (n, name) in names.into_iter().enumerate() {
                fs::write(backup.join("state").join(name), format!("native name {n}")).unwrap();
            }
        }
        if case == "dropped-observer" {
            fs::write(
                backup.join("state/large-payload"),
                vec![0x86; 16 * 1024 * 1024],
            )
            .unwrap();
        }
        let mut captured = layout::host_storage_areas(&original, &storage).unwrap();
        let mut destinations = vec![(JsString::from("state"), runtime.clone())];
        if case == "external-area" {
            let path = backup.join("services/fixture/data/payload.txt");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"separate owned storage").unwrap();
            destinations.push(("services/fixture/data".into(), target.join("external-data")));
        }
        if case == "retained-area" {
            let retained = jp(&root.join("retained-project"));
            let hash = hash_json(&retained).unwrap()[7..].to_owned();
            let key = format!("retained-projects/agent/{hash}");
            captured.push(StorageArea {
                key: key.clone().into(),
                path: text(&retained),
            });
            for (key, destination) in [
                ("projects/agent".to_owned(), target.join("projects")),
                (
                    key,
                    target.join("projects").join(format!("restored-{hash}")),
                ),
            ] {
                let path = backup.join(&key).join("payload.txt");
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, format!("captured {key}")).unwrap();
                destinations.push((key.into(), destination));
            }
        }
        fs::create_dir(backup.join("ignored")).unwrap();
        fs::write(
            backup.join("ignored/not-storage"),
            b"not a mapped storage member",
        )
        .unwrap();
        if case == "missing-journal" {
            fs::remove_file(backup.join("state/deployments.sqlite")).unwrap();
        }
        let mut configuration = original.clone();
        configuration.insert("runtimeRoot", jp(&runtime));
        if case.starts_with("relative-") {
            configuration.insert(
                "runtimeRoot",
                if case == "relative-root" {
                    "unadmitted-root".into()
                } else {
                    "unadmitted/ancestor/root".into()
                },
            );
        }
        configuration.insert("artifactRoot", jp(&target.join("artifacts")));
        configuration.insert("stagingRoot", jp(&target.join("staging")));
        configuration.insert("configPath", jp(&target.join("restored-host.json")));
        configuration.insert("publicBaseUrl", "http://127.0.0.1:38082/ivy".into());
        for i in instances(&mut configuration) {
            i.insert("enabled", false.into());
        }
        if case == "external-area" {
            let paths = property(&mut instances(&mut configuration)[0], "paths");
            paths.insert("data", jp(&target.join("external-data")));
            paths.insert("work", jp(&runtime.join("instances/fixture/work")));
            paths.insert("logs", jp(&runtime.join("instances/fixture/logs")));
        }
        if case == "retained-area" {
            let settings = property(&mut instances(&mut configuration)[1], "settings");
            settings.insert(
                "codexHome",
                jp(&runtime.join("instances/agent/data/native")),
            );
            settings.insert("internalProjectRoot", jp(&target.join("projects")));
        }
        configuration.insert(
            "restoredFrom",
            J::object([
                ("backupId", "00000000-0000-4000-8000-000000000086".into()),
                ("manifestHash", format!("sha256:{}", "a".repeat(64)).into()),
                ("restoreId", "00000000-0000-4000-8000-000000000087".into()),
            ]),
        );
        validate_host("HostConfig", &configuration).unwrap();
        let records = backup_files::inventory_files(backup.clone(), None)
            .await
            .unwrap();
        let total = records.iter().map(|v| v.bytes).sum::<u64>() as f64;
        let mut manifest = J::object([
            ("schemaVersion", J::Number(1.0)),
            (
                "backupId",
                configuration
                    .required("restoredFrom")
                    .unwrap()
                    .required("backupId")
                    .unwrap()
                    .clone(),
            ),
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
            (
                "files",
                J::Array(records.iter().map(|v| v.json()).collect()),
            ),
            ("externalFiles", J::Array(vec![])),
            ("hiveSnapshots", J::Array(vec![])),
            ("excluded", J::Array(vec![])),
        ]);
        if case != "legacy-areas" {
            manifest.insert(
                "storageAreas",
                J::Array(captured.iter().map(|v| v.json()).collect()),
            );
        }
        validate_host("BackupManifest", &manifest).unwrap();
        let payload = backup.join("state/payload.txt");
        if case == "changed-source" {
            fs::write(&payload, b"changed captured bytes").unwrap();
        }
        if case == "same-size-source-change" {
            let mut bytes = fs::read(&payload).unwrap();
            bytes[0] ^= 1;
            fs::write(&payload, bytes).unwrap();
        }
        if case == "source-link" {
            fs::remove_file(&payload).unwrap();
            let external = root.join("outside-payload");
            fs::write(&external, b"outside authority").unwrap();
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(&external, &payload).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&external, &payload).unwrap();
        }
        if case == "existing-root" {
            fs::create_dir(&runtime).unwrap();
            fs::write(runtime.join("owner.txt"), b"preexisting authority").unwrap();
        }
        let captured_bytes = fs::read(backup.join("configuration.json")).unwrap();
        let mut backup_argument = backup.clone();
        if case == "unicode-dot-paths" {
            fs::create_dir(root.join("route")).unwrap();
            backup_argument = root.join("route/..").join(backup.file_name().unwrap());
            configuration.insert(
                "runtimeRoot",
                jp(&target.join("route/..").join(runtime.file_name().unwrap())),
            );
            fs::create_dir(target.join("route")).unwrap();
        }
        let before = state(&root);
        let b = Arc::new(RestoreBindings {
            storage,
            environment: host.environment,
            native_paths: Arc::new(FixturePolicy {
                exclusions: vec![],
                shared: vec![],
            }),
        });
        let action = restore::copy_restore_payload(
            node_path::from_native(&backup_argument),
            manifest,
            configuration.clone(),
            b,
            PathBuf::from(std::env::var_os("IVY_BACKUP_JOB").expect("actual Job helper")),
        );
        let result = if case == "dropped-observer" {
            let waiter = tokio::spawn(action);
            tokio::time::timeout(Duration::from_secs(30), async {
                while !runtime.exists() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("private root must be created before cancelling observer");
            assert!(
                !runtime
                    .join("recovery-original/configuration.json")
                    .exists()
            );
            waiter.abort();
            assert!(waiter.await.unwrap_err().is_cancelled());
            tokio::time::timeout(Duration::from_secs(30), async {
                while !completed_recovery(
                    &runtime.join("recovery-original/configuration.json"),
                    &captured_bytes,
                ) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("admitted payload copy must finish after observer is dropped");
            Ok(total)
        } else {
            tokio::time::timeout(Duration::from_secs(30), action)
                .await
                .expect("payload operation must terminate")
        };
        assert!(
            !layout::host_configuration_path(&configuration)
                .unwrap()
                .exists(),
            "{case}"
        );
        assert!(!target.join("ignored").exists(), "{case}");
        assert_eq!(
            fs::read(&source_journal).unwrap(),
            source_journal_bytes,
            "{case}"
        );
        let after = state(&root);
        assert_eq!(
            before
                .iter()
                .filter(|(p, _)| !p.starts_with("target"))
                .collect::<Vec<_>>(),
            after
                .iter()
                .filter(|(p, _)| !p.starts_with("target"))
                .collect::<Vec<_>>(),
            "source changed: {case}"
        );
        if [
            "changed-source",
            "same-size-source-change",
            "missing-journal",
            "source-link",
            "existing-root",
            "relative-root",
            "relative-nested-root",
        ]
        .contains(&case)
        {
            let code = match case {
                "changed-source" | "same-size-source-change" => "backup_changed",
                "missing-journal" => "internal_error",
                "relative-root" | "relative-nested-root" => "invalid_arguments",
                _ => "target_conflict",
            };
            assert_eq!(result.unwrap_err().code, code, "{case}");
            assert!(
                !runtime
                    .join("recovery-original/configuration.json")
                    .exists(),
                "{case}"
            );
            if case == "existing-root" || case.starts_with("relative-") {
                assert_eq!(before, after, "{case}");
            }
            continue;
        }
        assert_eq!(result.unwrap(), total, "{case}");
        for record in &records {
            let Some((key, destination)) = destinations.iter().find(|(key, _)| {
                record
                    .path
                    .0
                    .starts_with(&[key.0.clone(), vec![47]].concat())
            }) else {
                continue;
            };
            let local = node_path::to_native(&JsString(record.path.0[key.0.len() + 1..].to_vec()));
            let reserved = key == &JsString::from("state")
                && (RESERVED_FILES.iter().any(|name| local == Path::new(name))
                    || RESERVED_DIRECTORIES
                        .iter()
                        .any(|name| local.starts_with(name)));
            let output = if reserved {
                destination.join("previous-recovery").join(local)
            } else {
                destination.join(local)
            };
            assert_eq!(
                fs::read(&output).unwrap(),
                fs::read(backup.join(node_path::to_native(&record.path))).unwrap(),
                "{case}: {output:?}"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&output).unwrap().permissions().mode() & 0o777,
                    record.mode,
                    "{case}"
                );
            }
        }
        for name in RESERVED_FILES {
            assert!(!runtime.join(name).exists(), "{case}");
        }
        assert_eq!(
            fs::read(runtime.join("recovery-original/deployments.sqlite")).unwrap(),
            source_journal_bytes,
            "{case}"
        );
        assert_eq!(
            fs::read(runtime.join("recovery-original/configuration.json")).unwrap(),
            captured_bytes,
            "{case}"
        );
        let working = runtime.join("deployments.sqlite");
        let detached = target.join("verified-working.sqlite");
        fs::copy(&working, &detached).unwrap();
        let db = Connection::open(&detached).unwrap();
        assert_eq!(
            db.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok",
            "{case}"
        );
        db.execute("UPDATE operations SET phase='needs_attention'", [])
            .unwrap();
        drop(db);
        fs::copy(&detached, &working).unwrap();
        assert_ne!(fs::read(&working).unwrap(), source_journal_bytes, "{case}");
        assert_eq!(
            fs::read(runtime.join("recovery-original/deployments.sqlite")).unwrap(),
            source_journal_bytes,
            "{case}"
        );
        assert_eq!(
            fs::read(backup.join("state/deployments.sqlite")).unwrap(),
            source_journal_bytes,
            "{case}"
        );
        for (p, entry) in before.iter().filter(|(p, _)| p.starts_with("target")) {
            if matches!(entry, Entry::File(_)) {
                assert_eq!(
                    after.get(p),
                    Some(entry),
                    "replacement alias changed: {case}"
                );
            }
        }
    }
}
