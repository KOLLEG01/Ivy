// The composed factory fixture instantiates the complete borrowed engine graph.
#![recursion_limit = "256"]

use ivy_contracts::{
    IvyJson as Json, hash::hash_json, json::parse, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::turn_output::*;
use std::{path::Path, process::Command};

#[path = "../fixtures/task-board-frozen-contracts.rs"]
mod frozen_contracts;
#[path = "../fixtures/task-board-host-service.rs"]
mod host_service;
#[path = "../fixtures/task-board-inspector.rs"]
mod inspector;
#[path = "../fixtures/task-board-signals.rs"]
mod signals;
#[path = "../fixtures/task-board-native-comments.rs"]
mod native_comments;
#[path = "../fixtures/task-board-orchestrator.rs"]
mod orchestrator;
#[path = "../fixtures/task-board-rate-limit.rs"]
mod rate_limit;
#[path = "../fixtures/task-board-reconciler.rs"]
mod reconciler;
#[path = "../fixtures/task-board-native-publication.rs"]
mod native_publication;
#[path = "../fixtures/task-board-categories.rs"]
mod categories;
#[path = "../fixtures/task-board-engine-read.rs"]
mod engine_read;
#[path = "../fixtures/task-board-engine-execution.rs"]
mod engine_execution;
#[path = "../fixtures/task-board-engine-admission.rs"]
mod engine_admission;
#[path = "../fixtures/task-board-engine-publication.rs"]
mod engine_publication;
#[path = "../fixtures/task-board-engine-actions.rs"]
mod engine_actions;
#[path = "../fixtures/task-board-engine-validation.rs"]
mod engine_validation;
#[path = "../fixtures/task-board-checks.rs"]
mod checks;
#[path = "../fixtures/task-board-read-scopes.rs"]
mod read_scopes;
#[path = "../fixtures/task-board-task-routing.rs"]
mod task_routing;
#[path = "../fixtures/task-board-task-written.rs"]
mod task_written;
#[path = "../fixtures/task-board-prompts.rs"]
mod prompts;
#[path = "../fixtures/task-board-service.rs"]
mod service;
#[path = "../fixtures/task-board-local-executor.rs"]
mod local_executor;

#[test]
fn native_output_helpers_match_original_typescript() {
    let result = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-output-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let cases = parse(
        std::str::from_utf8(&result.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for case in cases.as_array().unwrap() {
        let args = case.required("args").unwrap().as_array().unwrap();
        let string = |index: usize| args[index].as_js_string().unwrap();
        let actual = match case.required("method").unwrap().as_str().unwrap().as_str() {
            "pageCursor" => match page_cursor(args.first()) {
                Ok(value) => value.map(Json::String).unwrap_or(Json::Null),
                Err(error) => Json::object([(
                    "error",
                    Json::object([
                        ("code", Json::String(error.code_js())),
                        ("message", Json::String(error.message_js())),
                        ("outcome", error.outcome.to_json()),
                    ]),
                )]),
            },
            "finalOutputText" => Json::String(final_output_text(&args[0])),
            "boundedOutputSummary" => Json::String(bounded_output_summary(string(0))),
            "turnsParams" => turns_params(string(0), args[1].as_js_string()),
            "outputParams" => output_params(string(0), string(1), args[2].as_js_string()),
            method => panic!("Unknown oracle method: {method}"),
        };
        assert_eq!(
            hash_json(&actual).unwrap(),
            case.required("expectedHash").unwrap().as_str().unwrap(),
            "{}",
            case.required("name").unwrap().as_str().unwrap()
        );
    }
}
