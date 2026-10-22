//! Non-dereferencing copies used by cache and candidate preparation.
use crate::{node_path, preparation_links};
use std::{fs, io, path::{Path, PathBuf}};

fn fs_path(path: &Path) -> std::path::PathBuf {
    node_path::filesystem_path(&node_path::from_native(path))
}

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
    let metadata = fs::symlink_metadata(fs_path(source))?;
    let prior =
        preparation_links::check_copy_paths(source, destination, metadata.is_dir(), parents)?;
    if metadata.file_type().is_symlink() {
        return preparation_links::copy_link_entry(source, destination, verbatim_symlinks, parents);
    }
    if parents {
        let parent = node_path::dirname(
            node_path::Style::native(),
            &node_path::from_native(destination),
        );
        match fs::metadata(node_path::filesystem_path(&parent)) {
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(node_path::filesystem_path(&parent))?;
            }
            Err(error) => return Err(error),
        }
    }
    if metadata.is_dir() {
        let created = prior.is_none();
        if created {
            fs::create_dir(fs_path(destination))?;
        }
        for item in fs::read_dir(fs_path(source))? {
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
            fs::set_permissions(fs_path(destination), metadata.permissions())?;
        }
    } else if metadata.is_file() {
        if prior.is_some() {
            fs::remove_file(fs_path(destination))?;
        }
        fs::copy(fs_path(source), fs_path(destination))?;
    } else {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(())
}
