use ivy_host_runtime::process::{self, Command, LaunchContext};
use std::{collections::BTreeMap, ffi::OsString, io::Write, path::PathBuf, time::Duration};

// Exercise the private retained-handle failure paths through this selected target.
#[cfg(windows)]
#[path = "../../packages/host-runtime/rust/src/platform/windows_launch.rs"]
mod launch_retirement_checks;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-process-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture_command() -> (Command, BTreeMap<String, PathBuf>) {
    (
        Command {
            executable: "fixture".into(),
            args: vec![
                "--exact".into(),
                "child_fixture".into(),
                "--nocapture".into(),
            ],
            cwd: None,
            timeout_ms: 10000,
        },
        BTreeMap::from([("fixture".into(), std::env::current_exe().unwrap())]),
    )
}
fn context(fixture: &str) -> LaunchContext {
    LaunchContext {
        environment: BTreeMap::from([("IVY_PROCESS_FIXTURE".into(), fixture.into())]),
        #[cfg(windows)]
        job_launcher: Some(PathBuf::from(env!("CARGO_BIN_EXE_ivy-job"))),
        ..Default::default()
    }
}

#[test]
fn child_fixture() {
    let Ok(fixture) = std::env::var("IVY_PROCESS_FIXTURE") else {
        return;
    };
    match fixture.as_str() {
        "redact" => {
            print!("prefix-secr");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_millis(20));
            println!("et-token-suffix");
            eprintln!("error-secret-token-diagnostic");
        }
        "sleep" => {
            println!("ready");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
        "exit" => std::process::exit(7),
        "input" => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).unwrap();
            println!("input-bytes={}", bytes.len());
        }
        "large" => {
            std::io::stdout().write_all(&vec![b'x'; 200000]).unwrap();
        }
        "short-lived" => {
            for _ in 0..8 {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .arg("--list")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .unwrap();
                assert!(status.success());
            }
            println!("descendants-finished=8");
        }
        "tree" | "tree-exit" => {
            let root = PathBuf::from(std::env::var_os("IVY_PROCESS_TREE_ROOT").unwrap());
            let mut descendant = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "child_fixture", "--nocapture"])
                .env("IVY_PROCESS_FIXTURE", "sleep")
                .spawn()
                .unwrap();
            std::fs::write(
                root.join("identities"),
                format!("{} {}", std::process::id(), descendant.id()),
            )
            .unwrap();
            let until = std::time::Instant::now() + Duration::from_secs(30);
            while std::time::Instant::now() < until {
                if fixture == "tree-exit" && root.join("release").is_file() {
                    // The Job owner, not this direct child, must retire descendants.
                    std::process::exit(0);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = descendant.kill();
            let _ = descendant.wait();
        }
        #[cfg(windows)]
        "owner" => {
            let root = PathBuf::from(std::env::var_os("IVY_PROCESS_TREE_ROOT").unwrap());
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let (command, executables) = fixture_command();
                let mut launch = context("tree");
                launch
                    .environment
                    .insert("IVY_PROCESS_TREE_ROOT".into(), root.as_os_str().to_owned());
                launch.job_name = Some(std::env::var("IVY_PROCESS_JOB_NAME").unwrap());
                let process = process::spawn(&command, &root, &executables, launch)
                    .await
                    .unwrap();
                std::fs::write(root.join("helper"), process.pid.to_string()).unwrap();
                tokio::time::sleep(Duration::from_secs(30)).await;
                let _ = process.stop(Duration::ZERO).await;
            });
        }
        _ => panic!("Unknown fixture"),
    }
}

#[tokio::test]
async fn diagnostic_streams_are_redacted_and_bounded() {
    let root = Root::new();
    let (command, executables) = fixture_command();
    let mut launch = context("redact");
    launch.redact = vec!["secret-token".into()];
    let result = process::run_command(&command, &root.0, &executables, launch, None)
        .await
        .unwrap();
    assert!(result.stdout.contains("prefix-[redacted]-suffix"));
    assert!(result.stderr.contains("error-[redacted]-diagnostic"));
    assert!(!result.stdout.contains("secret-token"));
    let result = process::run_command(&command, &root.0, &executables, context("large"), None)
        .await
        .unwrap();
    assert_eq!(result.stdout.len(), 65536);
    assert!(result.truncated);
}

#[tokio::test]
async fn bounded_stdin_and_exit_outcomes_are_preserved() {
    let root = Root::new();
    let (command, executables) = fixture_command();
    let result = process::run_command(
        &command,
        &root.0,
        &executables,
        context("input"),
        Some(b"fixture"),
    )
    .await
    .unwrap();
    assert!(result.stdout.contains("input-bytes=7"), "{result:?}");
    let error = process::run_command(&command, &root.0, &executables, context("exit"), None)
        .await
        .unwrap_err();
    assert_eq!(error.code, "command_failed");
    assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
    let error = process::run_command(
        &command,
        &root.0,
        &executables,
        context("input"),
        Some(&vec![0; 65537]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "limit_exceeded");
    assert_eq!(error.outcome, ivy_contracts::Outcome::NotExecuted);
}

#[tokio::test]
async fn already_finished_descendants_preserve_normal_completion() {
    let root = Root::new();
    let (command, executables) = fixture_command();
    let result = process::run_command(
        &command,
        &root.0,
        &executables,
        context("short-lived"),
        None,
    )
    .await
    .unwrap();
    assert!(
        result.stdout.contains("descendants-finished=8"),
        "{result:?}"
    );
}

#[tokio::test]
async fn deadline_and_explicit_stop_release_owned_processes() {
    let root = Root::new();
    let (mut command, executables) = fixture_command();
    command.timeout_ms = 100;
    let error = process::run_command(&command, &root.0, &executables, context("sleep"), None)
        .await
        .unwrap_err();
    assert_eq!(error.code, "deadline_exceeded");
    let process = process::spawn(&command, &root.0, &executables, context("sleep"))
        .await
        .unwrap();
    let result = process.stop(Duration::from_millis(100)).await.unwrap();
    assert_ne!(result.error_code.as_deref(), Some("outcome_unknown"));
    assert_eq!(*process.stop(Duration::ZERO).await.unwrap(), *result);
}

#[tokio::test]
async fn raw_protocol_pipes_bypass_diagnostic_capture() {
    use tokio::io::AsyncReadExt;
    let root = Root::new();
    let (command, executables) = fixture_command();
    let mut launch = context("redact");
    launch.raw_output = true;
    let mut process = process::spawn(&command, &root.0, &executables, launch)
        .await
        .unwrap();
    let mut stdout = String::new();
    process
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .await
        .unwrap();
    let result = process.completion.wait().await;
    assert!(stdout.contains("secret-token"));
    assert_eq!(result.stdout, "");
    assert_eq!(result.stderr, "");
}

#[test]
fn command_resolution_rejects_script_suffixes_including_dot_only_basenames() {
    let root = Root::new();
    let (mut command, _) = fixture_command();
    for (name, rejected) in [(".CMD", true), (".bat", true), (".Ps1", true), ("script.CMD", true), ("script.bAt", true), ("script.Ps1", true), (".cmd.exe", false), ("script.ps1.backup", false)] {
        std::fs::write(root.0.join(name), b"fixture; never executed").unwrap();
        command.executable = name.into();
        let result = process::resolve_command(&command, &root.0, &BTreeMap::new());
        if rejected {
            let error = result.err().unwrap();
            assert_eq!(error.code, "invalid_arguments", "{name}");
            assert_eq!(error.message, "Commands need a direct executable; shell scripts require an explicit interpreter and argument array.");
        } else { assert!(result.is_ok(), "{name}"); }
    }
    std::fs::create_dir(root.0.join("directory.exe")).unwrap();
    command.executable = "directory.exe".into();
    assert_eq!(process::resolve_command(&command, &root.0, &BTreeMap::new()).err().unwrap().code, "invalid_arguments");
}

#[test]
fn configured_paths_and_case_insensitive_environment_overrides_are_explicit() {
    let root = Root::new();
    let (mut command, executables) = fixture_command();
    assert!(process::resolve_command(&command, &root.0, &executables).is_ok());
    command.cwd = Some("..".into());
    assert_eq!(
        process::resolve_command(&command, &root.0, &executables)
            .err()
            .unwrap()
            .code,
        "invalid_arguments"
    );
    let extra = BTreeMap::from([(OsString::from("pAtH"), OsString::from("explicit"))]);
    let environment = process::runtime_environment(&extra);
    assert_eq!(
        environment
            .iter()
            .filter(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("path"))
            .count(),
        1
    );
    assert_eq!(
        environment.get(&OsString::from("pAtH")).unwrap(),
        "explicit"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn exact_named_job_excludes_a_second_owner_and_disappears_after_stop() {
    use ivy_host_runtime::platform::windows_job::job_stopped;
    let root = Root::new();
    let (command, executables) = fixture_command();
    let name = format!("Global\\Ivy.test-{}", uuid::Uuid::new_v4());
    assert!(job_stopped(&name).unwrap());
    let mut launch = context("sleep");
    launch.job_name = Some(name.clone());
    let first = process::spawn(&command, &root.0, &executables, launch)
        .await
        .unwrap();
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    while job_stopped(&name).unwrap() && tokio::time::Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!job_stopped(&name).unwrap());
    let mut launch = context("sleep");
    launch.job_name = Some(name.clone());
    let second = process::spawn(&command, &root.0, &executables, launch).await;
    assert_eq!(second.err().unwrap().code, "spawn_failed");
    assert!(
        !job_stopped(&name).unwrap(),
        "the rejected launcher must leave the existing owner intact"
    );
    first.stop(Duration::from_millis(100)).await.unwrap();
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    while !job_stopped(&name).unwrap() && tokio::time::Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(job_stopped(&name).unwrap());
}

#[cfg(windows)]
mod retirement {
    use super::*;
    use std::os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    };
    use windows_sys::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::{
            JobObjects::OpenJobObjectW,
            Threading::{
                OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
                WaitForSingleObject,
            },
        },
    };

    fn process_handle(pid: u32) -> OwnedHandle {
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
        assert!(!raw.is_null(), "{}", std::io::Error::last_os_error());
        unsafe { OwnedHandle::from_raw_handle(raw) }
    }
    fn job_observer(name: &str) -> OwnedHandle {
        let wide: Vec<_> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let raw = unsafe { OpenJobObjectW(0x0004, 0, wide.as_ptr()) };
        assert!(!raw.is_null(), "{}", std::io::Error::last_os_error());
        unsafe { OwnedHandle::from_raw_handle(raw) }
    }
    async fn tree_handles(root: &Root) -> Vec<OwnedHandle> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(text) = std::fs::read_to_string(root.0.join("identities")) {
                let ids: Vec<u32> = text
                    .split_whitespace()
                    .filter_map(|id| id.parse().ok())
                    .collect();
                if ids.len() == 2 {
                    return ids.into_iter().map(process_handle).collect();
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "fixture did not publish identities"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    fn assert_retired(handles: &[OwnedHandle], context: &str) {
        // Snapshot every immediate result before collecting delayed diagnostics;
        // the later waits must never turn premature completion into a pass.
        let immediate: Vec<_> = handles
            .iter()
            .map(|handle| unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) })
            .collect();
        if immediate.iter().any(|&state| state != WAIT_OBJECT_0) {
            let later: Vec<_> = handles
                .iter()
                .map(|handle| unsafe { WaitForSingleObject(handle.as_raw_handle(), 250) })
                .collect();
            panic!(
                "completion preceded actual process termination: {context}; \
                 [child, grandchild] immediate={immediate:?}, later={later:?}"
            );
        }
    }

    #[tokio::test]
    async fn child_and_grandchild_retire_before_completion_with_external_job_handles() {
        for launcher in [
            env!("CARGO_BIN_EXE_ivy-job"),
            env!("CARGO_BIN_EXE_ivy-host-job"),
        ] {
            for finish in ["normal", "stop", "launcher-crash"] {
                let root = Root::new();
                let (command, executables) = fixture_command();
                let name = format!("Global\\Ivy.test-{}", uuid::Uuid::new_v4());
                let mut launch = context(if finish == "normal" {
                    "tree-exit"
                } else {
                    "tree"
                });
                launch.job_launcher = Some(launcher.into());
                launch.job_name = Some(name.clone());
                launch.raw_output = true;
                launch.environment.insert(
                    "IVY_PROCESS_TREE_ROOT".into(),
                    root.0.as_os_str().to_owned(),
                );
                let process = process::spawn(&command, &root.0, &executables, launch)
                    .await
                    .unwrap();
                let handles = tree_handles(&root).await;
                let observer = job_observer(&name);
                let result = match finish {
                    "normal" => {
                        std::fs::write(root.0.join("release"), b"exit").unwrap();
                        tokio::time::timeout(Duration::from_secs(5), process.completion.wait())
                            .await
                            .unwrap()
                    }
                    "stop" => process.stop(Duration::from_millis(100)).await.unwrap(),
                    _ => {
                        let helper = process_handle(process.pid);
                        assert_ne!(unsafe { TerminateProcess(helper.as_raw_handle(), 123) }, 0);
                        tokio::time::timeout(Duration::from_secs(5), process.completion.wait())
                            .await
                            .unwrap()
                    }
                };
                assert_eq!(result.error_code, None, "{launcher}, {finish}: {result:?}");
                assert_retired(&handles, &format!("{launcher}, {finish}: {result:?}"));
                assert!(
                    !ivy_host_runtime::platform::windows_job::job_stopped(&name).unwrap(),
                    "a held observer keeps the name alive after definite retirement"
                );
                drop(observer);
            }
        }
    }

    #[tokio::test]
    async fn parent_death_retires_the_tree_even_with_an_external_job_handle() {
        let root = Root::new();
        let name = format!("Global\\Ivy.test-{}", uuid::Uuid::new_v4());
        let mut owner = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_fixture", "--nocapture"])
            .env("IVY_PROCESS_FIXTURE", "owner")
            .env("IVY_PROCESS_TREE_ROOT", &root.0)
            .env("IVY_PROCESS_JOB_NAME", &name)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let handles = tree_handles(&root).await;
        let observer = job_observer(&name);
        owner.kill().unwrap();
        owner.wait().unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while handles.iter().any(
            |handle| unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } != WAIT_OBJECT_0,
        ) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "parent death left a live child"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_retired(&handles, "parent death");
        drop(observer);
    }
}

#[path = "host-exec-file-cases.rs"]
mod exec_file_cases;
