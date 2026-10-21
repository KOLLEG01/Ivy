use ivy_contracts::{IvyJson as J, JsString, hash::digest, json::parse};
use ivy_contracts::{
    error::JavaScriptExceptionKind, task_board_questions::open_task_questions_raw,
};
use ivy_sdk::{IvyResult, open_task_questions};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn json(bytes: &[u8]) -> J {
    parse(std::str::from_utf8(bytes).unwrap(), usize::MAX).unwrap()
}

fn rows(value: &J) -> &[J] {
    value.get("cases").unwrap().as_array().unwrap()
}

fn evidence(name: &str, value: &J) {
    if let Some(dir) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Path::new(&dir).join(name),
            ivy_contracts::json::stringify(value, usize::MAX).unwrap(),
        )
        .unwrap();
    }
}

fn projection(task: &J, run_id: Option<&JsString>) -> J {
    match open_task_questions(task, run_id) {
        Ok(value) => J::object([("value", J::Array(value))]),
        Err(error) => J::object([("error", error.to_wire())]),
    }
}

#[test]
fn captured_current_consumer_views_reuse_all_twenty_three_cases() {
    let fixture = json(include_bytes!("../fixtures/task-board-questions.json"));
    assert_eq!(rows(&fixture).len(), 23);
    assert_eq!(
        fixture.get("sourceHash"),
        Some(&digest(include_bytes!("../fixtures/task-board-questions.ts.txt")).into())
    );
    let mut observations = Vec::new();
    for row in rows(&fixture) {
        let task = row.get("task").unwrap();
        let before = task.clone();
        for mode in ["all", "run"] {
            let run_id = if mode == "run" {
                row.get("runId").and_then(J::as_js_string)
            } else {
                None
            };
            let actual = J::Array(open_task_questions(task, run_id).unwrap());
            let expected = row.get("expected").unwrap().get(mode).unwrap();
            observations.push(J::object([
                ("name", row.get("name").unwrap().clone()),
                ("mode", mode.into()),
                ("actual", actual.clone()),
                ("expected", expected.clone()),
                ("matches", (actual == *expected).into()),
            ]));
        }
        assert_eq!(*task, before);
    }
    let observations = J::Array(observations);
    evidence("task-board-questions-captured.json", &observations);
    assert!(
        observations
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row.get("matches") == Some(&true.into()))
    );
}

#[test]
fn additional_source_oracle_boundaries_preserve_values_and_failure() {
    let output = std::process::Command::new("node")
        .args([
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-questions-oracle.mjs",
        ])
        .current_dir(root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = json(&output.stdout);
    assert_eq!(
        original.get("sourceHash"),
        Some(&digest(include_bytes!("../fixtures/task-board-questions.ts.txt")).into())
    );
    evidence("task-board-questions-original.json", &original);
    let mut observations = Vec::new();
    for row in rows(&original) {
        let task = row.get("task").unwrap();
        let before = task.clone();
        for mode in ["all", "run"] {
            let run_id = if mode == "run" {
                row.get("runId").and_then(J::as_js_string)
            } else {
                None
            };
            let actual = projection(task, run_id);
            let expected = row.get("expected").unwrap().get(mode).unwrap();
            let mut boundary = expected.clone();
            boundary.remove("originalError");
            boundary.remove("originalMessage");
            let raw = match open_task_questions_raw(task, run_id) {
                Ok(value) => J::object([("value", J::Array(value))]),
                Err(error) => J::object([(
                    "error",
                    J::object([
                        (
                            "kind",
                            match error.kind {
                                JavaScriptExceptionKind::TypeError => "TypeError",
                                JavaScriptExceptionKind::RangeError => "RangeError",
                            }
                            .into(),
                        ),
                        ("message", J::String(error.message)),
                    ]),
                )]),
            };
            let raw_expected = if expected.get("error").is_some() {
                J::object([(
                    "error",
                    J::object([
                        ("kind", expected.get("originalError").unwrap().clone()),
                        ("message", expected.get("originalMessage").unwrap().clone()),
                    ]),
                )])
            } else {
                expected.clone()
            };
            observations.push(J::object([
                ("name", row.get("name").unwrap().clone()),
                ("mode", mode.into()),
                ("actual", actual.clone()),
                ("expected", expected.clone()),
                ("raw", raw.clone()),
                ("rawExpected", raw_expected.clone()),
                (
                    "matches",
                    (actual == boundary && raw == raw_expected).into(),
                ),
            ]));
        }
        assert_eq!(*task, before);
    }
    let observations = J::Array(observations);
    evidence("task-board-questions-native.json", &observations);
    let mismatches: Vec<_> = observations
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row.get("matches") != Some(&true.into()))
        .collect();
    assert!(mismatches.is_empty(), "{mismatches:?}");
}

#[test]
fn sdk_export_fits_the_existing_fallible_consumer_callback() {
    type Resolver = dyn Fn(&J, Option<&JsString>) -> IvyResult<Vec<J>>;
    let resolver: &Resolver = &open_task_questions;
    let task = json(
        br#"{"comments":[{"purpose":"question","commentId":"q","requests":[],"extra":"\ud800"}]}"#,
    );
    assert_eq!(
        resolver(&task, None).unwrap(),
        task.get("comments").unwrap().as_array().unwrap()
    );
    assert_eq!(
        resolver(&J::Null, None).unwrap_err(),
        ivy_contracts::IvyError::internal()
    );
    assert_eq!(
        resolver(&task, None).unwrap(),
        ivy_contracts::task_board_questions::open_task_questions(&task, None).unwrap()
    );
}
