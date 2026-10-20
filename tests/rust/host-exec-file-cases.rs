use ivy_host_runtime::{exec_file::{self,FileCommand,FileOptions,FileFailure,FileResult},process::OutputStream};
use ivy_contracts::{IvyJson,JsString,json};
use std::{collections::BTreeMap,path::PathBuf,ffi::OsString,time::Duration};
use tokio_util::sync::CancellationToken;
fn os_json(value:&std::ffi::OsStr)->IvyJson {IvyJson::String(ivy_host_runtime::node_path::from_native(std::path::Path::new(value)))}

fn node() -> PathBuf {
    let output=std::process::Command::new("node").args(["-p","process.execPath"]).output().unwrap();
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}
fn command(mode:&str) -> FileCommand {
    FileCommand {executable:node().into_os_string(),args:vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/host-exec-file-child.mjs").into_os_string(),mode.into()],cwd:None,timeout_ms:3000,maximum_buffer_bytes:65536}
}
fn options() -> FileOptions {
    FileOptions {#[cfg(windows)]job_launcher:Some(PathBuf::from(env!("CARGO_BIN_EXE_ivy-job"))),..Default::default()}
}
async fn run(command:FileCommand,options:FileOptions)->FileResult {exec_file::exec_file(command,PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),options).await.unwrap()}
fn project(result:&FileResult)->IvyJson {
    let failure=match result.failure {None=>IvyJson::Null,Some(FileFailure::Spawn)=>"spawn".into(),Some(FileFailure::Exit)=>"exit".into(),Some(FileFailure::Timeout)=>"timeout".into(),Some(FileFailure::MaxBuffer(_))=>"maxBuffer".into(),_=>"unknown".into()};
    IvyJson::object([("stdout",IvyJson::String(result.stdout.clone())),("stderr",IvyJson::String(result.stderr.clone())),("failure",failure)])
}
fn oracle(command:&FileCommand,environment:Option<&BTreeMap<OsString,OsString>>)->IvyJson {
    use std::{io::Write,process::{Command,Stdio}};
    let cwd=command.cwd.clone().unwrap_or_else(||std::env::current_dir().unwrap());
    let mut input=IvyJson::object([("file",os_json(&command.executable)),("args",IvyJson::Array(command.args.iter().map(|s|os_json(s)).collect())),("cwd",os_json(cwd.as_os_str())),("maximum",IvyJson::Number(command.maximum_buffer_bytes as f64)),("timeout",IvyJson::Number(command.timeout_ms as f64))]);
    if let Some(environment)=environment {input.insert("env",IvyJson::object(environment.iter().map(|(k,v)|(k.to_string_lossy().to_string(),v.to_string_lossy().to_string().into()))));}
    let mut child=Command::new("node").arg("tests/fixtures/host-exec-file-oracle.mjs").current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(json::stringify(&input,usize::MAX).unwrap().as_bytes()).unwrap();let output=child.wait_with_output().unwrap();assert!(output.status.success());json::parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap()
}

#[cfg(windows)]
#[test]
fn private_job_argv0_is_lossless_optional_and_rejects_missing_values() {
    use std::{os::windows::ffi::OsStringExt,process::Command};
    let root=super::Root::new();let executable=root.0.join("argv0-receiver.exe");
    let status=Command::new("rustc").arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/host-argv0-receiver.rs")).arg("-o").arg(&executable).status().unwrap();assert!(status.success());
    let values:Vec<Option<OsString>>=vec![None,Some("".into()),Some("original spaces and trailing\\".into()),Some(OsString::from_wide(&[0xd800,32,0xdc00,92]))];
    for (index,value) in values.into_iter().enumerate() {
        let output=root.0.join(format!("args-{index}.json"));let mut helper=Command::new(env!("CARGO_BIN_EXE_ivy-job"));helper.args(["--parent","0"]);
        if let Some(value)=&value {helper.arg("--argv0").arg(value);}
        let tail:Vec<OsString>=vec!["".into(),"spaces and \"quotes\"".into(),"trailing\\".into(),OsString::from_wide(&[0xd800,34,32,0xdc00,92])];
        let result=helper.arg(&executable).args(&tail).env("IVY_EXEC_FILE_ARGUMENTS",&output).output().unwrap();assert!(result.status.success(),"index={index}: {}",String::from_utf8_lossy(&result.stderr));
        let args=json::parse(&std::fs::read_to_string(output).unwrap(),usize::MAX).unwrap();let args=args.as_array().unwrap();
        let as_units=|value:&std::ffi::OsStr| {use std::os::windows::ffi::OsStrExt;IvyJson::Array(value.encode_wide().map(|n|IvyJson::Number(n as f64)).collect())};
        assert_eq!(args[0],as_units(value.as_deref().unwrap_or(executable.as_os_str())));
        assert_eq!(&args[1..],&tail.iter().map(|s|as_units(s)).collect::<Vec<_>>());
    }
    for args in [vec!["--parent","0","--argv0"],vec!["--parent","0","--argv0","original"],vec!["--parent","0","--argv0","bad\"file","executable"]] {
        let output=Command::new(env!("CARGO_BIN_EXE_ivy-job")).args(args).output().unwrap();assert!(!output.status.success());
    }
}

#[tokio::test]
async fn exec_file_matches_node_utf8_arguments_environment_exit_and_buffer_failures() {
    for mode in ["text","args","exit","missing","stdout","stderr"] {
        let mut command=command(if matches!(mode,"stdout"|"stderr") {"large"}else{mode});
        if matches!(mode,"stdout"|"stderr") {command.args.push(mode.into());command.maximum_buffer_bytes=1024;}
        if mode=="missing" {command.executable=std::env::temp_dir().join("ivy-missing-executable-98713").into_os_string();}
        if mode=="args" {command.args.extend(["".into(),"spaces and quotes\"".into(),"trailing\\".into(),"€😀".into()]);}
        let mut environment:BTreeMap<_,_>=std::env::vars_os().collect();environment.insert("IVY_EXEC_FILE_VALUE".into(),"inherited-exact-value".into());
        let expected=oracle(&command,Some(&environment));let mut options=options();options.environment=Some(environment);
        let actual=run(command,options).await;
        assert_eq!(json::canonical(&project(&actual),usize::MAX).unwrap(),json::canonical(&expected,usize::MAX).unwrap(),"{mode}");
        if mode=="exit" {assert_eq!(actual.exit_code,Some(7));}
    }
}

#[tokio::test]
async fn exec_file_timeout_still_applies_after_pipe_eof_and_keeps_stdin_open() {
    for mode in ["closed","stdin"] {
        let mut command=command(mode);command.timeout_ms=300;
        let expected=oracle(&command,None);let started=std::time::Instant::now();let actual=run(command,options()).await;
        assert_eq!(actual.failure,Some(FileFailure::Timeout));assert!(started.elapsed()<Duration::from_secs(5));
        assert_eq!(json::canonical(&project(&actual),usize::MAX).unwrap(),json::canonical(&expected,usize::MAX).unwrap(),"{mode}");
    }
}

#[tokio::test]
async fn exec_file_cancellation_and_dropped_waiters_retain_tree_ownership() {
    let root=super::Root::new();let pid_file=root.0.join("pid");let mut command=command("sleep");command.args.push(pid_file.as_os_str().to_owned());
    let token=CancellationToken::new();let mut launch=options();launch.cancellation=token.clone();
    let task=tokio::spawn(run(command,launch));
    tokio::time::timeout(Duration::from_secs(4),async {while !pid_file.is_file() {tokio::time::sleep(Duration::from_millis(10)).await}}).await.unwrap();
    let pid=std::fs::read_to_string(&pid_file).unwrap().parse::<u32>().unwrap();
    token.cancel();let result=task.await.unwrap();assert_eq!(result.failure,Some(FileFailure::Cancelled));assert_eq!(result.stdout,JsString::from("ready"));assert_retired(pid);
    let pid_file=root.0.join("dropped-pid");let mut command=self::command("sleep");command.timeout_ms=1000;command.args.push(pid_file.as_os_str().to_owned());
    let waiter=tokio::spawn(run(command,options()));
    tokio::time::timeout(Duration::from_secs(4),async {while !pid_file.is_file() {tokio::time::sleep(Duration::from_millis(10)).await}}).await.unwrap();
    let pid=std::fs::read_to_string(pid_file).unwrap().parse::<u32>().unwrap();waiter.abort();
    tokio::time::sleep(Duration::from_millis(1500)).await;assert_retired(pid);
    let cancelled=options();cancelled.cancellation.cancel();assert_eq!(run(self::command("args"),cancelled).await.failure,Some(FileFailure::Cancelled));
}

fn assert_retired(pid:u32) {
    #[cfg(windows)] unsafe {use windows_sys::Win32::{Foundation::{CloseHandle,WAIT_OBJECT_0},System::Threading::{OpenProcess,PROCESS_SYNCHRONIZE,WaitForSingleObject}};let handle=OpenProcess(PROCESS_SYNCHRONIZE,0,pid);if !handle.is_null(){assert_eq!(WaitForSingleObject(handle,0),WAIT_OBJECT_0);CloseHandle(handle);}}
    #[cfg(unix)] assert_eq!(unsafe{libc::kill(pid as i32,0)},-1);
}

#[tokio::test]
async fn exec_file_searches_selected_cwd_path_and_ignores_windows_pathext() {
    let root=super::Root::new();let directory=root.0.join("bin;quoted");std::fs::create_dir(&directory).unwrap();
    let name=if cfg!(windows){"fixture-exec.exe"}else{"fixture-exec"};std::fs::copy(node(),directory.join(name)).unwrap();
    let mut command=command("argv0");command.executable="fixture-exec".into();command.cwd=Some(root.0.clone());
    let mut env:BTreeMap<_,_>=std::env::vars_os().filter(|(k,_)|!k.to_string_lossy().eq_ignore_ascii_case("PATH")).collect();
    env.insert("PATH".into(),if cfg!(windows){format!("\"{}\"",directory.display()).into()}else{directory.as_os_str().to_owned()});
    let expected=oracle(&command,Some(&env));let mut launch=options();launch.environment=Some(env);
    let result=run(command,launch).await;assert_eq!(json::canonical(&project(&result),usize::MAX).unwrap(),json::canonical(&expected,usize::MAX).unwrap());
    #[cfg(windows)] {
        std::fs::write(root.0.join("script.cmd"),"@echo unsafe").unwrap();let mut command=self::command("args");command.executable=root.0.join("script.cmd").into_os_string();
        assert_eq!(run(command,options()).await.failure,Some(FileFailure::Spawn));
    }
}

#[tokio::test]
async fn exec_file_cancellation_retires_child_and_grandchild_before_returning() {
    let root=super::Root::new();let mut env:BTreeMap<_,_>=std::env::vars_os().collect();
    env.insert("IVY_PROCESS_FIXTURE".into(),"tree".into());env.insert("IVY_PROCESS_TREE_ROOT".into(),root.0.as_os_str().to_owned());
    let token=CancellationToken::new();let mut launch=options();launch.environment=Some(env);launch.cancellation=token.clone();
    let command=FileCommand{executable:std::env::current_exe().unwrap().into_os_string(),args:vec!["--exact".into(),"child_fixture".into(),"--nocapture".into()],cwd:Some(root.0.clone()),timeout_ms:10000,maximum_buffer_bytes:65536};
    let task=tokio::spawn(run(command,launch));
    tokio::time::timeout(Duration::from_secs(4),async{while !root.0.join("identities").is_file(){tokio::time::sleep(Duration::from_millis(10)).await}}).await.unwrap();
    let identities=std::fs::read_to_string(root.0.join("identities")).unwrap();let pids:Vec<u32>=identities.split_whitespace().map(|s|s.parse().unwrap()).collect();assert_eq!(pids.len(),2);
    token.cancel();assert_eq!(task.await.unwrap().failure,Some(FileFailure::Cancelled));for pid in pids{assert_retired(pid);}
}
