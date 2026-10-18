//! Assignment completion after original-owner reads and complete item verification.
//! These domain checks do not authenticate a native observation or its item pages.
use crate::assignment_decision::assignment_decision;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};

fn failure(code: &str, message: &str) -> IvyError {
    IvyError::new(code, message)
}

/// Select the retained execution's single original turn from an admitted read.
pub fn assignment_turn<'a>(execution: &IvyJson, read: &'a IvyJson) -> IvyResult<&'a IvyJson> {
    if !execution
        .get("threadId")
        .and_then(IvyJson::as_js_string)
        .is_some_and(|s| !s.0.is_empty())
    {
        return Err(failure(
            "secretary_execution_thread_missing",
            "The running Secretary execution has no task.",
        ));
    }
    let rows = read
        .get("reply")
        .and_then(|v| v.get("result"))
        .and_then(|v| v.get("data"))
        .and_then(IvyJson::as_array)
        .filter(|rows| rows.len() == 1)
        .ok_or_else(|| {
            failure(
                "secretary_execution_turn_missing",
                "The Secretary task turn is unavailable.",
            )
        })?;
    let row = &rows[0];
    if row.get("id") != execution.get("turnId") {
        return Err(failure(
            "secretary_execution_turn_changed",
            "A different task turn replaced the Secretary execution.",
        ));
    }
    Ok(row)
}

/// None means the original turn is still running. `items` must be the complete
/// output of the shared native contents verifier, not an unchecked read fragment.
pub fn assignment_completion(
    execution: &IvyJson,
    turn: &IvyJson,
    items: &[IvyJson],
    now: &JsString,
) -> Option<IvyJson> {
    if turn.get("status") == Some(&"inProgress".into()) {
        return None;
    }
    let answers: Vec<&JsString> = items
        .iter()
        .filter(|item| {
            item.get("type") == Some(&"agentMessage".into())
                && item.get("phase") == Some(&"final_answer".into())
        })
        .filter_map(|item| item.get("text").and_then(IvyJson::as_js_string))
        .collect();
    let error = if turn.get("status") != Some(&"completed".into()) {
        Some("secretary_execution_turn_failed".to_owned())
    } else if answers.len() != 1 {
        Some("secretary_execution_answer_missing".to_owned())
    } else if char::decode_utf16(answers[0].0.iter().copied()).count() > 131_072 {
        Some("secretary_execution_answer_invalid".to_owned())
    } else {
        assignment_decision(answers[0]).err().map(|e| e.code)
    };
    let reuse = execution
        .get("assignmentSnapshot")
        .and_then(|v| v.get("execution"))
        .and_then(|v| v.get("reuse"))
        .filter(|v| !v.is_null());
    let reused = reuse.is_some_and(|v| v != &"new".into());
    let phase = if error.is_some() {
        if reused { "failed" } else { "archiving" }
    } else {
        "completed"
    };
    let mut next = execution.clone();
    next.insert("phase", phase.into());
    next.insert(
        "result",
        if error.is_some() {
            IvyJson::Null
        } else {
            IvyJson::String(answers[0].clone())
        },
    );
    next.insert("errorCode", error.map_or(IvyJson::Null, IvyJson::from));
    next.insert("completedAt", IvyJson::String(now.clone()));
    next.insert("updatedAt", IvyJson::String(now.clone()));
    Some(next)
}
