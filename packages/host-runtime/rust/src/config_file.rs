//! Bounded regular JSON files shared by configuration and package readers.
use crate::{IvyError, IvyJson, IvyResult};
use ivy_contracts::json;
use std::{fs, path::{Path, PathBuf}};
pub const CONFIGURATION_BYTES: usize = 1024 * 1024;

pub(crate) enum FileError { Missing, Invalid(IvyError), Io(std::io::Error), Syntax }
impl FileError {
    pub(crate) fn error(self) -> IvyError {
        match self { Self::Missing | Self::Io(_) | Self::Syntax => IvyError::internal(), Self::Invalid(error) => error }
    }
}
fn file_error(error: std::io::Error) -> FileError {
    if error.kind() == std::io::ErrorKind::NotFound { FileError::Missing }
    else { FileError::Io(error) }
}
pub(crate) fn read_json(path: &Path, limit: usize) -> Result<IvyJson, FileError> {
    let path = crate::node_path::filesystem_path(&crate::node_path::from_native(path));
    let metadata = fs::metadata(&path).map_err(file_error)?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(FileError::Invalid(IvyError::new("invalid_arguments", "Configuration must be a bounded regular file.")));
    }
    let bytes = crate::filesystem::read_file_bytes(&path).map_err(file_error)?;
    // Node readFile(..., 'utf8') replaces malformed input before JSON.parse.
    json::parse(&String::from_utf8_lossy(&bytes), usize::MAX)
        .map_err(|_| FileError::Syntax)
}
pub async fn json_file(path: impl Into<PathBuf>, limit: usize) -> IvyResult<IvyJson> {
    let path = path.into();
    tokio::task::spawn_blocking(move || read_json(&path, limit).map_err(FileError::error))
        .await.map_err(|_| IvyError::internal())?
}
/// Only ENOENT from stat or read is absent; present JSON null stays Some(Null).
pub async fn json_file_if_present(path: impl Into<PathBuf>, limit: usize) -> IvyResult<Option<IvyJson>> {
    let path = path.into();
    tokio::task::spawn_blocking(move || match read_json(&path, limit) {
        Ok(value) => Ok(Some(value)),
        Err(FileError::Missing) => Ok(None),
        Err(error) => Err(error.error()),
    }).await.map_err(|_| IvyError::internal())?
}
