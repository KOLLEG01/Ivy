//! Current service-owned persistence reads and explicit Shared bindings.
use super::JournalStore;
use crate::engine_read::condition_policy::ConditionStore;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{parse, try_js_string},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
use ivy_store::rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;

#[path = "read_ports.rs"]
pub mod read_ports;
#[path = "record_upgrades.rs"]
pub mod record_upgrades;
#[path = "task_comments.rs"]
pub mod task_comments;
#[path = "verification_sizes.rs"]
pub mod verification_sizes;

fn property<'a>(value: &'a Json, key: &str) -> IvyResult<Option<&'a Json>> {
    if value.is_null() {
        return Err(IvyError::internal());
    }
    Ok(value.get(key))
}
fn needed<'a>(value: &'a Json, key: &str) -> IvyResult<&'a Json> {
    property(value, key)?.ok_or_else(IvyError::internal)
}
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null | Json::Bool(false) => false,
        Json::Number(n) => *n != 0.0 && !n.is_nan(),
        Json::String(s) => !s.0.is_empty(),
        _ => true,
    }
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn spread(value: &Json) -> Json {
    match value {
        Json::Object(_) => value.clone(),
        Json::Array(items) => Json::object(
            items
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v.clone())),
        ),
        Json::String(s) => Json::object(
            s.0.iter()
                .enumerate()
                .map(|(i, unit)| (i.to_string(), Json::String(JsString(vec![*unit])))),
        ),
        _ => Json::Object(vec![]),
    }
}
fn map_key_equal(a: Option<&Json>, b: Option<&Json>) -> bool {
    match (a, b) {
        (None, None) | (Some(Json::Null), Some(Json::Null)) => true,
        (Some(Json::String(a)), Some(Json::String(b))) => a == b,
        (Some(Json::Bool(a)), Some(Json::Bool(b))) => a == b,
        (Some(Json::Number(a)), Some(Json::Number(b))) => a == b || a.is_nan() && b.is_nan(),
        (Some(a @ (Json::Object(_) | Json::Array(_))), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}

/// Query the current retained Operation revision without using an action cache.
/// Native SQLite/JSON access failures use the original non-Ivy error boundary.
pub fn failed_handoff(connection: &Connection, run_id: &JsString) -> IvyResult<Option<Json>> {
    let raw: Option<String> = connection.query_row(
        "SELECT r.value_json FROM documents d
         JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision
         WHERE d.key='task-board/operation' AND json_extract(r.value_json,'$.handoffRun.objectId')=?
           AND json_extract(r.value_json,'$.phase')='failed'
           AND json_extract(r.value_json,'$.error.code') IN ('content_too_large','storage_full','quota_exceeded','database_error')
         ORDER BY d.updated_at DESC LIMIT 1",
        [crate::local_journal::text(run_id)],
        |row| row.get(0),
    ).optional().map_err(|_| IvyError::internal())?;
    let Some(raw) = raw else { return Ok(None) };
    let operation = parse(&raw, MANAGEMENT_FRAME_BYTES).map_err(|_| IvyError::internal())?;
    let error = needed(&operation, "error")?;
    let message = needed(error, "message")?;
    let truncated = match message {
        Json::String(s) => Json::String(JsString(s.0.iter().copied().take(1800).collect())),
        Json::Array(items) => Json::Array(items.iter().take(1800).cloned().collect()),
        _ => return Err(IvyError::internal()),
    };
    let mut detail = JsString::from("Die Übergabe konnte nicht gespeichert werden: ");
    detail.0.extend(try_js_string(&truncated)?.0);
    let mut result = Json::object([
        ("code", needed(error, "code")?.clone()),
        ("detail", Json::String(detail)),
    ]);
    if let Some(id) = operation.get("operationId") {
        result.insert("operationId", id.clone());
    }
    Ok(Some(result))
}
impl<R, P, C, I> ConditionStore for JournalStore<'_, R, P, C, I> {
    fn failed_handoff(&self, run_id: &JsString) -> IvyResult<Option<Json>> {
        failed_handoff(self.journal.connection, run_id)
    }
}

/// Materialize inline records or an append-only retained conversation prefix.
/// `None` selects the separate immutable-tree reader, never an empty conversation.
pub async fn read_legacy_conversation(
    client: &dyn RpcClient,
    value: &Json,
    task_id: &JsString,
) -> IvyResult<Option<Json>> {
    let reference = property(value, "commentStore")?;
    let Some(reference) = reference.filter(|v| truthy(v)) else {
        return Ok(Some(value.clone()));
    };
    if !matches!(reference, Json::Object(_) | Json::Array(_)) {
        return Err(IvyError::internal());
    }
    if reference.get("kind").is_some() {
        return Ok(None);
    }
    let mut params = Json::Object(vec![]);
    if let Some(id) = reference.get("objectId") {
        params.insert("objectId", id.clone());
    }
    let saved = client
        .request("objects.read", params, RequestOptions::default())
        .await?;
    let object = needed(&saved, "object")?;
    require(
        property(object, "contractKey")? == Some(&Json::from("task-board/comments"))
            && property(object, "ownerObjectId")? == Some(&Json::Null)
            && property(needed(&saved, "content")?, "encoding")? == Some(&Json::from("json")),
        "task_board_contract_mismatch",
        "The ticket conversation is not a retained comment record.",
    )?;
    let log = needed(needed(&saved, "content")?, "value")?;
    let comments = property(log, "comments")?.and_then(Json::as_array);
    let length = match reference {
        Json::Array(items) => Some(items.len() as f64),
        _ => reference.get("length").and_then(Json::as_f64),
    };
    require(
        property(log, "taskId")?.and_then(Json::as_js_string) == Some(task_id)
            && property(object, "parentId")?.and_then(Json::as_js_string) == Some(task_id)
            && comments.is_some()
            && length.is_some_and(|n| {
                n.is_finite()
                    && n.fract() == 0.0
                    && n >= 0.0
                    && n <= 9007199254740991.0
                    && n <= comments.unwrap().len() as f64
            }),
        "task_board_evidence_mismatch",
        "The ticket conversation prefix is unavailable.",
    )?;
    let states = needed(reference, "nativeInputs")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let mut inputs = HashMap::new();
    let mut other_keys = Vec::new();
    for state in states {
        match property(state, "commentId")? {
            Some(Json::String(id)) => {
                inputs.insert(id.clone(), state);
            }
            key => other_keys.push((key, state)),
        }
    }
    let mut hydrated = Vec::new();
    for comment in &comments.unwrap()[..length.unwrap() as usize] {
        let id = property(comment, "commentId")?;
        let mut body = spread(comment);
        for key in ["nativeInput", "handoff", "delivery"] {
            body.remove(key);
        }
        let state = match id {
            Some(Json::String(id)) => inputs.get(id).copied(),
            _ => other_keys
                .iter()
                .rev()
                .find(|(key, _)| map_key_equal(*key, id))
                .map(|(_, state)| *state),
        };
        if let Some(state) = state {
            for (key, value) in spread(state).as_object().unwrap() {
                body.insert(key.clone(), value.clone());
            }
        }
        hydrated.push(body);
    }
    let mut task = spread(value);
    task.remove("commentStore");
    task.insert("comments", Json::Array(hydrated));
    Ok(Some(task))
}

/// Compatibility API for existing typed callers. Lossless read ports use the
/// same core with their original optional-parameter reader and raw failures.
pub async fn fill_verification_sizes(
    client: &dyn RpcClient,
    key: &str,
    value: &Json,
) -> IvyResult<Json> {
    verification_sizes::fill_verification_sizes_with(key, value, |params| {
        record_upgrades::read_verification(client, params)
    })
    .await
    .map_err(|error| match error {
        record_upgrades::RecordFailure::Ivy(error) => error,
        record_upgrades::RecordFailure::JavaScript(_) => IvyError::internal(),
    })
}
