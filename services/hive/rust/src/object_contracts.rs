//! Immutable contract registration and the original object content boundary.
use crate::store::{AuthenticatedContext, LedgerResult, ReceiptKind, Store};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    core_schema::validate_shared,
    hash::digest,
    json::{canonical, compare_versions, parse},
    limits::{
        BINARY_OBJECT_CONTENT_BYTES, JSON_OBJECT_CONTENT_BYTES, MANAGEMENT_FRAME_BYTES,
        TEXT_OBJECT_CONTENT_BYTES,
    },
};
use rusqlite::{OptionalExtension, params};
use std::cmp::Ordering;

fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &IvyJson, field: &str) -> IvyResult<String> {
    value.get(field).ok_or_else(IvyError::internal)?.as_str()
}
fn owner_mode(definition: &IvyJson) -> &'static str {
    if definition
        .get("retention")
        .and_then(|r| r.get("objects"))
        .and_then(|r| r.get("mode"))
        == Some(&"owned".into())
    {
        "owned"
    } else {
        "retain"
    }
}
/// Called directly by trusted publishers, or inside the agent mutation receipt.
/// Keep direct registration's SQL boundaries; the caller owns its transaction.
pub fn register(store: &mut Store, definition: &IvyJson, owner: &IvyJson) -> IvyResult<IvyJson> {
    validate_shared("DataContract", definition)?;
    require(
        definition
            .get("retention")
            .and_then(|v| v.get("awaitOwnerReadiness"))
            .and_then(IvyJson::as_bool)
            != Some(true)
            || definition.get("owner").and_then(|v| v.get("kind")) == Some(&"service".into()),
        "invalid_arguments",
        "Owner-readiness retention requires a service-owned contract.",
    )?;
    let version = text(definition, "version")?;
    compare_versions(&version, &version)?;
    let definition_owner = canonical(
        definition.get("owner").ok_or_else(IvyError::internal)?,
        MANAGEMENT_FRAME_BYTES,
    )?;
    let owner = canonical(owner, MANAGEMENT_FRAME_BYTES)?;
    require(
        definition_owner == owner,
        "contract_owner_mismatch",
        "Contract owner does not match the registered publisher.",
    )?;
    if let Some(schema) = definition.get("jsonSchema") {
        store.validators().compile(schema)?;
    }
    // SQLite's Node binding replaces lone UTF-16 units at the SQL boundary.
    // Family media equality still compares the original JavaScript string.
    let key = String::from_utf16_lossy(
        &definition
            .get("key")
            .and_then(IvyJson::as_js_string)
            .ok_or_else(IvyError::internal)?
            .0,
    );
    let media = definition
        .get("mediaType")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)?;
    let mode = owner_mode(definition);
    let family = store
        .connection()
        .query_row(
            "SELECT owner,media_type,object_mode FROM contract_families WHERE key=?",
            [&key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|_| IvyError::internal())?;
    require(
        family
            .as_ref()
            .is_none_or(|(old_owner, old_media, old_mode)| {
                old_owner == &owner
                    && &JsString::from(old_media.as_str()) == media
                    && old_mode == mode
            }),
        "contract_owner_mismatch",
        "Contract family owner, media type and Object ownership class are immutable.",
    )?;
    let existing = store
        .connection()
        .query_row(
            "SELECT definition FROM contracts WHERE key=? AND version=?",
            params![key, version],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|_| IvyError::internal())?;
    let encoded = canonical(definition, MANAGEMENT_FRAME_BYTES)?;
    require(
        existing.as_ref().is_none_or(|old| old == &encoded),
        "contract_definition_conflict",
        "An immutable contract version already has a different definition.",
    )?;
    if existing.is_none() {
        store
            .connection()
            .execute(
                "INSERT OR IGNORE INTO contract_families VALUES (?,?,?,?)",
                params![key, owner, String::from_utf16_lossy(&media.0), mode],
            )
            .map_err(|_| IvyError::internal())?;
        store
            .connection()
            .execute(
                "INSERT INTO contracts VALUES (?,?,?)",
                params![key, version, encoded],
            )
            .map_err(|_| IvyError::internal())?;
        store.invalidate("services".into());
    }
    Ok(definition.clone())
}
pub fn register_agent(
    store: &mut Store,
    context: &AuthenticatedContext,
    parameters: &IvyJson,
    now_ms: u64,
) -> IvyResult<IvyJson> {
    store.mutate(
        context,
        &"contracts.register".into(),
        parameters,
        now_ms,
        |store, _| {
            let definition = parameters
                .get("definition")
                .ok_or_else(IvyError::internal)?;
            let value = register(
                store,
                definition,
                &IvyJson::object([("kind", "agent".into())]),
            )?;
            Ok(LedgerResult {
                value,
                kind: ReceiptKind::Contract,
                reference: Some(IvyJson::object([
                    (
                        "key",
                        definition
                            .get("key")
                            .ok_or_else(IvyError::internal)?
                            .clone(),
                    ),
                    (
                        "version",
                        definition
                            .get("version")
                            .ok_or_else(IvyError::internal)?
                            .clone(),
                    ),
                ])),
            })
        },
    )
}
pub fn get_contract(
    store: &Store,
    key: &JsString,
    version: Option<&JsString>,
) -> IvyResult<IvyJson> {
    if let Some(version) = version {
        return store.contract(key, version);
    }
    let mut query = store
        .connection()
        .prepare_cached("SELECT definition FROM contracts WHERE key=?")
        .map_err(|_| IvyError::internal())?;
    let definitions = query
        .query_map([String::from_utf16_lossy(&key.0)], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|_| IvyError::internal())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| IvyError::internal())?;
    let mut latest = None;
    for encoded in definitions {
        let next = parse(&encoded, MANAGEMENT_FRAME_BYTES).map_err(|_| IvyError::internal())?;
        if latest
            .as_ref()
            .map(|previous| compare_versions(&text(&next, "version")?, &text(previous, "version")?))
            .transpose()?
            .is_none_or(|order| order == Ordering::Greater)
        {
            latest = Some(next);
        }
    }
    latest.ok_or_else(|| IvyError::new("contract_not_found", "Data Contract not found."))
}

pub enum ContentData {
    Text(String),
    Bytes(Vec<u8>),
}
pub struct DecodedContent {
    pub data: ContentData,
    pub content_hash: String,
    pub byte_length: usize,
}
fn json_media(media: &JsString) -> bool {
    let units = &media.0;
    let prefix: Vec<_> = "application/".encode_utf16().collect();
    let suffix: Vec<_> = "+json".encode_utf16().collect();
    media == &JsString::from("application/json")
        || (units.starts_with(&prefix)
            && units.ends_with(&suffix)
            && units.len() > prefix.len() + suffix.len()
            && units[prefix.len()..units.len() - suffix.len()]
                .iter()
                .all(|c| matches!(c,65..=90|97..=122|48..=57|33|35|36|38|94|95|46|43|45)))
}
pub fn decode(store: &Store, contract: &IvyJson, content: &IvyJson) -> IvyResult<DecodedContent> {
    let media = contract
        .get("mediaType")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)?;
    let encoding = if json_media(media) {
        "json"
    } else if media.0.starts_with(&[116, 101, 120, 116, 47]) {
        "text"
    } else {
        "base64"
    };
    require(
        content.get("encoding") == Some(&encoding.into()),
        "invalid_arguments",
        "Content encoding does not match the contract media type.",
    )?;
    let data =
        match encoding {
            "json" => {
                let value = content.get("value").ok_or_else(|| {
                    IvyError::new("invalid_arguments", "Value is outside the JSON domain.")
                })?;
                let data = canonical(value, JSON_OBJECT_CONTENT_BYTES)?;
                let schema = contract.get("jsonSchema").ok_or_else(|| {
                    IvyError::new("contract_not_found", "JSON contract has no schema.")
                })?;
                require(
                    store.validators().compile(schema)?.matches(value)?,
                    "invalid_arguments",
                    "Content does not satisfy its exact contract.",
                )?;
                ContentData::Text(data)
            }
            "text" => {
                let value = content
                    .get("value")
                    .and_then(IvyJson::as_js_string)
                    .ok_or_else(IvyError::internal)?;
                ContentData::Text(String::from_utf16(&value.0).map_err(|_| {
                    IvyError::new("invalid_arguments", "Text must be valid Unicode.")
                })?)
            }
            _ => {
                let value = content
                    .get("value")
                    .and_then(IvyJson::as_js_string)
                    .ok_or_else(IvyError::internal)?;
                let invalid =
                    || IvyError::new("invalid_arguments", "Expected canonical base64 content.");
                let value = String::from_utf16(&value.0).map_err(|_| invalid())?;
                let bytes = STANDARD.decode(&value).map_err(|_| invalid())?;
                require(
                    STANDARD.encode(&bytes) == value,
                    "invalid_arguments",
                    "Expected canonical base64 content.",
                )?;
                ContentData::Bytes(bytes)
            }
        };
    let bytes = match &data {
        ContentData::Text(value) => value.as_bytes(),
        ContentData::Bytes(value) => value,
    };
    let maximum = match encoding {
        "json" => JSON_OBJECT_CONTENT_BYTES,
        "text" => TEXT_OBJECT_CONTENT_BYTES,
        _ => BINARY_OBJECT_CONTENT_BYTES,
    };
    require(
        bytes.len() <= maximum,
        "content_too_large",
        "Decoded content exceeds its storage limit.",
    )?;
    Ok(DecodedContent {
        content_hash: digest(bytes),
        byte_length: bytes.len(),
        data,
    })
}
