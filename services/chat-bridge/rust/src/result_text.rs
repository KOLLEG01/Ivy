//! Complete immutable reply text and its separate, bounded presentation parts.
use crate::{
    limits::{COLLECTION_BYTES, EVIDENCE_CHUNK_BYTES},
    store::ChatStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, chat_schema::validate_chat, hash::digest,
};
use ivy_sdk::rpc::RequestOptions;
use std::collections::HashSet;

fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn array<'a>(value: &'a Json, key: &str) -> IvyResult<&'a [Json]> {
    value
        .get(key)
        .and_then(Json::as_array)
        .ok_or_else(IvyError::internal)
}
fn number(value: &Json, key: &str) -> IvyResult<f64> {
    value.required(key)?.as_f64().ok_or_else(IvyError::internal)
}

/// Read only the original immutable text revisions. No current-revision or
/// replacement-content fallback is permitted when a part cannot be verified.
pub async fn read_chat_result_text(
    store: &ChatStore,
    input_id: &JsString,
    result: &Json,
) -> IvyResult<JsString> {
    validate_chat("Result", result)?;
    require(
        result.get("inputId") == Some(&Json::String(input_id.clone())),
        "chat_evidence_mismatch",
        "Result text must belong to its original input.",
    )?;
    let mut identities = HashSet::new();
    let mut total = 0_f64;
    let mut text = String::new();
    for artifact in array(result, "textParts")? {
        let pin = artifact.required("object")?;
        let id = pin
            .required("objectId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        require(
            pin.get("revision") == Some(&Json::Number(1.0))
                && artifact.get("mediaType") == Some(&"text/plain".into())
                && !identities.contains(id),
            "chat_evidence_mismatch",
            "Result text must retain distinct immutable text parts.",
        )?;
        identities.insert(id.clone());
        total += number(artifact, "byteLength")?;
        require(
            total <= COLLECTION_BYTES as f64
                && number(artifact, "byteLength")? <= EVIDENCE_CHUNK_BYTES as f64,
            "chat_collection_limit",
            "Complete result text must fit its retained part and total byte bounds.",
        )?;
        let saved = store
            .client
            .request("objects.read", pin.clone(), RequestOptions::default())
            .await?;
        let object = saved.required("object")?;
        let revision = saved.required("revision")?;
        let content = saved.required("content")?;
        require(
            object.get("id") == pin.get("objectId")
                && object.get("parentId") == Some(&Json::String(input_id.clone()))
                && !object
                    .get("effectivelyArchived")
                    .and_then(Json::as_bool)
                    .unwrap_or(false)
                && object.get("contractKey") == Some(&"chat-bridge/result-text".into())
                && revision.get("revision") == pin.get("revision")
                && revision.get("contractVersion") == Some(&"1.0.0".into())
                && revision.get("mediaType") == artifact.get("mediaType")
                && content.get("encoding") == Some(&"text".into()),
            "chat_evidence_mismatch",
            "A reply text part must keep its exact original scope, contract and revision.",
        )?;
        let mismatch = || {
            IvyError::new(
                "chat_evidence_mismatch",
                "Retained reply text differs from its complete original bytes.",
            )
        };
        let value = content
            .required("value")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        // Retained text must be valid UTF-8 before verifying its exact bytes.
        let value = value.to_utf8().map_err(|_| mismatch())?;
        require(
            artifact.get("byteLength") == Some(&Json::Number(value.len() as f64))
                && revision.get("byteLength") == Some(&Json::Number(value.len() as f64))
                && artifact.get("contentHash") == Some(&digest(value.as_bytes()).into())
                && revision.get("contentHash") == artifact.get("contentHash"),
            "chat_evidence_mismatch",
            "Retained reply text differs from its complete original bytes.",
        )?;
        text.push_str(&value);
    }
    Ok(text.into())
}

fn part(text: JsString) -> Json {
    Json::object([
        ("text", Json::String(text)),
        ("artifacts", Json::Array(Vec::new())),
    ])
}

/// Split UTF-8 text by Unicode characters, retaining artifact order.
pub fn chat_reply_parts(result: &Json, text: &JsString) -> IvyResult<Vec<Json>> {
    let mut parts = Vec::new();
    let text = text.to_utf8()?;
    let mut characters = text.char_indices();
    while let Some((start, _)) = characters.next() {
        let end = characters
            .by_ref()
            .nth(16_382)
            .map(|(offset, value)| offset + value.len_utf8())
            .unwrap_or(text.len());
        parts.push(part(text[start..end].into()));
    }
    if parts.is_empty() {
        let state = result.required("nativeState")?.as_str()?;
        parts.push(part(format!("Native turn {state}.").into()));
    }
    for (index, artifacts) in array(result, "textParts")?.chunks(32).enumerate() {
        if index == parts.len() {
            parts.push(part(JsString::default()));
        }
        parts[index].insert("artifacts", Json::Array(artifacts.to_vec()));
    }
    require(
        parts.len() <= 65_536,
        "chat_collection_limit",
        "A complete reply must fit its ordered part count.",
    )?;
    Ok(parts)
}
