use super::{ServiceFixture,assert_json_eq,fixture_node};
use ivy_host_runtime::{IvyJson,artifact,config,portable_native_files::portable_native_files};
use std::{fs,io::Write,path::PathBuf,process::{Command,Stdio},sync::Mutex};
static CORPUS:Mutex<()>=Mutex::new(());
fn record(case:&str,candidate:&IvyJson,result:&ivy_host_runtime::IvyResult<IvyJson>){if let Some(file)=std::env::var_os("IVY_HOST_NATIVE_PREFLIGHT_CORPUS"){let _guard=CORPUS.lock().unwrap();let outcome=match result{Ok(value)=>IvyJson::object([("ok",true.into()),("value",value.clone())]),Err(error)=>IvyJson::object([("ok",false.into()),("error",error.to_wire())])};writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(),"{}",ivy_contracts::json::stringify(&IvyJson::object([("case",case.into()),("candidate",candidate.clone()),("actual",outcome)]),usize::MAX).unwrap()).unwrap();}}

fn platform()->String{format!("{}-{}",if cfg!(windows){"win32"}else{std::env::consts::OS},match std::env::consts::ARCH{"x86_64"=>"x64","aarch64"=>"arm64",v=>v})}
fn binary(component:&str)->String{format!("dist/native/{}/ivy-{}{}",platform(),component,if cfg!(windows){".exe"}else{""})}
fn stamp()->String{format!("dist/native/{}/build-info.json",platform())}
async fn fixture(component:&str,main:&str,health:&str)->(ServiceFixture,IvyJson,IvyJson){
    let(f,mut candidate,mut manifest)=super::artifact_cases::fixture().await;let root=f.0.join("artifact");
    candidate.insert("componentId",component.into());manifest.insert("componentId",component.into());manifest.insert("kind","native".into());manifest.remove("app");
    let command=|path:&str|IvyJson::object([("executable","node".into()),("args",IvyJson::Array(vec![path.into()])),("timeoutMs",IvyJson::Number(10000.0))]);
    manifest.insert("entrypoint",command(main));manifest.insert("readiness",IvyJson::object([("timeoutMs",IvyJson::Number(10000.0)),("command",command(health))]));
    manifest.insert("shutdown",IvyJson::object([("timeoutMs",IvyJson::Number(5000.0))]));manifest.insert("restart",IvyJson::object([("policy","never".into()),("minimumDelayMs",IvyJson::Number(100.0)),("maximumDelayMs",IvyJson::Number(100.0))]));
    config::atomic_json(root.join("manifest.json"),manifest.clone()).await.unwrap();
    let input=IvyJson::object([("plan",manifest.clone()),("root",f.0.join("builder").to_str().unwrap().into()),("outputRoot",root.join("dist").to_str().unwrap().into())]);
    let mut child=Command::new("node").arg("tests/fixtures/host-native-dispatch-fixture.mjs").current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();child.stdin.take().unwrap().write_all(ivy_contracts::json::stringify(&input,usize::MAX).unwrap().as_bytes()).unwrap();let output=child.wait_with_output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    for file in [binary(component),binary("health")].into_iter().chain(if cfg!(windows){portable_native_files(component).iter().map(|file|file.to_string()).collect::<Vec<_>>()}else{vec![]}){let file=root.join(file);fs::create_dir_all(file.parent().unwrap()).unwrap();fs::write(file,b"native fixture bytes").unwrap();}
    config::atomic_json(root.join(stamp()),IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("buildId",candidate.required("buildId").unwrap().clone()),("version",manifest.required("version").unwrap().clone())])).await.unwrap();
    (f,candidate,manifest)
}
async fn verify(f:&ServiceFixture,candidate:&IvyJson)->ivy_host_runtime::IvyResult<IvyJson>{artifact::verify_candidate(candidate.clone(),f.0.join("artifact"),&fixture_node("24.18.0")).await}
async fn launch(f:&ServiceFixture,candidate:&IvyJson)->ivy_host_runtime::IvyResult<()>{let manifest=artifact::read_release_manifest(f.0.join("artifact/manifest.json")).await?;artifact::verify_launch_candidate_with_manifest(candidate.clone(),f.0.join("artifact"),None,manifest).await}

#[tokio::test]
async fn native_dispatch_preflight_recognizes_actual_shared_builder_bytes_and_selected_identity(){
    for(main,health)in[("dist/main.js","dist/health.js"),("dist/src/main.js","dist/native/health.js"),("dist/native/launch/main.js","dist/health/check.js"),("dist/a//main.js","dist/a/b/health.js")]{
        let(f,candidate,manifest)=fixture("fixture",main,health).await;let result=verify(&f,&candidate).await;record(main,&candidate,&result);assert_json_eq(&result.unwrap(),&manifest);launch(&f,&candidate).await.unwrap();
    }
}

#[tokio::test]
async fn native_launch_refuses_incomplete_counterparts_current_target_and_wrong_stamp_before_execution(){
    for mode in ["dispatcher","service","health","stamp","main","health-shim","changed-main","changed-health","empty-service","wrong-build","wrong-version","wrong-component","wrong-schema","other-platform","job"]{
        let(f,candidate,_)=fixture(if mode=="job"{"service-manager"}else{"fixture"},"dist/main.js","dist/health.js").await;let root=f.0.join("artifact");
        match mode{
            "dispatcher"=>fs::remove_file(root.join("dist/native/dispatch.mjs")).unwrap(),"service"=>fs::remove_file(root.join(binary("fixture"))).unwrap(),"health"=>fs::remove_file(root.join(binary("health"))).unwrap(),"stamp"=>fs::remove_file(root.join(stamp())).unwrap(),"main"=>fs::remove_file(root.join("dist/main.js")).unwrap(),"health-shim"=>fs::remove_file(root.join("dist/health.js")).unwrap(),
            "changed-main"=>fs::write(root.join("dist/main.js"),b"throw Error('never evaluated');").unwrap(),"changed-health"=>fs::write(root.join("dist/health.js"),b"throw Error('never evaluated');").unwrap(),"empty-service"=>fs::write(root.join(binary("fixture")),b"").unwrap(),
            "wrong-build"|"wrong-version"|"wrong-component"|"wrong-schema"=>{let path=root.join(stamp());let mut value=config::json_file(path.clone(),1024).await.unwrap();match mode{"wrong-build"=>value.insert("buildId",format!("sha256:{}","b".repeat(64)).into()),"wrong-version"=>value.insert("version","2.0.0".into()),"wrong-component"=>value.insert("componentId","other".into()),_=>value.insert("schemaVersion",IvyJson::Number(2.0))};config::atomic_json(path,value).await.unwrap();},
            "other-platform"=>{let current=root.join("dist/native").join(platform());fs::rename(current,root.join("dist/native/other-x64")).unwrap();},
            "job"=>{if cfg!(windows){fs::remove_file(root.join("dist/native/ivy-job.exe")).unwrap();}else{fs::remove_file(root.join(binary("health"))).unwrap();}},_=>unreachable!(),
        }
        let result=verify(&f,&candidate).await;record(mode,&candidate,&result);assert!(result.is_err(),"candidate: {mode}");assert!(launch(&f,&candidate).await.is_err(),"launch: {mode}");
    }
}

#[tokio::test]
async fn native_dispatch_refuses_linked_payload_files_and_target_directories_without_following_them(){
    for mode in ["service","health-shim","dispatcher","inside-directory","outside-directory"]{
        let(f,candidate,_)=fixture("fixture","dist/main.js","dist/health.js").await;let root=f.0.join("artifact");
        if mode.ends_with("directory"){
            let selected=root.join("dist/native").join(platform());let outside=if mode=="inside-directory"{root.join("elsewhere")}else{f.0.join("outside")};fs::rename(&selected,&outside).unwrap();
            #[cfg(windows)]{let script="require('node:fs').symlinkSync(process.argv[1],process.argv[2],'junction')";assert!(Command::new("node").arg("-e").arg(script).arg(&outside).arg(&selected).status().unwrap().success());}
            #[cfg(unix)]std::os::unix::fs::symlink(&outside,&selected).unwrap();
        }else{
            let selected=root.join(match mode{"service"=>binary("fixture"),"health-shim"=>"dist/health.js".into(),_=>"dist/native/dispatch.mjs".into()});let outside=f.0.join("retained-payload");fs::rename(&selected,&outside).unwrap();
            #[cfg(windows)]std::os::windows::fs::symlink_file(&outside,&selected).unwrap();
            #[cfg(unix)]std::os::unix::fs::symlink(&outside,&selected).unwrap();
        }
        let result=verify(&f,&candidate).await;record(mode,&candidate,&result);assert!(result.is_err(),"{mode}");assert!(launch(&f,&candidate).await.is_err(),"{mode}");
    }
}

#[tokio::test]
async fn ordinary_nonmatching_v1_entries_keep_legacy_preflight_without_evaluating_javascript(){
    let(f,candidate,manifest)=fixture("fixture","dist/main.js","dist/health.js").await;let root=f.0.join("artifact");
    // Neither exact shim remains, so this is deliberately the ordinary v1 path.
    // The absent marker cannot attest arbitrary modified-package equivalence.
    fs::write(root.join("dist/main.js"),b"throw Error('preflight must not execute main');").unwrap();fs::write(root.join("dist/health.js"),b"throw Error('preflight must not execute health');").unwrap();fs::remove_dir_all(root.join("dist/native")).unwrap();
    let result=verify(&f,&candidate).await;record("ordinary-v1-entries-no-evaluation",&candidate,&result);assert_json_eq(&result.unwrap(),&manifest);launch(&f,&candidate).await.unwrap();
    super::compare_config_oracle(IvyJson::object([("kind","artifact".into()),("operation","verifyCandidate".into()),("args",IvyJson::Array(vec![candidate.clone(),IvyJson::object([("artifactRoot",root.to_str().unwrap().into())])])),("node","v24.18.0".into())]),Ok(manifest.clone()));
    fs::remove_file(root.join("manifest.json")).unwrap();artifact::verify_launch_candidate_with_manifest(candidate.clone(),root.clone(),None,manifest).await.unwrap();
    super::compare_config_oracle(IvyJson::object([("kind","artifact".into()),("operation","verifyLaunchCandidate".into()),("args",IvyJson::Array(vec![candidate.clone(),IvyJson::object([("artifactRoot",root.to_str().unwrap().into())]),IvyJson::Null]))]),artifact::verify_launch_candidate(candidate,root,None).await.map(|_|IvyJson::Null));
}
