//! The observed-link branch of Node cp(...,{verbatimSymlinks:true}).
use crate::{filesystem, node_path::{self,Context,Style}, paths};
use ivy_contracts::JsString;
use std::{fs,io,path::{Path,PathBuf}};

fn fs_path(path:&Path)->PathBuf { node_path::filesystem_path(&node_path::from_native(path)) }
fn invalid<T>()->io::Result<T> { Err(io::ErrorKind::InvalidInput.into()) }
fn read_link(path:&Path)->io::Result<PathBuf> { fs::read_link(fs_path(path)).map(paths::node_filesystem_result) }
fn identical(from:&Path,to:&Path)->io::Result<bool> {
    let left=filesystem::lstat_identity(from)?;let right=filesystem::lstat_identity(to)?;
    Ok(left.dev!="0"&&left.ino!="0"&&left==right)
}
fn subdirectory(context:&Context,source:&JsString,destination:&JsString)->bool {
    let source=context.resolve(std::slice::from_ref(source));let destination=context.resolve(std::slice::from_ref(destination));
    let separator=if context.style==Style::Windows {92} else {47};
    let mut destination=destination.0.split(|c|*c==separator).filter(|part|!part.is_empty());
    source.0.split(|c|*c==separator).filter(|part|!part.is_empty()).all(|part|destination.next()==Some(part))
}
fn parent_paths(context:&Context,source:&Path,destination:&Path)->io::Result<()> {
    let source_parent=context.resolve(&[node_path::dirname(context.style,&node_path::from_native(source))]);
    let source_identity=filesystem::lstat_identity(source)?;
    let mut destination_parent=context.resolve(&[node_path::dirname(context.style,&node_path::from_native(destination))]);
    loop {
        let parent=node_path::dirname(context.style,&destination_parent);
        if destination_parent==source_parent||parent==destination_parent { return Ok(()); }
        let identity=match filesystem::stat_identity(&node_path::to_native(&destination_parent)) {
            Ok(value)=>value,Err(error) if error.kind()==io::ErrorKind::NotFound=>return Ok(()),Err(error)=>return Err(error),
        };
        if source_identity.dev!="0"&&source_identity.ino!="0"&&identity==source_identity { return invalid(); }
        destination_parent=parent;
    }
}
fn create(target:&Path,destination:&Path)->io::Result<()> {
    #[cfg(windows)] {
        let context=Context::current()?;
        let absolute=context.resolve(&[node_path::from_native(destination),"..".into(),node_path::from_native(target)]);
        if fs::metadata(node_path::filesystem_path(&absolute)).is_ok_and(|metadata|metadata.is_dir()) {
            std::os::windows::fs::symlink_dir(fs_path(target),fs_path(destination))
        } else { std::os::windows::fs::symlink_file(fs_path(target),fs_path(destination)) }
    }
    #[cfg(unix)] { std::os::unix::fs::symlink(fs_path(target),fs_path(destination)) }
}
fn unlink(path:&Path)->io::Result<()> {
    #[cfg(windows)] {
        use std::os::windows::fs::MetadataExt;
        if fs::symlink_metadata(fs_path(path))?.file_attributes()&0x10!=0 { return fs::remove_dir(fs_path(path)); }
    }
    fs::remove_file(fs_path(path))
}

pub(super) fn check_paths(source:&Path,destination:&Path,directory:bool,parents:bool)->io::Result<Option<fs::Metadata>> {
    let context=Context::current()?;
    let destination_metadata=match fs::symlink_metadata(fs_path(destination)) {
        Ok(value)=>Some(value),Err(error) if error.kind()==io::ErrorKind::NotFound=>None,Err(error)=>return Err(error),
    };
    if let Some(other)=&destination_metadata {
        if identical(source,destination)?||directory!=other.is_dir() {return invalid()}
    }
    if directory&&subdirectory(&context,&node_path::from_native(source),&node_path::from_native(destination)){return invalid()}
    if parents {parent_paths(&context,source,destination)?;}
    Ok(destination_metadata)
}

pub(super) fn copy(source:&Path,destination:&Path)->io::Result<()> {
    let context=Context::current()?;
    let source_metadata=fs::symlink_metadata(fs_path(source))?;
    let destination_metadata=check_paths(source,destination,false,true)?;
    if !source_metadata.file_type().is_symlink() { return invalid(); }
    let parent=node_path::dirname(context.style,&node_path::from_native(destination));
    match fs::metadata(node_path::filesystem_path(&parent)) {
        Ok(_)=>(),Err(error) if error.kind()==io::ErrorKind::NotFound=>fs::create_dir_all(node_path::filesystem_path(&parent))?,Err(error)=>return Err(error),
    }
    let target=read_link(source)?;
    if destination_metadata.is_none() { return create(&target,destination); }
    let destination_target=match read_link(destination) {
        Ok(value)=>value,
        Err(error) if error.kind()==io::ErrorKind::InvalidInput||cfg!(windows)&&error.raw_os_error()==Some(4390)=>return create(&target,destination),
        Err(error)=>return Err(error),
    };
    let source_target=node_path::from_native(&target);let mut destination_target=node_path::from_native(&destination_target);
    if !node_path::is_absolute(context.style,&destination_target) {
        destination_target=context.resolve(&[parent,destination_target]);
    }
    if fs::metadata(fs_path(source)).is_ok_and(|metadata|metadata.is_dir())&&subdirectory(&context,&source_target,&destination_target) { return invalid(); }
    if fs::metadata(fs_path(source))?.is_dir()&&subdirectory(&context,&destination_target,&source_target) { return invalid(); }
    unlink(destination)?;
    create(&target,destination)
}
