//! Explicit, offline MCP import; never invoked by normal refresh/bootstrap.
use crate::{
    environment_text::{BEGIN_MCP, BEGIN_PERMISSIONS, END_MCP, END_PERMISSIONS},
    mcp_files::{conflict, internal, regular_bytes, replace_config, state_root, text},
    mcp_migration::plan_mcp_import,
    mcp_output::{McpOutputInput, state_present},
    mcp_prepare::PreparedMcp,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    hash::{digest, hash_json},
    json::parse,
};
use ivy_host_runtime::{executor_lock::ExecutorLock, paths};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

pub struct ImportedMcp {
    pub target: PathBuf,
    pub state_path: PathBuf,
    /// Exact source bytes survive under the existing private state directory.
    pub source_backup: Option<PathBuf>,
    pub changed: bool,
}
fn retain(path: &Path, source: Option<&[u8]>) -> IvyResult<()> {
    let existing = regular_bytes(path)?;
    match (existing.as_deref(), source) {
        (Some(before), Some(after)) if before == after => Ok(()),
        (None, None) => Ok(()),
        (None, Some(bytes)) => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = internal(options.open(path))?;
            internal(file.write_all(bytes))?;
            internal(file.sync_all())
        }
        _ => Err(conflict("MCP import backup differs from its source.")),
    }
}
fn import_locked(home: PathBuf, prepared: PreparedMcp) -> IvyResult<ImportedMcp> {
    let target = home.join("config.toml");
    let state_path = home.join(".ivy-agent-mcp/state.json");
    let before = regular_bytes(&target)?;
    let raw_state = regular_bytes(&state_path)?;
    let state = raw_state
        .as_ref()
        .map(|bytes| {
            parse(&String::from_utf8_lossy(bytes), usize::MAX).map_err(|_| IvyError::internal())
        })
        .transpose()?;
    crate::mcp_migration::validate_authority(state.as_ref(), &prepared.authority)?;
    let source = text(before.as_deref())?;
    let version = state.as_ref().and_then(|s| s.get("schemaVersion"));
    let needed = version == Some(&IvyJson::Number(1.0))
        || version != Some(&IvyJson::Number(2.0))
            && [BEGIN_MCP, END_MCP, BEGIN_PERMISSIONS, END_PERMISSIONS]
                .iter()
                .any(|marker| source.contains(marker));
    // Use the unchanged old converter, including its desired-hash recovery and
    // local-edit/unmanaged permission checks, only during this explicit step.
    let planned = plan_mcp_import(McpOutputInput {
        source,
        state: state.as_ref(),
        authority: &prepared.authority,
        sections: prepared.sections.as_deref(),
        server_names: &prepared.server_names,
        app_id: prepared.app_id.as_deref(),
        desired_version: &prepared.desired_version,
    })?;
    if !needed {
        return Ok(ImportedMcp {
            target,
            state_path,
            source_backup: None,
            changed: false,
        });
    }
    let mut converted = state
        .as_ref()
        .filter(|s| state_present(Some(s)))
        .and_then(IvyJson::as_object)
        .map(|fields| IvyJson::object(fields.iter().cloned()))
        .unwrap_or_else(|| IvyJson::object([] as [(&str, IvyJson); 0]));
    converted.insert("schemaVersion", IvyJson::Number(2.0));
    converted.insert("authority", prepared.authority.into());
    converted.insert(
        "appliedBlockHash",
        planned.state.required("appliedBlockHash")?.clone(),
    );
    // Source metadata remains a record of the source installation. Normal
    // synchronization later records its own complete desired revision identity.
    if converted.get("appliedVersion").is_none() {
        converted.insert("appliedVersion", IvyJson::Null);
    }
    let identity = hash_json(&IvyJson::object([
        (
            "config",
            before
                .as_ref()
                .map(|b| IvyJson::from(digest(b)))
                .unwrap_or(IvyJson::Null),
        ),
        (
            "state",
            raw_state
                .as_ref()
                .map(|b| IvyJson::from(digest(b)))
                .unwrap_or(IvyJson::Null),
        ),
    ]))?;
    let backup = home
        .join(".ivy-agent-mcp")
        .join(format!("import-source-{}", &identity[7..]));
    state_root(&backup)?;
    retain(&backup.join("config.toml"), before.as_deref())?;
    retain(&backup.join("state.json"), raw_state.as_deref())?;
    if before.as_deref() != Some(planned.bytes.as_slice()) {
        replace_config(&target, before.as_deref(), &planned.bytes)?;
    }
    // As in the existing application, a crash between files is not atomic.
    // Never remove backups or copied values on a possibly published state error.
    internal(ivy_store::atomic::atomic_json(&state_path, &converted))?;
    Ok(ImportedMcp {
        target,
        state_path,
        source_backup: Some(backup),
        changed: true,
    })
}

/// The caller stops the old home owner and explicitly selects the snapshot.
/// This operation is independent of Hive availability. It retains original
/// config/state bytes and unknown state metadata; no runtime recovery format
/// or startup migration branch is introduced. Inspect an interrupted import
/// before any retry rather than inferring success from partial file effects.
pub async fn import_mcp(home: PathBuf, prepared: PreparedMcp) -> IvyResult<ImportedMcp> {
    let home =
        internal(tokio::task::spawn_blocking(move || internal(paths::realpath(&home))).await)??;
    let root = home.join(".ivy-agent-mcp");
    internal(
        tokio::task::spawn_blocking({
            let root = root.clone();
            move || state_root(&root)
        })
        .await,
    )??;
    let lock = ExecutorLock::acquire(root, false).await?;
    internal(
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            import_locked(home, prepared)
        })
        .await,
    )?
}
