use super::{IvyError,IvyJson,IvyResult,config,node_path};
use std::{fs,io::Write,path::{Path,PathBuf},process::{Command,Stdio},sync::Mutex,time::{SystemTime,UNIX_EPOCH}};
static CORPUS:Mutex<()>=Mutex::new(());
const DOCUMENT:&[u8]=br#"{"schemaVersion":1,"value":"retained"}"#;
struct Fixture(PathBuf);
impl Fixture{
    fn new()->Self{
        let temporary=std::env::temp_dir();let root=temporary.join(format!("ivy-protected-config-{}-{}",std::process::id(),SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        assert_eq!(root.parent(),Some(temporary.as_path()));fs::create_dir(&root).unwrap();Self(root)
    }
    fn write(&self,name:&str,bytes:&[u8])->PathBuf{
        let path=self.0.join(name);fs::create_dir_all(path.parent().unwrap()).unwrap();fs::write(&path,bytes).unwrap();
        #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;fs::set_permissions(&path,fs::Permissions::from_mode(0o600)).unwrap();}
        path
    }
}
impl Drop for Fixture{fn drop(&mut self){
    #[cfg(windows)]fn writable(path:&Path){if let Ok(metadata)=fs::symlink_metadata(path){if metadata.file_type().is_symlink(){return}if metadata.is_dir(){if let Ok(entries)=fs::read_dir(path){for entry in entries.flatten(){writable(&entry.path())}}}if metadata.permissions().readonly(){let mut permissions=metadata.permissions();permissions.set_readonly(false);let _=fs::set_permissions(path,permissions);}}}
    #[cfg(windows)]writable(&self.0);
    let _=fs::remove_dir_all(&self.0);
}}
fn invalid()->IvyError{IvyError::new("invalid_arguments","Protected fixture input is invalid.").with_details(IvyJson::object([("marker","caller-owned".into())]))}
fn wire(result:IvyResult<Option<IvyJson>>)->IvyJson{match result{Ok(value)=>IvyJson::object([("ok",true.into()),("value",value.unwrap_or(IvyJson::Null))]),Err(error)=>IvyJson::object([("ok",false.into()),("error",error.to_wire())])}}
fn source_root()->PathBuf{std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(||PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),PathBuf::from)}
fn append(row:IvyJson){if let Some(file)=std::env::var_os("IVY_HOST_PROTECTED_CONFIG_CORPUS"){let _guard=CORPUS.lock().unwrap();writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(),"{}",ivy_contracts::json::stringify(&row,usize::MAX).unwrap()).unwrap();}}
async fn compare(path:&Path,maximum:usize)->IvyJson{
    // The Host reader imposes no envelope; this fixture composes the original
    // caller's object/schemaVersion policy solely for original-code comparison.
    let result=config::protected_json_file(path.to_owned(),maximum,invalid()).await.and_then(|value|{
        if value.as_ref().is_some_and(|value|value.as_object().is_none()||value.get("schemaVersion")!=Some(&IvyJson::Number(1.0))){Err(invalid())}else{Ok(value)}
    });
    let actual=wire(result);
    let input=IvyJson::object([("path",IvyJson::String(node_path::from_native(path))),("maximum",IvyJson::Number(maximum as f64))]);
    let mut child=Command::new("node").args(["--experimental-transform-types","--disable-warning=ExperimentalWarning","tests/fixtures/host-protected-config-oracle.mjs"]).current_dir(source_root())
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(ivy_contracts::json::stringify(&input,usize::MAX).unwrap().as_bytes()).unwrap();
    let output=child.wait_with_output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let expected=ivy_contracts::json::parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap();
    // The original oracle crosses JSON.stringify: non-finite parsed numbers
    // become null there. Canonical validation would impose a different policy.
    let observed=ivy_contracts::json::parse(&ivy_contracts::json::stringify(&actual,usize::MAX).unwrap(),usize::MAX).unwrap();
    assert_eq!(observed,expected,"{input:?}");
    append(IvyJson::object([("input",input),("actual",actual.clone()),("expected",expected)]));actual
}
fn code(value:&IvyJson)->Option<String>{value.get("error").and_then(|error|error.get("data")).and_then(|data|data.get("code")).and_then(|value|value.as_str().ok())}
fn directory_link(from:&Path,to:&Path){
    #[cfg(windows)]{let output=Command::new("node").args(["-e","require('node:fs').symlinkSync(process.argv[1],process.argv[2],'junction')"]).arg(from).arg(to).output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));}
    #[cfg(unix)]std::os::unix::fs::symlink(from,to).unwrap();
}

#[tokio::test]
async fn protected_reader_accepts_exact_single_link_files_and_linked_parents(){
    let fixture=Fixture::new();let file=fixture.write("parent/config ü 空 😀.json",DOCUMENT);
    assert_eq!(compare(&file,DOCUMENT.len()).await.get("ok"),Some(&true.into()));
    assert_eq!(code(&compare(&file,DOCUMENT.len()-1).await),Some("invalid_arguments".into()));
    assert_eq!(compare(&fixture.0.join("parent/./config ü 空 😀.json"),DOCUMENT.len()).await.get("ok"),Some(&true.into()));
    let alias=fixture.0.join("parent-link");directory_link(file.parent().unwrap(),&alias);
    assert_eq!(compare(&alias.join(file.file_name().unwrap()),DOCUMENT.len()).await.get("ok"),Some(&true.into()));
    let readonly=fixture.write("readonly.json",DOCUMENT);let original=fs::metadata(&readonly).unwrap().permissions();let mut permissions=original.clone();permissions.set_readonly(true);fs::set_permissions(&readonly,permissions).unwrap();
    assert_eq!(compare(&readonly,DOCUMENT.len()).await.get("ok"),Some(&true.into()));fs::set_permissions(readonly,original).unwrap();
    assert_eq!(fs::read(&file).unwrap(),DOCUMENT);
    #[cfg(windows)]{let differently_cased=fixture.0.join("parent/CONFIG ü 空 😀.json");assert_eq!(code(&compare(&differently_cased,DOCUMENT.len()).await),Some("invalid_arguments".into()));}
}

#[tokio::test]
async fn protected_reader_rejects_hardlinks_and_observed_directory_links_without_touching_targets(){
    let fixture=Fixture::new();let file=fixture.write("file.json",DOCUMENT);let hardlink=fixture.0.join("hardlink.json");fs::hard_link(&file,&hardlink).unwrap();
    for path in [&file,&hardlink]{assert_eq!(code(&compare(path,1024).await),Some("invalid_arguments".into()));}
    fs::remove_file(&hardlink).unwrap();assert_eq!(compare(&file,1024).await.get("ok"),Some(&true.into()));
    let directory=fixture.0.join("directory-link");directory_link(&fixture.0,&directory);
    assert_eq!(code(&compare(&directory,1024).await),Some("invalid_arguments".into()));
    let broken=fixture.0.join("dangling-directory-link");directory_link(&fixture.0.join("absent"),&broken);
    assert_eq!(code(&compare(&broken,1024).await),Some("invalid_arguments".into()));assert_eq!(fs::read(&file).unwrap(),DOCUMENT);
    let leaf=fixture.0.join("file-link.json");
    #[cfg(windows)]let created=std::os::windows::fs::symlink_file(&file,&leaf);
    #[cfg(unix)]let created=std::os::unix::fs::symlink(&file,&leaf);
    match created{
        Ok(())=>assert_eq!(code(&compare(&leaf,1024).await),Some("invalid_arguments".into())),
        Err(error)if cfg!(windows)&&(error.kind()==std::io::ErrorKind::PermissionDenied||error.raw_os_error()==Some(1314))=>{
            append(IvyJson::object([("unavailable","file-symlink fixture setup".into()),("reason","Windows account lacks file-symlink privilege; original/reader not run for this case".into())]));
            eprintln!("UNAVAILABLE: file-symlink fixture setup lacks Windows privilege");
            assert!(std::env::var_os("IVY_REQUIRE_FILE_SYMLINKS").is_none(),"native acceptance requires the file-symlink case");
        }
        Err(error)=>panic!("file-symlink setup failed: {error}"),
    }
}

#[tokio::test]
async fn protected_reader_absence_nonfiles_and_io_failures_keep_the_original_classification(){
    let fixture=Fixture::new();
    for path in [fixture.0.join("absent"),fixture.0.join("absent-parent/file.json")]{assert_eq!(compare(&path,1024).await.get("value"),Some(&IvyJson::Null));}
    assert_eq!(code(&compare(&fixture.0,1024).await),Some("invalid_arguments".into()));
    let file=fixture.write("parent-is-file",DOCUMENT);let _=compare(&file.join("child"),1024).await;
    #[cfg(windows)]{
        use std::os::windows::fs::OpenOptionsExt;
        let held=fs::OpenOptions::new().read(true).share_mode(0).open(&file).unwrap();
        assert_eq!(code(&compare(&file,1024).await),Some("internal_error".into()));drop(held);
    }
}

#[tokio::test]
async fn protected_reader_json_and_byte_limits_preserve_caller_errors_and_utf8_replacement(){
    let fixture=Fixture::new();
    let mut bodies=vec![DOCUMENT.to_vec(),br#"{"schemaVersion":1,"__proto__":{"retained":true},"value":"\ud800"}"#.to_vec(),br#"{"schemaVersion":1,"number":1e400}"#.to_vec(),b"{".to_vec(),Vec::new(),b"null".to_vec(),b"[]".to_vec(),b"false".to_vec(),b"{}".to_vec(),br#"{"schemaVersion":"1"}"#.to_vec(),[&[0xef,0xbb,0xbf][..],DOCUMENT].concat()];
    bodies.push([br#"{"schemaVersion":1,"value":""#.as_slice(),&[0xff],br#""}"#].concat());
    for(index,bytes)in bodies.into_iter().enumerate(){let file=fixture.write(&format!("case-{index}.json"),&bytes);let _=compare(&file,bytes.len()).await;let _=compare(&file,0).await;}
    let file=fixture.write("invalid.json",b"{");let actual=config::protected_json_file(file,1024,invalid()).await.unwrap_err();assert_eq!(actual,invalid());
}

#[tokio::test]
async fn protected_reader_leaves_document_envelopes_to_its_caller(){
    let fixture=Fixture::new();
    for bytes in [b"null".as_slice(),b"[]",b"false",b"42",b"\"text\"",b"{}"]{
        let file=fixture.write("generic.json",bytes);let value=config::protected_json_file(file,1024,invalid()).await.unwrap();assert!(value.is_some());
        assert_eq!(value.unwrap(),ivy_contracts::json::parse(std::str::from_utf8(bytes).unwrap(),usize::MAX).unwrap());
    }
    let file=fixture.write("overflow.json",b"1e400");
    assert_eq!(config::protected_json_file(file,1024,invalid()).await.unwrap(),Some(IvyJson::Number(f64::INFINITY)));
}

#[cfg(unix)]
#[tokio::test]
async fn protected_reader_unix_permissions_require_no_group_or_other_bits(){
    use std::os::unix::fs::PermissionsExt;
    let fixture=Fixture::new();let file=fixture.write("mode.json",DOCUMENT);
    for mode in [0o600,0o400,0o700,0o640,0o604,0o777]{fs::set_permissions(&file,fs::Permissions::from_mode(mode)).unwrap();let value=compare(&file,1024).await;if mode&0o077==0{assert_eq!(value.get("ok"),Some(&true.into()));}else{assert_eq!(code(&value),Some("invalid_arguments".into()));}}
}
