//! SDK callback payloads. Native reverse requests remain owned by Adapter.
use super::{
    error::{AdapterError, Result},
    value::*,
};
use ivy_contracts::{IvyJson as J, JsString};
pub fn questions(input: &J) -> Result<Vec<J>> {
    let raw = field(input, "questions").as_array().unwrap_or(&[]);
    let mut out = vec![];
    for (i, q) in raw.iter().enumerate() {
        let options = field(q, "options");
        let mut options = if !truthy(options) {
            vec![]
        } else {
            options
                .as_array()
                .ok_or_else(|| {
                    AdapterError::typed("TypeError", "optionsRaw.map is not a function")
                })?
                .iter()
                .map(|o| {
                    J::object([
                        (
                            "label",
                            string(&coalesce(field(o, "label"), &"".into())).into_json(),
                        ),
                        (
                            "description",
                            string(&coalesce(field(o, "description"), &"".into())).into_json(),
                        ),
                    ])
                })
                .collect::<Vec<_>>()
        };
        options.push(J::object([
            ("label", "Other".into()),
            ("description", "Provide a custom answer".into()),
        ]));
        out.push(J::object([
            ("id", format!("q{i}").into()),
            (
                "header",
                slice(
                    &string(&coalesce(
                        field(q, "header"),
                        &format!("Question {}", i + 1).into(),
                    )),
                    0,
                    12,
                )
                .into_json(),
            ),
            (
                "question",
                string(&coalesce(field(q, "question"), &"".into())).into_json(),
            ),
            ("isOther", false.into()),
            ("isSecret", false.into()),
            ("options", J::Array(options)),
        ]));
    }
    Ok(out)
}
pub fn format_answers(input: &J, questions: &[J], answers: &J) -> Result<JsString> {
    let original = field(input, "questions").as_array().unwrap_or(&[]);
    let mut by_question = object();
    let mut annotations = object();
    for (i, q) in questions.iter().enumerate() {
        let original = original.get(i).unwrap_or(&NULL);
        let text = string(&coalesce(field(original, "question"), field(q, "question")));
        let key = string(field(q, "id"));
        let slot = field(answers, "answers").get_key(&key).unwrap_or(&NULL);
        // Assignment to a plain original JS object invokes __proto__'s setter.
        // Keep that historical JSON result instead of inventing a wire field.
        if text == JsString::from("__proto__") {
            continue;
        }
        if !truthy(slot) {
            by_question.insert(text, "".into());
            continue;
        }
        let picked = field(slot, "answers")
            .as_array()
            .unwrap_or(&[])
            .iter()
            .filter(|v| !is(v, "Other"))
            .map(string)
            .collect::<Vec<_>>();
        let notes = field(slot, "notes").as_js_string().cloned();
        let value = if picked.is_empty() && notes.as_ref().is_some_and(|s| !s.0.is_empty()) {
            notes.as_ref().unwrap().clone()
        } else {
            join(&picked, ", ")
        };
        by_question.insert(text.clone(), value.into_json());
        if let Some(notes) = notes.filter(|s| !s.0.is_empty()) {
            annotations.insert(text, J::object([("notes", notes.into_json())]));
        }
    }
    let mut payload = J::object([
        (
            "questions",
            field(input, "questions")
                .as_array()
                .map(|a| J::Array(a.to_vec()))
                .unwrap_or_else(|| J::Array(vec![])),
        ),
        ("answers", by_question),
    ]);
    if annotations.as_object().is_some_and(|a| !a.is_empty()) {
        payload.insert("annotations", annotations)
    }
    Ok(encode(&payload)?.into())
}
pub fn permission_result(input: &J, decision: &J, auto_allow: bool) -> J {
    if auto_allow {
        return J::object([("behavior", "allow".into())]);
    }
    if is(field(decision, "decision"), "accept")
        || is(field(decision, "decision"), "acceptForSession")
    {
        J::object([
            ("behavior", "allow".into()),
            (
                "updatedInput",
                coalesce(field(decision, "updatedInput"), input),
            ),
        ])
    } else {
        J::object([
            ("behavior", "deny".into()),
            ("message", "denied by user".into()),
        ])
    }
}
pub enum Callback {
    Immediate(J),
    Permission {
        event: J,
        input: J,
    },
    UserInput {
        event: J,
        input: J,
        questions: Vec<J>,
    },
}
pub fn callback(
    context: &J,
    tool_name: &J,
    input: &J,
    tool_id: &J,
    auto_allow: bool,
    ids: &IdSource,
) -> Callback {
    let tool_id = if truthy(tool_id) {
        string(tool_id)
    } else {
        concat(&[&"tool-".into(), &(ids)()])
    };
    let prefix = concat(&[
        &string(field(context, "threadId")),
        &":".into(),
        &string(field(context, "turnId")),
        &":".into(),
    ]);
    if is(tool_name, "AskUserQuestion") {
        let questions = match questions(input) {
            Ok(q) => q,
            Err(e) => {
                return Callback::Immediate(J::object([
                    ("behavior", "deny".into()),
                    (
                        "message",
                        concat(&[&"AskUserQuestion failed: ".into(), &e.message]).into_json(),
                    ),
                ]));
            }
        };
        if questions.is_empty() {
            return Callback::Immediate(J::object([
                ("behavior", "deny".into()),
                ("message", "no questions provided".into()),
            ]));
        }
        let request = concat(&[&prefix, &"askq:".into(), &tool_id]);
        let event = J::object([
            ("type", "user_input_request".into()),
            ("requestId", request.into_json()),
            ("toolUseId", tool_id.into_json()),
            ("questions", J::Array(questions.clone())),
        ]);
        return Callback::UserInput {
            event,
            input: input.clone(),
            questions,
        };
    }
    if auto_allow {
        return Callback::Immediate(permission_result(input, &J::Null, true));
    }
    let request = concat(&[&prefix, &string(tool_name), &":".into(), &tool_id]);
    let event = J::object([
        ("type", "permission_request".into()),
        ("requestId", request.into_json()),
        ("toolUseId", tool_id.into_json()),
        ("toolName", tool_name.clone()),
        ("input", input.clone()),
    ]);
    Callback::Permission {
        event,
        input: input.clone(),
    }
}
pub fn user_input_result(input: &J, questions: &[J], answers: &J) -> Result<J> {
    Ok(J::object([
        ("behavior", "deny".into()),
        (
            "message",
            format_answers(input, questions, answers)?.into_json(),
        ),
    ]))
}
