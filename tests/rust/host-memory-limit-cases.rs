use ivy_host_runtime::process::{self, Command, LaunchContext, ProcessMemoryLimit};
use std::{collections::BTreeMap, num::NonZeroU64, path::PathBuf};

fn limit() -> ProcessMemoryLimit {
    ProcessMemoryLimit {
        maximum_job_commit_bytes: NonZeroU64::new(64 * 1024 * 1024).unwrap(),
    }
}

#[cfg(not(windows))]
#[tokio::test]
async fn requested_job_accounting_refuses_unsupported_target_before_launch() {
    let root = super::Root::new();
    let (command, executables) = super::fixture_command();
    let result = process::spawn_exact_environment_with_memory_limit(
        &command,
        &root.0,
        &executables,
        LaunchContext::default(),
        limit(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(result.err().unwrap().error.code, "invalid_arguments");
}

#[cfg(windows)]
mod windows {
    use super::*;
    use process::{MemoryAccounting, MemoryLimitNotice, MemoryLimitedProcess};
    use std::{
        fs,
        io::Write,
        mem::{size_of, zeroed},
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
            process::CommandExt,
        },
        path::Path,
        ptr::{null, null_mut},
        time::Duration,
    };
    use tokio::io::{AsyncRead, AsyncReadExt};
    use windows_sys::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0},
        System::{
            JobObjects::{
                IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_JOB_MEMORY,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
                JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
                JobObjectExtendedLimitInformation, OpenJobObjectW, QueryInformationJobObject,
            },
            Threading::{
                GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_SYNCHRONIZE, WaitForSingleObject,
            },
        },
    };

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(
            address: *const u8,
            size: usize,
            allocation_type: u32,
            protection: u32,
        ) -> *mut u8;
        fn VirtualFree(address: *mut u8, size: usize, free_type: u32) -> i32;
    }

    struct Pages(Vec<*mut u8>);
    impl Pages {
        fn new() -> Self {
            Self(Vec::new())
        }
        fn commit(&mut self, bytes: usize) -> bool {
            let pointer = unsafe { VirtualAlloc(null(), bytes, 0x3000, 4) };
            if pointer.is_null() {
                return false;
            }
            self.0.push(pointer);
            true
        }
    }
    impl Drop for Pages {
        fn drop(&mut self) {
            for &pointer in &self.0 {
                unsafe {
                    VirtualFree(pointer, 0, 0x8000);
                }
            }
        }
    }

    fn job(name: &str) -> OwnedHandle {
        let name: Vec<_> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let raw = unsafe { OpenJobObjectW(4, 0, name.as_ptr()) };
        assert!(!raw.is_null(), "{}", std::io::Error::last_os_error());
        unsafe { OwnedHandle::from_raw_handle(raw) }
    }
    fn membership(process: HANDLE, job: &OwnedHandle) -> bool {
        let mut member = 0;
        assert_ne!(
            unsafe { IsProcessInJob(process, job.as_raw_handle(), &mut member) },
            0
        );
        member != 0
    }
    fn assert_limit(job: &OwnedHandle) {
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        assert_ne!(
            unsafe {
                QueryInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    null_mut(),
                )
            },
            0
        );
        assert_eq!(
            limits.JobMemoryLimit as u64,
            limit().maximum_job_commit_bytes.get()
        );
        let flags = limits.BasicLimitInformation.LimitFlags;
        assert_eq!(
            flags & (JOB_OBJECT_LIMIT_JOB_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE),
            JOB_OBJECT_LIMIT_JOB_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        );
        assert_eq!(
            flags
                & (JOB_OBJECT_LIMIT_PROCESS_MEMORY
                    | JOB_OBJECT_LIMIT_BREAKAWAY_OK
                    | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK),
            0
        );
    }
    fn assert_empty(job: &OwnedHandle) {
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        assert_ne!(
            unsafe {
                QueryInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    null_mut(),
                )
            },
            0
        );
        assert_eq!(accounting.ActiveProcesses, 0);
    }
    fn held_process(pid: u32) -> OwnedHandle {
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        assert!(!raw.is_null(), "{}", std::io::Error::last_os_error());
        unsafe { OwnedHandle::from_raw_handle(raw) }
    }
    fn assert_signaled(handle: &OwnedHandle) {
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
    }
    fn fixture_command() -> (Command, BTreeMap<String, PathBuf>) {
        (
            Command {
                executable: "memory-fixture".into(),
                args: vec![
                    "--exact".into(),
                    "memory_limit_cases::windows::memory_child_fixture".into(),
                    "--nocapture".into(),
                ],
                cwd: None,
                timeout_ms: 10000,
            },
            BTreeMap::from([("memory-fixture".into(), std::env::current_exe().unwrap())]),
        )
    }
    fn context(root: &Path, mode: &str, name: &str, raw: bool) -> LaunchContext {
        let mut environment: BTreeMap<_, _> = std::env::vars_os()
            .filter(|(key, _)| {
                ["systemroot", "temp", "tmp"]
                    .contains(&key.to_string_lossy().to_lowercase().as_str())
            })
            .collect();
        environment.extend([
            ("IVY_MEMORY_FIXTURE".into(), mode.into()),
            ("IVY_MEMORY_ROOT".into(), root.as_os_str().to_owned()),
            ("IVY_MEMORY_JOB".into(), name.into()),
        ]);
        LaunchContext {
            environment,
            job_name: Some(name.into()),
            raw_output: raw,
            job_launcher: Some(PathBuf::from(env!("CARGO_BIN_EXE_ivy-job"))),
            maximum_output_bytes: Some(65536),
            ..Default::default()
        }
    }
    async fn launch(root: &Path, mode: &str, name: &str, raw: bool) -> MemoryLimitedProcess {
        let (command, executables) = fixture_command();
        tokio::time::timeout(
            Duration::from_secs(8),
            process::spawn_exact_environment_with_memory_limit(
                &command,
                root,
                &executables,
                context(root, mode, name, raw),
                limit(),
            ),
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn published(root: &Path, name: &str) -> u32 {
        let until = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(text) = fs::read_to_string(root.join(name))
                && let Ok(pid) = text.parse()
            {
                return pid;
            }
            assert!(
                tokio::time::Instant::now() < until,
                "missing fixture record {name}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    fn gate(root: &Path, name: &str) {
        let until = std::time::Instant::now() + Duration::from_secs(12);
        while !root.join(name).is_file() {
            assert!(std::time::Instant::now() < until, "missing gate {name}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn record(name: &str, value: impl std::fmt::Debug) {
        if let Some(path) = std::env::var_os("IVY_MEMORY_EVIDENCE") {
            writeln!(
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .unwrap(),
                "{name}: {value:?}"
            )
            .unwrap();
        }
    }
    async fn complete(process: &process::OwnedProcess) -> std::sync::Arc<process::ProcessResult> {
        tokio::time::timeout(Duration::from_secs(5), process.completion.wait())
            .await
            .unwrap()
    }
    fn drain<R: AsyncRead + Unpin + Send + 'static>(pipe: R) -> tokio::task::JoinHandle<Vec<u8>> {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            pipe.take(65537).read_to_end(&mut bytes).await.unwrap();
            assert!(
                bytes.len() < 65537,
                "private fixture output exceeded bound instead of reaching EOF"
            );
            bytes
        })
    }
    async fn eof(reader: tokio::task::JoinHandle<Vec<u8>>) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .unwrap()
            .unwrap()
    }

    #[test]
    fn memory_child_fixture() {
        let Ok(mode) = std::env::var("IVY_MEMORY_FIXTURE") else {
            return;
        };
        let root = PathBuf::from(std::env::var_os("IVY_MEMORY_ROOT").unwrap());
        let name = std::env::var("IVY_MEMORY_JOB").unwrap();
        if mode == "owner" {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let process = launch(&root, "tree", &name, true).await;
                fs::write(root.join("helper"), process.process.pid.to_string()).unwrap();
                tokio::time::sleep(Duration::from_secs(15)).await;
                let _ = process.process.stop(Duration::ZERO).await;
            });
            return;
        }
        let owned_job = job(&name);
        // First fixture action: no requested allocation or descendant runs until
        // the actual kernel membership and installed hard cap have been checked.
        assert_limit(&owned_job);
        assert!(membership(unsafe { GetCurrentProcess() }, &owned_job));
        fs::write(
            root.join(if mode == "descendant" {
                "descendant"
            } else {
                "child"
            }),
            std::process::id().to_string(),
        )
        .unwrap();
        let mut descendant = if mode == "aggregate" || mode == "tree" {
            Some(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "memory_limit_cases::windows::memory_child_fixture",
                        "--nocapture",
                    ])
                    .env("IVY_MEMORY_FIXTURE", "descendant")
                    .creation_flags(0x08000000)
                    .spawn()
                    .unwrap(),
            )
        } else {
            None
        };
        let mut pages = Pages::new();
        match mode.as_str() {
            "normal" => {
                gate(&root, "go");
                assert!(pages.commit(4 * 1024 * 1024));
                std::io::stdout().write_all(&vec![b'x'; 150000]).unwrap();
                eprintln!("bounded-memory-diagnostic");
            }
            "fake" => {
                gate(&root, "go");
                eprintln!("out of memory JOB_OBJECT_MSG_JOB_MEMORY_LIMIT10");
                std::process::exit(23);
            }
            "single" | "descendant" => {
                gate(
                    &root,
                    if mode == "descendant" {
                        "allocate-descendant"
                    } else {
                        "go"
                    },
                );
                let maximum = if mode == "descendant" { 20 } else { 40 };
                let mut committed = 0;
                for _ in 0..maximum {
                    if !pages.commit(2 * 1024 * 1024) {
                        break;
                    }
                    committed += 2 * 1024 * 1024;
                }
                // A real native notice may retire us before this diagnostic write.
                let _ = fs::write(root.join("allocation-result"), committed.to_string());
                std::thread::sleep(Duration::from_secs(2));
            }
            "aggregate" => {
                gate(&root, "go");
                assert!(pages.commit(32 * 1024 * 1024));
                fs::write(
                    root.join("parent-commit"),
                    (32 * 1024 * 1024u32).to_string(),
                )
                .unwrap();
                fs::write(root.join("allocate-descendant"), b"go").unwrap();
                std::thread::sleep(Duration::from_secs(3));
            }
            "tree" => {
                gate(&root, "exit");
            }
            _ => panic!("unknown memory fixture"),
        }
        if let Some(child) = descendant.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[tokio::test]
    async fn memory_limit_rejects_escape_modes_before_any_payload() {
        let root = super::super::Root::new();
        let (command, executables) = fixture_command();
        for (without_job, breakaway) in [(true, false), (false, true)] {
            let mut launch = context(&root.0, "normal", r"Global\Ivy.memory-rejected", false);
            launch.without_job = without_job;
            launch.allow_windows_breakaway = breakaway;
            let result = process::spawn_exact_environment_with_memory_limit(
                &command,
                &root.0,
                &executables,
                launch,
                limit(),
            )
            .await;
            assert!(result.is_err());
            let error = result.err().unwrap().error;
            record("rejected-mode", (&error, without_job, breakaway));
            assert_eq!(error.code, "invalid_arguments");
            assert!(!root.0.join("child").exists());
        }
    }

    #[tokio::test]
    async fn memory_limit_is_installed_before_execution_and_normal_capture_is_bounded() {
        let root = super::super::Root::new();
        let name = format!(r"Global\Ivy.memory-{}", uuid::Uuid::new_v4());
        let process = launch(&root.0, "normal", &name, false).await;
        let child = held_process(published(&root.0, "child").await);
        let helper = held_process(process.process.pid);
        let owned_job = job(&name);
        assert_limit(&owned_job);
        assert!(!membership(unsafe { GetCurrentProcess() }, &owned_job));
        assert!(!membership(helper.as_raw_handle(), &owned_job));
        assert_eq!(process.memory_limit_evidence(), None);
        fs::write(root.0.join("go"), b"go").unwrap();
        let result = complete(&process.process).await;
        record("normal", (&result, process.memory_limit_evidence()));
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.error_code, None);
        assert!(result.truncated);
        assert_eq!(result.stdout.len(), 65536);
        assert!(result.stderr.contains("bounded-memory-diagnostic"));
        assert_eq!(process.memory_limit_evidence(), None);
        assert!(
            process::owned_process_tree_stopped(&process.process)
                .await
                .unwrap()
        );
        assert_signaled(&child);
        assert_signaled(&helper);
        assert_empty(&owned_job);
    }

    #[tokio::test]
    async fn memory_limit_native_notice_is_positive_for_single_and_aggregate_commit() {
        for mode in ["single", "aggregate"] {
            let root = super::super::Root::new();
            let name = format!(r"Global\Ivy.memory-{}", uuid::Uuid::new_v4());
            let mut process = launch(&root.0, mode, &name, true).await;
            let child = held_process(published(&root.0, "child").await);
            let helper = held_process(process.process.pid);
            let descendant = if mode == "aggregate" {
                Some(held_process(published(&root.0, "descendant").await))
            } else {
                None
            };
            let owned_job = job(&name);
            let out = drain(process.process.stdout.take().unwrap());
            let err = drain(process.process.stderr.take().unwrap());
            fs::write(root.0.join("go"), b"go").unwrap();
            let result = complete(&process.process).await;
            let stdout = eof(out).await;
            let stderr = eof(err).await;
            let evidence = process.memory_limit_evidence();
            record(mode, (&result, evidence, stdout.len(), stderr.len()));
            assert_eq!(result.error_code, None);
            let evidence = evidence.expect(
                "actual native Job-memory-limit notice is required for this qualified fixture",
            );
            assert_eq!(
                evidence.configured_bytes,
                limit().maximum_job_commit_bytes.get()
            );
            assert_eq!(evidence.accounting, MemoryAccounting::WindowsJobCommitted);
            assert_eq!(
                evidence.native_notice,
                MemoryLimitNotice::WindowsJobMemoryLimit
            );
            if mode == "aggregate" {
                assert_eq!(
                    fs::read_to_string(root.0.join("parent-commit")).unwrap(),
                    "33554432"
                );
            }
            assert!(
                process::owned_process_tree_stopped(&process.process)
                    .await
                    .unwrap()
            );
            assert_signaled(&child);
            assert_signaled(&helper);
            if let Some(descendant) = descendant {
                assert_signaled(&descendant);
            }
            assert_empty(&owned_job);
            assert_eq!(process.memory_limit_evidence(), Some(evidence));
        }
    }

    #[tokio::test]
    async fn memory_limit_never_infers_a_notice_from_stderr_or_exit() {
        let root = super::super::Root::new();
        let name = format!(r"Global\Ivy.memory-{}", uuid::Uuid::new_v4());
        let mut process = launch(&root.0, "fake", &name, true).await;
        published(&root.0, "child").await;
        let out = drain(process.process.stdout.take().unwrap());
        let err = drain(process.process.stderr.take().unwrap());
        fs::write(root.0.join("go"), b"go").unwrap();
        let result = complete(&process.process).await;
        let _ = eof(out).await;
        let stderr = eof(err).await;
        assert!(String::from_utf8(stderr).unwrap().contains("out of memory"));
        record("fake", (&result, process.memory_limit_evidence()));
        assert_eq!(result.exit_code, Some(23));
        assert_eq!(result.error_code, None);
        assert_eq!(process.memory_limit_evidence(), None);
        assert!(
            process::owned_process_tree_stopped(&process.process)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn memory_limit_timeout_drop_and_helper_death_use_the_same_retirement_and_eof() {
        for action in ["timeout", "drop", "helper-death"] {
            let root = super::super::Root::new();
            let name = format!(r"Global\Ivy.memory-{}", uuid::Uuid::new_v4());
            let mut process = launch(&root.0, "tree", &name, true).await;
            let child = held_process(published(&root.0, "child").await);
            let descendant = held_process(published(&root.0, "descendant").await);
            let helper = held_process(process.process.pid);
            let owned_job = job(&name);
            let out = drain(process.process.stdout.take().unwrap());
            let err = drain(process.process.stderr.take().unwrap());
            let completion = process.process.completion.clone();
            match action {
                "timeout" => {
                    assert!(
                        tokio::time::timeout(Duration::from_millis(50), completion.wait())
                            .await
                            .is_err()
                    );
                    process
                        .process
                        .stop(Duration::from_millis(20))
                        .await
                        .unwrap();
                }
                "drop" => {
                    drop(process);
                }
                _ => {
                    use windows_sys::Win32::System::Threading::{
                        PROCESS_TERMINATE, TerminateProcess,
                    };
                    let raw = unsafe { OpenProcess(PROCESS_TERMINATE, 0, process.process.pid) };
                    assert!(!raw.is_null());
                    let termination = unsafe { OwnedHandle::from_raw_handle(raw) };
                    assert_ne!(
                        unsafe { TerminateProcess(termination.as_raw_handle(), 1) },
                        0
                    );
                }
            }
            let result = tokio::time::timeout(Duration::from_secs(5), completion.wait())
                .await
                .unwrap();
            record(action, &result);
            assert_eq!(result.error_code, None);
            let _ = eof(out).await;
            let _ = eof(err).await;
            assert_signaled(&child);
            assert_signaled(&descendant);
            assert_signaled(&helper);
            assert_empty(&owned_job);
        }
    }

    #[tokio::test]
    async fn memory_limit_parent_death_retires_the_tree_despite_an_outside_job_handle() {
        let root = super::super::Root::new();
        let name = format!(r"Global\Ivy.memory-{}", uuid::Uuid::new_v4());
        let launch = context(&root.0, "owner", &name, false);
        let mut owner = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "memory_limit_cases::windows::memory_child_fixture",
                "--nocapture",
            ])
            .env_clear()
            .envs(launch.environment)
            .creation_flags(0x08000000)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let child = held_process(published(&root.0, "child").await);
        let descendant = held_process(published(&root.0, "descendant").await);
        let helper = held_process(published(&root.0, "helper").await);
        let owned_job = job(&name);
        owner.kill().unwrap();
        owner.wait().unwrap();
        for handle in [&child, &descendant, &helper] {
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle(), 5000) },
                WAIT_OBJECT_0
            );
        }
        assert_empty(&owned_job);
        record(
            "parent-death",
            "direct child, descendant and helper signaled; retained Job empty",
        );
    }

    #[tokio::test]
    #[ignore = "requires the exact retained pre-memory helper image"]
    async fn memory_limit_old_helper_refuses_before_payload_execution() {
        let root = super::super::Root::new();
        let (command, executables) = fixture_command();
        let mut launch = context(&root.0, "normal", r"Global\Ivy.memory-old-helper", true);
        launch.job_launcher = Some(PathBuf::from(
            std::env::var_os("IVY_PRE_MEMORY_JOB").expect("exact retained helper is required"),
        ));
        let result = process::spawn_exact_environment_with_memory_limit(
            &command,
            &root.0,
            &executables,
            launch,
            limit(),
        )
        .await;
        assert!(result.is_err());
        let failure = result.err().unwrap();
        record("pre-memory-helper", &failure);
        assert_eq!(failure.error.code, "spawn_failed");
        assert_eq!(failure.error.outcome, ivy_contracts::Outcome::Unknown);
        assert!(!root.0.join("child").exists());
    }
}
