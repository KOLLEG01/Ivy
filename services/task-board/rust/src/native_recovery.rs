//! Saved recovery lineage and failed empty contexts. These reads neither dispatch
//! native work nor replace an uncertain original execution.
use crate::operations::Store;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use std::collections::HashSet;

fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(value) => *value,
        Json::Number(value) => *value != 0.0 && !value.is_nan(),
        Json::String(value) => !value.0.is_empty(),
        _ => true,
    }
}

pub async fn context_recovery_history<S: Store>(
    store: &S,
    task: &Json,
    task_id: &JsString,
) -> IvyResult<Option<Json>> {
    let mut pin = field(task, "lastHistory").clone();
    let mut visited = HashSet::new();
    while truthy(&pin) && truthy(field(task, "lastRun")) {
        if !visited.insert(string(&pin, "objectId")?.clone()) {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "The recovery History lineage contains a cycle.",
            ));
        }
        let history = store.read("task-board/history", &pin, true).await?;
        if history.value.get("taskId").and_then(Json::as_js_string) != Some(task_id) {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "Recovery History must belong to this ticket.",
            ));
        }
        if field(&history.value, "priorRun").get("objectId")
            != field(task, "lastRun").get("objectId")
            || field(&history.value, "kind") == &Json::from("reassigned")
        {
            break;
        }
        if field(&history.value, "kind") == &Json::from("recovered")
            && field(&history.value, "actor").get("source") == Some(&Json::from("scheduler"))
        {
            return Ok(Some(history.value));
        }
        pin = field(&history.value, "previousHistory").clone();
    }
    Ok(None)
}

pub async fn fresh_context_recovery<S: Store>(store: &S, task: &Json) -> IvyResult<Option<Json>> {
    if truthy(field(task, "primaryResourceRef")) || !truthy(field(task, "lastRun")) {
        return Ok(None);
    }
    let run = store
        .read("task-board/run", field(task, "lastRun"), true)
        .await?;
    if field(&run.value, "phase") != &Json::from("failed")
        || truthy(field(&run.value, "turnId"))
        || truthy(field(&run.value, "cancellation"))
    {
        return Ok(None);
    }
    Ok(
        if context_recovery_history(store, task, string(&run.value, "taskId")?)
            .await?
            .is_some()
        {
            Some(run.value)
        } else {
            None
        },
    )
}

pub fn missing_native_thread(reply: &Json, thread_id: &JsString) -> IvyResult<bool> {
    let Some(error) = reply.get("error") else {
        return Ok(false);
    };
    let message = string(error, "message")?;
    if !thread_id.0.is_empty()
        && !message
            .0
            .windows(thread_id.0.len())
            .any(|part| part == thread_id.0)
    {
        return Ok(false);
    }
    // The source's non-Unicode /i expression contains only ASCII keywords. Its
    // wildcard cannot cross any JavaScript line terminator; the two ordered
    // alternatives together accept either keyword order on the same line.
    let lower = message
        .0
        .iter()
        .map(|unit| {
            if (65..=90).contains(unit) {
                unit + 32
            } else {
                *unit
            }
        })
        .collect::<Vec<_>>();
    let contains = |line: &[u16], word: &str| {
        line.windows(word.len())
            .any(|part| part.iter().copied().eq(word.encode_utf16()))
    };
    Ok(lower
        .split(|unit| matches!(unit, 10 | 13 | 0x2028 | 0x2029))
        .any(|line| {
            contains(line, "no rollout found")
                || ((contains(line, "thread") || contains(line, "rollout"))
                    && (contains(line, "not found") || contains(line, "does not exist")))
        }))
}
