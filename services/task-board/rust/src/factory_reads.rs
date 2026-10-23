//! Native storage errors retained by the generation's read owner.
use crate::engine_read::read_failure::ReadFailure;
use crate::{native_lifecycle::LifecycleDocument, operations::Document};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult};
pub(super) enum NativeReadFailure {
    Parse(IvyError),
    Sqlite(ivy_store::rusqlite::Error),
}
pub(super) type Failure = ReadFailure<NativeReadFailure>;

pub(super) fn lifecycle_document(envelope: Json) -> IvyResult<LifecycleDocument> {
    Ok(LifecycleDocument {
        document: Document {
            pin: envelope.required("pin")?.clone(),
            value: envelope.required("value")?.clone(),
            parent_id: envelope
                .required("metadata")?
                .get("parentId")
                .and_then(Json::as_js_string)
                .cloned(),
            content_hash: envelope
                .required("revision")?
                .required("contentHash")?
                .as_str()?,
        },
        metadata: envelope.required("metadata")?.clone(),
    })
}
