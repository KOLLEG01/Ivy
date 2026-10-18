//! Existing instruction ownership and pending-write recovery under the home lock.
use crate::instructions::ComposedInstructions;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
    json::parse,
};
use ivy_host_runtime::executor_lock::ExecutorLock;
use std::{
    fs::{self, OpenOptions},
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    pin::Pin,
};

/// Read authoritative sources and any included local AGENTS.md for this home.
/// The writer calls this again after obtaining ownership; an earlier projection
/// must never win over a peer's more recent work while acquiring the lock.
pub trait InstructionsSource: Send + Sync {
    fn desired<'a>(
        &'a self,
        home: &'a Path,
    ) -> Pin<Box<dyn Future<Output = IvyResult<ComposedInstructions>> + Send + 'a>>;
}

pub enum AppliedInstructions {
    /// Preserve the previous applied-version, byte count and displayed home.
    Unmanaged { desired_version: String },
    Managed {
        home: PathBuf,
        desired_version: String,
        applied_version: IvyJson,
        active: bool,
        bytes: usize,
    },
}
fn internal<T>(result: Result<T, impl std::fmt::Debug>) -> IvyResult<T> {
    result.map_err(|_| IvyError::internal())
}
fn require(condition: bool, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new("target_conflict", message))
    }
}
fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null | IvyJson::Bool(false) => false,
        IvyJson::Number(value) => *value != 0.0 && !value.is_nan(),
        IvyJson::String(value) => !value.0.is_empty(),
        _ => true,
    }
}
/// This is the original one-MiB lstat/read guard, including ENOENT on either
/// operation. Local instruction bytes are decoded only by composition.
pub fn regular_bytes(path: &Path) -> IvyResult<Option<Vec<u8>>> {
    let info = match fs::symlink_metadata(path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IvyError::internal()),
    };
    require(
        info.is_file() && !info.file_type().is_symlink() && info.len() <= 1024 * 1024,
        "Instruction file must be a bounded regular file.",
    )?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(IvyError::internal()),
    }
}
fn bytes_hash(bytes: Option<&[u8]>) -> IvyJson {
    bytes
        .map(digest)
        .map(IvyJson::from)
        .unwrap_or(IvyJson::Null)
}
fn create_file(path: &Path, bytes: &[u8]) -> IvyResult<()> {
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
    require(
        info.is_dir() && !info.file_type().is_symlink(),
        "Managed instruction state is not a real directory.",
    )?;
    for suffix in ["", "-journal", "-wal", "-shm"] {
        regular_bytes(&root.join(format!("executor-lock.sqlite{suffix}")))?;
    }
    Ok(())
}
fn save_blob(root: &Path, bytes: Option<&[u8]>) -> IvyResult<IvyJson> {
    let Some(bytes) = bytes else {
        return Ok(IvyJson::Null);
    };
    let hash = digest(bytes);
    let path = root.join(format!("{}.md", &hash[7..]));
    if let Some(existing) = regular_bytes(&path)? {
        require(
            digest(&existing) == hash,
            "Instruction backup bytes changed.",
        )?;
    } else {
        create_file(&path, bytes)?;
    }
    Ok(hash.into())
}
fn load_blob(root: &Path, hash: &IvyJson) -> IvyResult<Option<Vec<u8>>> {
    if hash == &IvyJson::Null {
        return Ok(None);
    }
    let hash = hash.as_js_string().and_then(|s| s.to_utf8().ok());
    let hash = hash.as_deref().filter(|hash| {
        hash.len() == 71
            && hash.starts_with("sha256:")
            && hash.as_bytes()[7..]
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    });
    require(hash.is_some(), "Invalid instruction backup reference.")?;
    let hash = hash.unwrap();
    let bytes = regular_bytes(&root.join(format!("{}.md", &hash[7..])))?;
    require(
        bytes.as_deref().map(digest).as_deref() == Some(hash),
        "Instruction backup is unavailable or changed.",
    )?;
    Ok(bytes)
}
fn field<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a IvyJson> {
    value.get(name).ok_or_else(IvyError::internal)
}
fn updated(value: &IvyJson, changes: &[(&str, IvyJson)]) -> IvyResult<IvyJson> {
    let mut entries = value.as_object().ok_or_else(IvyError::internal)?.to_vec();
    for (name, value) in changes {
        if let Some((_, previous)) = entries
            .iter_mut()
            .find(|(key, _)| key == &JsString::from(*name))
        {
            *previous = value.clone();
        } else {
            entries.push((JsString::from(*name), value.clone()));
        }
    }
    Ok(IvyJson::Object(entries))
}
fn save_state(path: &Path, state: &IvyJson) -> IvyResult<()> {
    // Original raw filesystem failures become internal_error at the manager.
    internal(ivy_store::atomic::atomic_json(path, state))
}
fn finish_pending(home: &Path, root: &Path, state: &mut IvyJson) -> IvyResult<()> {
    let pending = field(state, "pending")?;
    let target = home.join("AGENTS.override.md");
    let current = regular_bytes(&target)?;
    let current_hash = bytes_hash(current.as_deref());
    let before = pending.get("before");
    let after = pending.get("after");
    require(
        Some(&current_hash) == before || Some(&current_hash) == after,
        "Local instructions changed during application; user edits are retained.",
    )?;
    if Some(&current_hash) != after {
        let bytes = load_blob(root, field(pending, "after")?)?;
        if let Some(bytes) = bytes {
            let temporary = home.join(format!(".ivy-instructions-{}.tmp", uuid::Uuid::new_v4()));
            // Preserve original write/sync failure ordering: cleanup starts only
            // after the complete temporary file has been written and closed.
            create_file(&temporary, &bytes)?;
            let result = (|| {
                require(
                    Some(&bytes_hash(regular_bytes(&target)?.as_deref())) == before,
                    "Local instructions changed.",
                )?;
                internal(fs::rename(&temporary, &target))
            })();
            let _ = fs::remove_file(&temporary);
            result?;
        } else {
            require(
                Some(&bytes_hash(regular_bytes(&target)?.as_deref())) == before,
                "Local instructions changed.",
            )?;
            internal(fs::remove_file(&target))?;
        }
        #[cfg(unix)]
        internal(fs::File::open(home).and_then(|directory| directory.sync_all()))?;
    }
    let completed = updated(
        state,
        &[
            ("appliedHash", field(pending, "after")?.clone()),
            ("appliedVersion", field(pending, "version")?.clone()),
            ("active", field(pending, "active")?.clone()),
            ("pending", IvyJson::Null),
        ],
    )?;
    save_state(&root.join("state.json"), &completed)?;
    *state = completed;
    Ok(())
}
fn apply_locked(
    home: PathBuf,
    authority: String,
    desired: ComposedInstructions,
) -> IvyResult<AppliedInstructions> {
    let root = home.join(".ivy-agent-instructions");
    let target = home.join("AGENTS.override.md");
    let state_path = root.join("state.json");
    let raw = regular_bytes(&state_path)?;
    let mut state = match raw {
        // An empty Buffer is truthy in JS, so empty JSON is still a parse error.
        Some(bytes) => Some(internal(parse(
            &String::from_utf8_lossy(&bytes),
            usize::MAX,
        ))?),
        None => None,
    };
    state = state.filter(truthy);
    require(
        state.as_ref().is_none_or(|state| {
            state.get("schemaVersion") == Some(&IvyJson::Number(1.0))
                && state.get("authority") == Some(&IvyJson::from(authority.as_str()))
        }),
        "Another Hive or host owns this home instruction output.",
    )?;
    if state
        .as_ref()
        .and_then(|s| s.get("pending"))
        .is_some_and(truthy)
    {
        finish_pending(&home, &root, state.as_mut().unwrap())?;
    }
    let current = regular_bytes(&target)?;
    if state.is_none() {
        if desired.body.is_none() {
            return Ok(AppliedInstructions::Unmanaged {
                desired_version: desired.version,
            });
        }
        let original_hash = save_blob(&root, current.as_deref())?;
        state = Some(IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("authority", authority.into()),
            ("originalHash", original_hash.clone()),
            ("appliedHash", original_hash),
            ("appliedVersion", IvyJson::Null),
            ("pending", IvyJson::Null),
            ("active", false.into()),
        ]));
    }
    let state = state.as_mut().unwrap();
    require(
        state.get("appliedHash") == Some(&bytes_hash(current.as_deref())),
        "AGENTS.override.md has user changes; Ivy will not overwrite or remove them.",
    )?;
    let after = if let Some(body) = &desired.body {
        save_blob(&root, Some(body))?
    } else {
        field(state, "originalHash")?.clone()
    };
    let version = IvyJson::from(desired.version.as_str());
    let active = IvyJson::Bool(desired.body.is_some());
    if state.get("appliedHash") != Some(&after)
        || state.get("appliedVersion") != Some(&version)
        || state.get("active") != Some(&active)
    {
        *state = updated(
            state,
            &[(
                "pending",
                IvyJson::object([
                    ("before", field(state, "appliedHash")?.clone()),
                    ("after", after),
                    ("version", version),
                    ("active", active),
                ]),
            )],
        )?;
        save_state(&state_path, state)?;
        finish_pending(&home, &root, state)?;
    }
    Ok(AppliedInstructions::Managed {
        home,
        desired_version: desired.version,
        applied_version: field(state, "appliedVersion")?.clone(),
        active: desired.body.is_some(),
        bytes: desired.body.as_ref().map_or(0, Vec::len),
    })
}

/// Filesystem application only. The source owns Hive reads/discovery/composition;
/// the writer owns the lock through re-read, pending intent, effects and commit.
pub async fn apply_instructions(
    home: PathBuf,
    host_id: &JsString,
    hive_url: &JsString,
    source: &dyn InstructionsSource,
    desired_observed: &(dyn Fn(&str) + Send + Sync),
) -> IvyResult<AppliedInstructions> {
    let home = internal(
        tokio::task::spawn_blocking(move || internal(ivy_host_runtime::paths::realpath(&home)))
            .await,
    )??;
    let root = home.join(".ivy-agent-instructions");
    let check = root.clone();
    let exists = internal(
        tokio::task::spawn_blocking(move || match fs::symlink_metadata(check) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(IvyError::internal()),
        })
        .await,
    )??;
    if !exists {
        let desired = source.desired(&home).await?;
        if desired.body.is_none() {
            return Ok(AppliedInstructions::Unmanaged {
                desired_version: desired.version,
            });
        }
    }
    let prepare = root.clone();
    internal(tokio::task::spawn_blocking(move || state_root(&prepare)).await)??;
    let lock = ExecutorLock::acquire(root, false).await?;
    let desired = source.desired(&home).await?;
    desired_observed(&desired.version);
    let authority = hash_json(&IvyJson::object([
        ("hive", IvyJson::String(hive_url.clone())),
        ("hostId", IvyJson::String(host_id.clone())),
    ]))?;
    internal(
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            apply_locked(home, authority, desired)
        })
        .await,
    )?
}
