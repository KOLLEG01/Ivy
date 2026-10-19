use ivy_host_runtime::process::{self, Command, LaunchContext};
use std::{collections::BTreeMap, fs, path::{Path, PathBuf}, time::{Duration, SystemTime, UNIX_EPOCH}};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-tree-observation-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&path).unwrap(); Self(path)
    }
}
impl Drop for Root { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

#[test]
fn tree_child_fixture() {
    let Ok(mode) = std::env::var("IVY_TREE_OBSERVATION_FIXTURE") else { return };
    let root = PathBuf::from(std::env::var_os("IVY_TREE_OBSERVATION_ROOT").unwrap());
    fs::write(root.join(&mode), std::process::id().to_string()).unwrap();
    let mut descendant = if mode == "tree" {
        Some(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process_tree_cases::tree_child_fixture", "--nocapture"])
            .env("IVY_TREE_OBSERVATION_FIXTURE", "descendant").spawn().unwrap())
    } else { None };
    let until = std::time::Instant::now() + Duration::from_secs(20);
    let release = root.join(if mode == "descendant" { "release-descendant" } else { "release" });
    while !release.exists() && std::time::Instant::now() < until { std::thread::sleep(Duration::from_millis(10)); }
    if let Some(child) = descendant.as_mut() { let _ = child.kill(); let _ = child.wait(); }
}

async fn launch(root: &Root, mode: &str, without_job: bool) -> process::OwnedProcess {
    let command = Command { executable: "fixture".into(), args: vec!["--exact".into(), "process_tree_cases::tree_child_fixture".into(), "--nocapture".into()], cwd: None, timeout_ms: 10000 };
    let mut context = LaunchContext { without_job, environment: BTreeMap::from([
        ("IVY_TREE_OBSERVATION_FIXTURE".into(), mode.into()), ("IVY_TREE_OBSERVATION_ROOT".into(), root.0.as_os_str().to_owned()),
    ]), ..Default::default() };
    #[cfg(windows)]
    if !without_job {
        context.job_launcher = Some(std::env::var_os("IVY_HOST_PROCESS_TREE_JOB").map(PathBuf::from)
            .or_else(|| option_env!("CARGO_BIN_EXE_ivy-job").map(PathBuf::from)).expect("actual owned Job launcher is required"));
    }
    process::spawn(&command, &root.0, &BTreeMap::from([("fixture".into(), std::env::current_exe().unwrap())]), context).await.unwrap()
}
async fn published_pid(root: &Path, name: &str) -> u32 {
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(root.join(name)) && let Ok(pid) = text.parse() { return pid }
        assert!(tokio::time::Instant::now() < until, "child identity was not published");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn complete(process: &process::OwnedProcess) -> std::sync::Arc<process::ProcessResult> {
    tokio::time::timeout(Duration::from_secs(6), process.completion.wait()).await.unwrap()
}
fn retain(mode: &str, result: &process::ProcessResult, stopped: bool) {
    use std::io::Write;
    if let Some(path) = std::env::var_os("IVY_HOST_PROCESS_TREE_CORPUS") {
        writeln!(fs::OpenOptions::new().create(true).append(true).open(path).unwrap(), "{mode}: stopped={stopped}, result={result:?}").unwrap();
    }
}

#[cfg(windows)]
fn held_process(pid: u32) -> std::os::windows::io::OwnedHandle {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(!raw.is_null(), "{}", std::io::Error::last_os_error());
    unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) }
}
#[cfg(windows)]
fn assert_signaled(handle: &std::os::windows::io::OwnedHandle) {
    use std::os::windows::io::AsRawHandle;
    assert_eq!(unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(handle.as_raw_handle(), 0) }, windows_sys::Win32::Foundation::WAIT_OBJECT_0);
}

#[cfg(windows)]
#[tokio::test]
async fn owned_tree_observation_never_promotes_without_job_completion_or_pid_exit() {
    let root = Root::new(); let process = launch(&root, "leaf", true).await;
    let handle = held_process(published_pid(&root.0, "leaf").await);
    assert!(!process::owned_process_tree_stopped(&process).await.unwrap());
    fs::write(root.0.join("release"), b"finish").unwrap(); let result = complete(&process).await;
    assert_eq!(result.exit_code, Some(0)); assert_eq!(result.error_code, None); assert_signaled(&handle);
    let stopped = process::owned_process_tree_stopped(&process).await.unwrap(); retain("without-job", &result, stopped); assert!(!stopped);
}

#[cfg(windows)]
#[tokio::test]
async fn owned_tree_observation_requires_actual_job_proof_for_leaf_and_descendant() {
    for mode in ["leaf", "tree"] {
        let root = Root::new(); let process = launch(&root, mode, false).await;
        let mut handles = vec![held_process(published_pid(&root.0, mode).await)];
        if mode == "tree" { handles.push(held_process(published_pid(&root.0, "descendant").await)); }
        assert!(!process::owned_process_tree_stopped(&process).await.unwrap());
        let result = if mode == "leaf" {
            fs::write(root.0.join("release"), b"finish").unwrap(); complete(&process).await
        } else { process.stop(Duration::from_millis(50)).await.unwrap() };
        let stopped = process::owned_process_tree_stopped(&process).await.unwrap(); retain(mode, &result, stopped);
        for handle in &handles { assert_signaled(handle); }
        assert_eq!(result.error_code, None, "{mode}: {result:?}"); assert!(stopped, "{mode}: Job retirement remained unproved");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn owned_tree_observation_waits_for_its_original_group_and_accepts_esrch() {
    let root = Root::new(); let mut process = launch(&root, "leaf", false).await;
    published_pid(&root.0, "leaf").await;
    let started = std::time::Instant::now(); assert!(!process::owned_process_tree_stopped(&process).await.unwrap());
    assert!(started.elapsed() >= Duration::from_millis(490));
    // The public launcher PID is not the retained process-group identity.
    process.pid = std::process::id(); fs::write(root.0.join("release"), b"finish").unwrap(); let result = complete(&process).await;
    let stopped = process::owned_process_tree_stopped(&process).await.unwrap(); retain("unix-group", &result, stopped); assert!(stopped);
}
