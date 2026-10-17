use ivy_contracts::{IvyError, Outcome};
use rusqlite::{Connection, OpenFlags};
use std::{path::Path, time::Duration};

pub fn error(error: rusqlite::Error) -> IvyError {
    IvyError::new("storage_unavailable", error.to_string()).with_outcome(Outcome::Unknown)
}

/// Explicit per-owner options; no pool, implicit tables, migrations or global PRAGMAs.
pub struct OpenOptions {
    pub busy_timeout: Duration,
    pub read_only: bool,
}
pub fn open(path: &Path, options: OpenOptions) -> rusqlite::Result<Connection> {
    let flags = if options.read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    };
    let db = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    db.busy_timeout(options.busy_timeout)?;
    Ok(db)
}

/// Kernel-backed SQLite owner lock. Its live transaction is released on drop or process death.
pub struct OwnerLock {
    connection: Connection,
}
impl OwnerLock {
    pub fn acquire(path: &Path, exclusive: bool) -> rusqlite::Result<Self> {
        let connection = open(
            path,
            OpenOptions {
                busy_timeout: Duration::from_millis(100),
                read_only: false,
            },
        )?;
        connection.execute_batch(if exclusive {
            "BEGIN EXCLUSIVE"
        } else {
            "BEGIN; SELECT count(*) FROM sqlite_master;"
        })?;
        Ok(Self { connection })
    }
}
impl Drop for OwnerLock {
    fn drop(&mut self) {
        let _ = self.connection.execute_batch("ROLLBACK");
    }
}
