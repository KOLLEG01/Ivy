//! One immutable accepted upload body, referenced by later operation stages.
use crate::operations::{Document, Store};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, Outcome, hash::hash_json, json::canonical_equal,
    limits::MANAGEMENT_FRAME_BYTES,
};

fn mismatch() -> IvyError {
    IvyError::new(
        "task_board_evidence_mismatch",
        "The upload's original accepted request is unavailable or differs.",
    )
    .with_outcome(Outcome::Unknown)
}
fn require(ok: bool) -> IvyResult<()> {
    if ok { Ok(()) } else { Err(mismatch()) }
}

pub(crate) fn retain_original(operation: &mut Json, id: &Json) {
    if operation
        .get("phase")
        .is_some_and(|phase| phase.is_str("succeeded"))
    {
        return;
    }
    let Some(request) = operation.get_mut("request") else {
        return;
    };
    if request
        .get("action")
        .is_some_and(|action| action.is_str("uploadAttachment"))
        && request.get("bytesBase64").is_some()
    {
        // Revision1 is already durable and retained with this operation root.
        // Never rewrite that original or prune historical evidence here.
        request.remove("bytesBase64");
        request.insert(
            "originalRequest",
            Json::object([("objectId", id.clone()), ("revision", Json::Number(1.0))]),
        );
    }
}

/// Resolve only at upload execution, after the task's current owner/base guards.
/// A descriptor is evidence of the accepted original, never replay authority.
pub async fn original_upload_request<S: Store>(
    store: &S,
    operation: &Document,
) -> IvyResult<Option<Json>> {
    let request = operation.value.required("request")?;
    if !request
        .get("action")
        .is_some_and(|action| action.is_str("uploadAttachment"))
    {
        return Ok(None);
    }
    let Some(pin) = request.get("originalRequest") else {
        return Ok(None);
    };
    require(
        operation.pin.revision > 1.0
            && pin.get("objectId").and_then(Json::as_js_string) == Some(&operation.pin.object_id)
            && pin.get("revision").and_then(Json::as_f64) == Some(1.0),
    )?;
    let mut original = store
        .read("task-board/operation", pin, false)
        .await
        .map_err(|error| error.with_outcome(Outcome::Unknown))?;
    require(
        original.pin.object_id == operation.pin.object_id
            && original.pin.revision == 1.0
            && original.parent_id == operation.parent_id
            && ["operationId", "callerPrincipalId", "requestHash", "actor"]
                .iter()
                .all(|key| original.value.get(key) == operation.value.get(key)),
    )?;
    let mut materialized = original.value.remove("request").ok_or_else(mismatch)?;
    let hash = hash_json(&materialized).map_err(|error| error.with_outcome(Outcome::Unknown))?;
    require(
        operation
            .value
            .get("requestHash")
            .is_some_and(|saved| saved.is_str(&hash)),
    )?;
    let body = materialized
        .remove("bytesBase64")
        .filter(|value| value.as_js_string().is_some())
        .ok_or_else(mismatch)?;
    materialized.insert("originalRequest", pin.clone());
    require(
        canonical_equal(&materialized, request, MANAGEMENT_FRAME_BYTES)
            .map_err(|error| error.with_outcome(Outcome::Unknown))?,
    )?;
    materialized.remove("originalRequest");
    materialized.insert("bytesBase64", body);
    Ok(Some(materialized))
}
