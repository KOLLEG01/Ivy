//! The original Secretary directory policy over shared native path primitives.
use ivy_contracts::{IvyError, IvyResult};
use std::{fs, path::Path};
pub(crate) fn safe_directory(root: &Path) -> IvyResult<()> {
    if !root.is_absolute() {
        return Err(IvyError::new(
            "path_unsafe",
            "A directory must be an explicit absolute path.",
        ));
    }
    let path = std::path::absolute(root).map_err(|_| IvyError::internal())?;
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    for (index, part) in ancestors.iter().enumerate() {
        let metadata = fs::symlink_metadata(part).map_err(|_| IvyError::internal())?;
        let direct = !metadata.file_type().is_symlink();
        #[cfg(windows)]
        let direct = {
            use std::os::windows::fs::MetadataExt;
            direct && metadata.file_attributes() & 0x400 == 0
        };
        let valid = metadata.is_dir() && (index == 0 || direct);
        if !valid {
            return Err(IvyError::new(
                "path_unsafe",
                "The configured directory must contain no links.",
            ));
        }
    }
    Ok(())
}
