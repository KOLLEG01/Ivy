//! Explicit nested-build tool environment; no arbitrary parent credentials.
use crate::{IvyError,IvyJson,IvyResult,config,node_path::{self,Style},node_runtime::NodeRuntime,paths};
use ivy_contracts::json;
use std::{collections::BTreeMap,ffi::OsString,fs,path::PathBuf};

#[derive(Debug)]
pub struct PreparationTools { pub environment:BTreeMap<OsString,OsString> }
fn require(ok:bool,message:&str)->IvyResult<()>{if ok{Ok(())}else{Err(IvyError::new("invalid_arguments",message))}}
fn internal<T,E>(value:Result<T,E>)->IvyResult<T>{value.map_err(|_|IvyError::internal())}

/// The executing Host's discovered intentional Node runtime is explicit. It
/// replaces TS process.execPath; the current Rust binary is never a Node tool.
pub async fn preparation_tools(mut executables:IvyJson,bootstrap_root:PathBuf,temporary_root:PathBuf,node:NodeRuntime)->IvyResult<PreparationTools>{
    internal(tokio::task::spawn_blocking(move||{
        let temporary=internal(paths::realpath(&temporary_root))?;
        let mut environment:BTreeMap<OsString,OsString>=["TEMP","TMP","TMPDIR"].into_iter().map(|key|(key.into(),temporary.as_os_str().to_owned())).collect();
        require(executables.as_object().is_some(),"Preparation tool mappings require an object.")?;
        executables.insert("$node",crate::layout::path_json(&node.executable));
        if cfg!(windows){executables.insert("$job",crate::layout::path_json(&crate::native_plan::native_binary_path(&bootstrap_root,"ivy-job")));}
        for(name,requested)in json::object_entries(&executables){
            let requested=config::path_value(requested)?;
            require(node_path::is_absolute(Style::native(),&node_path::from_native(&requested)),"Preparation tool mappings require absolute executable paths.")?;
            let path=internal(paths::realpath(&requested))?;
            let metadata=internal(fs::metadata(&path))?;
            let lower=path.as_os_str().to_string_lossy().to_ascii_lowercase();
            require(metadata.is_file()&&![".cmd",".bat",".ps1"].iter().any(|suffix|lower.ends_with(*suffix)),"Preparation tools require direct executable files.")?;
            if name==&"dotnet".into(){environment.insert("IVY_DOTNET".into(),path.into_os_string());}
            else if name==&"gcc".into(){environment.insert("IVY_EVS_CC".into(),path.into_os_string());}
        }
        Ok(PreparationTools{environment})
    }).await)?
}
