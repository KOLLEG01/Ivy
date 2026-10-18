//! Managed MCP filesystem application with the existing SQLite owner lock.
use crate::mcp_prepare::{PreparedMcp, prepare_mcp};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::digest, json::parse};
use ivy_host_runtime::executor_lock::ExecutorLock;
use ivy_sdk::rpc::RpcClient;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub struct AppliedMcp {
    /// Native canonical path. The service's wire-path boundary owns presentation.
    pub target: PathBuf,
    pub desired_version: String,
    pub applied: bool,
    pub managed_bytes: usize,
}
fn internal<T>(result: Result<T, impl std::fmt::Debug>) -> IvyResult<T> {
    result.map_err(|_| IvyError::internal())
}
fn conflict(message: &str) -> IvyError {
    IvyError::new("target_conflict", message)
}

pub(crate) fn regular_bytes(path: &Path) -> IvyResult<Option<Vec<u8>>> {
    let info = match fs::symlink_metadata(path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IvyError::internal()),
    };
    if !info.is_file() || info.file_type().is_symlink() || info.len() > 4 * 1024 * 1024 {
        return Err(conflict("Managed target must be a bounded regular file."));
    }
    // Like the original, disappearance during read is absence. The caller's
    // compare-before-rename check detects changes since its initial snapshot.
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(IvyError::internal()),
    }
}
fn text(bytes: Option<&[u8]>) -> IvyResult<&str> {
    let source = std::str::from_utf8(bytes.unwrap_or_default())
        .map_err(|_| conflict("Managed Codex configuration is not valid UTF-8."))?;
    Ok(source.strip_prefix('\u{feff}').unwrap_or(source))
}
fn state_root(root: &Path) -> IvyResult<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    internal(builder.create(root))?;
    let info = internal(fs::symlink_metadata(root))?;
    if !info.is_dir() || info.file_type().is_symlink() {
        return Err(conflict("Managed MCP state must be a real directory."));
    }
    Ok(())
}
fn replace_config(path: &Path, before: Option<&[u8]>, after: &[u8]) -> IvyResult<()> {
    let temporary = path
        .parent()
        .ok_or_else(IvyError::internal)?
        .join(format!(".ivy-environment-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = internal(options.open(&temporary))?;
    internal(file.write_all(after))?;
    internal(file.sync_all())?;
    drop(file);
    let result = (|| {
        if regular_bytes(path)?.as_deref().map(digest) != before.map(digest) {
            return Err(conflict("Codex configuration changed during application."));
        }
        internal(fs::rename(&temporary, path))
    })();
    let _ = fs::remove_file(&temporary);
    result
}
fn apply_locked(home: PathBuf, prepared: PreparedMcp) -> IvyResult<AppliedMcp> {
    let target = home.join("config.toml");
    let state_path = home.join(".ivy-agent-mcp/state.json");
    let raw_state = regular_bytes(&state_path)?;
    let state = raw_state
        .as_ref()
        .map(|bytes| {
            // State uses Buffer.toString + JSON.parse; only config uses fatal UTF-8.
            parse(&String::from_utf8_lossy(bytes), usize::MAX).map_err(|_| IvyError::internal())
        })
        .transpose()?;
    crate::mcp_output::validate_authority(state.as_ref(), &prepared.authority)?;
    let before = regular_bytes(&target)?;
    let planned = prepared.plan(text(before.as_deref())?, state.as_ref())?;
    if before.as_deref() != Some(planned.bytes.as_slice()) {
        replace_config(&target, before.as_deref(), &planned.bytes)?;
    }
    // Preserve original raw filesystem error mapping at the manager boundary.
    internal(ivy_store::atomic::atomic_json(&state_path, &planned.state))?;
    Ok(AppliedMcp {
        target,
        desired_version: prepared.desired_version,
        applied: planned.applied,
        managed_bytes: planned.managed_bytes,
    })
}

/// One application; the service manager owns synchronization coalescing/status.
/// Resolve home before network reads. Once mutation starts, the blocking worker
/// owns the lock, so dropping the awaiter cannot unlock still-running writes.
pub async fn apply_mcp(
    home: PathBuf,
    connection: &dyn RpcClient,
    defaults: &IvyJson,
    host_id: &JsString,
    hive_url: &JsString,
    credential: &JsString,
) -> IvyResult<AppliedMcp> {
    let home = internal(
        tokio::task::spawn_blocking(move || internal(ivy_host_runtime::paths::realpath(&home)))
            .await,
    )??;
    let prepared = prepare_mcp(connection, defaults, host_id, hive_url, credential).await?;
    let root = home.join(".ivy-agent-mcp");
    let check_root = root.clone();
    internal(tokio::task::spawn_blocking(move || state_root(&check_root)).await)??;
    let lock = ExecutorLock::acquire(root, false).await?;
    internal(
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            apply_locked(home, prepared)
        })
        .await,
    )?
}
