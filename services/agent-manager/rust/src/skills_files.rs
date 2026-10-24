//! Current-layout managed skills. Legacy import is an explicit separate operation.
use crate::{
    skills_output::inspect_ownership,
    skills_prepare::{PreparedSkills, prepare_skills},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
    json::parse,
};
use ivy_host_runtime::{
    executor_lock::ExecutorLock,
    filesystem,
    node_path::{self, Context, Style},
    paths,
};
use ivy_sdk::rpc::RpcClient;
use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncWriteExt;

/// Host display ordering for skill names; equal names retain discovery order.
pub type SkillsComparator = dyn Fn(&JsString, &JsString) -> Ordering + Send + Sync;
#[derive(Clone)]
pub struct SkillsPaths {
    pub root: JsString,
    pub state_root: JsString,
    pub state_path: JsString,
}
pub struct AppliedSkills {
    pub target: JsString,
    pub desired_version: String,
    pub applied: bool,
    pub managed_bytes: usize,
}
pub(crate) fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
pub(crate) fn conflict(message: &str) -> IvyError {
    IvyError::new("target_conflict", message)
}
pub(crate) fn child(root: &JsString, name: JsString) -> JsString {
    node_path::join(Style::native(), &[root.clone(), name])
}
pub(crate) fn fs_path(path: &JsString) -> PathBuf {
    node_path::filesystem_path(path)
}

pub fn skills_paths(root: &JsString, context: &Context) -> IvyResult<SkillsPaths> {
    if !node_path::is_absolute(Style::native(), root) {
        return Err(IvyError::new(
            "invalid_arguments",
            "Managed skills root must be absolute.",
        ));
    }
    let root = context.resolve(&[root.clone()]);
    let hash = digest(String::from_utf16_lossy(&root.0));
    let state_root = child(
        &node_path::dirname(Style::native(), &root),
        format!(".ivy-agent-skills-{}", &hash[7..23]).into(),
    );
    Ok(SkillsPaths {
        root,
        state_path: child(&state_root, "state.json".into()),
        state_root,
    })
}
pub(crate) fn regular_bytes(path: &Path, maximum: u64) -> IvyResult<Option<Vec<u8>>> {
    let info = match fs::symlink_metadata(path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IvyError::internal()),
    };
    if !info.is_file() || info.file_type().is_symlink() || info.len() > maximum {
        return Err(conflict("Managed target must be a bounded regular file."));
    }
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(IvyError::internal()),
    }
}
pub(crate) fn make_directory(path: &JsString) -> IvyResult<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    internal(builder.create(fs_path(path)))
}
fn real_directory(path: &JsString, message: &str) -> IvyResult<()> {
    let info = internal(fs::symlink_metadata(fs_path(path)))?;
    if !info.is_dir() || info.file_type().is_symlink() {
        return Err(conflict(message));
    }
    Ok(())
}
pub(crate) fn ensure_roots(paths: &SkillsPaths) -> IvyResult<()> {
    make_directory(&paths.root)?;
    real_directory(&paths.root, "Managed skills root must be a real directory.")?;
    make_directory(&paths.state_root)?;
    real_directory(
        &paths.state_root,
        "Managed skill state must be a real directory.",
    )
}
pub(crate) fn read_state(path: &JsString) -> IvyResult<Option<IvyJson>> {
    regular_bytes(&fs_path(path), 4 * 1024 * 1024)?
        .map(|bytes| {
            parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| IvyError::internal())
        })
        .transpose()
}
/// Hash relative paths and content digests in depth-first display order.
pub fn tree_hash(path: &JsString, compare: &SkillsComparator) -> IvyResult<Option<String>> {
    let path = fs_path(path);
    let info = match fs::symlink_metadata(&path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IvyError::internal()),
    };
    if !info.is_dir() || info.file_type().is_symlink() {
        return Err(conflict("Managed skill target must be a real directory."));
    }
    fn visit(
        directory: &Path,
        prefix: &JsString,
        compare: &SkillsComparator,
        entries: &mut Vec<IvyJson>,
    ) -> IvyResult<()> {
        let mut children = internal(fs::read_dir(directory))?
            .map(|entry| {
                internal(entry).map(|entry| (node_path::node_utf8(&entry.file_name()), entry))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        children.sort_by(|left, right| compare(&left.0, &right.0));
        for (name, entry) in children {
            let kind = internal(entry.file_type())?;
            if kind.is_symlink() {
                return Err(conflict(
                    "Managed skills must not contain filesystem links.",
                ));
            }
            let path = entry.path();
            let mut local = prefix.clone();
            if !local.0.is_empty() {
                local.0.push(47);
            }
            local.0.extend(name.0);
            if kind.is_dir() {
                visit(&path, &local, compare, entries)?;
            } else {
                if !kind.is_file() {
                    return Err(conflict("Managed skills must contain regular files only."));
                }
                let bytes = regular_bytes(&path, 256 * 1024)?
                    .ok_or_else(|| conflict("Managed skill file disappeared during inspection."))?;
                entries.push(IvyJson::Array(vec![
                    IvyJson::String(local),
                    digest(bytes).into(),
                ]));
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    visit(&path, &JsString::default(), compare, &mut entries)?;
    Ok(Some(hash_json(&IvyJson::Array(entries))?))
}
pub async fn resolved_paths(home: JsString, root: JsString) -> IvyResult<SkillsPaths> {
    internal(
        tokio::task::spawn_blocking(move || {
            let paths = skills_paths(
                &root,
                &Context::current().map_err(|_| IvyError::internal())?,
            )?;
            // Retain the original native-home existence/canonicalization boundary.
            internal(paths::realpath(&fs_path(&home)))?;
            Ok(paths)
        })
        .await,
    )?
}
async fn hash_tree(path: JsString, compare: Arc<SkillsComparator>) -> IvyResult<Option<String>> {
    internal(tokio::task::spawn_blocking(move || tree_hash(&path, compare.as_ref())).await)?
}
async fn apply_locked(
    paths: SkillsPaths,
    prepared: PreparedSkills,
    compare: Arc<SkillsComparator>,
) -> IvyResult<AppliedSkills> {
    let names = prepared
        .groups
        .iter()
        .map(|group| group.name.clone())
        .collect::<Vec<_>>();
    let ownership = internal(
        tokio::task::spawn_blocking({
            let paths = paths.clone();
            let compare = compare.clone();
            let authority = prepared.authority.clone();
            move || {
                let state = read_state(&paths.state_path)?;
                inspect_ownership(state.as_ref(), &authority, &names, |name| {
                    tree_hash(&child(&paths.root, name.clone()), compare.as_ref())
                })
            }
        })
        .await,
    )??;
    let stage = child(
        &paths.state_root,
        format!(".stage-{}", uuid::Uuid::new_v4()).into(),
    );
    internal(tokio::fs::create_dir(fs_path(&stage)).await)?;
    let applied = !prepared.groups.is_empty();
    let mut total = 0;
    let mutation = async {
        let mut after = Vec::new();
        for group in prepared.groups {
            let directory = child(&stage, group.name.clone());
            internal(tokio::fs::create_dir_all(fs_path(&directory)).await)?;
            for (local, bytes) in group.files {
                let path = child(&directory, local);
                let parent = node_path::dirname(Style::native(), &path);
                internal(tokio::fs::create_dir_all(fs_path(&parent)).await)?;
                let mut options = tokio::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    options.mode(0o600);
                }
                let mut file = internal(options.open(fs_path(&path)).await)?;
                total += bytes.len();
                internal(file.write_all(&bytes).await)?;
                // Tokio may have an in-flight blocking write after write_all.
                // Settle it before hashing or renaming the staged directory.
                internal(file.flush().await)?;
                drop(file);
            }
            let hash = hash_tree(directory, compare.clone())
                .await?
                .ok_or_else(IvyError::internal)?;
            after.push((group.name, hash));
        }
        let plan = internal(
            tokio::task::spawn_blocking({
                let paths = paths.clone();
                let compare = compare.clone();
                let version = prepared.desired_version.clone();
                move || {
                    ownership.plan(&after, &version, |name| {
                        tree_hash(&child(&paths.root, name.clone()), compare.as_ref())
                    })
                }
            })
            .await,
        )??;
        for replacement in plan.replacements {
            let staged = child(&stage, replacement.name.clone());
            if replacement.unchanged {
                filesystem::remove_force(fs_path(&staged)).await?;
                continue;
            }
            let target = child(&paths.root, replacement.name);
            let retired = child(
                &paths.state_root,
                format!(".retired-{}", uuid::Uuid::new_v4()).into(),
            );
            if replacement.owned {
                internal(tokio::fs::rename(fs_path(&target), fs_path(&retired)).await)?;
            }
            if tokio::fs::rename(fs_path(&staged), fs_path(&target))
                .await
                .is_err()
            {
                if replacement.owned {
                    internal(tokio::fs::rename(fs_path(&retired), fs_path(&target)).await)?;
                }
                return Err(IvyError::internal());
            }
            if replacement.owned {
                filesystem::remove_force(fs_path(&retired)).await?;
            }
        }
        Ok(plan.state)
    }
    .await;
    // This finally has the original error precedence, independently of AM01.
    filesystem::remove_force(fs_path(&stage)).await?;
    let state = mutation?;
    let path = fs_path(&paths.state_path);
    internal(
        tokio::task::spawn_blocking(move || {
            internal(ivy_store::atomic::atomic_json(&path, &state))
        })
        .await,
    )??;
    Ok(AppliedSkills {
        target: paths.root,
        desired_version: prepared.desired_version,
        applied,
        managed_bytes: total,
    })
}
pub async fn apply_skills(
    home: JsString,
    root: JsString,
    connection: Arc<dyn RpcClient>,
    defaults: IvyJson,
    host_id: JsString,
    hive_url: JsString,
    compare: Arc<SkillsComparator>,
) -> IvyResult<AppliedSkills> {
    let prepared = prepare_skills(connection.as_ref(), &defaults, &host_id, &hive_url).await?;
    let paths = resolved_paths(home, root).await?;
    internal(
        tokio::task::spawn_blocking({
            let paths = paths.clone();
            move || ensure_roots(&paths)
        })
        .await,
    )??;
    let lock = ExecutorLock::acquire(fs_path(&paths.state_root), false).await?;
    // This task owns mutation and the lock. Losing an awaiting caller cannot
    // release ownership while asynchronous filesystem operations are running.
    internal(
        tokio::spawn(async move {
            let _lock = lock;
            apply_locked(paths, prepared, compare).await
        })
        .await,
    )?
}
