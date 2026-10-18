//! Complete immutable reply text and its separate, bounded presentation parts.
use crate::{
    limits::{COLLECTION_BYTES, EVIDENCE_CHUNK_BYTES},
    store::ChatStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, chat_schema::validate_chat, hash::digest,
    json::try_js_string,
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
                    .is_some_and(crate::main_context::truthy)
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
        // Buffer's replacement encoding cannot round-trip an unpaired unit.
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

/// Match JavaScript string iteration, including unpaired UTF16 units, without
/// changing the complete separately retained text or its artifact order.
pub fn chat_reply_parts(result: &Json, text: &JsString) -> IvyResult<Vec<Json>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    let mut count = 0;
    while offset < text.0.len() {
        let paired = (0xd800..=0xdbff).contains(&text.0[offset])
            && text
                .0
                .get(offset + 1)
                .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit));
        offset += if paired { 2 } else { 1 };
        count += 1;
        if count == 16_384 {
            parts.push(part(JsString(text.0[start..offset].to_vec())));
            start = offset;
            count = 0;
        }
    }
    if start < text.0.len() {
        parts.push(part(JsString(text.0[start..].to_vec())));
    }
    if parts.is_empty() {
        let state = result
            .get("nativeState")
            .map(try_js_string)
            .transpose()?
            .unwrap_or_else(|| "undefined".into());
        let mut fallback = JsString::from("Native turn ");
        fallback.0.extend(state.0);
        fallback.0.push(b'.' as u16);
        parts.push(part(fallback));
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
