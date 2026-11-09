//! Native storage errors retained by the generation's read owner.
use crate::engine_read::read_failure::ReadFailure;
use crate::{native_lifecycle::LifecycleDocument, operations::StoredRecord};
#[cfg(test)]
use ivy_contracts::IvyJson as Json;
use ivy_contracts::{IvyError, IvyResult};
pub(super) enum NativeReadFailure {
    Parse(IvyError),
    Sqlite(ivy_store::rusqlite::Error),
}
pub(super) type Failure = ReadFailure<NativeReadFailure>;

pub(super) fn lifecycle_document(record: StoredRecord) -> IvyResult<LifecycleDocument> {
    let (document, metadata) = record.into_document_with_metadata()?;
    Ok(LifecycleDocument { document, metadata })
}

#[cfg(test)]
#[path = "../../../../tests/fixtures/task-board-record-ownership.rs"]
mod tests;
