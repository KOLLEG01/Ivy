//! Non-dereferencing copies used by cache and candidate preparation.
use crate::preparation_links;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub(crate) fn entry(
    source: &Path,
    destination: &Path,
    verbatim_symlinks: bool,
    parents: bool,
    filter: &impl Fn(&Path) -> io::Result<bool>,
) -> io::Result<()> {
    if !filter(source)? {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(source)?;
    let prior =
        preparation_links::check_copy_paths(source, destination, metadata.is_dir(), parents)?;
    if metadata.file_type().is_symlink() {
        return preparation_links::copy_link_entry(source, destination, verbatim_symlinks, parents);
    }
    if parents {
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        match fs::metadata(parent) {
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(parent)?;
            }
            Err(error) => return Err(error),
        }
    }
    if metadata.is_dir() {
        let created = prior.is_none();
        if created {
            fs::create_dir(destination)?;
        }
        for item in fs::read_dir(source)? {
            let name = PathBuf::from(item?.file_name());
            entry(
                &source.join(&name),
                &destination.join(name),
                verbatim_symlinks,
                false,
                filter,
            )?;
        }
        if created {
            fs::set_permissions(destination, metadata.permissions())?;
        }
    } else if metadata.is_file() {
        if prior.is_some() {
            fs::remove_file(destination)?;
        }
        fs::copy(source, destination)?;
    } else {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(())
}
