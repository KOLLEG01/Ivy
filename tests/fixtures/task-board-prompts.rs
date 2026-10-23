use ivy_contracts::{IvyJson as Json, JsString, hash::hash_json, json::parse};
use ivy_task_board::prompts::{handoff, task_prompt, voice_prompt};

fn vectors() -> Json {
    parse(include_str!("task-board-prompts.json"), 65_536).unwrap()
}
fn patched(base: &Json, patch: Option<&Json>) -> Json {
    let mut value = base.clone();
    if let Some(fields) = patch.and_then(Json::as_object) {
        for (key, field) in fields {
            value.insert(key.clone(), field.clone());
        }
    }
    value
}
fn run(method: &str) {
    let data = vectors();
    let mut cases = 0;
    for row in data.required("cases").unwrap().as_array().unwrap() {
        if row.required("method").unwrap() != &Json::from(method) {
            continue;
        }
        cases += 1;
        let task = patched(data.required("baseTask").unwrap(), row.get("task"));
        let mut comment = patched(data.required("baseComment").unwrap(), row.get("comment"));
        let truncated = row.get("bodyFill").and_then(Json::as_f64);
        if let Some(fill) = truncated {
            let mut body = JsString(vec![u16::from(b'a'); fill as usize]);
            body.0.extend(JsString::from("😀tail").0);
            comment.insert("body", Json::String(body));
        }
        let output = match method {
            "task" => task_prompt(
                &task,
                row.required("service").unwrap().as_js_string().unwrap(),
                row.required("executionChange").unwrap() == &Json::Bool(true),
            ),
            "handoff" => handoff(&task, &comment),
            "voice" => voice_prompt(&task, &comment),
            _ => unreachable!(),
        }
        .unwrap();
        assert_eq!(
            output.0.len() as f64,
            row.required("units").unwrap().as_f64().unwrap(),
            "{:?}",
            row.get("name")
        );
        assert_eq!(
            Json::from(hash_json(&Json::String(output.clone())).unwrap()),
            *row.required("expectedHash").unwrap(),
            "{:?}",
            row.get("name")
        );
        if truncated.is_some() {
            // ECMAScript slice caps UTF-16 units and may retain a high surrogate.
            assert_eq!(output.0.last(), Some(&0xd83d));
            assert_eq!(
                output.0.len(),
                if method == "handoff" { 16_384 } else { 4_096 }
            );
        }
    }
    assert_eq!(cases, if method == "task" { 14 } else { 5 });
}
#[test]
fn current_ticket_instructions_use_question_resolution_and_execution_change_semantics() {
    run("task");
}
#[test]
fn retained_main_instructions_keep_native_identity_json_order_and_utf16_boundary() {
    run("handoff");
}
#[test]
fn retained_voice_instructions_keep_native_identity_json_order_and_utf16_boundary() {
    run("voice");
}
#[test]
fn malformed_question_evidence_remains_an_error_before_regular_prompt_selection() {
    let data = vectors();
    let mut task = data.required("baseTask").unwrap().clone();
    task.insert(
        "comments",
        Json::Array(vec![Json::object([
            ("purpose", "question".into()),
            ("commentId", Json::Number(17.0)),
        ])]),
    );
    let failure = ivy_sdk::open_task_questions(&task, None).unwrap_err();
    assert_eq!(
        task_prompt(&task, &"board".into(), false)
            .unwrap_err()
            .to_wire(),
        failure.to_wire()
    );
    // Settings-change instruction selection precedes conversation inspection.
    assert!(task_prompt(&task, &"board".into(), true).is_ok());
    let mut native = data.required("baseComment").unwrap().clone();
    native.insert("nativeInput", Json::object([] as [(&str, Json); 0]));
    assert!(handoff(&task, &native).is_err());
    assert!(voice_prompt(&task, &native).is_err());
}
