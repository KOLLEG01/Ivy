use super::{ServiceFixture,compare_oracle_fixture};
use ivy_host_runtime::{IvyJson,IvyResult,node_runtime::NodeRuntime,preparation_tools,runtime_dependencies};
use ivy_contracts::JsString;
use std::{fs,io::Write,path::{Path,PathBuf},sync::Mutex};
static CORPUS:Mutex<()>=Mutex::new(());
fn json(source:&str)->IvyJson{ivy_contracts::json::parse(source,usize::MAX).unwrap()}
fn path(path:&Path)->IvyJson{IvyJson::String(ivy_host_runtime::node_path::from_native(path))}
fn one_package(value:IvyJson)->IvyJson{IvyJson::object([("lockfileVersion",IvyJson::Number(3.0)),("packages",IvyJson::object([("node_modules/a",value)]))])}
fn compare(input:IvyJson,result:IvyResult<IvyJson>){
    if let Some(file)=std::env::var_os("IVY_HOST_BUILD_INPUT_CORPUS"){
        let _guard=CORPUS.lock().unwrap();
        let actual=match &result{Ok(value)=>IvyJson::object([("ok",true.into()),("value",value.clone())]),Err(error)=>IvyJson::object([("ok",false.into()),("error",error.to_wire())])};
        writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(),"{}",ivy_contracts::json::stringify(&IvyJson::object([("input",input.clone()),("actual",actual)]),usize::MAX).unwrap()).unwrap();
    }
    compare_oracle_fixture(input,result,"tests/fixtures/host-build-input-oracle.mjs");
}
fn dependency(operation:&str,lock:IvyJson,names:Vec<JsString>){
    let mut args=vec![lock.clone()];
    let result=match operation{
        "developmentPackageLocations"=>runtime_dependencies::development_package_locations(&lock).map(|values|IvyJson::Array(values.into_iter().map(IvyJson::String).collect())),
        "workspacePackageLocations"=>runtime_dependencies::workspace_package_locations(&lock).map(|values|IvyJson::Array(values.into_iter().map(|(location,target)|IvyJson::Array(vec![IvyJson::String(location),IvyJson::String(target)])).collect())),
        _=>{args.push(IvyJson::Array(names.iter().cloned().map(IvyJson::String).collect()));runtime_dependencies::runtime_package_locations(&lock,names).map(|values|IvyJson::Array(values.into_iter().map(IvyJson::String).collect()))},
    };
    compare(IvyJson::object([("kind","runtimeDependencies".into()),("operation",operation.into()),("args",IvyJson::Array(args))]),result);
}

#[test]
fn exact_npm_locations_keep_development_workspace_and_runtime_closure_order(){
    let lock=json(r#"{"lockfileVersion":3,"packages":{"":{},"node_modules/dev":{"dev":true},"node_modules/dev-extra":{"devOptional":true},"node_modules/@scope/dev":{"dev":true,"optional":true},"node_modules/workspace":{"link":true,"resolved":"packages/workspace"},"packages/workspace":{},"node_modules/runtime":{"dependencies":{"shared":"1","nested":"1"},"optionalDependencies":{"optional":"1","absent":"1"},"peerDependencies":{"peer":"1"}},"node_modules/shared":{"dependencies":{"leaf":"1"}},"node_modules/leaf":{"dependencies":{"runtime":"1"}},"node_modules/runtime/node_modules/nested":{"dependencies":{"shared":"1"}},"node_modules/optional":{},"node_modules/peer":{}}}"#);
    dependency("developmentPackageLocations",lock.clone(),vec![]);
    dependency("workspacePackageLocations",lock.clone(),vec![]);
    dependency("runtimePackageLocations",lock.clone(),vec!["runtime".into(),"shared".into(),"runtime".into()]);
    dependency("runtimePackageLocations",lock.clone(),vec!["workspace".into()]);
    dependency("runtimePackageLocations",lock,vec!["unlocked".into()]);
}

#[test]
fn lock_classification_and_empty_requests_preserve_original_boundaries(){
    let mut cases=vec![IvyJson::Null,json("[]"),json("{}"),json(r#"{"lockfileVersion":2,"packages":{}}"#),json(r#"{"lockfileVersion":3}"#),json(r#"{"lockfileVersion":3,"packages":null}"#),json(r#"{"lockfileVersion":3,"packages":[]}"#)];
    for value in [IvyJson::Null,IvyJson::Bool(false),IvyJson::Number(0.0),"yes".into(),IvyJson::Array(vec![])]{
        cases.push(one_package(value));
    }
    for field in ["dev","devOptional","optional","link"]{
        for value in [IvyJson::Null,"true".into(),IvyJson::Number(1.0),IvyJson::Bool(true),IvyJson::Bool(false)]{
            cases.push(one_package(IvyJson::object([(field,value)])));
        }
    }
    cases.push(json(r#"{"lockfileVersion":3,"packages":{"node_modules/a":{"dev":true,"devOptional":true}}}"#));
    for lock in cases{
        dependency("developmentPackageLocations",lock.clone(),vec![]);dependency("workspacePackageLocations",lock.clone(),vec![]);
        dependency("runtimePackageLocations",lock.clone(),vec!["a".into()]);dependency("runtimePackageLocations",lock,vec![]);
    }
}

#[test]
fn npm_path_name_and_workspace_utf16_rules_match_original_without_prefix_guesses(){
    for location in ["node_modules/../retained","node_modules/@scope/../../retained","node_modules/a/file.js","node_modules/a/node_modules/","node_modules//a","node_modules/A_~-/node_modules/@scope/B.1","node_modules/ſ","node_modules/K","node_modules/@scope/a", "ordinary/non-package"]{
        let lock=IvyJson::object([("lockfileVersion",IvyJson::Number(3.0)),("packages",IvyJson::object([(location,IvyJson::object([("dev",true.into())]))]))]);
        dependency("developmentPackageLocations",lock,vec![]);
    }
    let mut targets:Vec<JsString>=["../outside","/outside","C:/outside","c:\\outside","","packages//a","packages/../a","packages\\a","packages/./a","packages/a","C:relative","packages/ü 空 😀"].into_iter().map(Into::into).collect();
    targets.push(JsString(vec![0xd800]));targets.push("a".repeat(2048).into());targets.push("a".repeat(2049).into());
    for target in targets{
        let lock=IvyJson::object([("lockfileVersion",IvyJson::Number(3.0)),("packages",IvyJson::object([("node_modules/workspace",IvyJson::object([("link",true.into()),("resolved",IvyJson::String(target))]))]))]);
        dependency("workspacePackageLocations",lock,vec![]);
    }
    let lock=json(r#"{"lockfileVersion":3,"packages":{"node_modules/a":{},"node_modules/@scope/b":{},"node_modules/-":{},"node_modules/constructor":{}}}"#);
    for name in ["a","@scope/b","-","constructor","../a","a/subpath","@scope","@scope/",".name","ſ","K",""]{
        dependency("runtimePackageLocations",lock.clone(),vec![name.into()]);
    }
}

#[test]
fn required_optional_peer_and_integer_key_dependency_failures_match_original(){
    for(field,values)in [("dependencies",vec![IvyJson::Null,"bad".into(),json("[]"),json(r#"{"absent":"1"}"#)]),("optionalDependencies",vec![IvyJson::Null,json(r#"{"absent":"1"}"#)]),("peerDependencies",vec![IvyJson::Null,json(r#"{"absent":"1"}"#)])]{
        for value in values{
            let lock=one_package(IvyJson::object([(field,value)]));
            dependency("runtimePackageLocations",lock,vec!["a".into()]);
        }
    }
    dependency("runtimePackageLocations",json(r#"{"lockfileVersion":3,"packages":{"node_modules/a":{"dependencies":{"10":"1","2":"1","b":"1"}},"node_modules/2":{},"node_modules/10":{},"node_modules/b":{}}}"#),vec!["a".into()]);
}

fn node()->NodeRuntime{
    let out=std::process::Command::new("node").args(["-p","process.execPath"]).output().unwrap();assert!(out.status.success());
    NodeRuntime{executable:PathBuf::from(String::from_utf8(out.stdout).unwrap().trim()),version:"24.18.0".into()}
}
async fn tools(executables:IvyJson,bootstrap:&Path,temporary:&Path){
    let result=preparation_tools::preparation_tools(executables.clone(),bootstrap.to_owned(),temporary.to_owned(),node()).await.map(|tools|{
        IvyJson::object([("environment",IvyJson::object(tools.environment.into_iter().map(|(key,value)|(key.to_string_lossy().into_owned(),IvyJson::String(ivy_host_runtime::node_path::node_utf8(&value))))))])
    });
    compare(IvyJson::object([("kind","preparationTools".into()),("args",IvyJson::Array(vec![executables,path(bootstrap),path(temporary)]))]),result);
}
#[tokio::test]
async fn explicit_preparation_tools_normalize_paths_and_private_overrides(){
    let fixture=ServiceFixture::new();let bootstrap=fixture.0.join("bootstrap");fs::create_dir_all(bootstrap.join("dist/native")).unwrap();fs::write(bootstrap.join("dist/native/ivy-job.exe"),b"metadata only; never executed").unwrap();
    let temporary=fixture.0.join("temporary ü 空 😀");fs::create_dir(&temporary).unwrap();
    let executable=fixture.0.join("compiler.exe");fs::write(&executable,b"metadata only; never executed").unwrap();
    tools(IvyJson::object([("dotnet",path(&executable)),("gcc",path(&executable)),("$node","overridden relative node".into()),("$job","overridden relative job".into())]),&bootstrap,&temporary).await;
    let linked=fixture.0.join("linked-temp");
    #[cfg(windows)]std::os::windows::fs::symlink_dir(&temporary,&linked).unwrap();
    #[cfg(unix)]std::os::unix::fs::symlink(&temporary,&linked).unwrap();
    tools(IvyJson::object([("DOTNET",path(&executable)),("other",path(&executable))]),&bootstrap,&linked).await;
    let file=fixture.0.join("temp-file");fs::write(&file,b"original only resolves temp; no directory check").unwrap();tools(IvyJson::object([]as[(&str,IvyJson);0]),&bootstrap,&file).await;
}
#[tokio::test]
async fn preparation_tool_mapping_errors_keep_original_order_and_direct_file_checks(){
    let fixture=ServiceFixture::new();let bootstrap=fixture.0.join("bootstrap");fs::create_dir_all(bootstrap.join("dist/native")).unwrap();fs::write(bootstrap.join("dist/native/ivy-job.exe"),b"metadata only").unwrap();
    for name in ["file.cmd","file.bat","file.PS1","directory","missing"]{
        let selected=fixture.0.join(name);if name=="directory"{fs::create_dir(&selected).unwrap();}else if name!="missing"{fs::write(&selected,b"metadata only").unwrap();}
        tools(IvyJson::object([("tool",path(&selected))]),&bootstrap,&fixture.0).await;
    }
    tools(IvyJson::object([("tool","relative".into())]),&bootstrap,&fixture.0).await;
    tools(IvyJson::object([("1",path(&fixture.0.join("missing"))),("a","relative".into())]),&bootstrap,&fixture.0).await;
    tools(IvyJson::object([("a","relative".into()),("1",path(&fixture.0.join("missing")))]),&bootstrap,&fixture.0).await;
    tools(IvyJson::object([]as[(&str,IvyJson);0]),&bootstrap,&fixture.0.join("missing-temp")).await;
}
