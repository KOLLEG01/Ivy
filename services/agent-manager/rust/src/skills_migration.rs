//! Explicit one-time skills import. Normal service startup never calls this.
use crate::{
    skills_files::{
        SkillsComparator, SkillsPaths, child, conflict, ensure_roots, fs_path, internal,
        read_state, regular_bytes, skills_paths, tree_hash,
    },
    skills_output::valid_name,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
    json::parse,
};
use ivy_host_runtime::{
    executor_lock::ExecutorLock,
    node_path::{self, Context, Style},
};
use std::{fs, io::Write, path::Path, sync::Arc};

pub struct SkillsImport {
    pub state_path: JsString,
    /// Exact-byte source state retained independently of the runtime state.
    pub source_backup: Option<JsString>,
    pub copied_skills: usize,
    pub unchanged: bool,
}
struct Mapping {
    source: JsString,
    name: JsString,
    hash: String,
}

fn exists(path: &JsString) -> IvyResult<bool> {
    match fs::symlink_metadata(fs_path(path)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(IvyError::internal()),
    }
}
fn new_bytes(path: &Path, bytes: &[u8]) -> IvyResult<()> {
    let mut options = fs::OpenOptions::new();
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
// Inputs have already passed tree_hash. Recheck entries while copying to
// refuse observed links, nonregular files and oversized files.
fn copy_tree(source: &Path, target: &Path) -> IvyResult<()> {
    internal(fs::create_dir(target))?;
    for entry in internal(fs::read_dir(source))? {
        let entry = internal(entry)?;
        let kind = internal(entry.file_type())?;
        if kind.is_symlink() {
            return Err(conflict(
                "Managed skills must not contain filesystem links.",
            ));
        }
        let source = entry.path();
        let target = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_file() {
            let bytes = regular_bytes(&source, 256 * 1024)?
                .ok_or_else(|| conflict("Managed skill file disappeared during inspection."))?;
            new_bytes(&target, &bytes)?;
        } else {
            return Err(conflict("Managed skills must contain regular files only."));
        }
    }
    Ok(())
}
fn convert(state: &IvyJson, authority: &str) -> IvyResult<(IvyJson, Vec<Mapping>)> {
    let version = match state.get("schemaVersion") {
        Some(IvyJson::Number(1.0)) => 1,
        Some(IvyJson::Number(2.0)) => 2,
        _ => {
            return Err(conflict(
                "Skills import requires version 1 or version 2 state.",
            ));
        }
    };
    if state.get("authority") != Some(&IvyJson::from(authority)) {
        return Err(conflict("Another Hive or host owns these managed skills."));
    }
    let before = state
        .get("applied")
        .and_then(IvyJson::as_object)
        .ok_or_else(|| conflict("Skills import requires an ownership object."))?;
    let mut mappings = Vec::new();
    for (source, value) in before {
        let name = if version == 1 {
            source
                .to_utf8()?
                .strip_prefix("ivy-managed-")
                .map(JsString::from)
                .ok_or_else(|| {
                    conflict("Managed skill state contains an invalid directory name.")
                })?
        } else {
            source.clone()
        };
        if !valid_name(&name) {
            return Err(conflict(
                "Managed skill state contains an invalid directory name.",
            ));
        }
        let hash = value
            .as_str()
            .map_err(|_| conflict("Skills import requires recorded tree hashes."))?;
        mappings.push(Mapping {
            source: source.clone(),
            name,
            hash,
        });
    }
    let mut converted = state.clone();
    converted.insert("schemaVersion", IvyJson::Number(2.0));
    converted.insert(
        "applied",
        IvyJson::object(
            mappings
                .iter()
                .map(|m| (m.name.clone(), m.hash.clone().into())),
        ),
    );
    Ok((converted, mappings))
}
fn import_locked(
    source_state: JsString,
    paths: SkillsPaths,
    authority: String,
    compare: Arc<SkillsComparator>,
) -> IvyResult<SkillsImport> {
    let parent = node_path::dirname(Style::native(), &source_state);
    let info = internal(fs::symlink_metadata(fs_path(&parent)))?;
    if !info.is_dir() || info.file_type().is_symlink() {
        return Err(conflict(
            "Skills import state must have a real parent directory.",
        ));
    }
    let bytes = regular_bytes(&fs_path(&source_state), 4 * 1024 * 1024)?
        .ok_or_else(|| IvyError::new("not_found", "Skills import state does not exist."))?;
    let original =
        parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| IvyError::internal())?;
    let (converted, mappings) = convert(&original, &authority)?;
    for mapping in &mappings {
        if tree_hash(
            &child(&paths.root, mapping.source.clone()),
            compare.as_ref(),
        )?
        .as_deref()
            != Some(mapping.hash.as_str())
        {
            return Err(conflict(
                "Skills import source has local changes or missing files.",
            ));
        }
    }
    let current = read_state(&paths.state_path)?;
    let in_place = source_state == paths.state_path;
    if current.as_ref().map(hash_json).transpose()?.as_ref() == Some(&hash_json(&converted)?) {
        for mapping in &mappings {
            if tree_hash(&child(&paths.root, mapping.name.clone()), compare.as_ref())?.as_deref()
                != Some(mapping.hash.as_str())
            {
                return Err(conflict(
                    "Previously imported skill has local changes or missing files.",
                ));
            }
        }
        return Ok(SkillsImport {
            state_path: paths.state_path,
            source_backup: None,
            copied_skills: 0,
            unchanged: true,
        });
    }
    if current.is_some() && !in_place {
        return Err(conflict("Skills import destination is already managed."));
    }
    for mapping in &mappings {
        if mapping.source != mapping.name && exists(&child(&paths.root, mapping.name.clone()))? {
            return Err(conflict(
                "Skills import destination already exists outside Ivy management.",
            ));
        }
    }
    let backup = child(
        &paths.state_root,
        format!("import-source-{}.json", &digest(&bytes)[7..]).into(),
    );
    if let Some(previous) = regular_bytes(&fs_path(&backup), 4 * 1024 * 1024)? {
        if previous != bytes {
            return Err(conflict("Skills import backup differs from its source."));
        }
    } else {
        new_bytes(&fs_path(&backup), &bytes)?;
    }
    let stage = child(
        &paths.state_root,
        format!(".import-{}", uuid::Uuid::new_v4()).into(),
    );
    internal(fs::create_dir(fs_path(&stage)))?;
    let mut created = Vec::new();
    let result = (|| {
        for mapping in mappings.iter().filter(|m| m.source != m.name) {
            let source = child(&paths.root, mapping.source.clone());
            let staged = child(&stage, mapping.name.clone());
            copy_tree(&fs_path(&source), &fs_path(&staged))?;
            if tree_hash(&staged, compare.as_ref())?.as_deref() != Some(mapping.hash.as_str())
                || tree_hash(&source, compare.as_ref())?.as_deref() != Some(mapping.hash.as_str())
            {
                return Err(conflict("Skills import source changed during copying."));
            }
        }
        // Every source and target has been checked before publishing any copy.
        for mapping in mappings.iter().filter(|m| m.source != m.name) {
            let target = child(&paths.root, mapping.name.clone());
            if exists(&target)? {
                return Err(conflict(
                    "Skills import destination appeared during copying.",
                ));
            }
            internal(fs::rename(
                fs_path(&child(&stage, mapping.name.clone())),
                fs_path(&target),
            ))?;
            created.push(target);
        }
        Ok(())
    })();
    if result.is_err() {
        for target in &created {
            internal(fs::remove_dir_all(fs_path(target)))?;
        }
    }
    internal(fs::remove_dir_all(fs_path(&stage)))?;
    result?;
    // An atomic write can fail after publishing its replacement. Keep copied
    // trees on that error: deleting them could invalidate published ownership.
    internal(ivy_store::atomic::atomic_json(
        &fs_path(&paths.state_path),
        &converted,
    ))?;
    Ok(SkillsImport {
        state_path: paths.state_path,
        source_backup: Some(backup),
        copied_skills: created.len(),
        unchanged: false,
    })
}

/// The operator explicitly selects the old state file, skills root and expected
/// authority with the old owner stopped. Version 1 prefixed directories are
/// copied to current names; version 2 only moves state. Source bytes and trees
/// remain available. This is not a crash-recovery protocol: an interrupted
/// import must be inspected before rerunning; the runtime does not recover it.
pub async fn import_skills(
    source_state: JsString,
    root: JsString,
    authority: String,
    compare: Arc<SkillsComparator>,
) -> IvyResult<SkillsImport> {
    let (source_state, paths) = internal(
        tokio::task::spawn_blocking(move || {
            let context = Context::current().map_err(|_| IvyError::internal())?;
            if !node_path::is_absolute(Style::native(), &source_state) {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "Skills import state path must be absolute.",
                ));
            }
            let paths = skills_paths(&root, &context)?;
            ensure_roots(&paths)?;
            Ok((context.resolve(&[source_state]), paths))
        })
        .await,
    )??;
    let lock = ExecutorLock::acquire(fs_path(&paths.state_root), false).await?;
    internal(
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            import_locked(source_state, paths, authority, compare)
        })
        .await,
    )?
}
