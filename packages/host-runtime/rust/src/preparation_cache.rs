//! npm cache materialization under the caller's live dependency-cache lease.
//! The default-locale comparator is mandatory; this module supplies no fallback.
use crate::{
    IvyError, IvyJson, IvyResult,
    concurrency::map_concurrent_with_error,
    config_file::{self, FileError},
    config_write, filesystem,
    node_path::{self, Context, Style},
    preparation_links,
};
use ivy_contracts::{JsString, hash::hash_json};
use std::{
    cmp::Ordering,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

pub type WorkspaceCompare = Arc<dyn Fn(&JsString, &JsString) -> Ordering + Send + Sync>;
type Links = Vec<(JsString, JsString)>;
#[derive(Clone)]
pub struct PreparationCache {
    key: JsString,
    links: Links,
    compare: WorkspaceCompare,
}
#[derive(Debug)]
enum Error {
    Io(io::Error),
    Ivy(IvyError),
    Syntax,
}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<IvyError> for Error {
    fn from(error: IvyError) -> Self {
        Self::Ivy(error)
    }
}
impl From<preparation_links::Error> for Error {
    fn from(error: preparation_links::Error) -> Self {
        match error {
            preparation_links::Error::Io(error) => Self::Io(error),
            preparation_links::Error::Ivy(error) => Self::Ivy(error),
        }
    }
}
impl Error {
    fn ivy(self) -> IvyError {
        match self {
            Self::Ivy(error) => error,
            _ => IvyError::internal(),
        }
    }
    fn missing(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind()==io::ErrorKind::NotFound)
    }
}
type Result<T> = std::result::Result<T, Error>;
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Error::Ivy(IvyError::internal()))?
}
fn fs_path(path: &Path) -> PathBuf {
    node_path::filesystem_path(&node_path::from_native(path))
}
fn local(root: &Path, path: &JsString) -> PathBuf {
    let mut parts = vec![node_path::from_native(root)];
    parts.extend(
        path.0
            .split(|c| *c == 47)
            .map(|part| JsString(part.to_vec())),
    );
    node_path::to_native(&node_path::join(Style::native(), &parts))
}
fn parent(path: &Path) -> PathBuf {
    node_path::to_native(&node_path::dirname(
        Style::native(),
        &node_path::from_native(path),
    ))
}
fn map_links(links: Links) -> Links {
    let mut result: Links = Vec::new();
    for (location, target) in links {
        if let Some((_, prior)) = result.iter_mut().find(|(key, _)| key == &location) {
            *prior = target;
        } else {
            result.push((location, target));
        }
    }
    result
}
fn cross_device(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(17)
    }
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::EXDEV)
    }
}
fn occupied(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
    )
}
fn read_metadata(path: &Path) -> Result<IvyJson> {
    config_file::read_json(&fs_path(path), 4096).map_err(|error| match error {
        FileError::Missing => Error::Io(io::ErrorKind::NotFound.into()),
        FileError::Invalid(error) => Error::Ivy(error),
        FileError::Io(error) => Error::Io(error),
        FileError::Syntax => Error::Syntax,
    })
}

/// Original process.platform/process.arch/process.version inputs are explicit.
/// In particular the Node version keeps its leading `v` in this cache identity.
pub fn dependency_cache_key(
    lock_hash: JsString,
    package_manager: JsString,
    os: JsString,
    arch: JsString,
    node: JsString,
) -> IvyResult<String> {
    dependency_cache_key_json(lock_hash, IvyJson::String(package_manager), os, arch, node)
}
/// Preserve an admitted package.json value in identity after JS regex coercion.
pub fn dependency_cache_key_json(
    lock_hash: JsString,
    package_manager: IvyJson,
    os: JsString,
    arch: JsString,
    node: JsString,
) -> IvyResult<String> {
    hash_json(&IvyJson::object([
        ("cacheSchemaVersion", IvyJson::Number(3.0)),
        ("lockHash", IvyJson::String(lock_hash)),
        ("os", IvyJson::String(os)),
        ("arch", IvyJson::String(arch)),
        ("node", IvyJson::String(node)),
        ("packageManager", package_manager),
    ]))
}
impl PreparationCache {
    pub fn new(key: JsString, links: Links, compare: WorkspaceCompare) -> Self {
        Self {
            key,
            links: map_links(links),
            compare,
        }
    }
    pub fn workspace_entries(&self) -> IvyJson {
        let mut links = self.links.clone();
        links.sort_by(|(left, _), (right, _)| (self.compare)(left, right));
        IvyJson::Array(
            links
                .into_iter()
                .map(|(location, target)| {
                    IvyJson::object([
                        ("location", IvyJson::String(location)),
                        ("target", IvyJson::String(target)),
                    ])
                })
                .collect(),
        )
    }
    pub fn valid_metadata(&self, metadata: &IvyJson) -> IvyResult<bool> {
        if metadata.as_object().is_none()
            || metadata.get("schemaVersion") != Some(&IvyJson::Number(3.0))
            || metadata.get("key") != Some(&IvyJson::String(self.key.clone()))
            || metadata.get("externalOnly") != Some(&IvyJson::Bool(true))
        {
            return Ok(false);
        }
        let Some(links) = metadata
            .get("workspaceLinks")
            .filter(|links| links.as_array().is_some())
        else {
            return Ok(false);
        };
        if !links.as_array().unwrap().iter().all(|link| {
            link.get("location")
                .is_some_and(|value| matches!(value, IvyJson::String(_)))
                && link
                    .get("target")
                    .is_some_and(|value| matches!(value, IvyJson::String(_)))
        }) {
            return Ok(false);
        }
        Ok(hash_json(links)? == hash_json(&self.workspace_entries())?)
    }
    /// Retain the entire admitted restoration after its waiter is cancelled.
    pub async fn restore(&self, source: PathBuf, cache: PathBuf) -> IvyResult<bool> {
        let identity = self.clone();
        tokio::spawn(async move {
            let result = async {
                let path = cache.join("cache.json");
                let metadata = blocking(move || read_metadata(&path)).await?;
                if !identity.valid_metadata(&metadata)? {
                    return Err(Error::Ivy(IvyError::new(
                        "build_mismatch",
                        "Dependency cache identity differs from the current lock/runtime key.",
                    )));
                }
                let modules = cache.join("node_modules");
                let check = modules.clone();
                blocking(move || {
                    fs::metadata(fs_path(&check))?;
                    Ok(())
                })
                .await?;
                preparation_links::link_dependency_directory_raw(
                    modules,
                    source.join("node_modules"),
                )
                .await?;
                preparation_links::restore_workspace_links_raw(source, identity.links).await?;
                Ok(true)
            }
            .await;
            match result {
                Err(Error::Ivy(error)) if error.code == "build_mismatch" => Ok(false),
                Err(error) if error.missing() => Ok(false),
                Err(error) => Err(error.ivy()),
                Ok(value) => Ok(value),
            }
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    /// Publish only external packages, then relink this build to that cache and
    /// restore its current workspace entries in the original Map order.
    pub async fn retain(&self, source: PathBuf, cache: PathBuf) -> IvyResult<()> {
        let identity = self.clone();
        tokio::spawn(async move { identity.retain_raw(source, cache).await.map_err(Error::ivy) })
            .await
            .map_err(|_| IvyError::internal())?
    }
    async fn retain_raw(&self, source: PathBuf, cache: PathBuf) -> Result<()> {
        let modules = source.join("node_modules");
        let check = modules.clone();
        match blocking(move || {
            fs::metadata(fs_path(&check))?;
            Ok(())
        })
        .await
        {
            Ok(()) => (),
            Err(error) if error.missing() => return Ok(()),
            Err(error) => return Err(error),
        }
        let path = cache.join("cache.json");
        let existing = async {
            let metadata = blocking(move || read_metadata(&path)).await?;
            self.valid_metadata(&metadata).map_err(Error::from)
        }
        .await;
        match existing {
            Ok(true) => return Ok(()),
            Ok(false) => (),
            Err(Error::Ivy(_)) => (),
            Err(error) if error.missing() => (),
            Err(error) => return Err(error),
        }
        let directory = parent(&cache);
        blocking(move || {
            fs::create_dir_all(fs_path(&directory))?;
            Ok(())
        })
        .await?;
        let mut temporary = cache.as_os_str().to_os_string();
        temporary.push(format!("-{}.tmp", uuid::Uuid::new_v4()));
        let temporary = PathBuf::from(temporary);
        let path = temporary.clone();
        blocking(move || {
            fs::create_dir_all(fs_path(&path))?;
            Ok(())
        })
        .await?;
        let result = async {
            let moved = async {
                let from = modules.clone();
                let to = temporary.join("node_modules");
                blocking(move || {
                    filesystem::rename_raw(&from, &to)?;
                    Ok(())
                })
                .await?;
                for (location, _) in &self.links {
                    filesystem::remove_force_raw(local(&temporary, location)).await?;
                }
                Ok::<(), Error>(())
            }
            .await;
            match moved {
                Ok(()) => (),
                Err(Error::Io(error)) if cross_device(&error) => {
                    copy_external(
                        source.clone(),
                        temporary.join("node_modules"),
                        self.links.clone(),
                    )
                    .await?
                }
                Err(error) => return Err(error),
            }
            config_write::atomic_json(
                fs_path(&temporary.join("cache.json")),
                IvyJson::object([
                    ("schemaVersion", IvyJson::Number(3.0)),
                    ("key", IvyJson::String(self.key.clone())),
                    ("externalOnly", true.into()),
                    ("workspaceLinks", self.workspace_entries()),
                ]),
            )
            .await?;
            let from = temporary.clone();
            let to = cache.clone();
            match blocking(move || {
                filesystem::rename_raw(&from, &to)?;
                Ok(())
            })
            .await
            {
                Ok(()) => (),
                Err(Error::Io(error)) if occupied(&error) => {
                    let path = cache.join("cache.json");
                    let current = blocking(move || read_metadata(&path))
                        .await
                        .unwrap_or(IvyJson::Null);
                    if self.valid_metadata(&current)? {
                        filesystem::remove_force_raw(temporary.clone()).await?;
                        return Ok(());
                    }
                    filesystem::remove_force_raw(cache.clone()).await?;
                    let from = temporary.clone();
                    let to = cache.clone();
                    blocking(move || {
                        filesystem::rename_raw(&from, &to)?;
                        Ok(())
                    })
                    .await?;
                }
                Err(error) => return Err(error),
            }
            filesystem::remove_force_raw(modules.clone()).await?;
            preparation_links::link_dependency_directory_raw(cache.join("node_modules"), modules)
                .await?;
            preparation_links::restore_workspace_links_raw(source, self.links.clone()).await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = filesystem::remove_force_raw(temporary).await;
        }
        result
    }
}

fn workspace_path(path: &JsString, links: &Links) -> bool {
    links.iter().any(|(location, _)| {
        path == location
            || path.0.starts_with(&location.0) && path.0.get(location.0.len()) == Some(&47)
    })
}
async fn copy_external(source: PathBuf, destination: PathBuf, links: Links) -> Result<()> {
    let root = destination.clone();
    blocking(move || {
        fs::create_dir_all(fs_path(&root))?;
        Ok(())
    })
    .await?;
    let scan = source.join("node_modules");
    let names = blocking(move || {
        fs::read_dir(fs_path(&scan))?
            .map(|entry| {
                Ok(node_path::to_native(&node_path::node_utf8(
                    &entry?.file_name(),
                )))
            })
            .collect::<Result<Vec<_>>>()
    })
    .await?;
    map_concurrent_with_error(names, 8, move |name, _| {
        let from = source.join("node_modules").join(&name);
        let to = destination.join(name);
        let source = source.clone();
        let links = links.clone();
        blocking(move || {
            crate::preparation_copy::entry(&from, &to, true, true, &|path| {
                let local = Context::current()?.relative(
                    &node_path::from_native(&source),
                    &node_path::from_native(path),
                );
                let local = JsString(
                    local
                        .0
                        .into_iter()
                        .map(|c| if c == 92 { 47 } else { c })
                        .collect(),
                );
                Ok(!workspace_path(&local, &links))
            })?;
            Ok(())
        })
    })
    .await?;
    Ok(())
}
/// The actual copy phase of the EXDEV path. This keeps source bytes and verbatim
/// links; invoking it directly does not qualify an actual cross-device rename.
pub async fn copy_external_dependencies(
    source: PathBuf,
    destination: PathBuf,
    links: Links,
) -> IvyResult<()> {
    tokio::spawn(copy_external(source, destination, map_links(links)))
        .await
        .map_err(|_| IvyError::internal())?
        .map_err(Error::ivy)
}
