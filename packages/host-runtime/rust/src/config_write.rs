//! The common atomic JSON writer, independent of configuration schemas.
use crate::{IvyError, IvyJson, IvyResult};
use ivy_contracts::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub async fn atomic_json(path: impl Into<PathBuf>, value: IvyJson) -> IvyResult<()> {
    let path = path.into();
    tokio::task::spawn_blocking(move || write_json(&path, &value))
        .await
        .map_err(|_| IvyError::internal())?
}
pub(crate) fn write_json(path: &Path, value: &IvyJson) -> IvyResult<()> {
    let path = crate::node_path::filesystem_path(&crate::node_path::from_native(path));
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory).map_err(|_| IvyError::internal())?;
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let temporary = PathBuf::from(temporary);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|_| IvyError::internal())?;
    // The original opens first and closes in finally. Serialization/write/sync
    // failures leave that file; only a failed replacement removes it.
    let result = (|| {
        let mut bytes =
            json::canonical(value, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES)?.into_bytes();
        bytes.push(b'\n');
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| IvyError::internal())
    })();
    drop(file);
    result?;
    if ivy_store::atomic::replace_file(&temporary, &path).is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(IvyError::internal());
    }
    #[cfg(unix)]
    fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| IvyError::internal())?;
    Ok(())
}
