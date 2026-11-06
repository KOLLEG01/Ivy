//! Package import scratch has a durable completion marker and a single owner.
use crate::{IvyError, IvyJson as J, IvyResult, config, executor_lock::ExecutorLock, paths, storage_space::RetentionProgress};
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}, sync::Arc};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record { schema_version: u8, archive_hash: String, build_id: String, completed: bool }
fn hash(v: &str) -> bool {
    v.strip_prefix("sha256:").is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}
fn conflict() -> IvyError { IvyError::new("target_conflict", "Package transfer storage is not directly owned.") }
fn io<T>(v: std::io::Result<T>) -> IvyResult<T> { v.map_err(|_| IvyError::internal()) }
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> IvyResult<T> + Send + 'static) -> IvyResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|_| IvyError::internal())?
}
fn direct(parent: &Path, child: &Path) -> IvyResult<()> {
    let m = io(fs::symlink_metadata(child))?;
    let actual = io(paths::realpath(child))?;
    if !m.is_dir() || m.file_type().is_symlink() || actual.parent() != Some(io(paths::realpath(parent))?.as_path()) || actual.file_name() != child.file_name() { return Err(conflict()); }
    Ok(())
}
fn guard(staging: &Path, directory: &Path) -> IvyResult<()> {
    let root = staging.join("package-transfers");
    direct(staging, &root)?; direct(&root, directory)
}
fn read(staging: &Path, directory: &Path) -> IvyResult<Record> {
    guard(staging, directory)?;
    let path = directory.join("transfer.json"); let m = io(fs::symlink_metadata(&path))?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > 4096 { return Err(conflict()); }
    let r: Record = serde_json::from_slice(&io(fs::read(path))?).map_err(|_| conflict())?;
    if r.schema_version != 1 || !hash(&r.archive_hash) || !hash(&r.build_id) || directory.file_name().and_then(|v| v.to_str()) != Some(&r.archive_hash[7..]) { return Err(conflict()); }
    Ok(r)
}
pub(crate) struct Transfer { host: J, directory: PathBuf, record: Record }
pub(crate) async fn complete_existing(host: J, archive: String, build: String) -> IvyResult<()> {
    let directory = config::path_value(host.required("stagingRoot")?)?.join("package-transfers").join(archive.get(7..).ok_or_else(conflict)?);
    match tokio::fs::symlink_metadata(&directory).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => {
            let staging = config::path_value(host.required("stagingRoot")?)?; let check = directory.clone();
            let record = blocking(move || read(&staging, &check)).await?;
            if record.archive_hash != archive || record.build_id != build { return Err(conflict()); }
            Transfer { host, directory, record }.complete().await
        },
        Err(_) => Err(IvyError::internal()),
    }
}
impl Transfer {
    pub(crate) fn archive(&self) -> PathBuf { self.directory.join("archive.tar.gz") }
    pub(crate) fn extraction(&self) -> PathBuf { self.directory.join("extracted") }
    /// The caller already holds the original build's exclusive SQLite lease.
    pub(crate) async fn begin(host: J, archive_hash: String, build_id: String) -> IvyResult<Self> {
        if !hash(&archive_hash) || !hash(&build_id) { return Err(conflict()); }
        let staging = config::path_value(host.required("stagingRoot")?)?;
        let directory = staging.join("package-transfers").join(&archive_hash[7..]);
        let check = directory.clone(); let archive = archive_hash.clone(); let build = build_id.clone();
        let completed = blocking(move || {
            let root = staging.join("package-transfers");
            match fs::symlink_metadata(&root) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => { io(fs::create_dir_all(&root))?; },
                Ok(_) => {}, Err(_) => return Err(IvyError::internal()),
            }
            direct(&staging, &root)?;
            match fs::symlink_metadata(&check) {
                Ok(_) => {
                    let r = read(&staging, &check)?;
                    if r.archive_hash != archive || r.build_id != build { return Err(conflict()); }
                    if !r.completed { return Err(IvyError::new("outcome_unknown", "Reconcile the original package import before reusing its scratch.").with_outcome(crate::Outcome::Unknown)); }
                    Ok(true)
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => { io(fs::create_dir(&check))?; guard(&staging, &check)?; Ok(false) },
                Err(_) => return Err(IvyError::internal()),
            }
        }).await?;
        if completed {
            remove_completed(host.clone(), directory.clone()).await?;
            let staging = config::path_value(host.required("stagingRoot")?)?; let check = directory.clone();
            blocking(move || { io(fs::create_dir(&check))?; guard(&staging, &check) }).await?;
        }
        let transfer = Self { host, directory, record: Record { schema_version: 1, archive_hash, build_id, completed: false } };
        transfer.save().await?; Ok(transfer)
    }
    async fn save(&self) -> IvyResult<()> {
        let staging = config::path_value(self.host.required("stagingRoot")?)?; let directory = self.directory.clone();
        blocking(move || guard(&staging, &directory)).await?;
        let text = serde_json::to_string(&self.record).map_err(|_| IvyError::internal())?;
        config::atomic_json(self.directory.join("transfer.json"), ivy_contracts::json::parse(&text, 4096)?).await
    }
    /// Only a definite drained outcome permits completion. Unknown stays owned.
    pub(crate) async fn complete(mut self) -> IvyResult<()> {
        self.record.completed = true; self.save().await?;
        remove_completed(self.host, self.directory).await
    }
}
async fn remove_completed(host: J, directory: PathBuf) -> IvyResult<()> {
    let staging = config::path_value(host.required("stagingRoot")?)?;
    let check = directory.clone(); let parent = staging.clone();
    let record = blocking(move || {
        let r = read(&parent, &check)?;
        if !r.completed { return Err(IvyError::new("transfer_unfinished", "The original package import is unresolved.")); }
        for entry in io(fs::read_dir(&check))? {
            let name = io(entry)?.file_name();
            if !["transfer.json", "archive.tar.gz", "extracted"].iter().any(|s| name == *s) { return Err(conflict()); }
        }
        Ok(r)
    }).await?;
    let artifacts = config::path_value(host.required("artifactRoot")?)?;
    let incoming = artifacts.join(format!("incoming-package-{}", &record.archive_hash[7..]));
    match tokio::fs::symlink_metadata(&incoming).await {
        Ok(_) => { let child = incoming.clone(); blocking(move || direct(&artifacts, &child)).await?; crate::filesystem::remove_payload(incoming, true, 0).await?; },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}, Err(_) => return Err(IvyError::internal()),
    }
    for (name, dir) in [("archive.tar.gz", false), ("extracted", true)] {
        let parent = staging.clone(); let check = directory.clone();
        blocking(move || guard(&parent, &check)).await?;
        let payload = directory.join(name);
        match tokio::fs::symlink_metadata(&payload).await {
            Ok(info) if !info.file_type().is_symlink() && (if dir { info.is_dir() } else { info.is_file() }) => crate::filesystem::remove_payload(payload, dir, 0).await?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            _ => return Err(conflict()),
        }
    }
    // Preserve the completion marker until all bytes have been released.
    blocking(move || { guard(&staging, &directory)?; io(fs::remove_file(directory.join("transfer.json")))?; io(fs::remove_dir(directory)) }).await
}
pub(crate) async fn collect(host: J, progress: Arc<dyn RetentionProgress>) -> IvyResult<()> {
    let staging = config::path_value(host.required("stagingRoot")?)?; let scan = staging.clone();
    let dirs = blocking(move || {
        let root = scan.join("package-transfers");
        match fs::symlink_metadata(&root) { Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()), Ok(_) => {}, Err(_) => return Err(IvyError::internal()) }
        direct(&scan, &root)?;
        let mut dirs = io(fs::read_dir(&root))?.take(1001).map(|e| io(e).map(|e| e.path())).collect::<IvyResult<Vec<_>>>()?;
        if dirs.len() > 1000 { return Err(IvyError::new("limit_exceeded", "Package transfer inventory exceeds its bound.")); }
        dirs.sort(); Ok(dirs)
    }).await?;
    for directory in dirs {
        let parent = staging.clone(); let check = directory.clone();
        let r = match blocking(move || read(&parent, &check)).await { Ok(r) => r, Err(_) => { progress.skipped("transfers", "unrecognized_transfer", true); continue; } };
        if !r.completed { progress.skipped("transfers", "unfinished_transfer", true); continue; }
        if !progress.permit_removal("transfers") { break; }
        let lock = match ExecutorLock::acquire(staging.join("build-locks").join(&r.build_id[7..]), false).await {
            Ok(lock) => lock,
            Err(e) if e.code == "executor_already_running" => { progress.skipped("transfers", "active_import", false); continue; },
            Err(e) => return Err(e),
        };
        let result = remove_completed(host.clone(), directory).await; lock.close().await?;
        match result { Ok(()) => progress.removed("payloads"), Err(e) => progress.skipped("transfers", &e.code, true) }
    }
    Ok(())
}
