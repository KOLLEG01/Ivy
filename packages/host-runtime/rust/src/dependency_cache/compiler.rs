//! One rebuildable compiler cache, protected by the existing source/build lease.
use crate::{IvyError, IvyJson as J, IvyResult, config, executor_lock::ExecutorLock, paths, preparations, storage_space::RetentionProgress};
use ivy_sdk::clock::{Clock, SystemClock};
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}, sync::Arc, time::Duration};
#[cfg(test)]
#[path = "../../../../../tests/rust/host-compiler-cache.rs"]
mod tests;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Owner { schema_version: u8, owner: String, last_used_at: i64 }
fn io<T>(v: std::io::Result<T>) -> IvyResult<T> { v.map_err(|_| IvyError::internal()) }
fn conflict() -> IvyError { IvyError::new("target_conflict", "Compiler cache must remain directly owned.") }
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> IvyResult<T> + Send + 'static) -> IvyResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|_| IvyError::internal())?
}
fn guard(staging: &Path, root: &Path) -> IvyResult<()> {
    let m = io(fs::symlink_metadata(root))?; let actual = io(paths::realpath(root))?;
    if !m.is_dir() || m.file_type().is_symlink() || actual.parent() != Some(io(paths::realpath(staging))?.as_path()) || actual.file_name() != root.file_name() { return Err(conflict()); }
    Ok(())
}
fn read(staging: &Path, root: &Path) -> IvyResult<Owner> {
    guard(staging, root)?;
    let path = root.join("owner.json"); let m = io(fs::symlink_metadata(&path))?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > 4096 { return Err(conflict()); }
    let owner: Owner = serde_json::from_slice(&io(fs::read(path))?).map_err(|_| conflict())?;
    if owner.schema_version != 1 || owner.owner != "ivy-compiler-cache" { return Err(conflict()); }
    Ok(owner)
}
async fn remove_retired(staging: PathBuf, root: PathBuf) -> IvyResult<()> {
    let parent = staging.clone(); let check = root.clone();
    blocking(move || {
        guard(&parent, &check)?;
        if io(fs::read_dir(&check))?.next().is_none() { return Ok(()); }
        read(&parent, &check)?;
        for entry in io(fs::read_dir(&check))? {
            let name = io(entry)?.file_name();
            if name != "owner.json" && name != "cargo" { return Err(conflict()); }
        }
        Ok(())
    }).await?;
    let cargo = root.join("cargo");
    match tokio::fs::symlink_metadata(&cargo).await {
        Ok(_) => { let parent = root.clone(); let check = cargo.clone(); blocking(move || guard(&parent, &check)).await?; crate::filesystem::remove_payload(cargo, true, 0).await?; },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}, Err(_) => return Err(IvyError::internal()),
    }
    // The owner is removed last, so an interrupted payload removal remains
    // identifiable. A crash after this point leaves only an empty directory.
    blocking(move || {
        guard(&staging, &root)?;
        match fs::remove_file(root.join("owner.json")) { Ok(()) => {}, Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}, Err(_) => return Err(IvyError::internal()) }
        io(fs::remove_dir(root))
    }).await
}
/// Called after the preparation record is durable, under a shared source lease.
pub(crate) async fn begin(staging: PathBuf) -> IvyResult<PathBuf> {
    let deadline = SystemClock.utc_millis() + 5000;
    let lock = loop {
        match ExecutorLock::acquire(staging.join("compiler-cache-init-lock"), false).await {
            Ok(lock) => break lock,
            Err(e) if e.code == "executor_already_running" && SystemClock.utc_millis() < deadline => tokio::time::sleep(Duration::from_millis(25)).await,
            Err(e) => return Err(e),
        }
    };
    let result = async {
        let parent = staging.clone(); let root = staging.join("compiler-cache"); let check = root.clone();
        blocking(move || {
            match fs::create_dir(&check) {
                Ok(()) => {},
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Err(error) = read(&parent, &check) { if io(fs::read_dir(&check))?.next().is_some() { return Err(error); } }
                },
                Err(_) => return Err(IvyError::internal()),
            }
            guard(&parent, &check)
        }).await?;
        let owner = Owner { schema_version: 1, owner: "ivy-compiler-cache".into(), last_used_at: SystemClock.utc_millis() };
        let text = serde_json::to_string(&owner).map_err(|_| IvyError::internal())?;
        config::atomic_json(root.join("owner.json"), ivy_contracts::json::parse(&text, 4096)?).await?;
        Ok(root)
    }.await;
    lock.close().await?; result
}
pub(crate) async fn collect(host: &J, now: i64, pressure: bool, progress: Arc<dyn RetentionProgress>) -> IvyResult<()> {
    let staging = config::path_value(host.required("stagingRoot")?)?;
    let lock = match ExecutorLock::acquire(staging.join("source-storage-lock"), false).await {
        Ok(lock) => lock,
        Err(e) if e.code == "executor_already_running" => { progress.skipped("compiler-cache", "active_build", false); return Ok(()); },
        Err(e) => return Err(e),
    };
    let result = async {
        let root = staging.join("compiler-cache"); let retired = staging.join(".retiring-compiler-cache");
        if !progress.permit_removal("compiler-cache") { return Ok(()); }
        if tokio::fs::symlink_metadata(&retired).await.is_ok() {
            if let Err(error) = remove_retired(staging.clone(), retired.clone()).await { progress.skipped("compiler-cache", &error.code, true); return Ok(()); }
            progress.removed("payloads");
        }
        match tokio::fs::symlink_metadata(&root).await { Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()), Ok(_) => {}, Err(_) => return Err(IvyError::internal()) }
        let parent = staging.clone(); let check = root.clone();
        let owner = match blocking(move || read(&parent, &check)).await { Ok(v) => v, Err(_) => { progress.skipped("compiler-cache", "unrecognized_owner", true); return Ok(()); } };
        let minimum_idle = if pressure { 3_600_000 } else { 7 * 86_400_000 };
        if now.saturating_sub(owner.last_used_at) < minimum_idle { progress.skipped("compiler-cache", "recent_use", false); return Ok(()); }
        if preparations::compiler_cache_referenced(staging.clone()).await? { progress.skipped("compiler-cache", "unfinished_preparation", true); return Ok(()); }
        let parent = staging.clone(); let check = root.clone(); let to = retired.clone();
        blocking(move || { if read(&parent, &check)?.last_used_at != owner.last_used_at { return Err(conflict()); } io(fs::rename(check, to)) }).await?;
        remove_retired(staging.clone(), retired).await?; progress.removed("payloads");
        Ok(())
    }.await;
    lock.close().await?; result
}
