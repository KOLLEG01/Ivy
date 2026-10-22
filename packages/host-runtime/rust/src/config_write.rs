//! Atomic JSON publication through the shared store writer.
use crate::{IvyError, IvyJson, IvyResult};
use ivy_contracts::json;
use std::path::{Path, PathBuf};
pub async fn atomic_json(path: impl Into<PathBuf>, value: IvyJson) -> IvyResult<()> {
    let path = path.into();
    tokio::task::spawn_blocking(move || write_json(&path, &value))
        .await.map_err(|_| IvyError::internal())?
}
pub(crate) fn write_json(path: &Path, value: &IvyJson) -> IvyResult<()> {
    let mut bytes = json::canonical(value, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES)?.into_bytes();
    bytes.push(b'\n');
    ivy_store::atomic::atomic_write(path, &bytes).map_err(|_| IvyError::internal())
}
