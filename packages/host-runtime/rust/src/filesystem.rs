//! Shared filesystem identity, access, timestamps and owned payload removal.
use crate::{IvyError,IvyResult,node_path};
use std::{fs,path::{Path,PathBuf}};
fn internal<T,E>(value:Result<T,E>)->IvyResult<T>{value.map_err(|_|IvyError::internal())}
async fn blocking<T:Send+'static>(work:impl FnOnce()->IvyResult<T>+Send+'static)->IvyResult<T>{internal(tokio::task::spawn_blocking(work).await)?}
fn fs_path(path:&Path)->PathBuf{node_path::filesystem_path(&node_path::from_native(path))}

pub const READ_OK: u32 = 4;
pub const WRITE_OK: u32 = 2;
pub const EXECUTE_OK: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity { pub dev: String, pub ino: String }

/// Node bigint stat identity, serialized as decimal strings for retained rows.
/// Follows links, like fs.stat; callers separately enforce directory admission.
pub fn stat_identity(path: &Path) -> std::io::Result<FileIdentity> { identity(&fs_path(path)) }

#[cfg(unix)]
fn identity(path: &Path) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path)?;
    Ok(FileIdentity { dev: metadata.dev().to_string(), ino: metadata.ino().to_string() })
}
#[cfg(windows)]
fn identity(path: &Path) -> std::io::Result<FileIdentity> {
    use std::os::windows::{fs::OpenOptionsExt,io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION,GetFileInformationByHandle,
        FILE_READ_ATTRIBUTES,FILE_FLAG_BACKUP_SEMANTICS,FILE_SHARE_READ,FILE_SHARE_WRITE,FILE_SHARE_DELETE};
    let file = fs::OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .share_mode(FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE).open(path)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 { return Err(std::io::Error::last_os_error()); }
    Ok(FileIdentity { dev: info.dwVolumeSerialNumber.to_string(), ino: (((info.nFileIndexHigh as u64)<<32)|info.nFileIndexLow as u64).to_string() })
}

/// fs.access semantics, including Windows' read-only-file check. This does not
/// replace opening a file for a later operation or checking its owned identity.
pub fn access(path: &Path, mode: u32) -> std::io::Result<()> {
    if mode > (READ_OK|WRITE_OK|EXECUTE_OK) { return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)); }
    access_native(&fs_path(path), mode)
}
#[cfg(unix)]
fn access_native(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::{ffi::CString,os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_|std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    if unsafe { libc::access(path.as_ptr(),mode as i32) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}
#[cfg(windows)]
fn access_native(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{Foundation::ERROR_ACCESS_DENIED,Storage::FileSystem::{GetFileAttributesW,INVALID_FILE_ATTRIBUTES,FILE_ATTRIBUTE_READONLY,FILE_ATTRIBUTE_DIRECTORY}};
    let path: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    let attributes = unsafe { GetFileAttributesW(path.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES { Err(std::io::Error::last_os_error()) }
    else if mode & WRITE_OK != 0 && attributes & FILE_ATTRIBUTE_READONLY != 0 && attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        Err(std::io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32))
    } else { Ok(()) }
}

/// Apply the integer milliseconds represented by JavaScript Date values. Follow
/// links and open Windows directories for attributes, without creating files.
pub fn set_times_ms(path: &Path, accessed_ms: i64, modified_ms: i64) -> std::io::Result<()> {
    const DATE_LIMIT: u64 = 8_640_000_000_000_000;
    if accessed_ms.unsigned_abs() > DATE_LIMIT || modified_ms.unsigned_abs() > DATE_LIMIT {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    times_native(&fs_path(path),accessed_ms,modified_ms)
}
#[cfg(unix)]
fn times_native(path: &Path, accessed: i64, modified: i64) -> std::io::Result<()> {
    use std::{ffi::CString,os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_|std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let time = |ms:i64| libc::timespec { tv_sec: ms.div_euclid(1000) as libc::time_t, tv_nsec: (ms.rem_euclid(1000)*1_000_000) as libc::c_long };
    let times=[time(accessed),time(modified)];
    if unsafe { libc::utimensat(libc::AT_FDCWD,path.as_ptr(),times.as_ptr(),0) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}
#[cfg(windows)]
fn times_native(path: &Path, accessed: i64, modified: i64) -> std::io::Result<()> {
    use std::{os::windows::fs::OpenOptionsExt,time::{SystemTime,UNIX_EPOCH,Duration}};
    use windows_sys::Win32::Storage::FileSystem::{FILE_WRITE_ATTRIBUTES,FILE_FLAG_BACKUP_SEMANTICS,FILE_SHARE_READ,FILE_SHARE_WRITE,FILE_SHARE_DELETE};
    let time = |ms:i64| -> std::io::Result<SystemTime> {
        let duration=Duration::from_millis(ms.unsigned_abs());
        (if ms < 0 { UNIX_EPOCH.checked_sub(duration) } else { UNIX_EPOCH.checked_add(duration) })
            .ok_or_else(||std::io::Error::from(std::io::ErrorKind::InvalidInput))
    };
    let times=fs::FileTimes::new().set_accessed(time(accessed)?).set_modified(time(modified)?);
    fs::OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .share_mode(FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE).open(path)?.set_times(times)
}

/// Node errno names needed by directory browsing. Preserve the distinction
/// between permission failures; unknown OS errors remain unclassified.
pub fn node_error_code(error: &std::io::Error) -> Option<&'static str> {
    #[cfg(unix)]
    match error.raw_os_error() {
        Some(libc::EACCES) => return Some("EACCES"), Some(libc::EPERM) => return Some("EPERM"),
        Some(libc::ENOENT) => return Some("ENOENT"), Some(libc::ENOTDIR) => return Some("ENOTDIR"), _ => {}
    }
    #[cfg(windows)]
    match error.raw_os_error() {
        Some(5|1314) => return Some("EPERM"), Some(740|1920|10013) => return Some("EACCES"), _ => {}
    }
    match error.kind() {
        std::io::ErrorKind::NotFound => Some("ENOENT"), std::io::ErrorKind::NotADirectory => Some("ENOTDIR"),
        std::io::ErrorKind::PermissionDenied => Some("EACCES"), _ => None,
    }
}
#[cfg(windows)]
fn remove_windows(path:&Path,force:bool)->std::io::Result<()>{
    let metadata=match fs::symlink_metadata(path){Ok(value)=>value,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(()),Err(e)=>return Err(e)};
    if metadata.is_dir()&&!metadata.file_type().is_symlink(){
        for entry in fs::read_dir(path)?{remove_windows(&entry?.path(),force)?;}
        if force && metadata.permissions().readonly(){let mut permissions=metadata.permissions();permissions.set_readonly(false);fs::set_permissions(path,permissions)?;}
        fs::remove_dir(path)
    }else{
        use std::os::windows::fs::MetadataExt;
        if metadata.file_type().is_symlink()&&metadata.file_attributes()&0x10!=0{return fs::remove_dir(path)}
        match fs::remove_file(path){Err(e) if e.kind()==std::io::ErrorKind::PermissionDenied=>{let mut permissions=metadata.permissions();permissions.set_readonly(false);fs::set_permissions(path,permissions)?;fs::remove_file(path)},result=>result}
    }
}
/// Node rm(path,{recursive:true,force:true}) with its default zero retries.
/// Missing entries are accepted before and during removal; links are removed
/// themselves. Keep an admitted removal alive if its caller stops waiting.
pub async fn remove_force(path:impl Into<PathBuf>)->IvyResult<()>{remove(path.into(),None,0,true).await}
pub(crate) async fn remove_payload(path:PathBuf,directory:bool,retries:u64)->IvyResult<()>{remove(path,Some(directory),retries,false).await}
async fn remove(path:PathBuf,directory:Option<bool>,retries:u64,force:bool)->IvyResult<()>{
    blocking(move||{
        let path=fs_path(&path);
        let metadata=match fs::symlink_metadata(&path){Ok(metadata)=>metadata,Err(error) if force&&error.kind()==std::io::ErrorKind::NotFound=>return Ok(()),Err(_)=>return Err(IvyError::internal())};
        let directory=directory.unwrap_or_else(||{
            #[cfg(windows)]{use std::os::windows::fs::MetadataExt;metadata.file_attributes()&0x10!=0}
            #[cfg(not(windows))]{metadata.is_dir()}
        });
        for attempt in 0..=retries {
            let result=if directory{fs::remove_dir_all(&path)}else{fs::remove_file(&path)};
            #[cfg(windows)] let result=match result{Err(e) if (directory||force)&&e.kind()==std::io::ErrorKind::PermissionDenied=>remove_windows(&path,force),result=>result};
            match result{
                Ok(())=>return Ok(()),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(()),
                Err(e) if attempt<retries&&retry_remove(&e)=>std::thread::sleep(std::time::Duration::from_millis((attempt+1)*100)),
                Err(_)=>return Err(IvyError::internal()),
            }
        }
        Err(IvyError::internal())
    }).await
}
fn retry_remove(error:&std::io::Error)->bool{
    #[cfg(unix)]{matches!(error.raw_os_error(),Some(libc::EBUSY|libc::EMFILE|libc::ENFILE|libc::ENOTEMPTY|libc::EPERM))}
    #[cfg(windows)]{matches!(error.kind(),std::io::ErrorKind::PermissionDenied|std::io::ErrorKind::DirectoryNotEmpty|std::io::ErrorKind::ResourceBusy)||error.raw_os_error()==Some(4)}
}
