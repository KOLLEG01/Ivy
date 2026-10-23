//! Streaming byte identity; filename and executable policy belong to callers.
use crate::{IvyError, IvyResult};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::{Path, PathBuf}};

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> { value.map_err(|_| IvyError::internal()) }
pub(super) fn hash_file(source: &Path) -> IvyResult<String> {
    let mut file = internal(fs::File::open(source))?;
    let mut hash = Sha256::new(); let mut buffer = vec![0; 4 * 1024 * 1024];
    loop { let count = internal(file.read(&mut buffer))?; if count == 0 { break } hash.update(&buffer[..count]); }
    Ok(format!("sha256:{}", hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>()))
}
pub async fn file_hash(source: impl Into<PathBuf>) -> IvyResult<String> {
    let source = source.into(); internal(tokio::task::spawn_blocking(move || hash_file(&source)).await)?
}
