//! Invalidate action reads after a confirmed remote write. Subsequent reads
//! obtain the record with their original archive, primary and hydration policy.
use super::ReadCache;
use ivy_contracts::{IvyJson as Json, IvyResult};

pub(crate) fn complete_write(cache: Option<&ReadCache>, result: &Json) -> IvyResult<Json> {
    let object = result.required("object")?;
    let revision = result.required("revision")?;
    let id = object.required("id")?;
    let pin = Json::object([
        ("objectId", id.clone()),
        ("revision", revision.required("revision")?.clone()),
    ]);
    if let Some(entries) = cache.and_then(ReadCache::entries) {
        entries
            .borrow_mut()
            .retain(|(_, record)| Some(record.object_id()) != id.as_js_string());
    }
    Ok(pin)
}
