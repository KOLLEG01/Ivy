//! Assignment completion after original-owner reads and complete item verification.
//! These domain checks do not authenticate a native observation or its item pages.
use crate::{
    assignment_decision::{AssignmentDecision, assignment_decision},
    assignment_runner::ExecutionPhase,
};
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

/// None means the original turn is still running. `answer` must come from the
/// shared verifier's complete contents, never an unchecked item fragment.
pub struct AssignmentCompletion {
    pub execution: IvyJson,
    pub decision: Option<AssignmentDecision>,
}
pub fn assignment_completion(
    execution: &IvyJson,
    turn: &IvyJson,
    answer: Option<&JsString>,
    now: &JsString,
) -> Option<AssignmentCompletion> {
    if turn.get("status") == Some(&"inProgress".into()) {
        return None;
    }
    let mut decision = None;
    let error = if turn.get("status") != Some(&"completed".into()) {
        Some("secretary_execution_turn_failed".to_owned())
    } else if answer.is_none() {
        Some("secretary_execution_answer_missing".to_owned())
    } else if char::decode_utf16(answer.unwrap().0.iter().copied())
        .take(131_073)
        .count()
        > 131_072
    {
        Some("secretary_execution_answer_invalid".to_owned())
    } else {
        match assignment_decision(answer.unwrap()) {
            Ok(parsed) => {
                decision = Some(parsed);
                None
            }
            Err(error) => Some(error.code),
        }
    };
    let reuse = execution
        .get("assignmentSnapshot")
        .and_then(|v| v.get("execution"))
        .and_then(|v| v.get("reuse"))
        .filter(|v| !v.is_null());
    let reused = reuse.is_some_and(|v| v != &"new".into());
    let phase = if error.is_some() {
        if reused {
            ExecutionPhase::Failed
        } else {
            ExecutionPhase::Archiving
        }
    } else {
        ExecutionPhase::Completed
    };
    let mut next = execution.clone();
    next.insert("phase", phase.json());
    next.insert(
        "result",
        if error.is_some() {
            IvyJson::Null
        } else {
            IvyJson::String(answer.unwrap().clone())
        },
    );
    next.insert("errorCode", error.map_or(IvyJson::Null, IvyJson::from));
    next.insert("completedAt", IvyJson::String(now.clone()));
    next.insert("updatedAt", IvyJson::String(now.clone()));
    Some(AssignmentCompletion {
        execution: next,
        decision,
    })
}
