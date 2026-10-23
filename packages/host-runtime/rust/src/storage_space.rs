//! Logical storage accounting and measured free-space gates for preparation.
use crate::{IvyError, IvyJson, IvyResult, config::path_value, paths::{inside, realpath}};
use std::{fs, io, path::{Path, PathBuf}};

const SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
pub trait RetentionProgress: Send + Sync {
    fn removed(&self, area: &str);
    fn skipped(&self, area: &str, reason: &str, requires_attention: bool);
}
fn internal<T, E>(result: Result<T, E>) -> IvyResult<T> { result.map_err(|_| IvyError::internal()) }

fn visit(path: &Path, boundary: &Path, depth: usize, files: &mut usize, bytes: &mut f64) -> IvyResult<()> {
    if depth > 64 { return Err(IvyError::new("limit_exceeded", "Storage inventory exceeds its traversal limit.")); }
    *files += 1;
    if *files > 200_000 { return Err(IvyError::new("limit_exceeded", "Storage inventory exceeds its traversal limit.")); }
    let metadata = internal(fs::symlink_metadata(path))?;
    if metadata.file_type().is_symlink() { return Ok(()); }
    if !internal(inside(boundary, &internal(realpath(path))?))? {
        return Err(IvyError::new("target_conflict", "Storage inventory leaves its owned directory."));
    }
    if metadata.is_dir() {
        let mut children = internal(internal(fs::read_dir(path))?.collect::<Result<Vec<_>, _>>())?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children { visit(&child.path(), boundary, depth + 1, files, bytes)?; }
    } else {
        if !metadata.is_file() { return Err(IvyError::new("target_conflict", "Storage has an unsupported payload type.")); }
        *bytes += metadata.len() as f64;
    }
    if *bytes > SAFE_INTEGER { return Err(IvyError::new("limit_exceeded", "Storage byte count is out of range.")); }
    Ok(())
}
/// Count logical bytes per file name, including hard links, without following
/// symlinks into shared caches. Inventory never changes or removes payloads.
pub async fn tree_bytes(path: impl Into<PathBuf>, boundary: Option<PathBuf>) -> IvyResult<f64> {
    let path = path.into();
    internal(tokio::task::spawn_blocking(move || {
        let boundary = match boundary { Some(value) => value, None => internal(realpath(&path))? };
        let mut files = 0; let mut bytes = 0.0;
        visit(&path, &boundary, 0, &mut files, &mut bytes)?;
        Ok(bytes)
    }).await)?
}
pub fn storage_is_low(available_bytes: f64, total_bytes: f64) -> bool {
    available_bytes < 2.0 * 1024.0_f64.powi(3) || total_bytes > 0.0 && available_bytes / total_bytes < 0.1
}
/// Keep periodic collection and pressure retries throttled independently.
pub struct RetentionSchedule { next_scheduled: f64, next_pressure: f64 }
impl RetentionSchedule {
    pub fn new(now: f64) -> Self { Self { next_scheduled: now + 60_000.0, next_pressure: 0.0 } }
    pub fn due(&self, now: f64, low_space: bool) -> bool { now >= self.next_scheduled || low_space && now >= self.next_pressure }
    pub fn started(&mut self, now: f64) { self.next_scheduled = now + 86_400_000.0; self.next_pressure = now + 3_600_000.0; }
    pub fn failed(&mut self, now: f64) { self.next_scheduled = now + 3_600_000.0; self.next_pressure = now + 3_600_000.0; }
}
#[derive(Clone, Copy)]
struct Space { available: u64, total: u64 }

#[cfg(windows)]
fn measured_space(path: &Path) -> io::Result<Space> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let mut directory = if fs::metadata(path)?.is_dir() {
        path.to_owned()
    } else {
        fs::canonicalize(path)?.parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "File has no parent directory."))?
            .to_owned()
    };
    // The Win32 directory parameter requires a trailing separator for UNC paths.
    directory.push("");
    let directory = directory.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    let mut value = Space { available: 0, total: 0 };
    // SAFETY: the path is NUL-terminated and both output values live through the call.
    if unsafe { GetDiskFreeSpaceExW(directory.as_ptr(), &mut value.available, &mut value.total, std::ptr::null_mut()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}
#[cfg(target_os = "linux")]
fn measured_space(path: &Path) -> io::Result<Space> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Path contains NUL."))?;
    let mut value = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: path is NUL-terminated; statvfs writes the complete native structure
    // on success. Uninitialized contents are never read on the error path.
    if unsafe { libc::statvfs(path.as_ptr(), value.as_mut_ptr()) } != 0 { return Err(io::Error::last_os_error()); }
    let value = unsafe { value.assume_init() };
    let block_size = if value.f_frsize == 0 { value.f_bsize } else { value.f_frsize } as u64;
    let bytes = |blocks: u64| block_size.checked_mul(blocks)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Filesystem byte count exceeds u64."));
    Ok(Space { available: bytes(value.f_bavail as u64)?, total: bytes(value.f_blocks as u64)? })
}
#[cfg(not(any(windows, target_os = "linux")))]
fn measured_space(_path: &Path) -> io::Result<Space> { Err(io::Error::new(io::ErrorKind::Unsupported, "Unsupported host platform.")) }

fn probe(mut path: PathBuf) -> io::Result<Space> {
    loop {
        match measured_space(&path) {
            Ok(value) => return Ok(value),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(parent) = path.parent() else { return Err(error) };
                let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
                if parent == path { return Err(error); }
                path = parent.to_owned();
            }
            Err(error) => return Err(error),
        }
    }
}
/// Caller-available and total bytes on an existing file or directory's volume,
/// in that order. Preserves OS errors without probing a missing path's parents.
pub fn measure_bytes(path: &Path) -> io::Result<(u64, u64)> {
    measured_space(path).map(|value| (value.available, value.total))
}
/// Available bytes for the caller on an existing file or directory's volume.
/// Preserves OS errors; missing paths are not replaced by an ancestor probe.
pub fn available_bytes(path: &Path) -> io::Result<u64> {
    measure_bytes(path).map(|(available, _)| available)
}
pub async fn available_storage_bytes(path: impl Into<PathBuf>) -> IvyResult<f64> {
    let path = path.into();
    internal(tokio::task::spawn_blocking(move || internal(available_bytes(&path)).map(|value| (value as f64).min(SAFE_INTEGER))).await)?
}
pub async fn require_storage_space(path: impl Into<PathBuf>, required_bytes: f64) -> IvyResult<()> {
    let available_bytes = available_storage_bytes(path).await?;
    if available_bytes < required_bytes {
        return Err(IvyError::new("storage_pressure", "Insufficient measured free space for safe preparation; inspect and compact verified duplicate workspaces.")
            .with_details(IvyJson::object([("availableBytes", IvyJson::Number(available_bytes)), ("requiredBytes", IvyJson::Number(required_bytes))])));
    }
    Ok(())
}
pub async fn host_storage_space(config: IvyJson) -> IvyResult<IvyJson> {
    let roots = ["runtimeRoot", "artifactRoot", "stagingRoot"].iter().map(|key| path_value(config.required(key)?)).collect::<IvyResult<Vec<_>>>()?;
    let probes = crate::concurrency::map_concurrent(roots, 3, |root, _| async move {
        internal(tokio::task::spawn_blocking(move || internal(probe(root))).await)?
    }).await?;
    Ok(IvyJson::object([
        ("availableBytes", IvyJson::object([("runtime", IvyJson::Number((probes[0].available as f64).min(SAFE_INTEGER))), ("artifacts", IvyJson::Number((probes[1].available as f64).min(SAFE_INTEGER))), ("staging", IvyJson::Number((probes[2].available as f64).min(SAFE_INTEGER)))])),
        ("lowSpace", probes.iter().any(|value| storage_is_low(value.available as f64, value.total as f64)).into()),
    ]))
}
