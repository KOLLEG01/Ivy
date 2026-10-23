//! Shared build leases protect cache links without serializing independent builds.
use crate::{IvyError, IvyResult, executor_lock::ExecutorLock, native_path};
use ivy_sdk::clock::{Clock,SystemClock};
use std::{path::PathBuf,time::Duration};
use std::{fs,path::Path,sync::Arc,time::{SystemTime,UNIX_EPOCH}};
use crate::{config::{read_json,FileError},paths,storage_space::RetentionProgress};

fn internal<T,E>(result:Result<T,E>)->IvyResult<T>{result.map_err(|_|IvyError::internal())}
async fn blocking<T:Send+'static>(work:impl FnOnce()->IvyResult<T>+Send+'static)->IvyResult<T>{internal(tokio::task::spawn_blocking(work).await)?}
fn same_path(left:&Path,right:&Path)->IvyResult<bool>{Ok(native_path::relative(&internal(paths::absolute(left))?,&internal(paths::absolute(right))?).as_os_str().is_empty())}
fn owned_root(staging:&Path,root:&Path)->IvyResult<PathBuf>{
    let metadata=internal(fs::symlink_metadata(root))?;let actual=internal(paths::realpath(root))?;let expected=internal(paths::realpath(staging))?.join("dependency-cache");
    if !metadata.is_dir()||metadata.file_type().is_symlink()||!same_path(&expected,&actual)?{return Err(IvyError::new("target_conflict","Dependency cache root must remain a directly owned directory."));}Ok(actual)
}
fn guard(staging:&Path,root:&Path,boundary:&Path)->IvyResult<()>{if same_path(&owned_root(staging,root)?,boundary)?{Ok(())}else{Err(IvyError::new("target_conflict","Dependency cache root must remain a directly owned directory."))}}
fn cache_name(name:&str)->bool{name.len()==64&&name.bytes().all(|c|c.is_ascii_digit()||(b'a'..=b'f').contains(&c))}
fn milliseconds(at:SystemTime)->f64{let(positive,duration)=match at.duration_since(UNIX_EPOCH){Ok(duration)=>(true,duration),Err(error)=>(false,error.duration())};let value=duration.as_secs() as f64*1000.0+duration.subsec_nanos() as f64/1_000_000.0;if positive{value}else{-value}}

/// Retain the latest reusable cache and live build leases. The agreed root
/// guard rejects linked cache roots before traversal. Configured staging may
/// itself resolve through a link. Rechecks reduce, but do not eliminate, races
/// with uncooperative external directory replacement.
pub async fn collect_dependency_caches(staging:PathBuf,progress:Arc<dyn RetentionProgress>)->IvyResult<()>{
    internal(tokio::spawn(collect(staging,progress)).await)?
}
async fn collect(staging:PathBuf,progress:Arc<dyn RetentionProgress>)->IvyResult<()>{
    let root=staging.join("dependency-cache");let initial_staging=staging.clone();let initial=root.clone();
    let boundary=blocking(move||{
        // Inspect an existing link before mkdir can follow it, including a
        // dangling junction. Ordinary absent roots keep their creation path.
        match fs::symlink_metadata(&initial){Ok(metadata) if metadata.file_type().is_symlink()=>return Err(IvyError::new("target_conflict","Dependency cache root must remain a directly owned directory.")),Ok(_)=>{},Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},Err(_)=>return Err(IvyError::internal())}
        internal(fs::create_dir_all(&initial))?;owned_root(&initial_staging,&initial)
    }).await?;
    let scan=root.clone();let names=blocking(move||{
        let mut names=Vec::new();for entry in internal(fs::read_dir(&scan))?{let entry=internal(entry)?;if !internal(entry.file_type())?.is_dir(){continue}let Some(name)=entry.file_name().to_str().map(str::to_owned)else{continue};if cache_name(&name)||name.strip_prefix(".retiring-").is_some_and(cache_name){names.push(name);}}
        names.sort();Ok(names)
    }).await?;
    let mut caches=Vec::new();
    for name in names{
        let target=root.join(&name);let checked=target.clone();let check_root=root.clone();let check_staging=staging.clone();let check_boundary=boundary.clone();
        blocking(move||{guard(&check_staging,&check_root,&check_boundary)?;if !internal(paths::inside(&check_boundary,&internal(paths::realpath(&checked))?))?{return Err(IvyError::new("target_conflict","Dependency cache leaves its storage root."));}Ok(())}).await?;
        if name.starts_with(".retiring-"){crate::filesystem::remove_payload(target,true,0).await?;progress.removed("payloads");continue}
        let check=target.clone();let key=format!("sha256:{name}");let used=blocking(move||{
            let metadata=match read_json(&check.join("cache.json"),65536){Ok(value)=>Some(value),Err(FileError::Missing)=>None,Err(error)=>return Err(error.error())};
            if metadata.as_ref().and_then(|value|value.get("key"))!=Some(&key.into()){return Ok(None)}
            Ok(Some(milliseconds(internal(internal(fs::symlink_metadata(check))?.modified())?)))
        }).await?;
        if let Some(at)=used{caches.push((name,at));}else{progress.skipped("dependency-cache","unrecognized_manifest",true);}
    }
    // Admitted names use only fixed-width lower-case hexadecimal digits.
    caches.sort_by(|left,right|right.1.total_cmp(&left.1).then_with(||left.0.cmp(&right.0)));
    for(name,used_at)in caches.into_iter().skip(1){
        let lock=match ExecutorLock::acquire(staging.join("dependency-cache-locks").join(&name),false).await{
            Ok(lock)=>lock,Err(error) if error.code=="executor_already_running"=>{progress.skipped("dependency-cache","active_build",false);continue},Err(error)=>return Err(error),
        };
        let checked_root=root.clone();let checked_staging=staging.clone();let checked_boundary=boundary.clone();
        let retired=blocking(move||{
            let root=checked_root;let staging=checked_staging;let boundary=checked_boundary;
            guard(&staging,&root,&boundary)?;let target=root.join(&name);
            let metadata=match fs::symlink_metadata(&target){Ok(value)=>value,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(_)=>return Err(IvyError::internal())};
            if milliseconds(internal(metadata.modified())?)!=used_at{return Ok(None)}
            let actual=match paths::realpath(&target){Ok(value)=>value,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(_)=>return Err(IvyError::internal())};
            if !internal(paths::inside(&boundary,&actual))?{return Err(IvyError::new("target_conflict","Dependency cache leaves its storage root."));}
            let retired=root.join(format!(".retiring-{name}"));match fs::rename(target,&retired){Ok(())=>Ok(Some(retired)),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(None),Err(_)=>Err(IvyError::internal())}
        }).await;
        lock.close().await?;
        if let Some(retired)=retired?{
            let checked_root=staging.join("dependency-cache");let check_staging=staging.clone();let check_boundary=boundary.clone();blocking(move||guard(&check_staging,&checked_root,&check_boundary)).await?;
            crate::filesystem::remove_payload(retired,true,0).await?;progress.removed("payloads");
        }
    }
    Ok(())
}

pub async fn use_dependency_cache(staging_root:PathBuf,key:&str)->IvyResult<ExecutorLock>{
    let root=staging_root.join("dependency-cache-locks").join(key);
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
    tokio::task::spawn_blocking(move||match touch(&root,milliseconds){
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
