#[path = "reference-data.rs"]
mod reference_data;
use ivy_contracts::{IvyJson as Json, json::parse, limits::MANAGEMENT_FRAME_BYTES};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::local_journal::{self, LocalJournal};
use std::{path::Path, process::Command};
#[test]
fn immutable_artifact_pins_preserve_original_bytes_hashes_and_precedence() {
    let output = reference_data::read(include_bytes!("task-board-artifact-cases.json.gz"));
    let rows = parse(
        std::str::from_utf8(&output).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for case in rows.as_array().unwrap() {
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        for blob in case.required("blobs").unwrap().as_array().unwrap() {
            let b = blob.as_array().unwrap();
            let hex = b[3].as_str().unwrap();
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|n| u8::from_str_radix(&hex[n..n + 2], 16).unwrap())
                .collect::<Vec<_>>();
            db.execute(
                "INSERT INTO blobs VALUES(?,?,?,?,?)",
                params![
                    b[0].as_str().unwrap(),
                    b[1].as_str().unwrap(),
                    "part",
                    b[2].as_js_string().map(|s| s.to_utf8().unwrap()),
                    bytes
                ],
            )
            .unwrap();
        }
        for doc in case.required("documents").unwrap().as_array().unwrap() {
            let d = doc.as_array().unwrap();
            db.execute(
                "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                params![
                    d[0].as_str().unwrap(),
                    d[1].as_str().unwrap(),
                    d[0].as_str().unwrap(),
                    d[2].as_js_string().map(|s| s.to_utf8().unwrap()),
                    d[3].as_f64().unwrap(),
                    "time",
                    "time"
                ],
            )
            .unwrap();
        }
        for revision in case.required("revisions").unwrap().as_array().unwrap() {
            let r = revision.as_array().unwrap();
            db.execute(
                "INSERT INTO revisions VALUES(?,?,?,?)",
                params![
                    r[0].as_str().unwrap(),
                    r[1].as_f64().unwrap(),
                    r[2].as_str().unwrap(),
                    "time"
                ],
            )
            .unwrap();
        }
        let journal = LocalJournal {
            connection: &db,
            root_object_id: None,
        };
        let result =
            match journal.artifact(case.required("input").unwrap().required("pin").unwrap()) {
                Ok(None) => Json::Null,
                Ok(Some(artifact)) => Json::object([
                    ("contractKey", artifact.contract_key.into()),
                    (
                        "parentId",
                        artifact.parent_id.map(Json::String).unwrap_or(Json::Null),
                    ),
                    ("contentHash", artifact.content_hash.into()),
                    ("mediaType", artifact.media_type.into()),
                    (
                        "bytes",
                        artifact
                            .bytes
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>()
                            .into(),
                    ),
                ]),
                Err(failure) => Json::object([(
                    "error",
                    Json::object([
                        ("code", failure.code.into()),
                        ("message", failure.message.into()),
                    ]),
                )]),
            };
        assert_eq!(
            result,
            *case.required("expected").unwrap(),
            "{}",
            case.required("name").unwrap().as_str().unwrap()
        );
    }
}
