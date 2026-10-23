//! Native journal files and measured space on the configured data volume.
use crate::{engine_factory::OpenedJournal, local_retention::HistoryLimits};
use ivy_contracts::{IvyError, IvyResult};
use ivy_host_runtime::storage_space::available_bytes;
use ivy_store::{rusqlite::Connection, sqlite::error};
use std::{fs, path::PathBuf};

/// The caller owns data-root selection. Opening never creates a missing root,
/// selects an ancestor volume or substitutes a new database after a failure.
pub struct NativeJournal {
    data_root: Option<PathBuf>,
    limits: HistoryLimits,
}
impl NativeJournal {
    pub fn new(data_root: Option<PathBuf>, limits: HistoryLimits) -> Self {
        Self { data_root, limits }
    }

    pub fn free_bytes(&self) -> IvyResult<Option<f64>> {
        self.data_root
            .as_deref()
            .map(|root| {
                available_bytes(root)
                    .map(|bytes| bytes as f64)
                    .map_err(|_| IvyError::internal())
            })
            .transpose()
    }

    pub fn open(&self) -> IvyResult<OpenedJournal> {
        let path = self
            .data_root
            .as_ref()
            .map(|root| root.join("task-board.sqlite"));
        let connection = match &path {
            Some(path) => Connection::open(path),
            None => Connection::open_in_memory(),
        }
        .map_err(error)?;
        let opened = OpenedJournal::new(
            connection,
            HistoryLimits {
                count: self.limits.count,
                bytes: self.limits.bytes,
            },
        )?;
        let connection = opened.connection();
        if let Some(path) = &path {
            let mode: i64 = connection
                .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
                .map_err(error)?;
            if mode == 0 {
                let file_bytes = fs::metadata(path).map_err(|_| IvyError::internal())?.len() as f64;
                let free_bytes = self.free_bytes()?.ok_or_else(IvyError::internal)?;
                if free_bytes >= (64.0_f64 * 1024.0 * 1024.0).max(file_bytes * 3.0) {
                    connection
                        .execute_batch("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;")
                        .map_err(error)?;
                }
            }
        }
        Ok(opened)
    }
}
