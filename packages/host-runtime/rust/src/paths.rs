//! Native path operations shared by host configuration, launch and recovery.
use crate::{IvyError, IvyResult, native_path};
use std::{
    io,
    path::{Path, PathBuf},
};

/// Retain the OS canonical spelling, including verbatim Windows paths.
pub fn realpath(path: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

pub fn absolute(path: &Path) -> io::Result<PathBuf> {
    std::path::absolute(path).map(|path| native_path::lexical(&path))
}

/// Lexical component containment, using Windows ordinal name comparison on
/// Windows. Resolve links separately at an effect boundary.
pub fn inside(root: &Path, path: &Path) -> io::Result<bool> {
    Ok(native_path::within(&absolute(root)?, &absolute(path)?))
}

/// Existing native payload admission, shared with helper selection.
pub(crate) fn direct_native_file(root: &Path, relative: &str) -> IvyResult<PathBuf> {
    let missing = || {
        IvyError::new(
            "artifact_missing",
            "Native launch needs its complete contained runtime payload.",
        )
    };
    let mut local = root.to_owned();
    let parts = relative.split('/').collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        local.push(part);
        let metadata = std::fs::symlink_metadata(&local).map_err(|_| missing())?;
        if metadata.file_type().is_symlink()
            || !if index + 1 == parts.len() {
                metadata.is_file() && metadata.len() > 0
            } else {
                metadata.is_dir()
            }
        {
            return Err(IvyError::new(
                "artifact_missing",
                "Native launch needs direct regular payload files and directories.",
            ));
        }
    }
    if !inside(root, &realpath(&local).map_err(|_| missing())?).map_err(|_| missing())? {
        return Err(IvyError::new(
            "artifact_missing",
            "Native runtime payload leaves its candidate root.",
        ));
    }
    Ok(local)
}

#[derive(Debug)]
struct NoAncestor;
impl std::fmt::Display for NoAncestor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Storage path has no existing ancestor.")
    }
}
impl std::error::Error for NoAncestor {}
pub fn future_path(path: &ivy_contracts::JsString) -> IvyResult<ivy_contracts::JsString> {
    future_native_path(&native_path::to_native(path)).map(|path| native_path::from_native(&path))
}
pub(crate) fn future_native_path(path: &Path) -> IvyResult<PathBuf> {
    resolved_future_path(path).map_err(|error| {
        if error
            .get_ref()
            .is_some_and(|error| error.is::<NoAncestor>())
        {
            IvyError::new("target_conflict", "Storage path has no existing ancestor.")
        } else {
            IvyError::internal()
        }
    })
}

/// Resolve only the existing ancestor; future descendants are never created by
/// a path check. Errors other than not-found must not authorize a new location.
pub fn resolved_future_path(path: &Path) -> io::Result<PathBuf> {
    let mut ancestor = std::path::absolute(path)?;
    let mut suffix = Vec::new();
    loop {
        match realpath(&ancestor) {
            Ok(mut actual) => {
                // Windows can report a missing path for a child of a regular
                // file. Such an ancestor cannot admit future descendants.
                if !suffix.is_empty() && !std::fs::metadata(&actual)?.is_dir() {
                    return Err(io::ErrorKind::NotADirectory.into());
                }
                for part in suffix.into_iter().rev() {
                    actual.push(part);
                }
                return Ok(native_path::lexical(&actual));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = match ancestor.components().next_back() {
                    Some(
                        part @ (std::path::Component::Normal(_) | std::path::Component::ParentDir),
                    ) => part.as_os_str().to_owned(),
                    _ => return Err(io::Error::other(NoAncestor)),
                };
                suffix.push(name);
                if !ancestor.pop() {
                    return Err(io::Error::other(NoAncestor));
                }
            }
            Err(error) => return Err(error),
        }
    }
}
