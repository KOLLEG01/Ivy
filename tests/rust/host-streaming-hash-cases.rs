use ivy_host_runtime::{IvyJson,node_path,streaming_hash};
use ivy_contracts::json;
use std::{fs,io::Write,path::{Path,PathBuf},process::{Command,Stdio},time::{SystemTime,UNIX_EPOCH}};
struct Fixture(PathBuf);
impl Fixture{fn new()->Self{let path=std::env::temp_dir().join(format!("ivy-stream-hash-{}-{}",std::process::id(),SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));fs::create_dir(&path).unwrap();Self(path)}}
impl Drop for Fixture{fn drop(&mut self){let _=fs::remove_dir_all(&self.0);}}
fn root()->PathBuf{std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(||PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),PathBuf::from)}
async fn compare(path:&Path)->IvyJson{
    let input=IvyJson::object([("path",IvyJson::String(node_path::from_native(path)))]);
    let mut child=Command::new("node").args(["--experimental-transform-types","--disable-warning=ExperimentalWarning","tests/fixtures/host-streaming-hash-oracle.mjs"]).current_dir(root()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(json::stringify(&input,usize::MAX).unwrap().as_bytes()).unwrap();
    let actual=match streaming_hash::file_hash(path).await{Ok(hash)=>IvyJson::object([("ok",true.into()),("value",hash.into())]),Err(error)=>IvyJson::object([("ok",false.into()),("error",error.to_wire())])};
    let output=child.wait_with_output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));let expected=json::parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap();
    if let Some(corpus)=std::env::var_os("IVY_HOST_STREAMING_HASH_CORPUS"){writeln!(fs::OpenOptions::new().create(true).append(true).open(corpus).unwrap(),"{}",json::stringify(&IvyJson::object([("input",input),("actual",actual.clone()),("expected",expected.clone())]),usize::MAX).unwrap()).unwrap();}
    assert_eq!(actual,expected);actual
}
#[tokio::test]
async fn streaming_hash_matches_original_empty_binary_large_and_non_executable_names(){
    let fixture=Fixture::new();
    for (name,bytes) in [("empty.data",vec![]),("cli ü 空 😀.js",b"opaque bytes\0\xff\xfe".to_vec()),("large.payload",(0..4*1024*1024+17).map(|n|(n%251)as u8).collect())]{let file=fixture.0.join(name);fs::write(&file,&bytes).unwrap();let result=compare(&file).await;assert_eq!(result.get("ok"),Some(&true.into()));assert_eq!(result.get("value"),Some(&ivy_contracts::hash::digest(&bytes).into()));}
}
#[tokio::test]
async fn streaming_hash_matches_original_missing_directory_and_invalid_path_errors(){
    let fixture=Fixture::new();for file in [fixture.0.clone(),fixture.0.join("missing"),fixture.0.join("invalid\0path")]{assert_eq!(compare(&file).await.get("ok"),Some(&false.into()));}
}
#[cfg(windows)]
#[tokio::test]
async fn streaming_hash_preserves_node_read_open_with_exclusive_windows_sharing(){
    use std::os::windows::fs::OpenOptionsExt;
    let fixture=Fixture::new();let file=fixture.0.join("normal-cli.data");fs::write(&file,b"normal CLI hash has no executable suffix policy").unwrap();
    let held=fs::OpenOptions::new().read(true).share_mode(0).open(&file).unwrap();compare(&file).await;drop(held);
}
