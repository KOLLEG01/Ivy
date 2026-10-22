//! Protected service configuration read with native filesystem and UTF-8 rules.
use crate::{IvyError, IvyJson, IvyResult, filesystem};
use std::{fs, io::{self, Read}, path::PathBuf};

fn read(path: PathBuf, maximum_bytes: usize, invalid: IvyError) -> IvyResult<Option<IvyJson>> {
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IvyError::internal()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink()
        || metadata.len() > maximum_bytes as u64
        || filesystem::stat_link_count(&path).map_err(|_| IvyError::internal())? != 1
    {
        return Err(invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 { return Err(invalid); }
    }
    let file = match fs::File::open(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IvyError::internal()),
    };
    let mut text = String::new();
    file.take((maximum_bytes as u64).saturating_add(1))
        .read_to_string(&mut text).map_err(|_| invalid.clone())?;
    if text.len() > maximum_bytes { return Err(invalid); }
    ivy_contracts::json::parse(&text, maximum_bytes).map(Some).map_err(|_| invalid)
}

/// Missing files return None; invalid protection or JSON uses the caller's error.
pub async fn protected_json_file(path: impl Into<PathBuf>, maximum_bytes: usize, invalid: IvyError)
    -> IvyResult<Option<IvyJson>>
{
    let path = path.into();
    tokio::task::spawn_blocking(move || read(path, maximum_bytes, invalid))
        .await.map_err(|_| IvyError::internal())?
}
