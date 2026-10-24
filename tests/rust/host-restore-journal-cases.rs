use super::publication_cases::captured_entry;
use super::target_cases::{instance, jp, state};
use super::*;
use ivy_contracts::core_schema::validate_host;
use ivy_host_runtime::{accepted_configuration, journal::HostJournal, paths};
use ivy_store::rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{collections::BTreeMap, time::Duration};

#[derive(Debug, PartialEq, Eq)]
struct JournalState {
    version: i64,
    tables: BTreeMap<String, Vec<Vec<String>>>,
}

fn journal_state(path: &Path) -> JournalState {
    // These disposable copies have no live owner. A read/write reader lets
    // SQLite remove transient WAL/SHM files when its final handle closes.
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    assert_eq!(
        db.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let version = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let mut tables = BTreeMap::new();
    for table in [
        "meta",
        "operations",
        "candidates",
        "installed",
        "observations",
        "runtime_targets",
        "candidate_retirements",
    ] {
        if db
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?",
                [table],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap()
            .is_none()
        {
            continue;
        }
        let mut statement = db.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let columns = statement.column_count();
        let mut rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, String>(column))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        tables.insert(table.to_owned(), rows);
    }
    db.close().unwrap();
    JournalState { version, tables }
}

#[tokio::test]
async fn copied_journal_reconciliation_preserves_terminal_outcomes_and_rolls_back_failures() {
    const AT: &str = "2026-10-21T00:00:00.000Z";
    let mut cases = vec![
        "valid",
        "wrong-host",
        "wrong-version",
        "malformed-entry",
        "legacy-no-retirements",
        "unicode-dot-runtime",
        "canonical-runtime",
        "linked-runtime",
        "database-directory",
        "late-trigger-rollback",
    ];
    #[cfg(windows)]
    cases.extend(["opaque-runtime-high", "opaque-runtime-low"]);
    for case in cases {
        let f = ServiceFixture::new();
        let root = f.0.join("restore");
        let source_runtime = root.join("source-state");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        let original = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("hostId", "restore-fixture".into()),
            ("runtimeRoot", jp(&source_runtime)),
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
        let journal = HostJournal::open(original.clone()).await.unwrap();
        journal.close().await.unwrap();
        let source = source_runtime.join("deployments.sqlite");
        let phases = [
            "preparing",
            "succeeded",
            "rolled_back",
            "failed",
            "needs_attention",
        ];
        let entries = phases
            .iter()
            .enumerate()
            .map(|(n, phase)| captured_entry(&root, n, phase))
            .collect::<Vec<_>>();
        {
            let db = Connection::open(&source).unwrap();
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
                        record.required("createdAt").unwrap().as_str().unwrap(),
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
                INSERT INTO meta VALUES ('storage-retention','preserved'),('bootstrap-rollback:fixture','preserved'),('unrelated-evidence','preserved');").unwrap();
            match case {
                "wrong-host" => {
                    db.execute("UPDATE meta SET value='other' WHERE key='hostId'", [])
                        .unwrap();
                }
                "wrong-version" => {
                    db.execute_batch("PRAGMA user_version=2").unwrap();
                }
                "malformed-entry" => {
                    db.execute(
                        "UPDATE operations SET entry_json='{}' WHERE deployment_id='deployment-2'",
                        [],
                    )
                    .unwrap();
                }
                "legacy-no-retirements" => {
                    db.execute_batch("DROP TABLE candidate_retirements")
                        .unwrap();
                }
                "late-trigger-rollback" => {
                    db.execute_batch("CREATE TRIGGER fixture_abort_installation BEFORE INSERT ON meta WHEN NEW.key='installation' BEGIN SELECT RAISE(ABORT,'fixture late rollback'); END;").unwrap();
                }
                _ => (),
            }
        }
        let before_sql = journal_state(&source);
        let source_bytes = fs::read(&source).unwrap();
        let mut runtime = target.join(if case == "unicode-dot-runtime" {
            "state Ω"
        } else {
            "state"
        });
        #[cfg(windows)]
        if case.starts_with("opaque-runtime") {
            use std::os::windows::ffi::OsStringExt;
            let unit = if case.ends_with("high") {
                0xd800
            } else {
                0xdfff
            };
            let name = std::ffi::OsString::from_wide(&[112, unit]);
            runtime = target.join(&name);
            let replacement = target.join(name.to_string_lossy().as_ref());
            fs::create_dir(&replacement).unwrap();
            fs::copy(&source, replacement.join("deployments.sqlite")).unwrap();
        }
        fs::create_dir(&runtime).unwrap();
        let working = runtime.join("deployments.sqlite");
        fs::copy(&source, &working).unwrap();
        let recovery = runtime.join("recovery-original");
        fs::create_dir(&recovery).unwrap();
        fs::copy(&source, recovery.join("deployments.sqlite")).unwrap();
        fs::write(runtime.join("retained.txt"), b"unrelated owned state").unwrap();
        let mut runtime_argument = runtime.clone();
        if case == "unicode-dot-runtime" {
            fs::create_dir(target.join("route")).unwrap();
            runtime_argument = target.join("route/..").join(runtime.file_name().unwrap());
        }
        if case == "canonical-runtime" {
            runtime_argument = fs::canonicalize(&runtime).unwrap();
        }
        if case == "linked-runtime" {
            runtime_argument = target.join("linked-state");
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(&runtime, &runtime_argument).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&runtime, &runtime_argument).unwrap();
        }
        if case == "database-directory" {
            fs::remove_file(&working).unwrap();
            fs::create_dir(&working).unwrap();
            fs::write(working.join("owner.txt"), b"existing directory authority").unwrap();
        }
        let mut configuration = original.clone();
        configuration.insert("runtimeRoot", jp(&runtime_argument));
        configuration.insert("artifactRoot", jp(&target.join("artifacts")));
        configuration.insert("stagingRoot", jp(&target.join("staging")));
        configuration.insert("configPath", jp(&target.join("host.json")));
        validate_host("HostConfig", &configuration).unwrap();
        let before = state(&root);
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            restore::reconcile_restored_journal(configuration.clone(), AT.into()),
        )
        .await
        .expect("journal reconciliation must terminate");
        let failure = [
            "wrong-host",
            "wrong-version",
            "malformed-entry",
            "database-directory",
            "late-trigger-rollback",
        ]
        .contains(&case)
            || case.starts_with("opaque-runtime");
        if failure {
            let error = result.unwrap_err();
            assert_eq!(
                error.code,
                match case {
                    "wrong-host" | "wrong-version" => "unsupported_storage",
                    "malformed-entry" => "invalid_arguments",
                    _ => "internal_error",
                },
                "{case}"
            );
            if case != "database-directory" && !case.starts_with("opaque-runtime") {
                assert_eq!(journal_state(&working), before_sql, "rollback: {case}");
                let db = Connection::open(&working).unwrap();
                db.busy_timeout(Duration::from_millis(100)).unwrap();
                db.execute_batch("BEGIN IMMEDIATE; ROLLBACK")
                    .expect("failed reconciliation must close its writer");
                db.close().unwrap();
            }
            assert_eq!(
                state(&root),
                before,
                "failed operation changed authority: {case}"
            );
        } else {
            result.unwrap();
            let after_sql = journal_state(&working);
            assert_eq!(after_sql.version, before_sql.version, "{case}");
            for table in [
                "candidates",
                "installed",
                "observations",
                "runtime_targets",
                "candidate_retirements",
            ] {
                if let Some(rows) = after_sql.tables.get(table) {
                    assert!(rows.is_empty(), "{case}: {table}");
                }
            }
            assert_eq!(
                after_sql.tables.contains_key("candidate_retirements"),
                case != "legacy-no-retirements",
                "{case}"
            );
            let kept_meta = |state: &JournalState| {
                state.tables["meta"]
                    .iter()
                    .filter(|row| {
                        !["installation", "storage-retention"].contains(&row[0].as_str())
                            && !row[0].starts_with("bootstrap-rollback:")
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(kept_meta(&after_sql), kept_meta(&before_sql), "{case}");
            assert!(
                !after_sql.tables["meta"]
                    .iter()
                    .any(|row| row[0] == "storage-retention"
                        || row[0].starts_with("bootstrap-rollback:")),
                "{case}"
            );
            assert_eq!(
                after_sql.tables["meta"]
                    .iter()
                    .find(|row| row[0] == "installation")
                    .unwrap()[1],
                accepted_configuration::installation_record(&configuration).unwrap(),
                "{case}"
            );
            let rows = &after_sql.tables["operations"];
            assert_eq!(rows.len(), 5, "{case}");
            assert_eq!(
                rows.iter()
                    .filter(|row| row[3] == "needs_attention")
                    .count(),
                2,
                "{case}"
            );
            let record_runtime = paths::absolute(&runtime_argument).unwrap();
            for (n, before_entry) in entries.iter().enumerate() {
                let row = rows
                    .iter()
                    .find(|row| row[1] == format!("deployment-{n}"))
                    .unwrap();
                let original_row = before_sql.tables["operations"]
                    .iter()
                    .find(|before| before[1] == row[1])
                    .unwrap();
                for column in [0, 1, 2, 4] {
                    assert_eq!(row[column], original_row[column], "{case}");
                }
                let after_entry = json::parse(&row[5], usize::MAX).unwrap();
                validate_host("JournalEntry", &after_entry).unwrap();
                let mut expected = before_entry.clone();
                expected.insert(
                    "configurationPath",
                    jp(&record_runtime
                        .join("accepted-configurations")
                        .join(format!("accepted-{n}.json"))),
                );
                let mut activation = expected.required("activation").unwrap().clone();
                activation.insert(
                    "previousConfigurationPath",
                    jp(&record_runtime
                        .join("accepted-configurations")
                        .join(format!("prior-{n}.json"))),
                );
                expected.insert("activation", activation);
                if n == 0 {
                    let mut record = expected.required("record").unwrap().clone();
                    record.insert("phase", "needs_attention".into());
                    record.insert("updatedAt", AT.into());
                    record.insert("observedBuild", J::Null);
                    record.insert("errorCode", "restore_reconciliation_required".into());
                    record.insert("readiness",J::object([("state","unknown".into()),("message","Restored deployment may have effects after its snapshot; original evidence is retained and no action is replayed.".into())]));
                    expected.insert("record", record);
                }
                assert_eq!(
                    row[3],
                    if n == 0 { "needs_attention" } else { phases[n] },
                    "{case}"
                );
                assert_json_eq(&after_entry, &expected);
            }
            let after = state(&root);
            let changed = working.strip_prefix(&root).unwrap();
            assert_eq!(
                before
                    .iter()
                    .filter(|(path, _)| path.as_path() != changed)
                    .collect::<Vec<_>>(),
                after
                    .iter()
                    .filter(|(path, _)| path.as_path() != changed)
                    .collect::<Vec<_>>(),
                "unrelated state changed: {case}"
            );
        }
        assert_eq!(fs::read(&source).unwrap(), source_bytes, "{case}");
        assert_eq!(
            fs::read(recovery.join("deployments.sqlite")).unwrap(),
            source_bytes,
            "{case}"
        );
    }
}
