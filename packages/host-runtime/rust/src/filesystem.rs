//! Node-style owned payload removal, including Windows read-only descendants.
use crate::{IvyError,IvyResult,node_path};
use std::{fs,path::{Path,PathBuf}};
fn internal<T,E>(value:Result<T,E>)->IvyResult<T>{value.map_err(|_|IvyError::internal())}
async fn blocking<T:Send+'static>(work:impl FnOnce()->IvyResult<T>+Send+'static)->IvyResult<T>{internal(tokio::task::spawn_blocking(work).await)?}
fn fs_path(path:&Path)->PathBuf{node_path::filesystem_path(&node_path::from_native(path))}
#[cfg(windows)]
fn remove_windows(path:&Path)->std::io::Result<()>{
    let metadata=match fs::symlink_metadata(path){Ok(value)=>value,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(()),Err(e)=>return Err(e)};
    if metadata.is_dir()&&!metadata.file_type().is_symlink(){
        for entry in fs::read_dir(path)?{remove_windows(&entry?.path())?;}fs::remove_dir(path)
    }else{
        use std::os::windows::fs::MetadataExt;
        if metadata.file_type().is_symlink()&&metadata.file_attributes()&0x10!=0{return fs::remove_dir(path)}
        match fs::remove_file(path){Err(e) if e.kind()==std::io::ErrorKind::PermissionDenied=>{let mut permissions=metadata.permissions();permissions.set_readonly(false);fs::set_permissions(path,permissions)?;fs::remove_file(path)},result=>result}
    }
}
pub(crate) async fn remove_payload(path:PathBuf,directory:bool,retries:u64)->IvyResult<()>{
    blocking(move||{
        let path=fs_path(&path);internal(fs::symlink_metadata(&path))?;
        for attempt in 0..=retries {
            let result=if directory{fs::remove_dir_all(&path)}else{fs::remove_file(&path)};
            #[cfg(windows)] let result=match result{Err(e) if directory&&e.kind()==std::io::ErrorKind::PermissionDenied=>remove_windows(&path),result=>result};
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
