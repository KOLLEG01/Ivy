#[path = "reference-data.rs"]
mod reference_data;
use super::{Memory, ready, s};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{operations::Operations, publication::Publication};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    path::Path,
    pin::Pin,
    process::Command,
};

#[test]
fn publication_program_matches_original_retained_steps_callbacks_and_restarts() {
    let cases = if let Ok(path) = std::env::var("IVY_TASK_BOARD_PROGRAM_FIXTURE") {
        parse(
            &std::fs::read_to_string(path).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    } else {
        let output = reference_data::read(include_bytes!("task-board-program-cases.json.gz"));
        parse(
            std::str::from_utf8(&output).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    };
    for case in cases.as_array().unwrap() {
        let operations = Operations {
            store: Memory::default(),
            now: || "2026-01-02T03:04:05.000Z".into(),
        };
        let request = Json::object([
            ("action", "edit".into()),
            ("operationId", "epoch:1:id".into()),
        ]);
        let actor = Json::object([
            ("principalId", "caller".into()),
            ("source", "worker".into()),
        ]);
        let operation = ready(operations.begin(&request, &actor)).unwrap();
        let events = RefCell::new(vec![]);
        let fail = Cell::new(false);
        let callback = |pin, value| -> Pin<Box<dyn Future<Output = IvyResult<()>> + '_>> {
            events.borrow_mut().push(Json::Array(vec![pin, value]));
            let fail = fail.replace(false);
            Box::pin(async move {
                if fail {
                    Err(IvyError::new("callback_failed", "Fixture callback failed."))
                } else {
                    Ok(())
                }
            })
        };
        let callback = case
            .required("callback")
            .unwrap()
            .as_bool()
            .unwrap()
            .then_some(&callback as &_);
        let mut publication = Publication {
            operations: &operations,
            operation: &operation,
            task_written: callback,
            index: 0.0,
        };
        let mut outcomes = vec![];
        for step in case.required("steps").unwrap().as_array().unwrap() {
            if step.get("reset").is_some() {
                publication.index = 0.0;
                continue;
            }
            if let Some(fault) = step.get("fault") {
                for (key, value) in fault.as_object().unwrap() {
                    operations
                        .store
                        .faults
                        .borrow_mut()
                        .insert(key.clone(), value.clone());
                }
                continue;
            }
            if step.get("callbackFailure").is_some() {
                fail.set(true);
                continue;
            }
            if step.get("finish").is_some() {
                ready(operations.finish(
                    &s(&operation.pin, "objectId"),
                    &Json::object([("operationId", "epoch:1:id".into()), ("task", Json::Null)]),
                ))
                .unwrap();
                continue;
            }
            let key = step.required("key").unwrap().as_str().unwrap();
            let result = if step.required("method").unwrap() == &Json::from("create") {
                ready(publication.create(&key, step.required("value").unwrap(), &s(step, "label")))
            } else {
                ready(publication.write(
                    &key,
                    step.required("value").unwrap(),
                    step.required("destination").unwrap(),
                ))
            };
            outcomes.push(match result {
                Ok(pin) => pin,
                Err(error) => Json::object([(
                    "error",
                    Json::object([
                        ("code", Json::String(error.code_js())),
                        ("message", Json::String(error.message_js())),
                        ("outcome", error.outcome.to_json()),
                    ]),
                )]),
            });
        }
        let actual = Json::object([
            ("outcomes", Json::Array(outcomes)),
            ("events", Json::Array(events.borrow().clone())),
            (
                "trace",
                Json::Array(operations.store.trace.borrow().clone()),
            ),
            (
                "effects",
                Json::Number(*operations.store.effects.borrow() as f64),
            ),
            ("index", Json::Number(publication.index)),
            ("pin", publication.pin().unwrap()),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(case.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            case.required("name").unwrap().as_str().unwrap()
        );
    }
}
