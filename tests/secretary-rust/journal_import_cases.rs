#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::*;
use base64::Engine;
use ivy_contracts::json::canonical;
use ivy_secretary::{
    follow_up_import::import_context_link,
    journal::{Document, Journal, Limits},
};
use std::{path::PathBuf, process::Command};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn document(value: Document) -> IvyJson {
    IvyJson::object([
        (
            "pin",
            IvyJson::object([
                ("objectId", value.pin.object_id.into()),
                ("revision", IvyJson::Number(value.pin.revision as f64)),
            ]),
        ),
        ("name", value.name.into()),
        ("value", value.value),
    ])
}
fn retained(before: &IvyJson, after: &IvyJson) {
    assert_eq!(
        before.required("schema").unwrap(),
        after.required("schema").unwrap()
    );
    for table in before.required("tables").unwrap().as_array().unwrap() {
        let next = after
            .required("tables")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate.get("name") == table.get("name"))
            .unwrap();
        if table.get("name") == Some(&"technical_documents".into()) {
            continue;
        }
        for row in table.required("rows").unwrap().as_array().unwrap() {
            assert!(
                next.required("rows")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .contains(row),
                "Every original row/history/blob is retained: {:?}",
                table.get("name")
            );
        }
    }
}
#[test]
fn explicit_context_links_and_checkpoint_ack_pages_preserve_copied_history_on_reopen() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = reference_data::read(include_bytes!("journal-import-cases.json.gz"));
    let corpus = parse(
        std::str::from_utf8(&output).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 21);
    for row in rows {
        let input = row.required("input").unwrap();
        let expected = row.required("expected").unwrap();
        let label = input.required("label").unwrap().as_str().unwrap();
        let directory = Directory(
            std::env::temp_dir().join(format!("secretary-journal-import-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&directory.0).unwrap();
        let source = directory.0.join("source.sqlite");
        let copy = directory.0.join("copy.sqlite");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(input.required("coreBase64").unwrap().as_str().unwrap())
            .unwrap();
        std::fs::write(&source, &bytes).unwrap();
        std::fs::copy(&source, &copy).unwrap();
        for (action, expected) in input
            .required("actions")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .zip(expected.required("results").unwrap().as_array().unwrap())
        {
            let journal = Journal::open(
                &copy,
                Limits {
                    count: if input.get("noRoom") == Some(&IvyJson::Bool(true)) {
                        0
                    } else {
                        10000
                    },
                    ..Limits::default()
                },
            )
            .unwrap();
            let opened = super::execution_import_cases::snapshot(&journal.db);
            assert_eq!(
                canonical(&opened, 32 * 1024 * 1024).unwrap(),
                canonical(expected.required("opened").unwrap(), 32 * 1024 * 1024).unwrap(),
                "Ordinary open performs no retained-data conversion: {label}"
            );
            let result = if action.get("kind") == Some(&"link".into()) {
                import_context_link(
                    &journal,
                    &action.required("name").unwrap().as_str().unwrap(),
                )
                .map(|value| {
                    value.map_or(IvyJson::Null, |value| {
                        IvyJson::object([
                            ("before", document(value.before)),
                            ("after", document(value.after)),
                        ])
                    })
                })
            } else {
                let after = action
                    .get("after")
                    .filter(|v| !v.is_null())
                    .map(|v| v.as_str().unwrap());
                journal
                    .import_capture_acknowledgements(
                        after.as_deref(),
                        action.required("limit").unwrap().as_f64().unwrap() as usize,
                    )
                    .map(|value| {
                        IvyJson::object([
                            (
                                "sources",
                                IvyJson::Array(value.sources.into_iter().map(document).collect()),
                            ),
                            (
                                "inserted",
                                IvyJson::Array(
                                    value.inserted.into_iter().map(IvyJson::from).collect(),
                                ),
                            ),
                            (
                                "nextCursor",
                                value.next_cursor.map_or(IvyJson::Null, IvyJson::from),
                            ),
                        ])
                    })
            };
            let result = match result {
                Ok(value) => IvyJson::object([("value", value)]),
                Err(error) => IvyJson::object([("code", IvyJson::String(error.code_js()))]),
            };
            assert_eq!(
                canonical(&result, 32 * 1024 * 1024).unwrap(),
                canonical(expected.required("result").unwrap(), 32 * 1024 * 1024).unwrap(),
                "Import result: {label}"
            );
            let after = super::execution_import_cases::snapshot(&journal.db);
            assert_eq!(
                canonical(&after, 32 * 1024 * 1024).unwrap(),
                canonical(expected.required("after").unwrap(), 32 * 1024 * 1024).unwrap(),
                "Exact SQLite schema/types/bytes after import: {label}"
            );
            retained(
                row.required("expected")
                    .unwrap()
                    .required("before")
                    .unwrap(),
                &after,
            );
            assert_eq!(
                journal
                    .db
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                37
            );
            drop(journal);
        }
        assert_eq!(
            std::fs::read(source).unwrap(),
            bytes,
            "Original source remains byte identical: {label}"
        );
    }
}
