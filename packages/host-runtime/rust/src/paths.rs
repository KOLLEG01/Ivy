//! Native path operations shared by host configuration, launch and recovery.
use std::{
    io,
    path::{Component, Path, PathBuf},
};

pub fn absolute(path: &Path) -> io::Result<PathBuf> {
    let path = std::path::absolute(path)?;
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    Ok(result)
}

/// Lexical containment, with the same case-insensitive Windows comparison used
/// by node:path.relative. Resolve links separately at an effect boundary.
pub fn inside(root: &Path, path: &Path) -> io::Result<bool> {
    let root = absolute(root)?;
    let path = absolute(path)?;
    #[cfg(windows)]
    {
        let root = root.to_string_lossy().to_lowercase();
        let path = path.to_string_lossy().to_lowercase();
        Ok(Path::new(&path).starts_with(Path::new(&root)))
    }
    #[cfg(not(windows))]
    {
        Ok(path.starts_with(root))
    }
}

/// Resolve only the existing ancestor; future descendants are never created by
/// a path check. Errors other than not-found must not authorize a new location.
pub fn resolved_future_path(path: &Path) -> io::Result<PathBuf> {
    let mut ancestor = absolute(path)?;
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(&ancestor) {
            Ok(mut actual) => {
                for member in suffix.iter().rev() {
                    actual.push(member);
                }
                return Ok(actual);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or(error)?.to_os_string();
                suffix.push(name);
                if !ancestor.pop() {
                    return Err(io::Error::other("Storage path has no existing ancestor."));
                }
            }
            Err(error) => return Err(error),
        }
    }
}
