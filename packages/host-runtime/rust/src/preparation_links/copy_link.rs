//! Native copies of observed links, retaining selected target spelling.
use crate::{filesystem, native_path, paths};
use std::{fs, io, path::Path};

fn invalid<T>() -> io::Result<T> {
    Err(io::ErrorKind::InvalidInput.into())
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| {
            if path.has_root() {
                path
            } else {
                Path::new(".")
            }
        })
}
fn identical(from: &Path, to: &Path) -> io::Result<bool> {
    let left = filesystem::lstat_identity(from)?;
    let right = filesystem::lstat_identity(to)?;
    Ok(left.dev != "0" && left.ino != "0" && left == right)
}
fn parent_paths(source: &Path, destination: &Path) -> io::Result<()> {
    let source_parent = paths::absolute(parent(source))?;
    let source_identity = filesystem::lstat_identity(source)?;
    let mut destination_parent = paths::absolute(parent(destination))?;
    while let Some(parent) = destination_parent.parent() {
        if native_path::within(&source_parent, &destination_parent)
            && native_path::within(&destination_parent, &source_parent)
        {
            return Ok(());
        }
        match filesystem::stat_identity(&destination_parent) {
            Ok(identity) => {
                if source_identity.dev != "0"
                    && source_identity.ino != "0"
                    && identity == source_identity
                {
                    return invalid();
                }
            }
            // A future descendant can still be reached through an existing
            // directory alias. Check that ancestor before creating parents.
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        destination_parent = parent.to_owned();
    }
    Ok(())
}
fn create(target: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let absolute = paths::absolute(&parent(destination).join(target))?;
        if fs::metadata(absolute).is_ok_and(|metadata| metadata.is_dir()) {
            std::os::windows::fs::symlink_dir(target, destination)
        } else {
            std::os::windows::fs::symlink_file(target, destination)
        }
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, destination)
    }
}
fn unlink(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if fs::symlink_metadata(path)?.file_attributes() & 0x10 != 0 {
            return fs::remove_dir(path);
        }
    }
    fs::remove_file(path)
}
pub(super) fn check_paths(
    source: &Path,
    destination: &Path,
    directory: bool,
    parents: bool,
) -> io::Result<Option<fs::Metadata>> {
    let destination_metadata = match fs::symlink_metadata(destination) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(other) = &destination_metadata {
        if identical(source, destination)? || directory != other.is_dir() {
            return invalid();
        }
    }
    if directory && paths::inside(source, destination)? {
        return invalid();
    }
    if parents {
        parent_paths(source, destination)?;
    }
    Ok(destination_metadata)
}

pub(super) fn copy(source: &Path, destination: &Path) -> io::Result<()> {
    copy_with_options(source, destination, true, true)
}
pub(super) fn copy_with_options(
    source: &Path,
    destination: &Path,
    verbatim: bool,
    parents: bool,
) -> io::Result<()> {
    let source_metadata = fs::symlink_metadata(source)?;
    let destination_metadata = check_paths(source, destination, false, parents)?;
    if !source_metadata.file_type().is_symlink() {
        return invalid();
    }
    match fs::metadata(parent(destination)) {
        Ok(_) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(parent(destination))?
        }
        Err(error) => return Err(error),
    }
    let target = fs::read_link(source)?;
    let target = if !verbatim && !target.is_absolute() {
        paths::absolute(&parent(source).join(target))?
    } else {
        target
    };
    if destination_metadata.is_none() {
        return create(&target, destination);
    }
    let destination_target = match fs::read_link(destination) {
        Ok(value) => value,
        Err(error)
            if error.kind() == io::ErrorKind::InvalidInput
                || cfg!(windows) && error.raw_os_error() == Some(4390) =>
        {
            return create(&target, destination);
        }
        Err(error) => return Err(error),
    };
    let destination_target = if destination_target.is_absolute() {
        destination_target
    } else {
        paths::absolute(&parent(destination).join(destination_target))?
    };
    if fs::metadata(source)?.is_dir()
        && (paths::inside(&target, &destination_target)?
            || paths::inside(&destination_target, &target)?)
    {
        return invalid();
    }
    unlink(destination)?;
    create(&target, destination)
}
