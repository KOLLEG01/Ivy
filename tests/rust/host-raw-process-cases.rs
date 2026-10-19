use super::Root;
use ivy_host_runtime::process::{self,Command,LaunchContext};
use std::{collections::BTreeMap,ffi::OsString,path::PathBuf,time::Duration};
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn exact_environment_raw_owner_preserves_complete_bytes_and_confirmed_retirement() {
    let root=Root::new();
    let binary=root.0.join(if cfg!(windows){"raw-command.exe"}else{"raw-command"});
    let compiler=std::process::Command::new("rustc").args(["--edition=2024"])
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/host-process-raw-child.rs"))
        .arg("-o").arg(&binary).output().unwrap();
    assert!(compiler.status.success(),"{}",String::from_utf8_lossy(&compiler.stderr));
    if let Some(directory)=std::env::var_os("IVY_HOST_RAW_PROCESS_NATIVE_DIRECTORY") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::copy(&binary,PathBuf::from(directory).join(binary.file_name().unwrap())).unwrap();
    }
    let executable=BTreeMap::from([("raw-command".into(),binary.clone())]);
    let mut environment:BTreeMap<OsString,OsString>=["SystemRoot","WINDIR","PATH","TEMP","TMP","TMPDIR","LANG"]
        .into_iter().map(|key|(key.into(),std::env::var_os(key).unwrap_or_default())).collect();
    environment.insert("LANG".into(),"C".into());
    let launch=||LaunchContext{environment:environment.clone(),raw_output:true,
        #[cfg(windows)] job_launcher:Some(PathBuf::from(env!("CARGO_BIN_EXE_ivy-job"))),
        ..Default::default()};
    for exact in [true,false] {
        let amount=if exact{5_760_000}else{1024};
        let report=root.0.join(if exact{"exact.txt"}else{"ordinary.txt"});
        let command=Command{executable:"raw-command".into(),args:vec!["probe".into(),report.clone().into_os_string(),amount.to_string().into()],cwd:None,timeout_ms:15000};
        let mut child=if exact{process::spawn_exact_environment(&command,&root.0,&executable,launch()).await}else{process::spawn(&command,&root.0,&executable,launch()).await}.unwrap();
        drop(child.stdin.take());
        let mut stdout=child.stdout.take().unwrap();let mut stderr=child.stderr.take().unwrap();
        let out=tokio::spawn(async move{let mut bytes=Vec::new();stdout.read_to_end(&mut bytes).await.unwrap();bytes});
        let err=tokio::spawn(async move{tokio::io::copy(&mut stderr,&mut tokio::io::sink()).await.unwrap()});
        let result=tokio::time::timeout(Duration::from_secs(15),child.completion.wait()).await.unwrap();
        assert_eq!(result.exit_code,Some(0));assert_eq!(result.error_code,None);
        assert!(result.stdout.is_empty()&&result.stderr.is_empty()&&!result.truncated);
        let bytes=out.await.unwrap();assert_eq!(bytes.len(),amount);assert!(bytes.iter().enumerate().all(|(i,b)|*b==(i%256)as u8));
        assert_eq!(err.await.unwrap(),256*1024);
        let expected=if exact{environment.clone()}else{process::runtime_environment(&environment)};
        assert_eq!(std::fs::read_to_string(report).unwrap(),format!("{expected:?}\nstdin=0\n"));
        assert!(process::owned_process_tree_stopped(&child).await.unwrap());
    }
    let command=|mode:&str|Command{executable:"raw-command".into(),args:vec![mode.into()],cwd:None,timeout_ms:15000};
    let mut child=process::spawn_exact_environment(&command("sleep"),&root.0,&executable,launch()).await.unwrap();
    drop(child.stdin.take());let mut stdout=child.stdout.take().unwrap();let mut stderr=child.stderr.take().unwrap();
    let drain=tokio::spawn(async move{tokio::io::copy(&mut stderr,&mut tokio::io::sink()).await.unwrap()});
    let mut ready=[0];tokio::time::timeout(Duration::from_secs(5),stdout.read_exact(&mut ready)).await.unwrap().unwrap();assert_eq!(ready,*b"R");
    let stopped=tokio::time::timeout(Duration::from_secs(5),child.stop(Duration::ZERO)).await.unwrap().unwrap();
    assert_ne!(stopped.error_code.as_deref(),Some("outcome_unknown"));assert!(process::owned_process_tree_stopped(&child).await.unwrap());drop(stdout);drain.await.unwrap();
    let mut child=process::spawn_exact_environment(&command("fail"),&root.0,&executable,launch()).await.unwrap();
    drop(child.stdin.take());drop(child.stdout.take());drop(child.stderr.take());let result=child.completion.wait().await;
    assert_eq!(result.exit_code,Some(7));assert!(process::owned_process_tree_stopped(&child).await.unwrap());
}
