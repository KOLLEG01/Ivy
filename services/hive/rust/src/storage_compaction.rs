//! Startup-only conversion on the data volume and bounded live page reclamation.
use crate::storage_read::STORAGE_FORMAT;
use ivy_contracts::{IvyError, IvyResult};
use rusqlite::{Connection, OptionalExtension, config::DbConfig};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn internal<T>(value: Result<T, impl std::fmt::Display>) -> IvyResult<T> {
    value.map_err(|error| IvyError::new("storage_invalid", &error.to_string()))
}
fn require(value: bool, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new("storage_invalid", message))
    }
}
fn scalar(db: &Connection, pragma: &str) -> IvyResult<i64> {
    internal(db.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0)))
}
fn connect(path: &Path) -> IvyResult<Connection> {
    let db = internal(Connection::open(path))?;
    internal(db.busy_timeout(Duration::from_secs(1)))?;
    for (option, enabled) in [
        (DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true),
        (DbConfig::SQLITE_DBCONFIG_DQS_DDL, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DML, false),
    ] {
        internal(db.set_db_config(option, enabled))?;
    }
    db.set_prepared_statement_cache_capacity(256);
    Ok(db)
}
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    value.into()
}
fn remove_temporary(path: &Path) -> IvyResult<()> {
    for suffix in ["-wal", "-shm", "-journal", ""] {
        let file = sibling(path, suffix);
        let info = match fs::symlink_metadata(&file) {
            Ok(info) => info,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return internal(Err(error)),
        };
        require(
            info.is_file() && !info.file_type().is_symlink(),
            "Database compaction path must be an owned regular file.",
        )?;
        internal(fs::remove_file(file))?;
    }
    Ok(())
}
pub fn open(path: &Path) -> IvyResult<(Connection, Option<String>)> {
    open_with_available(path, || {
        ivy_host_runtime::storage_space::available_bytes(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )
        .map_err(|e| IvyError::new("storage_invalid", &e.to_string()))
    })
}
pub fn open_with_available(
    path: &Path,
    available: impl FnOnce() -> IvyResult<u64>,
) -> IvyResult<(Connection, Option<String>)> {
    let mut db = Some(connect(path)?);
    if path == Path::new(":memory:") {
        return Ok((db.take().unwrap(), None));
    }
    let format = scalar(db.as_ref().unwrap(), "user_version")?;
    if format != 0 && format != STORAGE_FORMAT {
        return Err(IvyError::new(
            "unsupported_storage_format",
            "This binary cannot read the database format.",
        ));
    }
    let temporary = sibling(path, ".compacting");
    let converted: IvyResult<Option<String>> = (|| {
        remove_temporary(&temporary)?;
        let original = db.as_ref().unwrap();
        let pages = scalar(original, "page_count")?;
        if pages == 0 || scalar(original, "auto_vacuum")? == 2 {
            return Ok(None);
        }
        let source = internal(fs::symlink_metadata(path))?;
        require(
            source.is_file() && !source.file_type().is_symlink(),
            "Database compaction requires an owned regular database file.",
        )?;
        let required = (pages as u64)
            .saturating_mul(scalar(original, "page_size")? as u64)
            .saturating_mul(2)
            .saturating_add(64 * 1024 * 1024);
        if available()? < required {
            return Ok(Some("Automatic database compaction is waiting for temporary disk space; deleted pages remain reusable internally.".into()));
        }
        internal(original.execute_batch("PRAGMA locking_mode=EXCLUSIVE; PRAGMA synchronous=FULL; BEGIN EXCLUSIVE; COMMIT; PRAGMA wal_checkpoint(TRUNCATE); PRAGMA auto_vacuum=INCREMENTAL;"))?;
        internal(original.execute("VACUUM INTO ?", [temporary.to_string_lossy().as_ref()]))?;
        let candidate = connect(&temporary)?;
        internal(candidate.execute_batch("PRAGMA journal_mode=DELETE;"))?;
        let check: String =
            internal(candidate.query_row("PRAGMA quick_check", [], |row| row.get(0)))?;
        let foreign: Option<i64> = internal(
            candidate
                .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
                .optional(),
        )?;
        require(
            scalar(&candidate, "auto_vacuum")? == 2 && check == "ok" && foreign.is_none(),
            "Compacted database did not pass its integrity checks.",
        )?;
        internal(candidate.close().map_err(|(_, error)| error))?;
        internal(fs::set_permissions(&temporary, source.permissions()))?;
        internal(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&temporary),
        )?
        .sync_all()
        .map_err(|e| IvyError::new("storage_invalid", &e.to_string()))?;
        internal(db.take().unwrap().close().map_err(|(_, error)| error))?;
        internal(fs::rename(&temporary, path))?;
        #[cfg(unix)]
        internal(fs::File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        ))?
        .sync_all()
        .map_err(|e| IvyError::new("storage_invalid", &e.to_string()))?;
        db = Some(connect(path)?);
        Ok(None)
    })();
    let _ = remove_temporary(&temporary);
    let db = match db {
        Some(db) => db,
        None => connect(path)?,
    };
    let pending = match converted {
        Ok(pending) => pending,
        Err(error) => Some(format!(
            "Automatic database compaction is pending; the database remains usable and conversion retries on restart. {}",
            error.message.chars().take(200).collect::<String>()
        )),
    };
    Ok((db, pending))
}
pub fn reclaim_pages(db: &Connection) -> IvyResult<()> {
    if scalar(db, "auto_vacuum")? == 2 && scalar(db, "freelist_count")? >= 64 {
        internal(
            db.execute_batch("PRAGMA incremental_vacuum(16384); PRAGMA wal_checkpoint(TRUNCATE);"),
        )?;
    }
    Ok(())
}
