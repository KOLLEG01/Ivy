use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, json};
use ivy_sdk::open_task_questions;

#[test]
fn ordered_questions_keep_addressed_answers_resolutions_native_states_and_lossless_records() {
    let fixture = json::parse(
        include_str!("../fixtures/task-board-questions.json"),
        usize::MAX,
    )
    .unwrap();
    let cases = fixture.get("cases").unwrap().as_array().unwrap();
    assert_eq!(cases.len(), 23);
    for case in cases {
        let task = case.get("task").unwrap();
        let before = task.clone();
        for mode in ["all", "run"] {
            let run_id = if mode == "run" {
                case.get("runId").and_then(Json::as_js_string)
            } else {
                None
            };
            let actual = Json::Array(open_task_questions(task, run_id).unwrap());
            assert_eq!(
                &actual,
                case.get("expected").unwrap().get(mode).unwrap(),
                "{:?} {mode}",
                case.get("name")
            );
        }
        assert_eq!(task, &before);
    }
}

#[test]
fn malformed_question_data_returns_rust_errors_without_an_empty_projection() {
    for text in [
        "null",
        r#"{"comments":null}"#,
        r#"{"comments":{}}"#,
        r#"{"comments":[null]}"#,
        r#"{"comments":[{"purpose":"question","commentId":{}}]}"#,
        r#"{"comments":[{"purpose":"question","commentId":"q","requests":"invalid"}]}"#,
        r#"{"comments":[{"purpose":"question","commentId":"q"},{"resolvesQuestions":"q"}]}"#,
    ] {
        let task = json::parse(text, usize::MAX).unwrap();
        assert_eq!(
            open_task_questions(&task, None).unwrap_err(),
            IvyError::internal()
        );
    }
}

#[test]
fn sdk_callback_uses_the_common_json_api_and_preserves_unknown_wire_fields() {
    type Resolver = fn(&Json, Option<&JsString>) -> IvyResult<Vec<Json>>;
    let resolver: Resolver = open_task_questions;
    let task = json::parse(
        r#"{"comments":[{"purpose":"question","commentId":"q","requests":[],"extra":"\ud800"}]}"#,
        1000,
    )
    .unwrap();
    assert_eq!(
        resolver(&task, None).unwrap(),
        task.get("comments").unwrap().as_array().unwrap()
    );
    assert_eq!(
        resolver(&task, None).unwrap(),
        ivy_contracts::task_board_questions::open_task_questions(&task, None).unwrap()
    );
}
