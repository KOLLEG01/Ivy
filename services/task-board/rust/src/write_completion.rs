//! Refresh the action cache after a confirmed remote write.
use super::{CachedRecord, ReadCache};
use ivy_contracts::{IvyJson as Json, IvyResult, json::canonical, limits::MANAGEMENT_FRAME_BYTES};

pub(crate) fn complete_write(
    cache: Option<&ReadCache>,
    root: &Json,
    key: &str,
    value: &Json,
    result: &Json,
) -> IvyResult<Json> {
    let object = result.required("object")?;
    let revision = result.required("revision")?;
    let id = object.required("id")?;
    let pin = Json::object([
        ("objectId", id.clone()),
        ("revision", revision.required("revision")?.clone()),
    ]);
    if let Some(entries) = cache.and_then(ReadCache::entries) {
        let mut entries = entries.borrow_mut();
        entries.retain(|(_, record)| &record.object_id != id);
        if canonical(value, MANAGEMENT_FRAME_BYTES)?.len() <= 65536 && entries.len() < 256 {
            let primary = object.get("parentId") == Some(root);
            let mut text = ivy_contracts::JsString::from(format!("{key}:"));
            text.0.extend_from_slice(
                &id.as_js_string()
                    .ok_or_else(ivy_contracts::IvyError::internal)?
                    .0,
            );
            let number = revision
                .required("revision")?
                .as_f64()
                .ok_or_else(ivy_contracts::IvyError::internal)?;
            text.0.extend(format!(":{number}:{primary}").encode_utf16());
            let cache_key = Json::String(text);
            let document = Json::object([
                ("key", key.into()),
                ("pin", pin.clone()),
                ("value", value.clone()),
                ("metadata", object.clone()),
                ("revision", revision.clone()),
            ]);
            entries.push((
                cache_key,
                CachedRecord {
                    object_id: id.clone(),
                    document: None,
                    materialized: Some(document),
                },
            ));
        }
    }
    Ok(pin)
}
