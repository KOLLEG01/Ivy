//! Shared build leases protect cache links without serializing independent builds.
use crate::{IvyError, IvyResult, executor_lock::ExecutorLock, node_path::{self,Style}};
use ivy_sdk::clock::{Clock,SystemClock};
use std::{path::PathBuf,time::Duration};

pub async fn use_dependency_cache(staging_root:PathBuf,key:&str)->IvyResult<ExecutorLock>{
    let root=node_path::filesystem_path(&node_path::join(Style::native(),&[node_path::from_native(&staging_root),"dependency-cache-locks".into(),key.into()]));
    let deadline=SystemClock.utc_millis()+5000;
    loop{
        match ExecutorLock::acquire(root.clone(),true).await{
            Ok(lock)=>return Ok(lock),Err(error) if error.code=="executor_already_running"&&SystemClock.utc_millis()<deadline=>tokio::time::sleep(Duration::from_millis(25)).await,
            Err(error)=>return Err(error),
        }
    }
}
pub async fn touch_dependency_cache(root:PathBuf)->IvyResult<()>{touch_dependency_cache_at(root,SystemClock.utc_millis()).await}
pub async fn touch_dependency_cache_at(root:PathBuf,milliseconds:i64)->IvyResult<()>{
    tokio::task::spawn_blocking(move||match touch(&node_path::filesystem_path(&node_path::from_native(&root)),milliseconds){
        Ok(())=>Ok(()),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(()),Err(_)=>Err(IvyError::internal()),
    }).await.map_err(|_|IvyError::internal())?
}
#[cfg(windows)]
fn touch(path:&std::path::Path,milliseconds:i64)->std::io::Result<()>{
    use std::{fs::{File,FileTimes},os::windows::fs::OpenOptionsExt,time::UNIX_EPOCH};
    use windows_sys::Win32::Storage::FileSystem::{FILE_WRITE_ATTRIBUTES,FILE_SHARE_READ,FILE_SHARE_WRITE,FILE_SHARE_DELETE,FILE_FLAG_BACKUP_SEMANTICS};
    let file=File::options().access_mode(FILE_WRITE_ATTRIBUTES).share_mode(FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path)?;
    let duration=Duration::from_millis(milliseconds.unsigned_abs());
    let now=if milliseconds>=0{UNIX_EPOCH.checked_add(duration)}else{UNIX_EPOCH.checked_sub(duration)}.ok_or_else(||std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    file.set_times(FileTimes::new().set_accessed(now).set_modified(now))
}
#[cfg(unix)]
fn touch(path:&std::path::Path,milliseconds:i64)->std::io::Result<()>{
    use std::{ffi::CString,os::unix::ffi::OsStrExt};
    let path=CString::new(path.as_os_str().as_bytes()).map_err(|_|std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let at=libc::timespec{tv_sec:milliseconds.div_euclid(1000) as _,tv_nsec:(milliseconds.rem_euclid(1000)*1_000_000) as _};let times=[at,at];
    // SAFETY: the NUL-terminated path and two native timespecs live for the call.
    if unsafe{libc::utimensat(libc::AT_FDCWD,path.as_ptr(),times.as_ptr(),0)}!=0{Err(std::io::Error::last_os_error())}else{Ok(())}
}
