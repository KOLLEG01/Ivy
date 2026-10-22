//! Preparation copies: immutable source inputs and independent runtime payloads.
use crate::{IvyError, IvyResult, concurrency::map_concurrent, node_path::{self, Context, Style}, paths};
use ivy_contracts::JsString;
use std::{collections::BTreeSet, fs, path::{Path, PathBuf}, sync::Arc};

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> { value.map_err(|_| IvyError::internal()) }
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok { Ok(()) } else { Err(IvyError::new(code, message)) }
}
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> IvyResult<T> + Send + 'static) -> IvyResult<T> {
    internal(tokio::task::spawn_blocking(work).await)?
}
fn fs_path(path: &Path) -> PathBuf { node_path::filesystem_path(&node_path::from_native(path)) }
fn entries(path: &Path) -> IvyResult<Vec<(PathBuf, fs::FileType)>> {
    internal(fs::read_dir(fs_path(path)))?.map(|entry| {
        let entry = internal(entry)?;
        let name = PathBuf::from(entry.file_name());
        Ok((name, internal(entry.file_type())?))
    }).collect()
}
fn local_path(root: &Path, location: &JsString) -> PathBuf {
    let mut parts = vec![node_path::from_native(root)];
    parts.extend(location.0.split(|c| *c == 47).map(|part| JsString(part.to_vec())));
    node_path::to_native(&node_path::join(Style::native(), &parts))
}
fn prefix(value: &JsString, root: &JsString, suffix: &[u16]) -> bool {
    value.0.starts_with(&root.0) && value.0[root.0.len()..].starts_with(suffix)
}
fn selected_path(path: &JsString, selected: &BTreeSet<JsString>) -> bool {
    path.0.iter().copied().eq("node_modules".encode_utf16()) || selected.iter().any(|location|
        path == location || prefix(path, location, &[47]) || prefix(location, path, &[47]))
}
fn excluded(path: &JsString) -> bool {
    path.0.split(|c| *c == 47).any(|part| [".bin", ".cache", ".local"].iter().any(|name| part.iter().copied().eq(name.encode_utf16())))
        || path.0.ends_with(&[46, 109, 97, 112])
}
fn inside(root: &Path, path: &Path) -> IvyResult<bool> { internal(paths::inside(root, path)) }
fn actual(path: &Path) -> IvyResult<PathBuf> { internal(paths::realpath(path)) }
fn copy_file(from: &Path, to: &Path, current_unix_mode: bool) -> IvyResult<()> {
    internal(fs::copy(fs_path(from), fs_path(to)))?;
    #[cfg(unix)] if current_unix_mode { internal(fs::set_permissions(fs_path(to), internal(fs::metadata(fs_path(from)))?.permissions()))?; }
    #[cfg(not(unix))] let _ = current_unix_mode;
    Ok(())
}

/// Inventory before creating output, rejecting observed source links and special
/// entries. Every started worker drains before a failed copy returns.
pub async fn copy_source_tree(source: PathBuf, destination: PathBuf) -> IvyResult<()> {
    internal(tokio::spawn(copy_source(source, destination)).await)?
}
async fn copy_source(source: PathBuf, destination: PathBuf) -> IvyResult<()> {
    let mut directories = vec![PathBuf::new()];
    let mut files = Vec::new();
    let mut index = 0;
    while index < directories.len() {
        let end = (index + 8).min(directories.len());
        let batch = directories[index..end].to_vec(); index = end;
        let nested = map_concurrent(batch, 8, |local, _| {
            let source = source.clone();
            blocking(move || {
                let mut children = Vec::new(); let mut files = Vec::new();
                for (name, kind) in entries(&source.join(&local))? {
                    let child = local.join(name);
                    require(!kind.is_symlink(), "artifact_changed", "Fixed source snapshots cannot contain filesystem links.")?;
                    if kind.is_dir() { children.push(child); }
                    else {
                        require(kind.is_file(), "artifact_changed", "Fixed source snapshot contains an unsupported entry.")?;
                        files.push(child);
                    }
                }
                Ok((children, files))
            })
        }).await?;
        for (children, found) in nested { directories.extend(children); files.extend(found); }
    }
    map_concurrent(directories, 8, |local, _| {
        let path = destination.join(local);
        blocking(move || internal(fs::create_dir_all(fs_path(&path))))
    }).await?;
    map_concurrent(files, 8, |local, _| {
        let from = source.join(&local); let to = destination.join(local);
        blocking(move || copy_file(&from, &to, false))
    }).await?;
    Ok(())
}

#[derive(Clone)]
struct Directory { from: PathBuf, to: PathBuf, local: JsString }
fn owned_dependency(path: &Path, source: &Path, cache: &Path) -> IvyResult<bool> {
    Ok(inside(source, path)? || inside(cache, path)?)
}

/// The installed tree decides whether an optional platform package exists.
/// Cache/workspace links are materialized into ordinary independent files.
pub async fn copy_runtime_dependencies(source: PathBuf, artifact: PathBuf, selected: Vec<JsString>, cache: PathBuf) -> IvyResult<()> {
    internal(tokio::spawn(copy_dependencies(source, artifact, selected.into_iter().collect(), cache)).await)?
}
async fn copy_dependencies(source: PathBuf, artifact: PathBuf, selected: BTreeSet<JsString>, cache: PathBuf) -> IvyResult<()> {
    if selected.is_empty() { return Ok(()); }
    let selected = Arc::new(selected);
    let nested_marker: Vec<_> = "/node_modules/".encode_utf16().collect();
    let mut roots = Vec::new();
    for location in selected.iter() {
        if selected.iter().any(|parent| parent != location && prefix(location, parent, &nested_marker)) { continue; }
        let from = local_path(&source, location);
        let exists = blocking(move || match paths::realpath(&from) {
            Ok(_) => Ok(true), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false), Err(_) => Err(IvyError::internal()),
        }).await?;
        if exists { roots.push(location.clone()); }
    }
    let mut seen = BTreeSet::new();
    let parents = roots.iter().map(|location| node_path::to_native(&node_path::dirname(Style::native(), &node_path::from_native(&local_path(&artifact, location))))).filter(|parent| seen.insert(parent.clone())).collect();
    map_concurrent(parents, 8, |path, _| blocking(move || internal(fs::create_dir_all(fs_path(&path))))).await?;
    map_concurrent(roots, 8, |location, _| {
        let source = source.clone(); let artifact = artifact.clone(); let cache = cache.clone(); let selected = selected.clone();
        async move {
            let from = local_path(&source, &location); let to = local_path(&artifact, &location);
            require(inside(&source.join("node_modules"), &from)? && inside(&artifact.join("node_modules"), &to)?,
                "artifact_changed", "Runtime dependency leaves its package root.")?;
            let from = blocking(move || actual(&from)).await?;
            require(owned_dependency(&from, &source, &cache)?, "artifact_changed", "Dependency link leaves its build workspace or host-owned dependency cache.")?;
            let mut directories = vec![Directory { from, to, local: location }]; let mut files = Vec::new(); let mut index = 0;
            while index < directories.len() {
                let end = (index + 8).min(directories.len()); let batch = directories[index..end].to_vec(); index = end;
                let nested = map_concurrent(batch, 8, |directory, _| {
                    let source = source.clone(); let cache = cache.clone(); let selected = selected.clone();
                    blocking(move || {
                        internal(fs::create_dir_all(fs_path(&directory.to)))?;
                        let mut children = Vec::new(); let mut files = Vec::new();
                        for (name, kind) in entries(&directory.from)? {
                            let mut local = directory.local.0.clone(); local.push(47); local.extend(node_path::from_native(&name).0.into_iter().map(|c| if c == 92 { 47 } else { c }));
                            let local = JsString(local);
                            if excluded(&local) || !selected_path(&local, &selected) { continue; }
                            let raw = directory.from.join(&name); let to = directory.to.join(&name);
                            if kind.is_symlink() {
                                let from = actual(&raw)?;
                                require(owned_dependency(&from, &source, &cache)?, "artifact_changed", "Nested dependency link leaves its build workspace or host-owned dependency cache.")?;
                                if internal(fs::metadata(fs_path(&from)))?.is_dir() { children.push(Directory { from, to, local }); }
                                else { files.push((from, to)); }
                            } else if kind.is_dir() { children.push(Directory { from: raw, to, local }); }
                            else {
                                require(kind.is_file(), "artifact_changed", "Runtime dependency contains an unsupported entry.")?;
                                files.push((raw, to));
                            }
                        }
                        Ok((children, files))
                    })
                }).await?;
                for (children, found) in nested { directories.extend(children); files.extend(found); }
            }
            map_concurrent(files, 8, |(from, to), _| blocking(move || copy_file(&from, &to, true))).await?;
            Ok(())
        }
    }).await?;
    Ok(())
}

#[derive(Clone)]
enum OutputPolicy { Ui(PathBuf), Service(PathBuf) }
impl OutputPolicy {
    fn accepts(&self, path: &Path) -> IvyResult<bool> {
        match self {
            Self::Ui(root) => {
                require(inside(root, &actual(path)?)?, "artifact_changed", "UI bundle link leaves its selected output.")?;
                Ok(!node_path::from_native(path).0.ends_with(&[46, 109, 97, 112]))
            }
            Self::Service(root) => {
                let local = internal(Context::current())?.relative(&node_path::from_native(root), &node_path::from_native(path));
                let local = JsString(local.0.into_iter().map(|c| if c == 92 { 47 } else { c }).collect());
                if excluded(&local) || local.0.starts_with(&[116, 101, 115, 116, 115, 47]) { return Ok(false); }
                require(inside(root, &actual(path)?)?, "artifact_changed", "Dependency/artifact link leaves its build workspace or host-owned dependency cache.")?;
                Ok(true)
            }
        }
    }
}
// cp(...,{recursive:true,dereference:true}) within the already-created output
// root. Preserve source directory modes only for newly created descendants.
fn copy_output_entry(from: &Path, to: &Path, policy: &OutputPolicy) -> IvyResult<()> {
    if !policy.accepts(from)? { return Ok(()); }
    let metadata = internal(fs::metadata(fs_path(from)))?;
    if metadata.is_dir() {
        let created = match fs::metadata(fs_path(to)) {
            Ok(value) => { if !value.is_dir() { return Err(IvyError::internal()); } false }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => { internal(fs::create_dir(fs_path(to)))?; true }
            Err(_) => return Err(IvyError::internal()),
        };
        for (name, _) in entries(from)? { copy_output_entry(&from.join(&name), &to.join(name), policy)?; }
        if created { internal(fs::set_permissions(fs_path(to), metadata.permissions()))?; }
    } else {
        if !metadata.is_file() { return Err(IvyError::internal()); }
        // Node cp's default force replaces an existing file/link before copying.
        match fs::metadata(fs_path(to)) {
            Ok(value) => { if value.is_dir() { return Err(IvyError::internal()); } internal(fs::remove_file(fs_path(to)))?; }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(IvyError::internal()),
        }
        copy_file(from, to, false)?;
    }
    Ok(())
}
async fn copy_output_directory(from: PathBuf, to: PathBuf, policy: OutputPolicy) -> IvyResult<()> {
    let root = to.clone(); blocking(move || internal(fs::create_dir_all(fs_path(&root)))).await?;
    let from_entries = from.clone(); let names = blocking(move || entries(&from_entries)).await?;
    map_concurrent(names, 8, |(name, _), _| {
        let from = from.join(&name); let to = to.join(name); let policy = policy.clone();
        blocking(move || copy_output_entry(&from, &to, &policy))
    }).await?;
    Ok(())
}

pub async fn collect_service_files(source: PathBuf, artifact: PathBuf, selected: Vec<JsString>, cache: PathBuf) -> IvyResult<()> {
    internal(tokio::spawn(async move {
        for name in ["dist", "node_modules", "package.json", "package-lock.json"] {
            let from = source.join(name); let stat_path = from.clone();
            let metadata = blocking(move || match fs::metadata(fs_path(&stat_path)) {
                Ok(value) => Ok(Some(value)), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None), Err(_) => Err(IvyError::internal()),
            }).await?;
            let Some(metadata) = metadata else {
                require(name == "node_modules", "artifact_missing", "Build did not produce its executable output.")?; continue;
            };
            if name == "node_modules" { copy_dependencies(source.clone(), artifact.clone(), selected.iter().cloned().collect(), cache.clone()).await?; }
            else if metadata.is_dir() { copy_output_directory(from, artifact.join(name), OutputPolicy::Service(source.clone())).await?; }
            else { let to = artifact.join(name); let policy = OutputPolicy::Service(source.clone()); blocking(move || copy_output_entry(&from, &to, &policy)).await?; }
        }
        Ok(())
    }).await)?
}

pub async fn collect_app_bundle(source: PathBuf, artifact: PathBuf, dist: JsString) -> IvyResult<()> {
    internal(tokio::spawn(async move {
        let context = internal(Context::current())?;
        let from = node_path::to_native(&context.resolve(&[node_path::from_native(&source), dist.clone()]));
        let to = node_path::to_native(&context.resolve(&[node_path::from_native(&artifact), dist]));
        let source_dist = source.join("dist"); let actual_from = from.clone(); let destination = to.clone();
        blocking(move || {
            let metadata = internal(fs::metadata(fs_path(&actual_from)))?;
            require(metadata.is_dir() && inside(&source_dist, &actual(&actual_from)?)? && inside(&artifact, &destination)?,
                "artifact_missing", "Build did not produce the selected ui bundle.")
        }).await?;
        copy_output_directory(from.clone(), to, OutputPolicy::Ui(from)).await
    }).await)?
}
