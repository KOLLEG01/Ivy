//! Filesystem admission for the private media journal and its original SQLite owner.
use ivy_contracts::{IvyError, IvyResult};
use ivy_host_runtime::{filesystem, node_path, paths};
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
fn native(path: &Path) -> std::path::PathBuf {
    node_path::filesystem_path(&node_path::from_native(path))
}
fn directory(root: &Path) -> IvyResult<()> {
    let context = node_path::Context::current().map_err(|_| IvyError::internal())?;
    let absolute = context.resolve(&[node_path::from_native(root)]);
    let path = node_path::to_native(&absolute);
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    for (index, part) in ancestors.iter().enumerate() {
        let metadata = fs::symlink_metadata(native(part)).map_err(|_| IvyError::internal())?;
        check(metadata.is_dir(), "path_unsafe")?;
        if index > 0 {
            let actual = paths::realpath(part).map_err(|_| IvyError::internal())?;
            check(
                !metadata.file_type().is_symlink()
                    && context
                        .relative(
                            &node_path::from_native(part),
                            &node_path::from_native(&actual),
                        )
                        .0
                        .is_empty(),
                "path_unsafe",
            )?;
        }
    }
    Ok(())
}
fn private_file(path: &Path) -> IvyResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(native(path), fs::Permissions::from_mode(0o600))
            .map_err(|_| IvyError::internal())?;
    }
    #[cfg(windows)]
    {
        // Node chmod(0600) changes Windows' read-only bit, not its DACL.
        let mut permissions = fs::metadata(native(path))
            .map_err(|_| IvyError::internal())?
            .permissions();
        permissions.set_readonly(false);
        fs::set_permissions(native(path), permissions).map_err(|_| IvyError::internal())?;
    }
    Ok(())
}
pub(crate) fn open(root: &Path) -> IvyResult<(Connection, Connection)> {
    check(
        node_path::is_absolute(node_path::Style::native(), &node_path::from_native(root)),
        "media_spool_path_invalid",
    )?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(native(root))
        .map_err(|_| IvyError::internal())?;
    directory(root)?;
    for name in [
        "owner.sqlite",
        "media.sqlite",
        "media.sqlite-wal",
        "media.sqlite-shm",
    ] {
        let path = root.join(name);
        match fs::symlink_metadata(native(&path)) {
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
        &native(&owner_path),
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
        &native(&path),
        sqlite::OpenOptions {
            busy_timeout: Duration::from_millis(5000),
            read_only: false,
        },
    )
    .map_err(sqlite::error)?;
    private_file(&path)?;
    Ok((db, owner))
}
