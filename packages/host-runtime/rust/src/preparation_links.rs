//! Host-owned cache links and ordered links into a copied workspace.
use crate::{IvyError, IvyResult, concurrency::map_concurrent_with_error, filesystem, node_path::{self, Style}, paths};
use ivy_contracts::JsString;
use std::{fs, io, path::{Path, PathBuf}};
#[path = "preparation_links/copy_link.rs"]
mod copy_link;
pub(crate) fn copy_link_entry(source:&Path,destination:&Path,verbatim:bool,parents:bool)->io::Result<()>{copy_link::copy_with_options(source,destination,verbatim,parents)}
pub(crate) fn check_copy_paths(source:&Path,destination:&Path,directory:bool,parents:bool)->io::Result<Option<fs::Metadata>>{copy_link::check_paths(source,destination,directory,parents)}

/// Cache restoration distinguishes native ENOENT from policy errors before the
/// public Ivy boundary. Keep that distinction through concurrent worker drain.
#[derive(Debug)]
pub(crate) enum Error { Io(io::Error), Ivy(IvyError) }
impl From<io::Error> for Error { fn from(error:io::Error)->Self { Self::Io(error) } }
impl From<IvyError> for Error { fn from(error:IvyError)->Self { Self::Ivy(error) } }
impl std::fmt::Display for Error {
    fn fmt(&self,formatter:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
        match self { Self::Io(error)=>error.fmt(formatter),Self::Ivy(error)=>error.fmt(formatter) }
    }
}
impl std::error::Error for Error {
    fn source(&self)->Option<&(dyn std::error::Error+'static)> {
        match self { Self::Io(error)=>Some(error),Self::Ivy(error)=>Some(error) }
    }
}
impl Error {
    pub(crate) fn into_ivy(self)->IvyError { match self { Self::Io(_)=>IvyError::internal(),Self::Ivy(error)=>error } }
}
type Result<T> = std::result::Result<T,Error>;
fn require(ok:bool,code:&str,message:&str)->Result<()> { if ok { Ok(()) } else { Err(IvyError::new(code,message).into()) } }
async fn blocking<T:Send+'static>(work:impl FnOnce()->Result<T>+Send+'static)->Result<T> {
    tokio::task::spawn_blocking(work).await.map_err(|_|Error::Ivy(IvyError::internal()))?
}
fn local(root:&Path,path:&JsString)->PathBuf{
    let mut parts=vec![node_path::from_native(root)];parts.extend(path.0.split(|c|*c==47).map(|part|JsString(part.to_vec())));
    node_path::to_native(&node_path::join(Style::native(),&parts))
}
fn fs_path(path:&Path)->PathBuf{node_path::filesystem_path(&node_path::from_native(path))}
fn entries(path:&Path)->Result<Vec<(PathBuf,fs::FileType)>> {
    fs::read_dir(fs_path(path))?.map(|entry| {
        let entry=entry?;
        Ok((PathBuf::from(entry.file_name()),entry.file_type()?))
    }).collect()
}
fn cross_device(error:&io::Error)->bool {
    #[cfg(windows)] { error.raw_os_error()==Some(17) }
    #[cfg(unix)] { error.raw_os_error()==Some(libc::EXDEV) }
}
fn linked(kind:fs::FileType,from:&Path,to:&Path)->Result<()> {
    if kind.is_dir() { filesystem::directory_link(from,to)?; }
    else if kind.is_symlink() { copy_link::copy(from,to)?; }
    else {
        require(kind.is_file(),"artifact_changed","Dependency cache contains an unsupported entry.")?;
        match fs::hard_link(fs_path(from),fs_path(to)) {
            Ok(())=>(),
            Err(error) if cross_device(&error)=>{fs::copy(fs_path(from),fs_path(to))?;},
            Err(error)=>return Err(error.into()),
        }
    }
    Ok(())
}

/// Directory packages use junctions on Windows and relative symlinks on Unix;
/// files use hardlinks with only the original cross-device copy fallback.
pub async fn link_dependency_directory(source:PathBuf,destination:PathBuf)->IvyResult<()> {
    tokio::spawn(link_dependency_directory_raw(source,destination)).await.map_err(|_|IvyError::internal())?
        .map_err(Error::into_ivy)
}
pub(crate) async fn link_dependency_directory_raw(source:PathBuf,destination:PathBuf)->Result<()> {
    let root=destination.clone();
    blocking(move||{fs::create_dir_all(fs_path(&root))?;Ok(())}).await?;
    let scan=source.clone();let found=blocking(move||entries(&scan)).await?;
    map_concurrent_with_error(found,8,move |(name,kind),_| {
        let from=source.join(&name);let to=destination.join(&name);
        async move {
            if kind.is_dir()&&node_path::from_native(&name).0.first()==Some(&64) {
                let scoped=to.clone();
                blocking(move||{fs::create_dir_all(fs_path(&scoped))?;Ok(())}).await?;
                let scan=from.clone();let children=blocking(move||entries(&scan)).await?;
                map_concurrent_with_error(children,8,move |(name,kind),_| {
                    let from=from.join(&name);let to=to.join(&name);
                    blocking(move||linked(kind,&from,&to))
                }).await?;
            } else { blocking(move||linked(kind,&from,&to)).await?; }
            Ok::<(),Error>(())
        }
    }).await?;
    Ok(())
}

/// Entries come from workspace_package_locations in their Map insertion order.
/// Check the source before removing a previous destination link or directory.
pub async fn restore_workspace_links(root:PathBuf,links:Vec<(JsString,JsString)>)->IvyResult<()>{
    tokio::spawn(restore_workspace_links_raw(root,links)).await.map_err(|_|IvyError::internal())?
        .map_err(Error::into_ivy)
}
pub(crate) async fn restore_workspace_links_raw(root:PathBuf,links:Vec<(JsString,JsString)>)->Result<()> {
    for(location,target)in links {
        let source=local(&root,&target);let destination=local(&root,&location);
        let checked_root=root.clone();let checked_source=source.clone();let checked_destination=destination.clone();
        blocking(move|| {
            require(paths::inside(&checked_root,&checked_source)?&&paths::inside(&checked_root,&checked_destination)?,
                "target_conflict","Workspace dependency link leaves the build workspace.")?;
            let metadata=fs::metadata(fs_path(&checked_source))?;
            require(metadata.is_dir()&&paths::inside(&checked_root,&paths::realpath(&checked_source)?)?,
                "artifact_changed","Workspace dependency source leaves its build workspace.")
        }).await?;
        filesystem::remove_force(destination.clone()).await?;
        blocking(move|| {
            let parent=node_path::dirname(Style::native(),&node_path::from_native(&destination));
            fs::create_dir_all(node_path::filesystem_path(&parent))?;
            filesystem::directory_link(&source,&destination)?;
            Ok(())
        }).await?;
    }
    Ok(())
}
