//! Derived journal indexes. Original documents and evidence remain unchanged.
use super::{Pin, StoreResult};
use ivy_contracts::{
    IvyJson as Json, hash::hash_json, json::canonical, operation::parse_operation_id_js,
};
use ivy_store::receipts::has_unresolved_outcome;
use rusqlite::{Connection, OptionalExtension, params, types::Value};
use std::collections::HashSet;

pub(super) const PAGE: i64 = 32;

fn at<'a>(value: &'a Json, path: &[&str]) -> Option<&'a Json> {
    path.iter().try_fold(value, |v, key| v.get(key))
}
fn text(value: Option<&Json>) -> Option<String> {
    value.and_then(|v| v.as_str().ok())
}
fn is(value: Option<&Json>, expected: &str) -> bool {
    value == Some(&expected.into())
}

fn select(value: &Json, path: &[&str], pins: &mut HashSet<Pin>, object_only: bool) {
    let Some((first, rest)) = path.split_first() else {
        if object_only {
            if let Some(object_id) = text(Some(value)) {
                pins.insert(Pin {
                    object_id,
                    revision: 0,
                });
            }
        } else if let (Some(object_id), Some(revision)) = (
            text(value.get("objectId")),
            value.get("revision").and_then(Json::as_f64),
        ) {
            if revision >= 1.0 && revision.fract() == 0.0 && revision <= 9_007_199_254_740_991.0 {
                pins.insert(Pin {
                    object_id,
                    revision: revision as i64,
                });
            }
        }
        return;
    };
    let (key, array) = first
        .strip_suffix("[]")
        .map(|key| (key, true))
        .unwrap_or((first, false));
    if let Some(next) = value.get(key) {
        if array {
            if let Some(values) = next.as_array() {
                for v in values {
                    select(v, rest, pins, object_only);
                }
            }
        } else {
            select(next, rest, pins, object_only);
        }
    }
}

/// Only schema-defined pins/IDs are edges. Opaque request data and text are not.
pub(super) fn references(key: &str, value: &Json) -> HashSet<Pin> {
    let paths: &[&str] = match key {
        "chat-bridge/main" => &[
            "binding",
            "conversation",
            "pendingAction",
            "publication.collection",
            "publication.result",
            "queue[].binding",
        ],
        "chat-bridge/binding" => &["predecessor"],
        "chat-bridge/native-call" => &["request", "predecessor", "evidence"],
        "chat-bridge/evidence" => &["chunks[].object"],
        "chat-bridge/collection" => &[
            "input",
            "binding",
            "turnCall",
            "previousAttempt",
            "reads[]",
            "result",
            "retryAuthorization.failed",
        ],
        "chat-bridge/offer" => &["replies[]"],
        "chat-bridge/acknowledgement" => &["offer"],
        "chat-bridge/operation" => &[
            "nativeCall",
            "request.expectedBinding",
            "request.resumeMain",
            "request.source",
            "request.payload.images[].object",
            "outcome.acknowledgement.object",
            "outcome.acknowledgement.data.offer",
            "outcome.binding.object",
            "outcome.binding.data.predecessor",
            "outcome.input.object",
            "outcome.input.data.binding",
            "outcome.input.data.result",
            "outcome.input.data.payload.images[].object",
            "outcome.input.data.nativeCalls.turn",
            "outcome.input.data.nativeCalls.resume",
            "outcome.input.data.nativeCalls.interrupt",
            "outcome.offer.object",
            "outcome.offer.data.replies[]",
            "outcome.reply.object",
            "outcome.reply.data.artifacts[].object",
            "outcome.reply.data.origin.binding",
            "outcome.reply.data.origin.claim",
            "outcome.reply.data.origin.operation",
            "outcome.reply.data.origin.result",
            "outcome.reply.data.origin.source",
            "outcome.replies[].object",
            "outcome.replies[].data.artifacts[].object",
            "outcome.replies[].data.origin.binding",
            "outcome.replies[].data.origin.claim",
            "outcome.replies[].data.origin.operation",
            "outcome.replies[].data.origin.result",
            "outcome.replies[].data.origin.source",
        ],
        _ => &[],
    };
    let ids: &[&str] = match key {
        "chat-bridge/main" => &["queue[].inputId", "publication.inputId"],
        "chat-bridge/operation" => &["request.inputId", "request.offerId", "request.replyIds[]"],
        "chat-bridge/acknowledgement" => &["replyIds[]"],
        _ => &[],
    };
    let mut pins = HashSet::new();
    for path in paths {
        select(
            value,
            &path.split('.').collect::<Vec<_>>(),
            &mut pins,
            false,
        );
    }
    for path in ids {
        select(value, &path.split('.').collect::<Vec<_>>(), &mut pins, true);
    }
    pins
}

fn sort_key(value: Option<&Json>) -> Value {
    match value {
        None | Some(Json::Null) => Value::Null,
        Some(Json::Bool(v)) => Value::Integer(i64::from(*v)),
        Some(Json::Number(v)) if v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0 => {
            Value::Integer(*v as i64)
        }
        Some(Json::Number(v)) => Value::Real(*v),
        Some(Json::String(v)) => v.to_utf8().map(Value::Text).unwrap_or(Value::Null),
        Some(v) => canonical(v, usize::MAX)
            .map(Value::Text)
            .unwrap_or(Value::Null),
    }
}

pub(super) fn save(
    db: &Connection,
    key: &str,
    id: &str,
    revision: i64,
    value: &Json,
) -> StoreResult<()> {
    let operation = key == "chat-bridge/operation";
    let terminal =
        operation && (is(value.get("phase"), "succeeded") || is(value.get("phase"), "failed"));
    let mut outcome = Json::object([] as [(&str, Json); 0]);
    for key in ["error", "nativeCall"] {
        if let Some(v) = value.get(key) {
            outcome.insert(key, v.clone());
        }
    }
    let blocked = operation && has_unresolved_outcome(&outcome)?;
    let open = match key {
        "chat-bridge/operation" => !terminal,
        "chat-bridge/native-call" => ["prepared", "accepted", "dispatched", "outcome_unknown"]
            .iter()
            .any(|state| is(value.get("state"), state)),
        "chat-bridge/collection" => ["collecting", "ready"]
            .iter()
            .any(|state| is(value.get("state"), state)),
        _ => false,
    };
    let retain = open
        || if operation {
            blocked
        } else {
            has_unresolved_outcome(value)?
        };
    let identity = value
        .get("operationId")
        .and_then(Json::as_js_string)
        .and_then(|id| {
            let parsed = parse_operation_id_js(id).ok()?;
            Some((id.to_utf8().ok()?, parsed.issued_at_unix_ms as i64))
        });
    let action = text(at(value, &["request", "action"]));
    let notice_hash = if operation && action.as_deref() == Some("notify") {
        Some(hash_json(&Json::Array(vec![
            value
                .get("callerPrincipalId")
                .cloned()
                .unwrap_or(Json::Null),
            value.get("operationId").cloned().unwrap_or(Json::Null),
        ]))?)
    } else {
        None
    };
    let input = text(at(value, &["outcome", "input", "object", "objectId"]));
    let reply = if is(at(value, &["outcome", "action"]), "notify") {
        text(at(value, &["outcome", "reply", "object", "objectId"]))
    } else {
        None
    };
    let offer = text(at(value, &["request", "offerId"]));
    let expected_main = if action.as_deref() == Some("createMain") {
        at(value, &["request", "expectedMainRevision"])
            .and_then(Json::as_f64)
            .map(|v| v as i64)
    } else {
        None
    };
    db.execute("INSERT OR REPLACE INTO journal_metadata SELECT ?,?,COALESCE((SELECT rowid FROM documents WHERE id=?),0),?,?,?,?,?,?,?,?,?,?,?,?", params![id,revision,id,retain,terminal,blocked,identity.as_ref().map(|v|&v.0),identity.as_ref().map(|v|v.1),sort_key(value.get("updatedAt")),action,notice_hash,input,reply,offer,expected_main])?;
    db.execute(
        "DELETE FROM journal_references WHERE source_id=? AND source_revision=?",
        params![id, revision],
    )?;
    let mut insert =
        db.prepare_cached("INSERT OR IGNORE INTO journal_references VALUES(?,?,?,?)")?;
    for pin in references(key, value) {
        insert.execute(params![id, revision, pin.object_id, pin.revision])?;
    }
    db.execute(
        "DELETE FROM journal_metadata_pending WHERE id=? AND revision=?",
        params![id, revision],
    )?;
    Ok(())
}

/// Adoption and externally changed original rows are processed one body at a time.
pub(super) fn refresh(db: &Connection) -> StoreResult<()> {
    loop {
        let mut query =
            db.prepare("SELECT id,revision FROM journal_metadata_pending ORDER BY rowid LIMIT ?")?;
        let page = query
            .query_map([PAGE], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if page.is_empty() {
            return Ok(());
        }
        for (id, revision) in page {
            let row = db.query_row("SELECT d.key,r.value_json FROM revisions r LEFT JOIN documents d ON d.id=r.id WHERE r.id=? AND r.revision=?",params![id,revision],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,String>(1)?))).optional()?;
            if let Some((key, body)) = row {
                save(
                    db,
                    key.as_deref().unwrap_or(""),
                    &id,
                    revision,
                    &super::decoded(&body)?,
                )?;
            } else {
                db.execute(
                    "DELETE FROM journal_metadata_pending WHERE id=? AND revision=?",
                    params![id, revision],
                )?;
            }
        }
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/chat-local-metadata.rs"]
mod tests;
