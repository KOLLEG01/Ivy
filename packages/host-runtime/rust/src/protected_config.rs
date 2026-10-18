//! Generic protected JSON input; the caller owns its document/schema policy.
use crate::{IvyError, IvyJson, IvyResult, filesystem, node_path::{self, Context, Style}, paths};
use std::{fs, io, path::{Path, PathBuf}};

enum ReadError { Io(io::Error), Invalid }
impl From<io::Error> for ReadError { fn from(error: io::Error) -> Self { Self::Io(error) } }
fn fs_path(path: &Path) -> PathBuf { node_path::filesystem_path(&node_path::from_native(path)) }
fn read_bytes(path: &Path) -> io::Result<Vec<u8>> {
    #[cfg(windows)] {
        use std::{io::Read, os::windows::fs::OpenOptionsExt};
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        // libuv supplies this flag for its ordinary read opens too. Preserve
        // the existing account's Windows sharing/backup semantics.
        let mut file = fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(fs_path(path))?;
        let mut bytes = Vec::new(); file.read_to_end(&mut bytes)?; Ok(bytes)
    }
    #[cfg(not(windows))] { fs::read(fs_path(path)) }
}
fn read(path: &Path, maximum_bytes: usize) -> Result<IvyJson, ReadError> {
    let metadata = fs::symlink_metadata(fs_path(path))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum_bytes as u64 { return Err(ReadError::Invalid); }
    if filesystem::stat_link_count(path)? != 1 { return Err(ReadError::Invalid); }
    #[cfg(unix)] { use std::os::unix::fs::MetadataExt; if metadata.mode() & 0o077 != 0 { return Err(ReadError::Invalid); } }
    let requested = node_path::from_native(path);
    let actual = paths::realpath(path)?;
    let parent = paths::realpath(&node_path::to_native(&node_path::dirname(Style::native(), &requested)))?;
    let expected = Context::current()?.resolve(&[node_path::from_native(&parent), node_path::basename(Style::native(), &requested)]);
    if node_path::from_native(&actual) != expected { return Err(ReadError::Invalid); }
    let bytes = read_bytes(path)?;
    // Match readFile(...,'utf8') followed by JSON.parse, including malformed
    // UTF-8 replacement and refusal of an initial BOM or malformed JSON.
    ivy_contracts::json::parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| ReadError::Invalid)
}

/// No document envelope is imposed. An absent file/ancestor returns None;
/// invalid protection/JSON uses the caller's error and other I/O stays internal.
/// These observed metadata/path checks do not claim handle-based race fencing.
pub async fn protected_json_file(path: impl Into<PathBuf>, maximum_bytes: usize, invalid: IvyError) -> IvyResult<Option<IvyJson>> {
    let path = path.into();
    tokio::task::spawn_blocking(move || match read(&path, maximum_bytes) {
        Ok(value) => Ok(Some(value)),
        Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(ReadError::Io(_)) => Err(IvyError::internal()),
        Err(ReadError::Invalid) => Err(invalid),
    }).await.map_err(|_| IvyError::internal())?
}
