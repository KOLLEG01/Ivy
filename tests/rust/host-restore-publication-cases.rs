use super::target_cases::{instance, instances, jp, state};
use super::*;
use ivy_contracts::{core_schema::validate_host, hash::hash_json};
use ivy_host_runtime::{backup_files, config, journal::HostJournal, layout};
use ivy_store::rusqlite::{Connection, params};
use std::{
    future::{Future, poll_fn},
    task::Poll,
    time::{Duration, Instant},
};

const AT: &str = "2026-10-20T00:00:00.000Z";

pub(super) fn captured_entry(root: &Path, n: usize, phase: &str) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        (
            "request",
            J::object([
                ("action", "restart".into()),
                ("operationId", format!("restore-op-{n}").into()),
                ("instanceId", "fixture".into()),
            ]),
        ),
        (
            "record",
            J::object([
                ("deploymentId", format!("deployment-{n}").into()),
                ("hostId", "restore-fixture".into()),
                ("instanceId", "fixture".into()),
                ("componentId", "fixture".into()),
                ("requestHash", format!("sha256:{}", "a".repeat(64)).into()),
                ("previousBuild", J::Null),
                ("targetBuild", format!("sha256:{}", "b".repeat(64)).into()),
                ("observedBuild", J::Null),
                ("phase", phase.into()),
                ("createdAt", AT.into()),
                ("updatedAt", AT.into()),
                (
                    "readiness",
                    J::object([("state", "not_run".into()), ("message", "".into())]),
                ),
            ]),
        ),
        ("candidateId", J::Null),
        ("previousCandidateId", J::Null),
        (
            "configurationPath",
            jp(&root.join(format!("old-config/accepted-{n}.json"))),
        ),
        (
            "activation",
            J::object([
                ("previousTarget", J::Null),
                ("previousEnabled", true.into()),
                ("targetEnabled", true.into()),
                ("rollbackTargetRevision", J::Null),
                (
                    "previousConfigurationPath",
                    jp(&root.join(format!("old-config/prior-{n}.json"))),
                ),
            ]),
        ),
    ])
}

#[tokio::test]
async fn restore_final_publication_reconciles_only_working_state_and_stays_disabled() {
    let mut cases = vec![
        "valid",
        "external-area",
        "report-accounting",
        "control-link",
        "health-directory",
        "config-publication-failure",
        "wrong-journal-host",
        "control-directory",
        "missing-controls",
        "dangling-control-link",
        "unicode-dot-data",
        "dropped-observer",
    ];
    #[cfg(windows)]
    cases.extend([
        "opaque-data-high",
        "opaque-data-low",
        "opaque-config-high",
        "opaque-config-low",
    ]);
    for case in cases {
        let f = ServiceFixture::new();
        let root = f.0.join("restore");
        let target = root.join("target");
        let runtime = target.join("state");
        fs::create_dir_all(&runtime).unwrap();
        let host = HostContext::current().unwrap();
        let storage = StorageContext {
            paths: host.paths,
            account_home: host.account_home,
            compare: Arc::new(|a, b| a.cmp(b)),
        };
        let original = J::object([
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
        validate_host("HostConfig", &original).unwrap();
        let journal = HostJournal::open(original.clone()).await.unwrap();
        journal.close().await.unwrap();
        let source_journal = root.join("source-state/deployments.sqlite");
        let entries = [
            captured_entry(&root, 0, "preparing"),
            captured_entry(&root, 1, "succeeded"),
        ];
        {
            let db = Connection::open(&source_journal).unwrap();
            for entry in &entries {
                validate_host("JournalEntry", entry).unwrap();
                let record = entry.required("record").unwrap();
                db.execute(
                    "INSERT INTO operations VALUES (?,?,?,?,?,?)",
                    params![
                        entry
                            .required("request")
                            .unwrap()
                            .required("operationId")
                            .unwrap()
                            .as_str()
                            .unwrap(),
                        record.required("deploymentId").unwrap().as_str().unwrap(),
                        record.required("requestHash").unwrap().as_str().unwrap(),
                        record.required("phase").unwrap().as_str().unwrap(),
                        AT,
                        json::canonical(entry, usize::MAX).unwrap(),
                    ],
                )
                .unwrap();
            }
            db.execute_batch("INSERT INTO candidates VALUES ('old','fixture','old-build','{}','{}');
                INSERT INTO candidate_retirements VALUES ('old','{}');
                INSERT INTO installed VALUES ('fixture','{}');
                INSERT INTO observations VALUES ('fixture','{}');
                INSERT INTO runtime_targets VALUES ('fixture','{}');
                INSERT INTO meta VALUES ('storage-retention','{}'),('bootstrap-rollback:old','{}'),('fixture-retained','keep');").unwrap();
        }
        let source_bytes = fs::read(&source_journal).unwrap();
        let backup = root.join("backup");
        fs::create_dir_all(backup.join("state")).unwrap();
        fs::copy(&source_journal, backup.join("state/deployments.sqlite")).unwrap();
        config::atomic_json(backup.join("configuration.json"), original.clone())
            .await
            .unwrap();
        let configuration_bytes = fs::read(backup.join("configuration.json")).unwrap();
        let mut configuration = original.clone();
        configuration.insert("runtimeRoot", jp(&runtime));
        configuration.insert("artifactRoot", jp(&target.join("artifacts")));
        configuration.insert("stagingRoot", jp(&target.join("staging")));
        configuration.insert("publicBaseUrl", "http://127.0.0.1:38082/ivy".into());
        let mut config_path = target.join("restored-host.json");
        let mut data = runtime.join("instances/fixture/data");
        if case == "external-area" {
            data = target.join("external-data");
        }
        if case == "unicode-dot-data" {
            fs::create_dir_all(target.join("external Ω/route")).unwrap();
            data = target.join("external Ω/route/../data Ω");
        }
        #[cfg(windows)]
        if case.starts_with("opaque-") {
            use std::os::windows::ffi::OsStringExt;
            let unit = if case.ends_with("high") {
                0xd800
            } else {
                0xdfff
            };
            let name = std::ffi::OsString::from_wide(&[112, unit]);
            let replacement = target.join(name.to_string_lossy().as_ref());
            if case.starts_with("opaque-data") {
                data = target.join(&name);
                fs::create_dir(&replacement).unwrap();
                for name in ["health.json", "control.json", "kept.json"] {
                    fs::write(replacement.join(name), b"distinct replacement authority").unwrap();
                }
            } else {
                config_path = target.join(&name);
                fs::write(replacement, b"distinct replacement authority").unwrap();
            }
        }
        configuration.insert("configPath", jp(&config_path));
        let i = &mut instances(&mut configuration)[0];
        i.insert("enabled", false.into());
        i.insert(
            "paths",
            J::object([
                ("data", jp(&data)),
                ("work", jp(&runtime.join("instances/fixture/work"))),
                ("logs", jp(&runtime.join("instances/fixture/logs"))),
            ]),
        );
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
        let total = records.iter().map(|file| file.bytes).sum::<u64>()
            + 31
            + if case == "report-accounting" { 22 } else { 0 };
        let manifest = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("backupId", "00000000-0000-4000-8000-000000000086".into()),
            ("hostId", "restore-fixture".into()),
            ("createdAt", AT.into()),
            ("completedAt", AT.into()),
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
                J::Array(records.iter().map(|file| file.json()).collect()),
            ),
            (
                "storageAreas",
                J::Array(
                    layout::host_storage_areas(&original, &storage)
                        .unwrap()
                        .iter()
                        .map(|v| v.json())
                        .collect(),
                ),
            ),
            ("externalFiles", J::Array(vec![])),
            ("hiveSnapshots", J::Array(vec![])),
            ("excluded", J::Array(vec![])),
        ]);
        validate_host("BackupManifest", &manifest).unwrap();
        let recovery = runtime.join("recovery-original");
        fs::create_dir(&recovery).unwrap();
        let working = runtime.join("deployments.sqlite");
        fs::copy(&source_journal, &working).unwrap();
        fs::copy(&source_journal, recovery.join("deployments.sqlite")).unwrap();
        fs::copy(
            backup.join("configuration.json"),
            recovery.join("configuration.json"),
        )
        .unwrap();
        let native_recovery = J::object([(
            "instances",
            J::Array(if case == "report-accounting" {
                vec![J::object([(
                    "originalFiles",
                    J::Array(vec![
                        J::object([("bytes", J::Number(17.0))]),
                        J::object([("bytes", J::Number(5.0))]),
                    ]),
                )])]
            } else {
                vec![]
            }),
        )]);
        config::atomic_json(
            runtime.join("native-recovery.json"),
            native_recovery.clone(),
        )
        .await
        .unwrap();
        for directory in [&data, &target.join("artifacts"), &target.join("staging")] {
            fs::create_dir_all(directory).unwrap();
        }
        let health = data.join("health.json");
        let control = data.join("control.json");
        fs::write(
            &health,
            br#"{"launchId":"captured-launch","name":"health.json"}"#,
        )
        .unwrap();
        fs::write(
            &control,
            br#"{"launchId":"captured-launch","name":"control.json"}"#,
        )
        .unwrap();
        fs::write(data.join("kept.json"), b"retained service payload").unwrap();
        if case == "unicode-dot-data" {
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStringExt;
                for (n, name) in [
                    std::ffi::OsString::from("health.json."),
                    "control.json ".into(),
                    std::ffi::OsString::from_wide(&[107, 0xd800]),
                    std::ffi::OsString::from_wide(&[107, 0xdfff]),
                    "k�".into(),
                ]
                .into_iter()
                .enumerate()
                {
                    fs::write(data.join(name), format!("distinct retained bytes {n}")).unwrap();
                }
            }
        }
        if ["health-directory", "control-directory"].contains(&case) {
            let path = if case == "health-directory" {
                &health
            } else {
                &control
            };
            fs::remove_file(path).unwrap();
            fs::create_dir(path).unwrap();
        }
        if case == "missing-controls" {
            fs::remove_file(&health).unwrap();
            fs::remove_file(&control).unwrap();
        }
        if ["control-link", "dangling-control-link"].contains(&case) {
            fs::remove_file(&control).unwrap();
            let external = root.join("outside-control");
            if case == "control-link" {
                fs::write(&external, b"external evidence").unwrap();
            }
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(&external, &control).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&external, &control).unwrap();
        }
        if case == "config-publication-failure" {
            fs::create_dir(&config_path).unwrap();
        }
        if case == "wrong-journal-host" {
            let db = Connection::open(&working).unwrap();
            db.execute("UPDATE meta SET value='different' WHERE key='hostId'", [])
                .unwrap();
        }
        let before = state(&root);
        let bindings = Arc::new(RestoreBindings {
            storage,
            environment: host.environment,
            native_paths: Arc::new(FixturePolicy {
                exclusions: vec![],
                shared: vec![],
            }),
        });
        let mut action = Box::pin(restore::publish_restored_host(
            configuration.clone(),
            manifest.clone(),
            native_recovery,
            31,
            AT.into(),
            Instant::now(),
            bindings,
        ));
        let result = if case == "dropped-observer" {
            poll_fn(|cx| {
                assert!(action.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(action);
            tokio::time::timeout(Duration::from_secs(30), async {
                while !config_path.is_file() {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("admitted publication must finish after observer is dropped");
            None
        } else {
            Some(
                tokio::time::timeout(Duration::from_secs(30), action)
                    .await
                    .expect("publication must terminate"),
            )
        };
        let failure = [
            "health-directory",
            "control-directory",
            "config-publication-failure",
            "wrong-journal-host",
        ]
        .contains(&case);
        if failure {
            let error = result.unwrap().unwrap_err();
            assert_eq!(
                error.code,
                if case == "wrong-journal-host" {
                    "unsupported_storage"
                } else {
                    "internal_error"
                },
                "{case}"
            );
            assert!(!config_path.is_file(), "{case}");
        } else {
            let published = config::json_file(config_path.clone(), config::CONFIGURATION_BYTES)
                .await
                .unwrap();
            assert_json_eq(&published, &configuration);
            assert_eq!(
                published.required("instances").unwrap().as_array().unwrap()[0].get("enabled"),
                Some(&false.into()),
                "{case}"
            );
            if let Some(result) = result {
                let mut result = result.unwrap();
                validate_host("RestoreResult", &result).unwrap();
                assert!(matches!(result.get("elapsedMs"), Some(J::Number(value)) if *value>=0.0));
                result.remove("elapsedMs");
                assert_json_eq(
                    &result,
                    &J::object([
                        ("schemaVersion", J::Number(1.0)),
                        (
                            "restoreId",
                            configuration
                                .required("restoredFrom")
                                .unwrap()
                                .required("restoreId")
                                .unwrap()
                                .clone(),
                        ),
                        ("backupId", manifest.required("backupId").unwrap().clone()),
                        (
                            "manifestHash",
                            configuration
                                .required("restoredFrom")
                                .unwrap()
                                .required("manifestHash")
                                .unwrap()
                                .clone(),
                        ),
                        (
                            "configPath",
                            jp(&layout::host_configuration_path(&configuration).unwrap()),
                        ),
                        ("state", "disabled".into()),
                        ("restoredBytes", J::Number(total as f64)),
                    ]),
                );
            }
        }
        assert_eq!(fs::read(&source_journal).unwrap(), source_bytes, "{case}");
        assert_eq!(
            fs::read(backup.join("state/deployments.sqlite")).unwrap(),
            source_bytes,
            "{case}"
        );
        assert_eq!(
            fs::read(recovery.join("deployments.sqlite")).unwrap(),
            source_bytes,
            "{case}"
        );
        assert_eq!(
            fs::read(recovery.join("configuration.json")).unwrap(),
            configuration_bytes,
            "{case}"
        );
        if case == "wrong-journal-host" {
            assert_eq!(
                state(&root),
                before,
                "identity failure must precede removal/publication"
            );
            continue;
        }
        assert_eq!(health.is_dir(), case == "health-directory", "{case}");
        assert_eq!(health.exists(), case == "health-directory", "{case}");
        assert_eq!(control.is_dir(), case == "control-directory", "{case}");
        assert_eq!(
            control.exists(),
            ["health-directory", "control-directory"].contains(&case),
            "{case}"
        );
        let db = Connection::open(&working).unwrap();
        assert_eq!(
            db.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok",
            "{case}"
        );
        for table in [
            "candidates",
            "candidate_retirements",
            "installed",
            "observations",
            "runtime_targets",
        ] {
            assert_eq!(
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                0,
                "{case}: {table}"
            );
        }
        assert_eq!(db.query_row("SELECT count(*) FROM meta WHERE key='storage-retention' OR key LIKE 'bootstrap-rollback:%'",[],|row|row.get::<_,i64>(0)).unwrap(),0,"{case}");
        assert_eq!(
            db.query_row(
                "SELECT value FROM meta WHERE key='fixture-retained'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "keep",
            "{case}"
        );
        assert_eq!(
            db.query_row(
                "SELECT value FROM meta WHERE key='installation'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            ivy_host_runtime::accepted_configuration::installation_record(&configuration).unwrap(),
            "{case}"
        );
        for (n, before) in entries.iter().enumerate() {
            let encoded: String = db
                .query_row(
                    "SELECT entry_json FROM operations WHERE deployment_id=?",
                    [format!("deployment-{n}")],
                    |row| row.get(0),
                )
                .unwrap();
            let after = json::parse(&encoded, usize::MAX).unwrap();
            validate_host("JournalEntry", &after).unwrap();
            assert_json_eq(
                after.required("request").unwrap(),
                before.required("request").unwrap(),
            );
            assert_eq!(
                after.get("configurationPath"),
                Some(&jp(&runtime
                    .join("accepted-configurations")
                    .join(format!("accepted-{n}.json")))),
                "{case}"
            );
            assert_eq!(
                after
                    .required("activation")
                    .unwrap()
                    .get("previousConfigurationPath"),
                Some(&jp(&runtime
                    .join("accepted-configurations")
                    .join(format!("prior-{n}.json")))),
                "{case}"
            );
            let record = after.required("record").unwrap();
            if n == 1 {
                assert_json_eq(record, before.required("record").unwrap());
            } else {
                assert_eq!(
                    record.get("phase"),
                    Some(&"needs_attention".into()),
                    "{case}"
                );
                assert_eq!(
                    record.get("errorCode"),
                    Some(&"restore_reconciliation_required".into()),
                    "{case}"
                );
                assert_eq!(record.get("updatedAt"), Some(&AT.into()), "{case}");
                assert_eq!(
                    record.required("readiness").unwrap().get("state"),
                    Some(&"unknown".into()),
                    "{case}"
                );
            }
        }
        drop(db);
        let data = fs::canonicalize(&data).unwrap();
        let changed = [
            working.strip_prefix(&root).unwrap().to_owned(),
            data.join("health.json")
                .strip_prefix(&fs::canonicalize(&root).unwrap())
                .unwrap()
                .to_owned(),
            data.join("control.json")
                .strip_prefix(&fs::canonicalize(&root).unwrap())
                .unwrap()
                .to_owned(),
            config_path.strip_prefix(&root).unwrap().to_owned(),
        ];
        let after = state(&root);
        assert_eq!(
            before
                .iter()
                .filter(|(p, _)| !changed.contains(p))
                .collect::<Vec<_>>(),
            after
                .iter()
                .filter(|(p, _)| !changed.contains(p))
                .collect::<Vec<_>>(),
            "retained tree changed: {case}"
        );
    }
}
