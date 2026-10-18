use ivy_host_runtime::{IvyJson,IvyResult,node_path,portable_dependencies::{self as portable,PortableDependency}};
use ivy_contracts::{JsString,json};
use base64::{Engine,engine::general_purpose::STANDARD};
use sha2::{Digest,Sha512};
use std::{collections::BTreeMap,fs,io::Write,path::{Path,PathBuf},process::{Command,Stdio},sync::Mutex,time::{SystemTime,UNIX_EPOCH}};
static CORPUS:Mutex<()>=Mutex::new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new()->Self{let path=std::env::temp_dir().join(format!("ivy-portable-{}-{}",std::process::id(),SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));fs::create_dir(&path).unwrap();Self(path)}
    fn write(&self,path:&str,bytes:&[u8])->PathBuf{let file=self.0.join(path);fs::create_dir_all(file.parent().unwrap()).unwrap();fs::write(&file,bytes).unwrap();file}
}
impl Drop for Fixture{fn drop(&mut self){let _=fs::remove_dir_all(&self.0);}}
fn path(value:&Path)->IvyJson{IvyJson::String(node_path::from_native(value))}
fn root()->PathBuf{std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(||PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),PathBuf::from)}
fn wire(result:IvyResult<IvyJson>)->IvyJson{match result{Ok(value)=>IvyJson::object([("ok",true.into()),("value",value)]),Err(error)=>IvyJson::object([("ok",false.into()),("error",error.to_wire())])}}
fn code(value:&IvyJson)->Option<String>{value.get("error").and_then(|value|value.get("data")).and_then(|value|value.get("code")).and_then(|value|value.as_str().ok())}
fn oracle(input:&IvyJson)->IvyJson{
    let mut process=Command::new("node").args(["--experimental-transform-types","--disable-warning=ExperimentalWarning","tests/fixtures/host-portable-dependencies-oracle.mjs"]).current_dir(root()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    process.stdin.take().unwrap().write_all(json::stringify(input,usize::MAX).unwrap().as_bytes()).unwrap();
    let output=process.wait_with_output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    json::parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap()
}
fn compare(input:IvyJson,result:IvyResult<IvyJson>)->IvyJson{
    let actual=wire(result);let expected=oracle(&input);assert_eq!(&actual,expected.get("operation").unwrap(),"{input:?}");
    if (input.get("operation")==Some(&"install".into())&&actual.get("ok")==Some(&true.into()))||input.get("operation")==Some(&"unlink".into()){assert_eq!(expected.get("actualTree"),expected.get("originalTree"));}
    if let Some(file)=std::env::var_os("IVY_HOST_PORTABLE_CORPUS"){let _guard=CORPUS.lock().unwrap();writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(),"{}",json::stringify(&IvyJson::object([("input",input),("actual",actual.clone()),("expected",expected)]),usize::MAX).unwrap()).unwrap();}
    actual
}
fn checksum(bytes:&[u8])->JsString{format!("sha512-{}",STANDARD.encode(Sha512::digest(bytes))).into()}
fn dependency(location:&str,bytes:&[u8])->PortableDependency{PortableDependency{location:location.into(),resolved:"https://registry.invalid/pinned.tgz".into(),integrity:checksum(bytes),version:"1.2.3".into()}}
fn value(value:&PortableDependency)->IvyJson{IvyJson::object([("location",IvyJson::String(value.location.clone())),("resolved",IvyJson::String(value.resolved.clone())),("integrity",IvyJson::String(value.integrity.clone())),("version",IvyJson::String(value.version.clone()))])}
fn metadata()->IvyJson{let mut value=value(&dependency("node_modules/fixture",b"bytes"));value.insert("os",IvyJson::Array(vec!["linux".into()]));value}
async fn plan(fixture:&Fixture,lock:IvyJson,selected:Vec<JsString>)->IvyJson{
    let result=portable::portable_dependency_plan(lock.clone(),selected.clone(),fixture.0.clone()).await.map(|values|IvyJson::Array(values.iter().map(value).collect()));
    compare(IvyJson::object([("operation","plan".into()),("lock",lock),("selected",IvyJson::Array(selected.into_iter().map(IvyJson::String).collect())),("artifactRoot",path(&fixture.0))]),result)
}
fn succeeds(value:&IvyJson){assert_eq!(value.get("ok"),Some(&true.into()));}
fn refuses(value:&IvyJson,expected:&str){assert_eq!(code(value).as_deref(),Some(expected));}

#[tokio::test]
async fn portable_plan_retains_order_supported_cross_host_variants_and_existing_path_shortcuts(){
    let fixture=Fixture::new();fixture.write("node_modules/existing",b"not a package directory");fs::create_dir_all(fixture.0.join("node_modules/directory")).unwrap();
    let mut packages=Vec::new();
    for(name,os,cpu)in [("linux-arm","linux","arm64"),("windows-x64","win32","x64"),("linux-x64","linux","x64"),("windows-arm","win32","arm64"),("darwin","darwin","arm64"),("wrong-cpu","linux","mips")]{
        let mut item=metadata();item.insert("os",IvyJson::Array(vec![os.into()]));item.insert("cpu",IvyJson::Array(vec![cpu.into()]));packages.push((format!("node_modules/{name}"),item));
    }
    for(name,item)in [("unconstrained",IvyJson::object(Vec::<(String,IvyJson)>::new())),("null-os",IvyJson::object([("os",IvyJson::Null)])),("string-cpu",IvyJson::object([("cpu","x64".into())])),("existing",IvyJson::Null),("directory",IvyJson::Null)]{packages.push((format!("node_modules/{name}"),item));}
    let selected=["existing","directory","windows-arm","linux-arm","unconstrained","darwin","linux-x64","wrong-cpu","windows-x64","null-os","string-cpu","absent","windows-arm"].map(|name|format!("node_modules/{name}").into()).to_vec();
    let result=plan(&fixture,IvyJson::object([("packages",IvyJson::object(packages))]),selected).await;succeeds(&result);
    let locations=result.get("value").unwrap().as_array().unwrap().iter().map(|entry|entry.get("location").unwrap().as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!(locations,["node_modules/windows-arm","node_modules/linux-arm","node_modules/linux-x64","node_modules/windows-x64"]);
}

#[tokio::test]
async fn portable_plan_keeps_empty_set_and_pinned_metadata_refusal_order(){
    let fixture=Fixture::new();succeeds(&plan(&fixture,IvyJson::Null,vec![]).await);
    for(lock,expected)in [(IvyJson::Null,"internal_error"),(IvyJson::object(Vec::<(String,IvyJson)>::new()),"invalid_arguments"),(IvyJson::object([("packages",IvyJson::Null)]),"invalid_arguments")]{refuses(&plan(&fixture,lock,vec!["node_modules/fixture".into()]).await,expected);}
    succeeds(&plan(&fixture,IvyJson::object([("packages",IvyJson::Array(vec![]))]),vec!["node_modules/fixture".into()]).await);
    for(field,entry)in [("resolved","http://registry.invalid/pinned".into()),("resolved","HTTPS://registry.invalid/pinned".into()),("resolved",IvyJson::Null),("integrity","sha512-".into()),("integrity","sha512-AAAA===".into()),("integrity","sha512-A_A".into()),("integrity","sha512-AAA\n".into()),("version",IvyJson::Number(123.0))]{
        let mut item=metadata();item.insert(field,entry);let lock=IvyJson::object([("packages",IvyJson::object([("node_modules/fixture",item)]))]);refuses(&plan(&fixture,lock,vec!["node_modules/fixture".into()]).await,"artifact_missing");
    }
    fixture.write("node_modules/parent",b"file");let result=plan(&fixture,IvyJson::object([("packages",IvyJson::object([("node_modules/parent/child",metadata())]))]),vec!["node_modules/parent/child".into()]).await;
    assert!(result.get("ok").is_some());
}

#[test]
fn response_status_header_number_and_decoded_byte_integrity_preserve_the_original_limits(){
    for declared in [None,Some(""),Some("0"),Some("-1"),Some("NaN"),Some("infinity"),Some(" 0x4000000 "),Some("67108864"),Some("67108865"),Some("Infinity"),Some("-Infinity"),Some("0b100"),Some("1, 2")]{
        let bytes=b"verified response bytes";let dep=dependency("node_modules/fixture",bytes);let input=IvyJson::object([("operation","policy".into()),("dependency",value(&dep)),("declared",declared.map_or(IvyJson::Null,IvyJson::from)),("bodyBase64",STANDARD.encode(bytes).into())]);
        compare(input,portable::check_portable_response(true,declared).and_then(|_|portable::verify_portable_bytes(&dep,bytes)).map(|_|IvyJson::object([("byteLength",IvyJson::Number(bytes.len()as f64))])));
    }
    let bytes=b"bytes";let dep=dependency("node_modules/fixture",bytes);
    refuses(&compare(IvyJson::object([("operation","policy".into()),("dependency",value(&dep)),("responseOk",false.into()),("declared","Infinity".into())]),portable::check_portable_response(false,Some("Infinity")).map(|_|IvyJson::Null)),"service_unavailable");
    for length in [0,portable::MAXIMUM_PORTABLE_PACKAGE_BYTES,portable::MAXIMUM_PORTABLE_PACKAGE_BYTES+1]{
        let bytes=vec![97;length];let dep=dependency("node_modules/fixture",&bytes);
        let result=portable::check_portable_response(true,None).and_then(|_|portable::verify_portable_bytes(&dep,&bytes)).map(|_|IvyJson::object([("byteLength",IvyJson::Number(length as f64))]));
        let answer=compare(IvyJson::object([("operation","policy".into()),("dependency",value(&dep)),("bodyLength",IvyJson::Number(length as f64))]),result);
        if length==portable::MAXIMUM_PORTABLE_PACKAGE_BYTES{succeeds(&answer)}else{refuses(&answer,"artifact_changed")}
    }
    refuses(&compare(IvyJson::object([("operation","policy".into()),("dependency",value(&dep)),("bodyBase64",STANDARD.encode(b"changed").into())]),portable::verify_portable_bytes(&dep,b"changed").map(|_|IvyJson::Null)),"artifact_changed");
}

#[test]
fn archive_listing_uses_original_package_root_segments_count_and_truncation_rules(){
    for stdout in ["","\n","package\n","package/\r\npackage/file\r\n","package","package\r","package//\n","package/./file\n","package/../file\n","package\\file\n","/package/file\n","elsewhere\n","package/.\n","package/empty//file\n","package/file\r\n\n"]{
        for truncated in [false,true]{compare(IvyJson::object([("operation","listing".into()),("stdout",stdout.into()),("truncated",truncated.into())]),portable::portable_archive_entries(stdout,truncated).map(|_|IvyJson::Null));}
    }
    for count in [50_000,50_001]{let stdout="package/file\n".repeat(count);compare(IvyJson::object([("operation","listing".into()),("stdout",stdout.clone().into()),("truncated",false.into())]),portable::portable_archive_entries(&stdout,false).map(|_|IvyJson::Null));}
}
fn tar()->PathBuf{std::env::var_os("IVY_HOST_SYSTEM_TAR").map(PathBuf::from).unwrap_or_else(||if cfg!(windows){PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(||"C:\\Windows".into())).join("System32/tar.exe")}else{PathBuf::from("/usr/bin/tar")})}
fn archive(fixture:&Fixture,entries:Vec<(&str,Vec<u8>,u32)>)->Vec<u8>{let file=fixture.0.join("fixture.tgz");let result=oracle(&IvyJson::object([("operation","archive".into()),("path",path(&file)),("entries",IvyJson::Array(entries.into_iter().map(|(name,bytes,mode)|IvyJson::object([("path",name.into()),("bodyBase64",STANDARD.encode(bytes).into()),("mode",IvyJson::Number(mode as f64))])).collect()))]));succeeds(result.get("operation").unwrap());fs::read(file).unwrap()}
async fn install(fixture:&Fixture,location:&str,bytes:Vec<u8>,collision:bool)->IvyJson{
    let actual=fixture.0.join("actual");let original=fixture.0.join("original");let actual_temp=fixture.0.join("actual-temp");let original_temp=fixture.0.join("original-temp");
    for root in [&actual,&original,&actual_temp,&original_temp]{fs::create_dir_all(root).unwrap();}
    let body=fixture.write("downloaded.tgz",&bytes);let dep=dependency(location,&bytes);let hash=ivy_contracts::hash::digest(location.as_bytes());let key=&hash[7..];
    if collision{for temp in [&actual_temp,&original_temp]{fs::write(temp.join(format!("{key}.tgz")),b"existing writer archive").unwrap();}}
    let executables=BTreeMap::from([("tar".into(),tar())]);
    let result=portable::install_portable_dependency(dep.clone(),bytes,actual.clone(),actual_temp.clone(),executables).await.map(|_|IvyJson::Null);
    let answer=compare(IvyJson::object([("operation","install".into()),("dependency",value(&dep)),("bodyFile",path(&body)),("actualRoot",path(&actual)),("originalRoot",path(&original)),("temporaryRoot",path(&original_temp)),("executables",IvyJson::object([("tar",path(&tar()))]))]),result);
    for temp in [&actual_temp,&original_temp]{let file=temp.join(format!("{key}.tgz"));if collision{assert_eq!(fs::read(file).unwrap(),b"existing writer archive");}else{assert!(!file.exists(),"archive cleanup after result");}}
    answer
}

#[tokio::test]
async fn system_tar_materializes_exact_normal_scoped_and_nested_package_identities(){
    for(location,name)in [("node_modules/fixture","fixture"),("node_modules/@scope/fixture","@scope/fixture"),("node_modules/outer/node_modules/@scope/fixture","@scope/fixture")]{
        let fixture=Fixture::new();let bytes=archive(&fixture,vec![("package/package.json",json::stringify(&IvyJson::object([("name",name.into()),("version","1.2.3".into())]),usize::MAX).unwrap().into_bytes(),0o644),("package/bin/tool",b"native payload bytes\0\xff".to_vec(),0o755)]);
        succeeds(&install(&fixture,location,bytes,false).await);
    }
}

#[tokio::test]
async fn system_tar_refusals_cleanup_owned_archives_and_preserve_exclusive_create_collisions(){
    for(label,entries,expected)in [
        ("empty",vec![],"artifact_changed"),
        ("wrong-root",vec![("other/file",b"bytes".to_vec(),0o644)],"artifact_changed"),
        ("missing-json",vec![("package/payload",b"bytes".to_vec(),0o644)],"internal_error"),
        ("null-json",vec![("package/package.json",b"null".to_vec(),0o644)],"internal_error"),
        ("syntax-json",vec![("package/package.json",b"{".to_vec(),0o644)],"internal_error"),
        ("wrong-name",vec![("package/package.json",br#"{"name":"other","version":"1.2.3"}"#.to_vec(),0o644)],"build_mismatch"),
        ("wrong-version",vec![("package/package.json",br#"{"name":"fixture","version":123}"#.to_vec(),0o644)],"build_mismatch"),
        ("large-json",vec![("package/package.json",vec![32;1024*1024+1],0o644)],"invalid_arguments")
    ]{let fixture=Fixture::new();let bytes=archive(&fixture,entries);let result=install(&fixture,"node_modules/fixture",bytes,false).await;assert_eq!(code(&result).as_deref(),Some(expected),"{label}");}
    let fixture=Fixture::new();refuses(&install(&fixture,"node_modules/fixture",b"not a gzip archive".to_vec(),false).await,"command_failed");
    let fixture=Fixture::new();let bytes=archive(&fixture,vec![("package/package.json",br#"{"name":"fixture","version":"1.2.3"}"#.to_vec(),0o644)]);refuses(&install(&fixture,"node_modules/fixture",bytes,true).await,"internal_error");
}

#[tokio::test]
async fn archive_file_cleanup_clears_readonly_and_preserves_real_directories(){
    for case in ["missing","file","readonly","directory"]{
        let fixture=Fixture::new();let actual=fixture.0.join("actual");let original=fixture.0.join("original");
        for root in [&actual,&original]{fs::create_dir(root).unwrap();if case=="directory"{fs::create_dir(root.join("archive")).unwrap();fs::write(root.join("archive/retained"),b"must remain").unwrap();}else if case!="missing"{let file=root.join("archive");fs::write(&file,b"bytes").unwrap();if case=="readonly"{let mut permissions=fs::metadata(&file).unwrap().permissions();permissions.set_readonly(true);fs::set_permissions(file,permissions).unwrap();}}}
        let result=ivy_host_runtime::filesystem::remove_file_force(actual.join("archive")).await.map(|_|IvyJson::Null);
        let answer=compare(IvyJson::object([("operation","unlink".into()),("actualRoot",path(&actual)),("originalRoot",path(&original)),("path",path(&original.join("archive")))]),result);
        if case=="directory"{refuses(&answer,"internal_error");assert_eq!(fs::read(actual.join("archive/retained")).unwrap(),b"must remain");}else{succeeds(&answer);}
    }
}

#[tokio::test]
async fn bounded_json_reader_preserves_original_regular_file_and_account_sharing_behavior(){
    let fixture=Fixture::new();let bytes=br#"{"name":"fixture","version":"1.2.3"}"#;let file=fixture.write("package ü 空 😀.json",bytes);
    for limit in [bytes.len(),bytes.len()-1]{compare(IvyJson::object([("operation","jsonFile".into()),("path",path(&file)),("limit",IvyJson::Number(limit as f64))]),ivy_host_runtime::config::json_file(&file,limit).await);}
    #[cfg(windows)]{
        use std::os::windows::fs::OpenOptionsExt;
        let held=fs::OpenOptions::new().read(true).share_mode(0).open(&file).unwrap();
        compare(IvyJson::object([("operation","jsonFile".into()),("path",path(&file)),("limit",IvyJson::Number(1024.0))]),ivy_host_runtime::config::json_file(&file,1024).await);drop(held);
    }
    for file in [fixture.0.join("absent"),fixture.0.clone(),fixture.0.join("invalid\0name")]{compare(IvyJson::object([("operation","jsonFile".into()),("path",path(&file)),("limit",IvyJson::Number(1024.0))]),ivy_host_runtime::config::json_file(&file,1024).await);}
}
