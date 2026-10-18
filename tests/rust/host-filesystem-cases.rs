use super::{ServiceFixture,assert_json_eq};
use ivy_host_runtime::{IvyJson,filesystem};
use std::{fs,io::Write,path::{Path,PathBuf},process::{Command,Stdio},sync::Mutex};
static CORPUS:Mutex<()>=Mutex::new(());
fn path(path:&Path)->IvyJson{IvyJson::String(ivy_host_runtime::node_path::from_native(path))}
fn oracle(input:IvyJson)->IvyJson{
    let mut child=Command::new("node").arg("tests/fixtures/host-filesystem-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(ivy_contracts::json::stringify(&input,usize::MAX).unwrap().as_bytes()).unwrap();
    let out=child.wait_with_output().unwrap();assert!(out.status.success());
    ivy_contracts::json::parse(&String::from_utf8(out.stdout).unwrap(),usize::MAX).unwrap()
}
fn compare(input:IvyJson,result:std::io::Result<IvyJson>){
    let actual=match result{Ok(value)=>IvyJson::object([("ok",true.into()),("value",value)]),Err(error)=>IvyJson::object([("ok",false.into()),("code",filesystem::node_error_code(&error).map_or(IvyJson::Null,IvyJson::from))])};
    let expected=oracle(input.clone());assert_json_eq(&actual,&expected);
    if let Some(file)=std::env::var_os("IVY_HOST_FILESYSTEM_CORPUS"){
        let _guard=CORPUS.lock().unwrap();writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(),"{}",ivy_contracts::json::stringify(&IvyJson::object([("input",input),("actual",actual),("expected",expected)]),usize::MAX).unwrap()).unwrap();
    }
}
fn identity(path_value:&Path){
    compare(IvyJson::object([("operation","identity".into()),("path",path(path_value))]),filesystem::stat_identity(path_value).map(|value|IvyJson::object([("dev",value.dev.into()),("ino",value.ino.into())])));
}
struct RestorePermissions(PathBuf,fs::Permissions);
impl Drop for RestorePermissions{fn drop(&mut self){let _=fs::set_permissions(&self.0,self.1.clone());}}

#[test]
fn bigint_file_identity_matches_node_across_aliases_rename_and_replacement(){
    let fixture=ServiceFixture::new();let directory=fixture.0.join("identity ü 空 😀");fs::create_dir(&directory).unwrap();
    let file=directory.join("payload");fs::write(&file,b"retained").unwrap();
    identity(&directory);identity(&file);
    let original=filesystem::stat_identity(&directory).unwrap();
    let moved=fixture.0.join("renamed");fs::rename(&directory,&moved).unwrap();
    assert_eq!(filesystem::stat_identity(&moved).unwrap(),original);identity(&moved);
    fs::create_dir(&directory).unwrap();assert_ne!(filesystem::stat_identity(&directory).unwrap(),original);identity(&directory);
    let link=fixture.0.join("linked");
    #[cfg(windows)]std::os::windows::fs::symlink_dir(&moved,&link).unwrap();
    #[cfg(unix)]std::os::unix::fs::symlink(&moved,&link).unwrap();
    assert_eq!(filesystem::stat_identity(&link).unwrap(),original);identity(&link);
    #[cfg(windows)]{
        let extended=PathBuf::from(format!("\\\\?\\{}",directory.display()));identity(&extended);
    }
    identity(&fixture.0.join("missing"));
}

#[test]
fn fs_access_matches_node_for_files_directories_readonly_and_missing_paths(){
    let fixture=ServiceFixture::new();let file=fixture.0.join("payload ü 空 😀");fs::write(&file,b"bytes").unwrap();
    let permissions=fs::metadata(&file).unwrap().permissions();let _restore=RestorePermissions(file.clone(),permissions.clone());
    let check=|p:&Path,mode:u32|compare(IvyJson::object([("operation","access".into()),("path",path(p)),("mode",IvyJson::Number(mode as f64))]),filesystem::access(p,mode).map(|_|IvyJson::Null));
    for mode in [0,filesystem::READ_OK,filesystem::WRITE_OK,filesystem::READ_OK|filesystem::WRITE_OK,filesystem::EXECUTE_OK]{check(&file,mode);check(&fixture.0,mode);check(&fixture.0.join("missing"),mode);}
    let mut readonly=permissions;readonly.set_readonly(true);fs::set_permissions(&file,readonly).unwrap();
    check(&file,filesystem::READ_OK);check(&file,filesystem::READ_OK|filesystem::WRITE_OK);
    let directory_permissions=fs::metadata(&fixture.0).unwrap().permissions();let _restore_directory=RestorePermissions(fixture.0.clone(),directory_permissions.clone());
    #[cfg(windows)]{
        let mut readonly=directory_permissions;readonly.set_readonly(true);fs::set_permissions(&fixture.0,readonly).unwrap();check(&fixture.0,filesystem::READ_OK|filesystem::WRITE_OK);
    }
    assert!(filesystem::access(&file,8).is_err());
}

#[test]
fn directory_and_file_times_match_node_date_milliseconds_without_creating_paths(){
    let fixture=ServiceFixture::new();
    for directory in [true,false]{
        let first=fixture.0.join(if directory{"directory ü 空 😀"}else{"file ü 空 😀"});let second=fixture.0.join(if directory{"node-directory"}else{"node-file"});
        if directory{fs::create_dir(&first).unwrap();fs::create_dir(&second).unwrap();}else{fs::write(&first,b"bytes").unwrap();fs::write(&second,b"bytes").unwrap();}
        for(accessed,modified)in [(-1,0),(1_725_000_000_123,1_792_354_000_987),(1_792_354_000_987,1_725_000_000_123)]{
            filesystem::set_times_ms(&first,accessed,modified).unwrap();
            let actual=oracle(IvyJson::object([("operation","readTimes".into()),("path",path(&first))]));
            let expected=oracle(IvyJson::object([("operation","times".into()),("path",path(&second)),("accessed",IvyJson::Number(accessed as f64)),("modified",IvyJson::Number(modified as f64))]));
            assert_json_eq(&actual,&expected);
            compare(IvyJson::object([("operation","readTimes".into()),("path",path(&first))]),Ok(actual.required("value").unwrap().clone()));
        }
        assert!(filesystem::set_times_ms(&first,i64::MAX,0).is_err());
    }
    let missing=fixture.0.join("missing");compare(IvyJson::object([("operation","times".into()),("path",path(&missing)),("accessed",IvyJson::Number(0.0)),("modified",IvyJson::Number(0.0))]),filesystem::set_times_ms(&missing,0,0).map(|_|IvyJson::Null));assert!(!missing.exists());
}

#[test]
fn directory_browsing_errno_matches_node_for_missing_file_and_intermediate_file(){
    let fixture=ServiceFixture::new();let file=fixture.0.join("file");fs::write(&file,b"bytes").unwrap();
    for selected in [file.clone(),file.join("child"),fixture.0.join("missing")]{
        let result=ivy_host_runtime::paths::realpath(&selected).and_then(fs::read_dir).map(|_|IvyJson::Null);
        compare(IvyJson::object([("operation","directory".into()),("path",path(&selected))]),result);
    }
    assert_eq!(filesystem::node_error_code(&std::io::Error::other("unclassified")),None);
}
