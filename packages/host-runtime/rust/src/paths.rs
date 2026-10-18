//! Native path operations shared by host configuration, launch and recovery.
use std::{
    io,
    path::{Component, Path, PathBuf},
};

/// Node's realpath returns ordinary DOS/UNC paths on Windows. Keep native UTF-16
/// while removing only the extended prefix introduced by std::fs::canonicalize.
pub fn realpath(path: &Path) -> io::Result<PathBuf> {
    let resolved = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units: Vec<u16> = resolved.as_os_str().encode_wide().collect();
        let extended: Vec<u16> = "\\\\?\\".encode_utf16().collect();
        let unc: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
        if units.starts_with(&unc) {
            let mut ordinary: Vec<u16> = "\\\\".encode_utf16().collect();
            ordinary.extend_from_slice(&units[unc.len()..]);
            return Ok(std::ffi::OsString::from_wide(&ordinary).into());
        }
        if units.starts_with(&extended) && units.get(5) == Some(&(b':' as u16)) {
            return Ok(std::ffi::OsString::from_wide(&units[extended.len()..]).into());
        }
    }
    Ok(resolved)
}

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
