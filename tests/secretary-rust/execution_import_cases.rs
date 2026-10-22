use super::*;
use base64::Engine;
use ivy_contracts::json::canonical;
use ivy_secretary::{
    execution_import::{ImportedExecution, import_execution_delivery},
    inbox_evidence::InboxEvidence,
    journal::{Journal, Limits},
    store::Store,
};
use ivy_store::rusqlite::Connection;
use std::{path::PathBuf, process::Command};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
pub(super) fn snapshot(db: &Connection) -> IvyJson {
    let schema = db
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let tables = schema
        .iter()
        .filter(|row| row.0 == "table")
        .map(|row| {
            let columns = db
                .prepare(&format!("PRAGMA table_info({})", quote(&row.1)))
                .unwrap()
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let projection = std::iter::once("rowid")
                .chain(columns.iter().map(String::as_str))
                .flat_map(|column| {
                    [
                        format!("typeof({})", quote(column)),
                        format!("hex(CAST({} AS BLOB))", quote(column)),
                    ]
                })
                .collect::<Vec<_>>();
            let mut statement = db
                .prepare(&format!(
                    "SELECT {} FROM {} ORDER BY rowid",
                    projection.join(","),
                    quote(&row.1)
                ))
                .unwrap();
            let rows = statement
                .query_map([], |row| {
                    (0..projection.len())
                        .map(|i| row.get::<_, String>(i).map(IvyJson::from))
                        .collect::<Result<Vec<_>, _>>()
                        .map(IvyJson::Array)
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            IvyJson::object([
                ("name", row.1.clone().into()),
                (
                    "columns",
                    IvyJson::Array(columns.into_iter().map(IvyJson::from).collect()),
                ),
                ("rows", IvyJson::Array(rows)),
            ])
        })
        .collect();
    IvyJson::object([
        (
            "schema",
            IvyJson::Array(
                schema
                    .into_iter()
                    .map(|(kind, name, table, sql)| {
                        IvyJson::object([
                            ("type", kind.into()),
                            ("name", name.into()),
                            ("tableName", table.into()),
                            ("sql", sql.map(IvyJson::from).unwrap_or(IvyJson::Null)),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("tables", IvyJson::Array(tables)),
    ])
}
fn report(imported: ImportedExecution) -> IvyJson {
    IvyJson::object([
        ("before", imported.before),
        ("after", imported.after),
        (
            "mappings",
            IvyJson::Array(
                imported
                    .mappings
                    .into_iter()
                    .map(|mapping| {
                        let source = mapping.source;
                        IvyJson::object([
                            ("sourceKey", mapping.source_key.into()),
                            ("targetField", mapping.target_field.into()),
                            ("applied", mapping.applied.into()),
                            (
                                "source",
                                IvyJson::object([
                                    (
                                        "pin",
                                        IvyJson::object([
                                            ("objectId", source.pin.object_id.into()),
                                            (
                                                "revision",
                                                IvyJson::Number(source.pin.revision as f64),
                                            ),
                                        ]),
                                    ),
                                    ("name", source.name.into()),
                                    ("value", source.value),
                                ]),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
fn result(value: Result<ImportedExecution, IvyError>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("value", report(value))]),
        Err(error) => IvyJson::object([(
            "error",
            IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
                ("outcome", error.outcome.to_json()),
            ]),
        )]),
    }
}

#[tokio::test]
async fn explicit_delivery_import_retains_copied_core_history_and_original_admission() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/execution-import-oracle.mjs")
        .current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 21);
    for row in rows {
        let input = row.required("input").unwrap();
        let directory = Directory(
            std::env::temp_dir().join(format!("secretary-import-copy-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&directory.0).unwrap();
        let source = directory.0.join("source.sqlite");
        let copy = directory.0.join("copy.sqlite");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(input.required("coreBase64").unwrap().as_str().unwrap())
            .unwrap();
        // The original Node owner closed/checkpointed this exact database before
        // producing the fixture. Copy its bytes, never any installed state/WAL.
        std::fs::write(&source, &bytes).unwrap();
        std::fs::copy(&source, &copy).unwrap();
        let journal = Journal::open(&copy, Limits { count: 0, bytes: 0 }).unwrap();
        let before = snapshot(&journal.db);
        let client = Arc::new(Scripted {
            steps: Mutex::new(
                input
                    .required("steps")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            trace: Mutex::default(),
        });
        let store = Store {
            client: client.clone(),
            root: corpus
                .required("rootId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            journal: &journal,
        };
        let cancellation = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancelled."),
            ..Default::default()
        };
        if input.get("cancel") == Some(&true.into()) {
            cancellation.cancellation.cancel();
        }
        let node = "fixture.secretary".into();
        let evidence = InboxEvidence {
            store: &store,
            settings: input.required("settings").unwrap(),
            service_node_id: &node,
            cancellation,
        };
        let mut results = vec![];
        for _ in 0..input.required("attempts").unwrap().as_f64().unwrap() as usize {
            results.push(result(
                import_execution_delivery(&evidence, input.required("execution").unwrap()).await,
            ));
        }
        let mut trace = client.trace.lock().unwrap().clone();
        for call in &mut trace {
            if call.get("method") == Some(&"objects.write".into()) {
                let mut params = call.required("params").unwrap().clone();
                ivy_contracts::operation::parse_operation_id_js(
                    params
                        .required("mutationId")
                        .unwrap()
                        .as_js_string()
                        .unwrap(),
                )
                .unwrap();
                params.insert("mutationId", "<scoped-operation>".into());
                call.insert("params", params);
            }
        }
        let expected = row.required("expected").unwrap();
        assert_eq!(
            canonical(&IvyJson::Array(results), 32 * 1024 * 1024).unwrap(),
            canonical(expected.required("results").unwrap(), 32 * 1024 * 1024).unwrap(),
            "Import results: {:?}",
            input.get("label")
        );
        assert_eq!(
            canonical(&IvyJson::Array(trace), 32 * 1024 * 1024).unwrap(),
            canonical(expected.required("trace").unwrap(), 32 * 1024 * 1024).unwrap(),
            "Import calls: {:?}",
            input.get("label")
        );
        assert!(client.steps.lock().unwrap().is_empty());
        let after = snapshot(&journal.db);
        assert_eq!(
            canonical(&before, 32 * 1024 * 1024).unwrap(),
            canonical(&after, 32 * 1024 * 1024).unwrap(),
            "Every copied table/value/history/blob stays unchanged"
        );
        assert_eq!(
            canonical(&after, 32 * 1024 * 1024).unwrap(),
            canonical(expected.required("core").unwrap(), 32 * 1024 * 1024).unwrap()
        );
        assert_eq!(
            journal
                .db
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            37
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            bytes,
            "Original copy source was untouched"
        );
    }
}
