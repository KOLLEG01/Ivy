//! The original online snapshot and streaming checksum, on the owner's connection.
use super::Store;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use sha2::{Digest, Sha256};
use std::{fmt::Write, future::Future, path::PathBuf, pin::Pin, time::Duration};
use tokio::io::AsyncReadExt;

pub struct BackupSnapshot {
    pub pages: i32,
    pub bytes_hash: String,
}
impl BackupSnapshot {
    pub fn to_json(self) -> IvyJson {
        IvyJson::object([
            ("pages", IvyJson::Number(f64::from(self.pages))),
            ("bytesHash", self.bytes_hash.into()),
        ])
    }
}
/// Not Send: copying and ordinary actions use one connection on its storage actor.
pub type OnlineBackup = Pin<Box<dyn Future<Output = IvyResult<BackupSnapshot>>>>;

impl Store {
    /// Return an owned task so mutable Store actions can run between copy batches.
    /// SQLite owns destination/URI validation and rollback; failed destinations
    /// are not unlinked or replaced by an extra staging file.
    pub fn backup_to<F>(&self, destination: JsString, mut progress: F) -> OnlineBackup
    where
        F: FnMut() -> IvyResult<()> + 'static,
    {
        let source = self.db.clone();
        // node:sqlite and the original file stream encode string paths as UTF-8,
        // replacing unpaired UTF-16 units at that filesystem boundary.
        let destination = PathBuf::from(String::from_utf16_lossy(&destination.0));
        Box::pin(async move {
            let pages = {
                let mut target = Connection::open_with_flags(
                    &destination,
                    OpenFlags::SQLITE_OPEN_READ_WRITE
                        | OpenFlags::SQLITE_OPEN_CREATE
                        | OpenFlags::SQLITE_OPEN_URI,
                )
                .map_err(|_| IvyError::internal())?;
                // rusqlite defaults to a five-second busy handler. Node's backup
                // destination has none; Busy/Locked are handled after each step.
                target
                    .busy_timeout(Duration::ZERO)
                    .map_err(|_| IvyError::internal())?;
                let backup = Backup::new(&source, &mut target).map_err(|_| IvyError::internal())?;
                let mut copied = -1;
                loop {
                    let result = backup.step(100).map_err(|_| IvyError::internal())?;
                    let state = backup.progress();
                    // Node invokes the SQLite progress callback only while work
                    // remains, including Busy/Locked with an established count.
                    if state.remaining != 0 {
                        let current = state.pagecount - state.remaining;
                        if current > copied {
                            copied = current;
                            progress()?;
                        }
                        tokio::task::yield_now().await;
                        continue;
                    }
                    if result != StepResult::Done {
                        return Err(IvyError::internal());
                    }
                    break state.pagecount;
                }
                // Drop the backup and destination before opening the hash stream,
                // just as Node finalizes sqlite3_backup before fulfilling backup().
            };
            let mut input = tokio::fs::File::open(&destination)
                .await
                .map_err(|_| IvyError::internal())?;
            let mut hash = Sha256::new();
            let mut buffer = vec![0; 64 * 1024];
            loop {
                let size = input
                    .read(&mut buffer)
                    .await
                    .map_err(|_| IvyError::internal())?;
                if size == 0 {
                    break;
                }
                hash.update(&buffer[..size]);
                progress()?;
                tokio::task::yield_now().await;
            }
            let mut bytes_hash = String::from("sha256:");
            for byte in hash.finalize() {
                write!(bytes_hash, "{byte:02x}").unwrap();
            }
            Ok(BackupSnapshot { pages, bytes_hash })
        })
    }
}
