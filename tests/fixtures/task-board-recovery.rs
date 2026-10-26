#[path = "reference-data.rs"]
mod reference_data;
use super::{Memory, Stored, ready, s};
use ivy_contracts::{
    IvyJson as Json,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    native_recovery::{context_recovery_history, fresh_context_recovery, missing_native_thread},
    operations::Document,
};
use std::{path::Path, process::Command};

#[test]
fn recovery_lineage_and_missing_thread_checks_match_original_reads_and_refusals() {
    let output = reference_data::read(include_bytes!("task-board-recovery-cases.json.gz"));
    let data = parse(
        std::str::from_utf8(&output).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for row in data.required("rows").unwrap().as_array().unwrap() {
        let input = row.required("input").unwrap();
        let store = Memory::default();
        for r in input.required("records").unwrap().as_array().unwrap() {
            let value = r.required("value").unwrap();
            store.documents.borrow_mut().push(Stored {
                key: r.required("key").unwrap().as_str().unwrap(),
                name: "record".into(),
                document: Document {
                    pin: Json::object([
                        ("objectId", r.required("id").unwrap().clone()),
                        ("revision", Json::Number(1.0)),
                    ]),
                    value: value.clone(),
                    parent_id: Some("root".into()),
                    content_hash: hash_json(value).unwrap(),
                },
            });
        }
        let result = if input.required("method").unwrap() == &Json::from("fresh") {
            ready(fresh_context_recovery(
                &store,
                input.required("task").unwrap(),
            ))
        } else {
            ready(context_recovery_history(
                &store,
                input.required("task").unwrap(),
                &s(input, "taskId"),
            ))
        };
        let value = match result {
            Ok(value) => value.unwrap_or(Json::Null),
            Err(error) => Json::object([(
                "error",
                Json::object([
                    ("code", Json::String(error.code_js())),
                    ("message", Json::String(error.message_js())),
                    ("outcome", error.outcome.to_json()),
                ]),
            )]),
        };
        let actual = Json::object([
            ("value", value),
            ("trace", Json::Array(store.trace.into_inner())),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(row.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            row.required("name").unwrap().as_str().unwrap()
        );
    }
    for input in data.required("missing").unwrap().as_array().unwrap() {
        assert_eq!(
            missing_native_thread(input.required("reply").unwrap(), &s(input, "threadId")).unwrap(),
            input.required("expected").unwrap().as_bool().unwrap(),
            "{}",
            canonical(input, MANAGEMENT_FRAME_BYTES).unwrap()
        );
    }
}
