//! The original online snapshot and streaming checksum, on the owner's connection.
use super::Store;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use sha2::{Digest, Sha256};
use std::{fmt::Write, future::Future, pin::Pin, time::Duration};
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
fn checksum_path(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    #[cfg(not(windows))]
    {
        Ok(path.to_owned())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use std::path::{Component, Path, Prefix};
        let first = path.components().next();
        if matches!(first, Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Verbatim(_) | Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(_, _) | Prefix::DeviceNS(_)))
        {
            return Ok(path.to_owned());
        }
        // Resolve the directory without Win32 trimming the requested filename.
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else if let Some(Component::Prefix(prefix)) = first {
            let drive = Path::new(prefix.as_os_str());
            std::path::absolute(drive)?.join(path.strip_prefix(drive).unwrap())
        } else {
            std::env::current_dir()?.join(path)
        };
        let absolute = ivy_host_runtime::native_path::lexical(&absolute);
        let units = absolute
            .as_os_str()
            .encode_wide()
            .map(|unit| {
                if unit == b'/' as u16 {
                    std::path::MAIN_SEPARATOR as u16
                } else {
                    unit
                }
            })
            .collect::<Vec<_>>();
        // The original file stream retains trailing dots/spaces through a namespace.
        let (prefix, suffix) = if matches!(absolute.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::UNC(_, _)))
        {
            (r"\\?\UNC\", &units[2..])
        } else {
            (r"\\?\", &units[..])
        };
        let mut result = std::ffi::OsString::from(prefix);
        result.push(std::ffi::OsString::from_wide(suffix));
        Ok(result.into())
    }
}
struct BackupLease(std::rc::Rc<std::cell::Cell<usize>>);
impl Drop for BackupLease {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

impl Store {
    /// Return an owned task so mutable Store actions can run between copy batches.
    /// SQLite owns destination/URI validation and rollback; failed destinations
    /// are not unlinked or replaced by an extra staging file.
    pub fn backup_to<F>(&self, destination: JsString, mut progress: F) -> OnlineBackup
    where
        F: FnMut() -> IvyResult<()> + 'static,
    {
        let source = self.db.clone();
        self.active_backups.set(self.active_backups.get() + 1);
        let lease = BackupLease(self.active_backups.clone());
        // Node's SQLite filename boundary replaces unpaired UTF16 surrogates.
        let destination = std::path::PathBuf::from(String::from_utf16_lossy(&destination.0));
        Box::pin(async move {
            let _lease = lease;
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
            let mut input = tokio::fs::File::open(
                checksum_path(&destination).map_err(|_| IvyError::internal())?,
            )
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
