//! The original Secretary directory policy over shared native path primitives.
use ivy_contracts::{IvyError, IvyResult};
use ivy_host_runtime::{node_path, paths};
use std::{fs, path::Path};

pub(crate) fn native(path: &Path) -> std::path::PathBuf {
    node_path::filesystem_path(&node_path::from_native(path))
}
pub(crate) fn same_path(a: &Path, b: &Path) -> IvyResult<bool> {
    let context = node_path::Context::current().map_err(|_| IvyError::internal())?;
    Ok(context
        .relative(&node_path::from_native(a), &node_path::from_native(b))
        .0
        .is_empty())
}
pub(crate) fn safe_directory(root: &Path) -> IvyResult<()> {
    let context = node_path::Context::current().map_err(|_| IvyError::internal())?;
    if !node_path::is_absolute(context.style, &node_path::from_native(root)) {
        return Err(IvyError::new(
            "path_unsafe",
            "A directory must be an explicit absolute path.",
        ));
    }
    let path = node_path::to_native(&context.resolve(&[node_path::from_native(root)]));
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    for (index, part) in ancestors.iter().enumerate() {
        let metadata = fs::symlink_metadata(native(part)).map_err(|_| IvyError::internal())?;
        let valid = metadata.is_dir()
            && (index == 0
                || (!metadata.file_type().is_symlink()
                    && same_path(
                        part,
                        &paths::realpath(part).map_err(|_| IvyError::internal())?,
                    )?));
        if !valid {
            return Err(IvyError::new(
                "path_unsafe",
                "The configured directory must contain no links.",
            ));
        }
    }
    Ok(())
}
