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

#[path = "record_upgrades.rs"]
pub mod record_upgrades;
#[path = "task_comments.rs"]
pub mod task_comments;

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

fn property_mut<'a>(value: &'a mut Json, key: &str) -> Option<&'a mut Json> {
    let Json::Object(items) = value else {
        return None;
    };
    let name = JsString::from(key);
    items.iter_mut().find(|(k, _)| k == &name).map(|(_, v)| v)
}
async fn fill_artifact(client: &dyn RpcClient, artifact: &mut Json) -> IvyResult<()> {
    if property(artifact, "byteLength")?.is_some() {
        return Ok(());
    }
    let saved = client
        .request(
            "objects.read",
            needed(artifact, "object")?.clone(),
            RequestOptions::default(),
        )
        .await?;
    let revision = needed(&saved, "revision")?;
    require(
        property(revision, "contentHash")? == property(artifact, "contentHash")?
            && property(revision, "mediaType")? == property(artifact, "mediaType")?,
        "task_board_evidence_mismatch",
        "Retained verification evidence differs from its immutable pin.",
    )?;
    if let Some(size) = revision.get("byteLength") {
        if !matches!(artifact, Json::Object(_)) {
            return Err(IvyError::internal());
        }
        artifact.insert("byteLength", size.clone());
    }
    Ok(())
}
async fn fill_findings(client: &dyn RpcClient, findings: &mut Json) -> IvyResult<()> {
    if findings.is_null() {
        return Ok(());
    }
    if matches!(findings, Json::String(_)) {
        return Ok(());
    }
    let Json::Array(items) = findings else {
        return Err(IvyError::internal());
    };
    for finding in items {
        property(finding, "evidence")?;
        if let Some(evidence) = property_mut(finding, "evidence") {
            match evidence {
                Json::Null => {}
                Json::Array(artifacts) => {
                    for artifact in artifacts {
                        fill_artifact(client, artifact).await?;
                    }
                }
                artifact => fill_artifact(client, artifact).await?,
            }
        }
    }
    Ok(())
}
fn gather_findings(value: Option<&Json>, findings: &mut Vec<Json>) -> IvyResult<()> {
    match value {
        None | Some(Json::Null | Json::String(_)) => Ok(()),
        Some(Json::Array(items)) => {
            findings.extend_from_slice(items);
            Ok(())
        }
        _ => Err(IvyError::internal()),
    }
}
/// Fill only the service-owned retained verification sizes. The actual Shared
/// record upgrader must still run afterward on remote materialized records.
pub async fn fill_verification_sizes(
    client: &dyn RpcClient,
    key: &str,
    value: &Json,
) -> IvyResult<Json> {
    let mut stored = value.clone();
    if key != "task-board/task" {
        return Ok(stored);
    }
    // The original gathers every finding/evidence list before its first read.
    let mut findings = Vec::new();
    gather_findings(property(&stored, "latestChecks")?, &mut findings)?;
    if let Some(content) = stored.get("content").filter(|v| !v.is_null()) {
        gather_findings(property(content, "checks")?, &mut findings)?;
    }
    if let Some(comments) = stored.get("comments").filter(|v| !v.is_null()) {
        for comment in comments.as_array().ok_or_else(IvyError::internal)? {
            if let Some(handoff) = property(comment, "handoff")?.filter(|v| !v.is_null()) {
                gather_findings(property(handoff, "checks")?, &mut findings)?;
            }
        }
    }
    for finding in &findings {
        property(finding, "evidence")?;
    }
    if let Some(checks) = property_mut(&mut stored, "latestChecks") {
        fill_findings(client, checks).await?;
    }
    if let Some(content) = property_mut(&mut stored, "content") {
        if !content.is_null() {
            if let Some(checks) = property_mut(content, "checks") {
                fill_findings(client, checks).await?;
            }
        }
    }
    if let Some(comments) = property_mut(&mut stored, "comments") {
        if !comments.is_null() {
            let Json::Array(comments) = comments else {
                return Err(IvyError::internal());
            };
            for comment in comments {
                property(comment, "handoff")?;
                if let Some(handoff) = property_mut(comment, "handoff") {
                    if !handoff.is_null() {
                        if let Some(checks) = property_mut(handoff, "checks") {
                            fill_findings(client, checks).await?;
                        }
                    }
                }
            }
        }
    }
    Ok(stored)
}
