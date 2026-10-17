use ivy_host_runtime::process::{self, Command, LaunchContext};
use std::{collections::BTreeMap, ffi::OsString, io::Write, path::PathBuf, time::Duration};

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
    assert!(result.stdout.contains("input-bytes=7"));
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
    let second = process::spawn(&command, &root.0, &executables, launch)
        .await
        .unwrap();
    assert_eq!(second.completion.wait().await.exit_code, Some(1));
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
