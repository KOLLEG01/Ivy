//! Native path operations shared by host configuration, launch and recovery.
use crate::{IvyError, IvyResult, node_path::{self, Context, Style}};
use std::{io, path::{Path, PathBuf}};

/// Node's realpath returns ordinary DOS/UNC paths through its UTF-8 filesystem
/// boundary. Lexical paths and direct native arguments have separate conversion.
pub fn realpath(path: &Path) -> io::Result<PathBuf> {
    let resolved = std::fs::canonicalize(node_path::filesystem_path(&node_path::from_native(path)))?;
    Ok(node_filesystem_result(resolved))
}

/// Decode a returned filesystem path, including Windows DOS/UNC spellings.
pub(crate) fn node_filesystem_result(resolved: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units: Vec<u16> = resolved.as_os_str().encode_wide().collect();
        let extended: Vec<u16> = "\\\\?\\".encode_utf16().collect();
        let unc: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
        if units.starts_with(&unc) {
            let mut ordinary: Vec<u16> = "\\\\".encode_utf16().collect();
            ordinary.extend_from_slice(&units[unc.len()..]);
            return node_path::to_native(&node_path::node_utf8(&std::ffi::OsString::from_wide(&ordinary)));
        }
        if units.starts_with(&extended) && units.get(5) == Some(&(b':' as u16)) {
            return node_path::to_native(&node_path::node_utf8(&std::ffi::OsString::from_wide(&units[extended.len()..])));
        }
    }
    node_path::to_native(&node_path::node_utf8(resolved.as_os_str()))
}

pub fn absolute(path: &Path) -> io::Result<PathBuf> {
    Ok(node_path::to_native(&Context::current()?.resolve(&[node_path::from_native(path)])))
}

/// Lexical containment, with the same case-insensitive Windows comparison used
/// by node:path.relative. Resolve links separately at an effect boundary.
pub fn inside(root: &Path, path: &Path) -> io::Result<bool> {
    Ok(Context::current()?.inside(&node_path::from_native(root), &node_path::from_native(path)))
}

/// Existing native payload admission, shared with helper selection.
pub(crate) fn direct_native_file(root:&Path,relative:&str)->IvyResult<PathBuf>{
    let missing=||IvyError::new("artifact_missing","Native launch needs its complete contained runtime payload.");
    let mut local=root.to_owned();let parts=relative.split('/').collect::<Vec<_>>();
    for(index,part)in parts.iter().enumerate(){
        local.push(part);let metadata=std::fs::symlink_metadata(&local).map_err(|_|missing())?;
        if metadata.file_type().is_symlink()||!if index+1==parts.len(){metadata.is_file()&&metadata.len()>0}else{metadata.is_dir()} {return Err(IvyError::new("artifact_missing","Native launch needs direct regular payload files and directories."));}
    }
    if !inside(root,&realpath(&local).map_err(|_|missing())?).map_err(|_|missing())? {return Err(IvyError::new("artifact_missing","Native runtime payload leaves its candidate root."));}Ok(local)
}

#[derive(Debug)]
struct NoAncestor;
impl std::fmt::Display for NoAncestor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("Storage path has no existing ancestor.") }
}
impl std::error::Error for NoAncestor {}
pub fn future_path(path: &ivy_contracts::JsString) -> IvyResult<ivy_contracts::JsString> {
    future(path).map_err(|error| {
        if error.get_ref().is_some_and(|error| error.is::<NoAncestor>()) {
            IvyError::new("target_conflict", "Storage path has no existing ancestor.")
        } else { IvyError::internal() }
    })
}

/// Resolve only the existing ancestor; future descendants are never created by
/// a path check. Errors other than not-found must not authorize a new location.
pub fn resolved_future_path(path: &Path) -> io::Result<PathBuf> {
    future(&node_path::from_native(path)).map(|path| node_path::to_native(&path))
}
fn future(path: &ivy_contracts::JsString) -> io::Result<ivy_contracts::JsString> {
    let mut ancestor = Context::current()?.resolve(std::slice::from_ref(path));
    let mut suffix = Vec::new();
    loop {
        match realpath(&node_path::to_native(&ancestor)) {
            Ok(actual) => {
                let mut parts = vec![node_path::from_native(&actual)];
                parts.extend(suffix.into_iter().rev());
                return Ok(node_path::join(Style::native(), &parts));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = node_path::dirname(Style::native(), &ancestor);
                if parent == ancestor { return Err(io::Error::other(NoAncestor)); }
                suffix.push(node_path::basename(Style::native(), &ancestor));
                ancestor = parent;
            }
            Err(error) => return Err(error),
        }
    }
}
