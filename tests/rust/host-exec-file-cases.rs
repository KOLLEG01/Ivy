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
async fn exec_file_preserves_utf8_arguments_environment_exit_and_buffer_limits() {
    for mode in ["text", "args", "exit", "missing", "stdout", "stderr"] {
        let mut command = command(if matches!(mode, "stdout" | "stderr") {
            "large"
        } else {
            mode
        });
        if matches!(mode, "stdout" | "stderr") {
            command.args.push(mode.into());
            command.maximum_buffer_bytes = 1024;
        }
        if mode == "missing" {
            command.executable = std::env::temp_dir()
                .join("ivy-missing-executable-98713")
                .into_os_string();
        }
        if mode == "args" {
            command.args.extend([
                "".into(),
                "spaces and quotes\"".into(),
                "trailing\\".into(),
                "€😀".into(),
            ]);
        }
        let mut environment: BTreeMap<_, _> = std::env::vars_os().collect();
        environment.insert("IVY_EXEC_FILE_VALUE".into(), "inherited-exact-value".into());
        let mut launch = options();
        launch.environment = Some(environment);
        let actual = run(command, launch).await;
        let (stdout, stderr, failure) = match mode {
            "text" => ("\u{feff}😀€��".into(), "retained stderr".into(), None),
            "args" => {
                let args = IvyJson::object([
                    (
                        "args",
                        IvyJson::Array(
                            ["", "spaces and quotes\"", "trailing\\", "€😀"]
                                .into_iter()
                                .map(IvyJson::from)
                                .collect(),
                        ),
                    ),
                    ("cwd", os_json(std::env::current_dir().unwrap().as_os_str())),
                    ("env", "inherited-exact-value".into()),
                ]);
                assert_eq!(
                    json::parse(&String::from_utf16(&actual.stdout.0).unwrap(), usize::MAX)
                        .unwrap(),
                    args
                );
                (actual.stdout.clone(), "".into(), None)
            }
            "exit" => (
                "partial stdout".into(),
                "partial stderr".into(),
                Some(FileFailure::Exit),
            ),
            "missing" => ("".into(), "".into(), Some(FileFailure::Spawn)),
            "stdout" => (
                "x".repeat(1024).into(),
                "".into(),
                Some(FileFailure::MaxBuffer(OutputStream::Stdout)),
            ),
            "stderr" => (
                "".into(),
                "x".repeat(1024).into(),
                Some(FileFailure::MaxBuffer(OutputStream::Stderr)),
            ),
            _ => unreachable!(),
        };
        assert_eq!(
            (actual.stdout, actual.stderr, actual.failure),
            (stdout, stderr, failure),
            "{mode}"
        );
        if mode == "exit" {
            assert_eq!(actual.exit_code, Some(7));
        }
    }

    let cases: &[(&str, usize, &[u8], &str, bool)] = &[
        ("both", 8, "😀€x".as_bytes(), "😀€x", false),
        ("stdout", 7, "😀€x".as_bytes(), "😀€", true),
        ("stdout", 3, "😀x".as_bytes(), "", true),
        ("stderr", 5, "a😀€".as_bytes(), "a😀", true),
        ("stdout", 0, b"", "", false),
        ("stdout", 0, b"a", "", true),
        ("stdout", 3, &[0xff, b'b'], "�", true),
        ("stderr", 2, &[0xff], "", true),
        ("stdout", 3, &[0xe2, 0x82], "�", false),
        ("stdout", 2, &[0xe2, 0x82], "", true),
        ("stdout", 3, "\u{feff}".as_bytes(), "\u{feff}", false),
        ("stdout", 2, "\u{feff}".as_bytes(), "", true),
        ("stdout", 4, "😀".as_bytes(), "😀", false),
    ];
    for &(stream, maximum, bytes, expected, exceeded) in cases {
        let mut command = command("bytes");
        command.args.extend([
            stream.into(),
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                .into(),
        ]);
        command.maximum_buffer_bytes = maximum;
        let actual = run(command, options()).await;
        let failure = exceeded.then_some(FileFailure::MaxBuffer(if stream == "stderr" {
            OutputStream::Stderr
        } else {
            OutputStream::Stdout
        }));
        assert_eq!(actual.failure, failure, "{stream}/{maximum}/{bytes:?}");
        let stdout = String::from_utf16(&actual.stdout.0).unwrap();
        let stderr = String::from_utf16(&actual.stderr.0).unwrap();
        assert_eq!(
            stdout,
            if stream == "stderr" { "" } else { expected },
            "{stream}/{maximum}/{bytes:?}"
        );
        assert_eq!(
            stderr,
            if stream == "stdout" { "" } else { expected },
            "{stream}/{maximum}/{bytes:?}"
        );
        assert!(stdout.len() <= maximum && stderr.len() <= maximum);
        if !exceeded {
            assert_eq!((actual.exit_code, actual.signal), (Some(0), None));
        }
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
    #[cfg(windows)]
    {
        use std::{
            fs,
            os::windows::ffi::{OsStrExt, OsStringExt},
            process::Command,
        };
        let root = super::Root::new();
        let receiver = root.0.join("argv0-receiver.exe");
        let build = Command::new("rustc")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tests/fixtures/host-argv0-receiver.rs"),
            )
            .arg("-o")
            .arg(&receiver)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let units = |value: &std::ffi::OsStr| {
            IvyJson::Array(
                value
                    .encode_wide()
                    .map(|n| IvyJson::Number(n as f64))
                    .collect(),
            )
        };
        let tail: Vec<OsString> = vec![
            "".into(),
            "spaces and \"quotes\"".into(),
            "trailing\\".into(),
            "Grüße-🙂".into(),
            OsString::from_wide(&[0xd800, 34, 32, 0xdc00, 92]),
        ];
        let mut environment: BTreeMap<_, _> = std::env::vars_os()
            .filter(|(key, _)| {
                ["SystemRoot", "WINDIR", "COMSPEC", "TEMP", "TMP", "TMPDIR"]
                    .iter()
                    .any(|name| key.as_encoded_bytes().eq_ignore_ascii_case(name.as_bytes()))
            })
            .collect();
        environment.insert("PATHEXT".into(), ".wrong".into());
        for (index, prefix) in ["bin;quoted-", "bin-ending-"].iter().enumerate() {
            let mut name: Vec<u16> = prefix.encode_utf16().chain([0xd800]).collect();
            if index == 1 {
                name.push(39);
            }
            let relative = PathBuf::from(OsString::from_wide(&name));
            let directory = root.0.join(&relative);
            fs::create_dir(&directory).unwrap();
            fs::copy(&receiver, directory.join("fixture-exec.com")).unwrap();
            fs::write(directory.join("fixture-exec.exe"), b"unselected executable").unwrap();
            fs::copy(&receiver, directory.join("path-only.wrong")).unwrap();
            for mode in [
                "com",
                "explicit-com",
                "exe",
                "relative-path",
                "relative-executable",
                "absolute",
            ] {
                if mode == "exe" {
                    fs::remove_file(directory.join("fixture-exec.com")).unwrap();
                    fs::copy(&receiver, directory.join("fixture-exec.exe")).unwrap();
                }
                let executable: OsString = match mode {
                    "explicit-com" => "fixture-exec.com".into(),
                    "relative-executable" => relative.join("fixture-exec.exe").into_os_string(),
                    "absolute" => directory.join("fixture-exec.exe").into_os_string(),
                    _ => "fixture-exec".into(),
                };
                let receipt = format!("args-{index}-{mode}.json");
                let mut env = environment.clone();
                let selected = if mode == "relative-path" {
                    relative.clone()
                } else {
                    directory.clone()
                };
                env.insert(
                    "Path".into(),
                    std::env::join_paths([
                        PathBuf::new(),
                        root.0.join("missing"),
                        selected,
                        PathBuf::new(),
                    ])
                    .unwrap(),
                );
                env.insert("IVY_EXEC_FILE_ARGUMENTS".into(), receipt.clone().into());
                let mut launch = options();
                launch.environment = Some(env);
                let result = run(
                    FileCommand {
                        executable: executable.clone(),
                        args: tail.clone(),
                        cwd: Some(root.0.clone()),
                        timeout_ms: 10000,
                        maximum_buffer_bytes: 65536,
                    },
                    launch,
                )
                .await;
                assert_eq!(
                    result.failure, None,
                    "directory={index}, mode={mode}: {result:?}"
                );
                assert_eq!((result.exit_code, result.signal), (Some(0), None));
                assert!(result.stdout.0.is_empty() && result.stderr.0.is_empty());
                let actual = json::parse(
                    &fs::read_to_string(root.0.join(receipt)).unwrap(),
                    usize::MAX,
                )
                .unwrap();
                let expected = IvyJson::Array(
                    std::iter::once(units(&executable))
                        .chain(tail.iter().map(|arg| units(arg)))
                        .collect(),
                );
                assert_eq!(actual, expected, "directory={index}, mode={mode}");
            }
            let executing = std::env::current_dir().unwrap();
            fs::create_dir(directory.join("child")).unwrap();
            let mut cwd_cases = vec![
                ("inherited", None, executing.clone()),
                ("empty", Some(PathBuf::new()), executing.clone()),
                ("absolute", Some(directory.clone()), directory.clone()),
                ("dot", Some(directory.join(".")), directory.clone()),
                (
                    "parent",
                    Some(directory.join("child").join("..")),
                    directory.clone(),
                ),
                (
                    "verbatim",
                    Some(fs::canonicalize(&directory).unwrap()),
                    directory.clone(),
                ),
                (
                    "ignored-absolute",
                    Some(directory.clone()),
                    directory.clone(),
                ),
            ];
            if let Some(common) = executing
                .ancestors()
                .find(|path| directory.starts_with(path))
            {
                let mut relative = PathBuf::new();
                for _ in executing.strip_prefix(common).unwrap().components() {
                    relative.push("..");
                }
                relative.push(directory.strip_prefix(common).unwrap());
                let drive: PathBuf = executing.components().take(1).collect();
                let drive_root: PathBuf = directory.components().take(2).collect();
                cwd_cases.extend([
                    ("relative", Some(relative.clone()), directory.clone()),
                    (
                        "drive-relative",
                        Some(drive.join(&relative)),
                        directory.clone(),
                    ),
                    (
                        "root-relative",
                        Some(PathBuf::from("\\").join(directory.strip_prefix(drive_root).unwrap())),
                        directory.clone(),
                    ),
                    ("ignored-relative", Some(relative), directory.clone()),
                ]);
            }
            for (mode, cwd, expected_cwd) in cwd_cases {
                let args_file = root.0.join(format!("cwd-args-{index}-{mode}.json"));
                let cwd_file = root.0.join(format!("cwd-{index}-{mode}.json"));
                let mut env = environment.clone();
                env.insert(
                    "IVY_EXEC_FILE_ARGUMENTS".into(),
                    args_file.as_os_str().to_owned(),
                );
                env.insert("IVY_EXEC_FILE_CWD".into(), cwd_file.as_os_str().to_owned());
                let executable = directory.join("fixture-exec.exe").into_os_string();
                if mode.starts_with("ignored-") {
                    let child = exec_file::spawn_ignored(
                        exec_file::SpawnCommand {
                            executable: executable.clone(),
                            args: tail.clone(),
                            cwd,
                        },
                        root.0.clone(),
                        exec_file::SpawnOptions {
                            environment: Some(env),
                            job_launcher: options().job_launcher,
                        },
                    )
                    .await
                    .unwrap();
                    assert!(
                        child.stdin.is_none() && child.stdout.is_none() && child.stderr.is_none()
                    );
                    let result =
                        tokio::time::timeout(Duration::from_secs(10), child.completion.wait())
                            .await
                            .unwrap();
                    assert_eq!(
                        (result.exit_code, result.signal.as_deref()),
                        (Some(0), None),
                        "{index}/{mode}: {result:?}"
                    );
                    assert!(result.error_code.is_none());
                    assert!(
                        ivy_host_runtime::process::owned_process_tree_stopped(&child)
                            .await
                            .unwrap()
                    );
                } else {
                    let mut launch = options();
                    launch.environment = Some(env);
                    let result = run(
                        FileCommand {
                            executable: executable.clone(),
                            args: tail.clone(),
                            cwd,
                            timeout_ms: 10000,
                            maximum_buffer_bytes: 65536,
                        },
                        launch,
                    )
                    .await;
                    assert_eq!(result.failure, None, "{index}/{mode}: {result:?}");
                    assert_eq!((result.exit_code, result.signal), (Some(0), None));
                    assert!(result.stdout.0.is_empty() && result.stderr.0.is_empty());
                }
                let args =
                    json::parse(&fs::read_to_string(args_file).unwrap(), usize::MAX).unwrap();
                let expected_args = IvyJson::Array(
                    std::iter::once(units(&executable))
                        .chain(tail.iter().map(|arg| units(arg)))
                        .collect(),
                );
                assert_eq!(args, expected_args, "{index}/{mode}");
                let cwd = json::parse(&fs::read_to_string(cwd_file).unwrap(), usize::MAX).unwrap();
                let cwd: Vec<u16> = cwd
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|unit| {
                        let IvyJson::Number(unit) = unit else {
                            panic!("native cwd unit")
                        };
                        *unit as u16
                    })
                    .collect();
                let actual_cwd = PathBuf::from(OsString::from_wide(&cwd));
                assert_eq!(
                    fs::canonicalize(actual_cwd).unwrap(),
                    fs::canonicalize(expected_cwd).unwrap(),
                    "{index}/{mode}"
                );
            }
            for (failure_index, cwd) in [
                directory.join("missing"),
                directory.join("fixture-exec.exe"),
                directory.join(OsString::from_wide(&[0])),
            ]
            .into_iter()
            .enumerate()
            {
                for ignored in [false, true] {
                    let receipt = root.0.join(format!(
                        "cwd-unlaunched-{index}-{failure_index}-{ignored}.json"
                    ));
                    let mut env = environment.clone();
                    env.insert(
                        "IVY_EXEC_FILE_ARGUMENTS".into(),
                        receipt.as_os_str().to_owned(),
                    );
                    let executable = directory.join("fixture-exec.exe").into_os_string();
                    if ignored {
                        let failure = exec_file::spawn_ignored(
                            exec_file::SpawnCommand {
                                executable,
                                args: tail.clone(),
                                cwd: Some(cwd.clone()),
                            },
                            root.0.clone(),
                            exec_file::SpawnOptions {
                                environment: Some(env),
                                job_launcher: options().job_launcher,
                            },
                        )
                        .await
                        .err()
                        .expect("invalid cwd must not launch");
                        assert_eq!(failure.error.code, "spawn_failed");
                    } else {
                        let mut launch = options();
                        launch.environment = Some(env);
                        let result = run(
                            FileCommand {
                                executable,
                                args: tail.clone(),
                                cwd: Some(cwd.clone()),
                                timeout_ms: 10000,
                                maximum_buffer_bytes: 65536,
                            },
                            launch,
                        )
                        .await;
                        assert_eq!(result.failure, Some(FileFailure::Spawn));
                    }
                    assert!(!receipt.exists());
                }
            }
            for (failure_index, executable) in [
                OsString::from("path-only"),
                root.0
                    .join("missing")
                    .join("fixture-exec.exe")
                    .into_os_string(),
                OsString::from(""),
                OsString::from("."),
                OsString::from_wide(&[b'x' as u16, 0, b'y' as u16]),
            ]
            .into_iter()
            .enumerate()
            {
                let receipt = format!("unlaunched-{index}-{failure_index}.json");
                let mut env = environment.clone();
                env.insert("PATH".into(), std::env::join_paths([&directory]).unwrap());
                env.insert("IVY_EXEC_FILE_ARGUMENTS".into(), receipt.clone().into());
                let mut launch = options();
                launch.environment = Some(env);
                let result = run(
                    FileCommand {
                        executable,
                        args: tail.clone(),
                        cwd: Some(root.0.clone()),
                        timeout_ms: 10000,
                        maximum_buffer_bytes: 65536,
                    },
                    launch,
                )
                .await;
                assert_eq!(
                    result.failure,
                    Some(FileFailure::Spawn),
                    "{index}/{failure_index}"
                );
                assert!(!root.0.join(receipt).exists());
            }
        }
        // Valid PE bytes under script names must still be refused before launch.
        for (index, name) in ["script.cmd", "script.BAT", ".CMD", "script.cmd. "]
            .iter()
            .enumerate()
        {
            fs::copy(&receiver, root.0.join(name)).unwrap();
            let receipt = format!("script-unlaunched-{index}.json");
            let mut env = environment.clone();
            env.insert("IVY_EXEC_FILE_ARGUMENTS".into(), receipt.clone().into());
            let mut launch = options();
            launch.environment = Some(env);
            let result = run(
                FileCommand {
                    executable: root.0.join(name).into_os_string(),
                    args: tail.clone(),
                    cwd: Some(root.0.clone()),
                    timeout_ms: 10000,
                    maximum_buffer_bytes: 65536,
                },
                launch,
            )
            .await;
            assert_eq!(result.failure, Some(FileFailure::Spawn), "{name}");
            assert!(!root.0.join(receipt).exists());
        }
    }
    #[cfg(not(windows))]
    {
        let root = super::Root::new();
        let directory = root.0.join("bin;quoted");
        std::fs::create_dir(&directory).unwrap();
        std::fs::copy(node(), directory.join("fixture-exec")).unwrap();
        let mut command = command("argv0");
        command.executable = "fixture-exec".into();
        command.cwd = Some(root.0.clone());
        let mut env: BTreeMap<_, _> = std::env::vars_os()
            .filter(|(k, _)| !k.to_string_lossy().eq_ignore_ascii_case("PATH"))
            .collect();
        env.insert("PATH".into(), directory.as_os_str().to_owned());
        let expected = oracle(&command, Some(&env));
        let mut launch = options();
        launch.environment = Some(env);
        let result = run(command, launch).await;
        assert_eq!(
            json::canonical(&project(&result), usize::MAX).unwrap(),
            json::canonical(&expected, usize::MAX).unwrap()
        );
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
