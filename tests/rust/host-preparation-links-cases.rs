use super::{IvyError,IvyJson,IvyResult,concurrency,filesystem,node_path,preparation_links};
use std::{fs,io::Write,path::{Path,PathBuf},process::{Command,Stdio},sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}},time::{Duration,SystemTime,UNIX_EPOCH}};
static CORPUS:Mutex<()>=Mutex::new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new()->Self { let root=std::env::temp_dir().join(format!("ivy-preparation-links-{}-{}",std::process::id(),SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));fs::create_dir(&root).unwrap();Self(root) }
    fn write(&self,local:&str,bytes:&[u8])->PathBuf { let file=self.0.join(local);fs::create_dir_all(file.parent().unwrap()).unwrap();fs::write(&file,bytes).unwrap();file }
}
impl Drop for Fixture { fn drop(&mut self) {
    #[cfg(windows)]fn writable(path:&Path){if let Ok(metadata)=fs::symlink_metadata(path){if metadata.file_type().is_symlink(){return}if metadata.is_dir(){if let Ok(entries)=fs::read_dir(path){for entry in entries.flatten(){writable(&entry.path())}}}if metadata.permissions().readonly(){let mut permissions=metadata.permissions();permissions.set_readonly(false);let _=fs::set_permissions(path,permissions);}}}
    #[cfg(windows)]writable(&self.0);
    let _=fs::remove_dir_all(&self.0);
}}
fn path(path:&Path)->IvyJson { IvyJson::String(node_path::from_native(path)) }
fn root()->PathBuf { std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(||PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),PathBuf::from) }
fn append(row:IvyJson){if let Some(file)=std::env::var_os("IVY_HOST_PREPARATION_LINKS_CORPUS"){let _guard=CORPUS.lock().unwrap();writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(),"{}",ivy_contracts::json::stringify(&row,usize::MAX).unwrap()).unwrap();}}
fn wire(result:IvyResult<()>)->IvyJson { match result { Ok(())=>IvyJson::object([("ok",true.into()),("value",IvyJson::Null)]),Err(error)=>IvyJson::object([("ok",false.into()),("error",error.to_wire())]) } }
fn code(value:&IvyJson)->Option<String> {value.get("error").and_then(|error|error.get("data")).and_then(|data|data.get("code")).and_then(|value|value.as_str().ok())}
fn succeeds(value:&IvyJson) { assert_eq!(value.get("ok"),Some(&true.into())); }
fn refuses(value:&IvyJson,expected:&str) { assert_eq!(code(value).as_deref(),Some(expected)); }
fn directory_link(source:&Path,destination:&Path) {
    #[cfg(windows)]{let output=Command::new("node").args(["-e","require('node:fs').symlinkSync(process.argv[1],process.argv[2],'junction')"]).arg(source).arg(destination).output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));}
    #[cfg(unix)]std::os::unix::fs::symlink(source,destination).unwrap();
}
async fn compare(operation:&str,source:&Path,actual:&Path,original:&Path,links:Vec<(&str,&str)>)->IvyJson {
    let result=match operation {
        "directory"=>preparation_links::link_dependency_directory(source.to_owned(),actual.to_owned()).await,
        "workspace"=>preparation_links::restore_workspace_links(actual.to_owned(),links.iter().map(|(location,target)|((*location).into(),(*target).into())).collect()).await,
        _=>unreachable!(),
    };
    let actual_result=wire(result);
    let input=IvyJson::object([("operation",operation.into()),("source",path(source)),("actual",path(actual)),("original",path(original)),("links",IvyJson::Array(links.into_iter().map(|(location,target)|IvyJson::Array(vec![location.into(),target.into()])).collect()))]);
    let mut child=Command::new("node").args(["--experimental-transform-types","--disable-warning=ExperimentalWarning","tests/fixtures/host-preparation-links-oracle.mjs"]).current_dir(root()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(ivy_contracts::json::stringify(&input,usize::MAX).unwrap().as_bytes()).unwrap();
    let output=child.wait_with_output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let expected=ivy_contracts::json::parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap();
    assert_eq!(&actual_result,expected.required("operation").unwrap(),"{input:?}");
    if actual_result.get("ok")==Some(&true.into()){assert_eq!(expected.get("actualTree"),expected.get("originalTree"),"{input:?}");}
    append(IvyJson::object([("input",input),("actual",actual_result.clone()),("expected",expected)]));actual_result
}

#[tokio::test]
async fn cached_directory_packages_and_scopes_link_while_files_share_identity(){
    let fixture=Fixture::new();let source=fixture.0.join("cache ü 空 😀/node_modules");
    fixture.write("cache ü 空 😀/node_modules/pkg/deep/value",b"directory bytes");
    fixture.write("cache ü 空 😀/node_modules/@scope/pkg/deep/value",b"scoped bytes");
    fixture.write("cache ü 空 😀/node_modules/@scope/ordinary",b"scoped hardlink");
    let ordinary=fixture.write("cache ü 空 😀/node_modules/.package-lock.json",b"shared bytes");
    fs::create_dir_all(source.join("@empty")).unwrap();
    for index in 0..20 { fixture.write(&format!("cache ü 空 😀/node_modules/item-{index}"),&[index,0xff]); }
    #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;fs::set_permissions(&ordinary,fs::Permissions::from_mode(0o400)).unwrap();}
    let actual=fixture.0.join("actual/node_modules");let original=fixture.0.join("original/node_modules");
    succeeds(&compare("directory",&source,&actual,&original,vec![]).await);
    assert_eq!(filesystem::stat_identity(&ordinary).unwrap(),filesystem::stat_identity(&actual.join(".package-lock.json")).unwrap());
    fs::write(actual.join("pkg/deep/value"),b"through directory link").unwrap();assert_eq!(fs::read(source.join("pkg/deep/value")).unwrap(),b"through directory link");
    fs::write(actual.join("item-0"),b"through hardlink").unwrap();assert_eq!(fs::read(source.join("item-0")).unwrap(),b"through hardlink");
}

#[tokio::test]
async fn ordered_workspace_restore_replaces_owned_destinations_and_follows_prior_entries(){
    let fixture=Fixture::new();let actual=fixture.0.join("actual");let original=fixture.0.join("original");
    for label in ["actual","original"] {
        fixture.write(&format!("{label}/packages/first/value ü 空 😀"),b"workspace bytes");
        fixture.write(&format!("{label}/node_modules/@scope/first/old"),b"obsolete destination");
        let readonly=fixture.write(&format!("{label}/node_modules/second"),b"obsolete readonly file");
        let mut permissions=fs::metadata(&readonly).unwrap().permissions();permissions.set_readonly(true);fs::set_permissions(readonly,permissions).unwrap();
    }
    let links=vec![("node_modules/@scope/first","packages/first"),("node_modules/second","node_modules/@scope/first")];
    succeeds(&compare("workspace",&actual,&actual,&original,links).await);
    assert_eq!(fs::read(actual.join("node_modules/second/value ü 空 😀")).unwrap(),b"workspace bytes");
}

#[tokio::test]
async fn workspace_source_checks_precede_destination_removal_and_stop_in_map_order(){
    let fixture=Fixture::new();let outside=fixture.0.join("outside");fixture.write("outside/retained",b"outside unchanged");
    let scenarios=[("../escape","packages/good","target_conflict"),("node_modules/pkg","../outside","target_conflict"),("node_modules/pkg","packages/file","artifact_changed"),("node_modules/pkg","packages/missing","internal_error"),("node_modules/pkg","packages/escape","artifact_changed")];
    for(index,(location,target,error))in scenarios.into_iter().enumerate(){
        let actual=fixture.0.join(format!("actual-{index}"));let original=fixture.0.join(format!("original-{index}"));
        for workspace in [&actual,&original] {
            fs::create_dir_all(workspace.join("packages/good")).unwrap();fs::write(workspace.join("packages/file"),b"not a directory").unwrap();
            fs::create_dir_all(workspace.join("node_modules/pkg")).unwrap();fs::write(workspace.join("node_modules/pkg/retained"),b"destination unchanged").unwrap();
            directory_link(&outside,&workspace.join("packages/escape"));
        }
        refuses(&compare("workspace",&actual,&actual,&original,vec![(location,target),("node_modules/later","packages/good")]).await,error);
        assert_eq!(fs::read(actual.join("node_modules/pkg/retained")).unwrap(),b"destination unchanged");
        assert!(!actual.join("node_modules/later").exists());
    }
    assert_eq!(fs::read(outside.join("retained")).unwrap(),b"outside unchanged");
}

#[tokio::test]
async fn cache_observed_links_keep_verbatim_targets_and_native_creation_failure(){
    let fixture=Fixture::new();let source=fixture.0.join("cache/node_modules");
    fixture.write("target/retained",b"outside unchanged");fs::create_dir_all(&source).unwrap();
    directory_link(&fixture.0.join("target"),&source.join("observed-link"));
    let actual=fixture.0.join("actual");let original=fixture.0.join("original");
    let value=compare("directory",&source,&actual,&original,vec![]).await;
    if cfg!(unix)||std::env::var_os("IVY_REQUIRE_FILE_SYMLINKS").is_some(){succeeds(&value);}
    else if value.get("ok")!=Some(&true.into()) {
        let probe=fixture.0.join("privilege-probe");
        #[cfg(windows)]{let error=std::os::windows::fs::symlink_file(fixture.0.join("target/retained"),&probe).unwrap_err();assert!(error.kind()==std::io::ErrorKind::PermissionDenied||error.raw_os_error()==Some(1314));}
        append(IvyJson::object([("unavailable","successful verbatim symlink copy requires native Windows symlink privilege".into()),("scope","the actual original/Rust refusal was compared; successful creation was not qualified".into())]));
    }
    assert_eq!(fs::read(fixture.0.join("target/retained")).unwrap(),b"outside unchanged");
}

#[tokio::test]
async fn existing_entries_and_missing_sources_keep_error_and_existing_bytes(){
    let fixture=Fixture::new();let source=fixture.0.join("cache");fixture.write("cache/pkg/value",b"cache bytes");
    for label in ["actual","original"] {fixture.write(&format!("{label}/pkg"),b"existing destination");}
    refuses(&compare("directory",&source,&fixture.0.join("actual"),&fixture.0.join("original"),vec![]).await,"internal_error");
    assert_eq!(fs::read(fixture.0.join("actual/pkg")).unwrap(),b"existing destination");
    let missing=fixture.0.join("missing");
    refuses(&compare("directory",&missing,&fixture.0.join("missing-actual"),&fixture.0.join("missing-original"),vec![]).await,"internal_error");
    assert!(fixture.0.join("missing-actual").is_dir());
}

#[tokio::test]
async fn shared_worker_core_retains_first_failure_and_drains_admitted_work(){
    let started=Arc::new(AtomicUsize::new(0));let finished=Arc::new(AtomicUsize::new(0));let barrier=Arc::new(tokio::sync::Barrier::new(3));
    let error=concurrency::map_concurrent((0..20).collect(),3,|item,_| {
        let started=started.clone();let finished=finished.clone();let barrier=barrier.clone();
        async move {
            started.fetch_add(1,Ordering::SeqCst);barrier.wait().await;
            if item==0{return Err(IvyError::new("expected_worker_failure","first retained error"));}
            tokio::time::sleep(Duration::from_millis(30)).await;finished.fetch_add(1,Ordering::SeqCst);Ok(item)
        }
    }).await.unwrap_err();
    assert_eq!(error.code,"expected_worker_failure");assert_eq!(started.load(Ordering::SeqCst),3);assert_eq!(finished.load(Ordering::SeqCst),2);
    assert_eq!(concurrency::map_concurrent((0..17).collect(),8,|item,_|async move{tokio::time::sleep(Duration::from_millis((17-item) as u64)).await;Ok(item*item)}).await.unwrap(),(0..17).map(|value|value*value).collect::<Vec<_>>());
}

#[tokio::test]
async fn cross_device_file_copy_fallback_uses_a_separately_supplied_native_volume(){
    let Some(temporary)=std::env::var_os("IVY_HOST_CROSS_DEVICE_TMP") else {
        append(IvyJson::object([("unavailable","native cross-device hardlink/copy fallback".into()),("reason","IVY_HOST_CROSS_DEVICE_TMP is not supplied; no volume is provisioned or guessed".into())]));
        assert!(std::env::var_os("IVY_REQUIRE_CROSS_DEVICE").is_none(),"native cross-device qualification requires its fixture root");return;
    };
    let fixture=Fixture::new();let source=fixture.0.join("cache");let file=fixture.write("cache/file",b"cross-device bytes");
    let temporary=PathBuf::from(temporary);assert!(temporary.is_absolute()&&temporary.is_dir());
    let destination=temporary.join(fixture.0.file_name().unwrap());fs::create_dir(&destination).unwrap();let cross=Fixture(destination);
    let error=fs::hard_link(&file,cross.0.join("must-not-link")).unwrap_err();
    #[cfg(windows)]assert_eq!(error.raw_os_error(),Some(17));
    #[cfg(unix)]assert_eq!(error.raw_os_error(),Some(libc::EXDEV));
    succeeds(&compare("directory",&source,&cross.0.join("actual"),&cross.0.join("original"),vec![]).await);
    assert_eq!(fs::read(cross.0.join("actual/file")).unwrap(),b"cross-device bytes");
    assert_ne!(filesystem::stat_identity(&file).unwrap(),filesystem::stat_identity(&cross.0.join("actual/file")).unwrap());
}

#[tokio::test]
async fn copied_links_preserve_existing_target_guards_before_replacement(){
    let fixture=Fixture::new();let target=fixture.0.join("target");
    fixture.write("target/child/nested/retained",b"unchanged source");fixture.write("other/retained",b"unchanged other");
    for(index,kind)in ["replace","same-target","descendant","ancestor","regular","dangling","dangling-existing"].into_iter().enumerate(){
        let source=fixture.0.join(format!("cache-{index}"));fs::create_dir(&source).unwrap();
        let source_target=if kind.starts_with("dangling"){fixture.0.join("absent-target")}else{target.join("child")};
        directory_link(&source_target,&source.join("link"));
        let actual=fixture.0.join(format!("actual-{index}"));let original=fixture.0.join(format!("original-{index}"));
        for destination in [&actual,&original] {
            fs::create_dir(destination).unwrap();
            let link=destination.join("link");
            match kind {
                "replace"|"dangling-existing"=>directory_link(&fixture.0.join("other"),&link),
                "same-target"=>directory_link(&target.join("child"),&link),
                "descendant"=>directory_link(&target.join("child/nested"),&link),
                "ancestor"=>directory_link(&target,&link),
                "regular"=>fs::write(link,b"existing file").unwrap(),
                _=>(),
            }
        }
        let value=compare("directory",&source,&actual,&original,vec![]).await;
        if ["same-target","descendant","ancestor","regular","dangling-existing"].contains(&kind){
            refuses(&value,"internal_error");
            if kind=="regular"{assert_eq!(fs::read(actual.join("link")).unwrap(),b"existing file");}
            else {assert!(fs::symlink_metadata(actual.join("link")).unwrap().file_type().is_symlink());}
        }else if cfg!(unix)||std::env::var_os("IVY_REQUIRE_FILE_SYMLINKS").is_some(){succeeds(&value);}
        assert_eq!(fs::read(target.join("child/nested/retained")).unwrap(),b"unchanged source");
        assert_eq!(fs::read(fixture.0.join("other/retained")).unwrap(),b"unchanged other");
    }
}
