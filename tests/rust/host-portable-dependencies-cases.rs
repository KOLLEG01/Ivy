use ivy_host_runtime::{IvyJson,IvyResult,node_path,portable_dependencies::{self as portable,PortableDependency}};
use ivy_contracts::{JsString,json};
use base64::{Engine,engine::general_purpose::STANDARD};
use sha2::{Digest,Sha512};
use std::{collections::BTreeMap,fs,io::Write,path::{Path,PathBuf},process::{Command,Stdio},sync::Mutex,time::{SystemTime,UNIX_EPOCH}};
#[path="host-portable-http-cases.rs"]
mod native_http_cases;
#[path="host-portable-io-cases.rs"]
mod io_cases;
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
fn succeeds(value:&IvyJson){assert_eq!(value.get("ok"),Some(&true.into()));}
fn refuses(value:&IvyJson,expected:&str){assert_eq!(code(value).as_deref(),Some(expected));}

#[test]
fn response_status_decimal_length_and_decoded_bytes_keep_bounded_integrity(){
    for declared in [None,Some(""),Some("0"),Some("-1"),Some("NaN"),Some("infinity"),Some(" 0x4000000 "),Some("67108864"),Some("67108865"),Some("Infinity"),Some("-Infinity"),Some("0b100"),Some("1, 2")]{
        let bytes=b"verified response bytes";let dep=dependency("node_modules/fixture",bytes);let input=IvyJson::object([("operation","policy".into()),("dependency",value(&dep)),("declared",declared.map_or(IvyJson::Null,IvyJson::from)),("bodyBase64",STANDARD.encode(bytes).into())]);
        let result=portable::check_portable_response(true,declared).and_then(|_|portable::verify_portable_bytes(&dep,bytes)).map(|_|IvyJson::object([("byteLength",IvyJson::Number(bytes.len()as f64))]));
        match declared {
            None|Some("0"|"67108864")=>{succeeds(&compare(input,result));},
            Some("67108865")=>assert_eq!(result.unwrap_err().code,"content_too_large"),
            _=>assert_eq!(result.unwrap_err().code,"artifact_changed"),
        }
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

async fn optional_json(file:&Path,limit:usize)->IvyJson{
    compare(IvyJson::object([("operation","optionalJson".into()),("path",path(file)),("limit",IvyJson::Number(limit as f64))]),
        ivy_host_runtime::config::json_file_if_present(file,limit).await.map(|value|value.map_or(IvyJson::Null,|value|IvyJson::object([("present",true.into()),("value",value)]))))
}
#[tokio::test]
async fn optional_json_reader_keeps_missing_present_null_and_nonmissing_errors_distinct(){
    let fixture=Fixture::new();
    for bytes in [b"null".as_slice(),b"false",b"0",b"[]",br#"{"name":"fixture"}"#,b"{\"name\":\"\xff\"}"]{
        let file=fixture.write("value.json",bytes);
        let result=optional_json(&file,bytes.len()).await;succeeds(&result);
        assert_eq!(result.required("value").unwrap().get("present"),Some(&true.into()));
        refuses(&optional_json(&file,bytes.len()-1).await,"invalid_arguments");
    }
    for bytes in [b"{".as_slice(),b"\xef\xbb\xbfnull"]{
        let file=fixture.write("syntax.json",bytes);refuses(&optional_json(&file,64*1024).await,"internal_error");
    }
    for file in [fixture.0.join("absent"),fixture.0.join("absent/child")]{
        let result=optional_json(&file,64*1024).await;succeeds(&result);assert_eq!(result.get("value"),Some(&IvyJson::Null));
    }
    refuses(&optional_json(&fixture.0,64*1024).await,"invalid_arguments");
    let parent=fixture.write("parent-file",b"null");
    let result=optional_json(&parent.join("child"),64*1024).await;
    #[cfg(windows)]{succeeds(&result);assert_eq!(result.get("value"),Some(&IvyJson::Null));}
    #[cfg(not(windows))]refuses(&result,"internal_error");
    refuses(&optional_json(&fixture.0.join("invalid\0name"),64*1024).await,"internal_error");
    #[cfg(windows)]{
        use std::os::windows::fs::OpenOptionsExt;
        let file=fixture.write("held.json",b"null");
        let held=fs::OpenOptions::new().read(true).share_mode(0).open(&file).unwrap();
        let result=optional_json(&file,64*1024).await;
        if result.get("ok")==Some(&true.into()){assert_eq!(result.required("value").unwrap().get("present"),Some(&true.into()));}
        else{refuses(&result,"internal_error");}drop(held);
        succeeds(&optional_json(&file,64*1024).await);
    }
}
#[tokio::test]
async fn optional_json_reader_follows_file_links_and_dangling_links_are_missing(){
    let fixture=Fixture::new();let target=fixture.write("target.json",b"null");let link=fixture.0.join("link.json");
    #[cfg(windows)]let created=std::os::windows::fs::symlink_file(&target,&link);
    #[cfg(unix)]let created=std::os::unix::fs::symlink(&target,&link);
    if let Err(error)=created{
        assert_ne!(std::env::var("IVY_REQUIRE_FILE_SYMLINKS").as_deref(),Ok("1"),"required native file link unavailable: {error}");
        if let Some(file)=std::env::var_os("IVY_HOST_PORTABLE_CORPUS"){
            let _guard=CORPUS.lock().unwrap();writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(),"{}",
                json::stringify(&IvyJson::object([("operation","optionalJsonLinkAvailability".into()),("available",false.into()),("nativeError",error.to_string().into())]),usize::MAX).unwrap()).unwrap();
        }
        return;
    }
    let result=optional_json(&link,64*1024).await;succeeds(&result);
    assert_eq!(result.required("value").unwrap().get("present"),Some(&true.into()));
    fs::remove_file(target).unwrap();
    let result=optional_json(&link,64*1024).await;succeeds(&result);assert_eq!(result.get("value"),Some(&IvyJson::Null));
}
