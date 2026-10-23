//! Immutable evidence bytes and shape checks. Native authority belongs to the
//! caller; native drafts require the native-plan validator supplied at this boundary.
use crate::{
    limits::{EVIDENCE_CHUNK_BYTES, MAXIMUM_EVIDENCE_BYTES},
    store::{ChatStore, Destination, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult,
    agent_schema::validate_agent,
    hash::digest,
    json::{canonical, parse},
};
use std::collections::HashSet;

pub type NativeDraftValidator<'a> = dyn Fn(&Json) -> IvyResult<()> + Send + Sync + 'a;

fn require(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("chat_evidence_mismatch", message))
    }
}
fn validate_body(format: &str, value: &Json, draft: &NativeDraftValidator<'_>) -> IvyResult<()> {
    match format {
        "native-request/canonical-json" => draft(value),
        "agent-operation/canonical-json" => validate_agent("Operation", value),
        "agent-read-observation/canonical-json" => validate_agent("ReadObservation", value),
        _ => Err(IvyError::new(
            "chat_evidence_mismatch",
            "Unknown native evidence format.",
        )),
    }
}
async fn scope(store: &ChatStore, parent_id: &str) -> IvyResult<()> {
    match store
        .read("chat-bridge/operation", Reference::Current(parent_id))
        .await
    {
        Ok(_) => Ok(()),
        Err(error) => {
            let error = error.into_ivy();
            if error.code != "chat_scope_mismatch" {
                return Err(error);
            }
            store
                .read("chat-bridge/input", Reference::Current(parent_id))
                .await
                .map(|_| ())
                .map_err(|error| error.into_ivy())
        }
    }
}
fn pin(value: &Json) -> IvyResult<Pin> {
    require(
        value.get("revision") == Some(&Json::Number(1.0)),
        "Local evidence blobs have one immutable revision.",
    )?;
    Ok(Pin {
        object_id: value.required("objectId")?.as_str()?,
        revision: 1,
    })
}
fn length_of(value: &Json) -> IvyResult<f64> {
    value.required("byteLength")?.as_f64().ok_or_else(|| {
        IvyError::new(
            "chat_evidence_mismatch",
            "The complete evidence body has another hash or length.",
        )
    })
}

pub async fn save_chat_evidence(
    store: &ChatStore,
    parent_id: &str,
    format: &str,
    original: &Json,
    draft: &NativeDraftValidator<'_>,
) -> IvyResult<Pin> {
    let encoded = canonical(original, MAXIMUM_EVIDENCE_BYTES)?;
    let value = parse(&encoded, MAXIMUM_EVIDENCE_BYTES)?;
    validate_body(format, &value, draft)?;
    let bytes = encoded.as_bytes();
    let content_hash = digest(bytes);
    let mut chunks = Vec::new();
    scope(store, parent_id).await?;
    for (index, part) in bytes.chunks(EVIDENCE_CHUNK_BYTES).enumerate() {
        let name = format!("Chat evidence {} part {index}", &content_hash[7..]);
        let object = store
            .save_local_blob("chat-bridge/evidence-chunk", &name, parent_id, part)
            .await
            .map_err(|error| error.into_ivy())?;
        chunks.push(Json::object([
            ("object", object.json()),
            ("contentHash", digest(part).into()),
            ("byteLength", Json::Number(part.len() as f64)),
            ("mediaType", "application/octet-stream".into()),
            (
                "label",
                format!("Native evidence part {}", index + 1).into(),
            ),
        ]));
    }
    let manifest = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("format", format.into()),
        ("byteLength", Json::Number(bytes.len() as f64)),
        ("contentHash", content_hash.clone().into()),
        ("chunks", Json::Array(chunks)),
    ]);
    let name = format!(
        "Chat evidence manifest {} {}",
        format.split('/').next().unwrap(),
        &content_hash[7..]
    );
    store
        .write(
            "chat-bridge/evidence",
            &manifest,
            &mutation(parent_id, &format!("{format}:{content_hash}:manifest"))?,
            Destination::Create {
                parent_id: Some(parent_id),
                name: &name,
            },
        )
        .await
        .map_err(|error| error.into_ivy())
}

pub async fn read_chat_evidence(
    store: &ChatStore,
    parent_id: &str,
    reference: &Pin,
    format: &str,
    draft: &NativeDraftValidator<'_>,
) -> IvyResult<Json> {
    scope(store, parent_id).await?;
    let manifest = store
        .read_in(
            "chat-bridge/evidence",
            Reference::Pinned(reference),
            Some(parent_id),
        )
        .await
        .map_err(|error| error.into_ivy())?
        .value;
    let chunks = manifest
        .get("chunks")
        .and_then(Json::as_array)
        .ok_or_else(IvyError::internal)?;
    let length = length_of(&manifest)?;
    let mut identities = HashSet::new();
    for chunk in chunks {
        let object = chunk.get("object").ok_or_else(IvyError::internal)?;
        // All chunks have one immutable revision, so their typed IDs are unique.
        identities.insert(pin(object)?.object_id);
    }
    require(
        manifest.get("format") == Some(&format.into())
            && chunks.len() as f64 == (length / EVIDENCE_CHUNK_BYTES as f64).ceil()
            && identities.len() == chunks.len(),
        "Evidence must retain its exact format and distinct ordered chunk pins.",
    )?;
    let mut body = Vec::with_capacity(length as usize);
    for (index, chunk) in chunks.iter().enumerate() {
        require(
            chunk.get("mediaType") == Some(&"application/octet-stream".into()),
            "An evidence chunk has another media type.",
        )?;
        let bytes = store
            .local_blob(
                "chat-bridge/evidence-chunk",
                &pin(chunk.required("object")?)?,
                parent_id,
            )
            .await
            .map_err(|error| error.into_ivy())?;
        require(
            bytes.len() as f64
                == (EVIDENCE_CHUNK_BYTES as f64)
                    .min(length - (index * EVIDENCE_CHUNK_BYTES) as f64)
                && chunk.get("byteLength") == Some(&Json::Number(bytes.len() as f64))
                && chunk.get("contentHash") == Some(&digest(&bytes).into()),
            "Evidence bytes differ from their exact ordered length and hash.",
        )?;
        body.extend_from_slice(&bytes);
    }
    require(
        manifest.get("byteLength") == Some(&Json::Number(body.len() as f64))
            && manifest.get("contentHash") == Some(&digest(&body).into()),
        "The complete evidence body has another hash or length.",
    )?;
    let invalid = || {
        IvyError::new(
            "chat_evidence_mismatch",
            "The evidence body is not valid UTF-8 JSON.",
        )
    };
    let decoded = std::str::from_utf8(&body).map_err(|_| invalid())?;
    // TextDecoder's default consumes one initial UTF-8 BOM before JSON.parse.
    let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(decoded);
    let value = parse(decoded, usize::MAX).map_err(|_| invalid())?;
    require(
        canonical(&value, MAXIMUM_EVIDENCE_BYTES)? == decoded,
        "Evidence must retain canonical JSON bytes.",
    )?;
    validate_body(format, &value, draft)?;
    Ok(value)
}
