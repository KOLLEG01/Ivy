//! Filesystem admission for the private media journal and its original SQLite owner.
use crate::local_files::safe_directory;
use ivy_contracts::{IvyError, IvyResult};
use ivy_host_runtime::filesystem;
use ivy_store::{rusqlite::Connection, sqlite};
use std::{fs, path::Path, time::Duration};

fn check(valid: bool, code: &str) -> IvyResult<()> {
    if valid {
        Ok(())
    } else {
        Err(IvyError::new(
            code,
            "Media spool must retain its exact original owner, intent, evidence and complete bytes.",
        ))
    }
}
fn private_file(path: &Path) -> IvyResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| IvyError::internal())?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
pub(crate) fn open(root: &Path) -> IvyResult<(Connection, Connection)> {
    check(root.is_absolute(), "media_spool_path_invalid")?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(root).map_err(|_| IvyError::internal())?;
    safe_directory(root)?;
    for name in [
        "owner.sqlite",
        "media.sqlite",
        "media.sqlite-wal",
        "media.sqlite-shm",
    ] {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => check(
                metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && filesystem::stat_link_count(&path).map_err(|_| IvyError::internal())? == 1,
                "media_spool_path_invalid",
            )?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(IvyError::internal()),
        }
    }
    let owner_path = root.join("owner.sqlite");
    let owner = sqlite::open(
        &owner_path,
        sqlite::OpenOptions {
            busy_timeout: Duration::ZERO,
            read_only: false,
        },
    )
    .map_err(sqlite::error)?;
    let claim = || -> IvyResult<()> {
        owner
            .execute_batch(
                "BEGIN EXCLUSIVE; CREATE TABLE IF NOT EXISTS owner(singleton INTEGER PRIMARY KEY);",
            )
            .map_err(sqlite::error)?;
        private_file(&owner_path)
    };
    if claim().is_err() {
        return Err(IvyError::new(
            "media_spool_owned",
            "Another process owns this media spool.",
        ));
    }
    let path = root.join("media.sqlite");
    let db = sqlite::open(
        &path,
        sqlite::OpenOptions {
            busy_timeout: Duration::from_millis(5000),
            read_only: false,
        },
    )
    .map_err(sqlite::error)?;
    private_file(&path)?;
    Ok((db, owner))
}
