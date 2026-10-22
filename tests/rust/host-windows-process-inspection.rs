#![cfg(windows)]
#[path = "host-parent-pid-cases.rs"]
mod parent_pid_cases;
use ivy_host_runtime::platform::windows_process_inspection::*;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::windows::{
        io::{AsHandle, AsRawHandle},
        process::CommandExt,
    },
    process::{Child, Command, Stdio},
};

fn serial_fixture() -> std::sync::MutexGuard<'static, ()> {
    static FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    FIXTURE.lock().unwrap_or_else(|error| error.into_inner())
}

struct Oracle {
    child: Child,
    output: BufReader<std::process::ChildStdout>,
    rows: Vec<Value>,
    build: Option<std::path::PathBuf>,
    executable: std::path::PathBuf,
}
impl Oracle {
    fn start() -> Self {
        let mut build = None;
        let executable = std::env::var_os("IVY_PROCESS_INSPECTION_ORACLE").unwrap_or_else(|| {
            let directory = std::env::temp_dir().join(format!(
                "ivy-owned-process-inspection-{}",
                uuid::Uuid::new_v4()
            ));
            let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../tests/fixtures/host-process-inspection");
            std::fs::create_dir_all(&directory).unwrap();
            let output = Command::new("dotnet")
                .arg("build")
                .arg(fixture.join("HostProcessInspectionFixture.csproj"))
                .args([
                    "--configuration",
                    "Release",
                    "--disable-build-servers",
                    "--verbosity",
                    "quiet",
                ])
                .arg("--output")
                .arg(directory.join("bin"))
                .arg(format!(
                    "/p:BaseIntermediateOutputPath={}/",
                    directory.join("obj").display()
                ))
                .arg(format!(
                    "/p:RestoreConfigFile={}",
                    fixture.join("NuGet.Config").display()
                ))
                .args(["/p:NuGetAudit=false", "/p:UseSharedCompilation=false"])
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "Owned fixture build failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let executable = directory.join("bin/HostProcessInspectionFixture.exe");
            build = Some(directory);
            executable.into_os_string()
        });
        let executable = std::path::PathBuf::from(executable);
        let mut child = Command::new(&executable)
            .creation_flags(0x08000000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            output,
            rows: Vec::new(),
            build,
            executable,
        }
    }
    fn read(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(
            self.output.read_line(&mut line).unwrap(),
            0,
            "Original fixture closed unexpectedly"
        );
        serde_json::from_str(&line).unwrap()
    }
    fn command(&mut self, command: &str) -> Value {
        writeln!(self.child.stdin.as_mut().unwrap(), "{command}").unwrap();
        self.child.stdin.as_mut().unwrap().flush().unwrap();
        self.read()
    }
    fn check(&mut self, label: &str, actual: Value, original: Value) {
        assert_eq!(actual, original, "{label}");
        self.rows
            .push(json!({"label":label,"actual":actual,"original":original}));
    }
}

#[test]
fn additional_caller_named_unicode_discovery_matches_original() {
    let _fixture = serial_fixture();
    let mut oracle = Oracle::start();
    let hello = oracle.read();
    let directory =
        std::env::temp_dir().join(format!("ivy-owned-process-names-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let parent = oracle.executable.parent().unwrap();
    for suffix in ["dll", "deps.json", "runtimeconfig.json"] {
        std::fs::copy(
            parent.join(format!("HostProcessInspectionFixture.{suffix}")),
            directory.join(format!("HostProcessInspectionFixture.{suffix}")),
        )
        .unwrap();
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for tail in ["é", "ß", "ı", "İ", "σ", "ς", "ſ", "𐐨"] {
            let name = format!("IvyOwnedInspection-{tail}");
            let executable = directory.join(format!("{name}.exe"));
            std::fs::copy(&oracle.executable, &executable).unwrap();
            let child = Command::new(&executable)
                .arg("--child")
                .creation_flags(0x08000000)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            struct ChildOwner(Child);
            impl Drop for ChildOwner {
                fn drop(&mut self) {
                    drop(self.0.stdin.take());
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            let child = ChildOwner(child);
            for query in [
                name.clone(),
                name.to_uppercase(),
                name.to_lowercase(),
                format!("{name}.exe"),
            ] {
                let original = oracle.command(&format!("name {query}"));
                let actual: Vec<_> = ProcessObservation::by_name(&query)
                    .unwrap()
                    .iter()
                    .map(ProcessObservation::pid)
                    .collect();
                oracle.check(&format!("unicode-name:{query}"), json!(actual), original);
            }
            drop(child);
            std::fs::remove_file(executable).unwrap();
        }
    }));
    assert!(directory.starts_with(std::env::temp_dir()));
    let _ = std::fs::remove_dir_all(&directory);
    if let Some(path) = std::env::var_os("IVY_PROCESS_INSPECTION_OBSERVATION") {
        let path = std::path::PathBuf::from(path).with_extension("unicode.json");
        std::fs::write(
            path,
            serde_json::to_vec_pretty(
                &json!({"hello":hello,"comparisons":oracle.rows,"completed":result.is_ok()}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
    oracle.command("done");
    assert!(oracle.child.wait().unwrap().success());
}

#[test]
fn additional_token_failure_context_and_handle_retirement_match_original() {
    let _fixture = serial_fixture();
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn GetCurrentThread() -> *mut std::ffi::c_void;
        fn GetProcessHandleCount(process: *mut std::ffi::c_void, count: *mut u32) -> i32;
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ImpersonateAnonymousToken(thread: *mut std::ffi::c_void) -> i32;
        fn RevertToSelf() -> i32;
    }
    let mut oracle = Oracle::start();
    let hello = oracle.read();
    let original = oracle.command("wrong-token");
    let pid = hello["pid"].as_u64().unwrap() as u32;
    let process = open_process_query_limited(pid).unwrap();
    let error = UserIdentity::from_token(process.as_handle()).err().unwrap();
    oracle.check(
        "wrong-native-handle-is-not-an-identity",
        json!(false),
        original["ok"].clone(),
    );
    assert_eq!(error.raw_os_error(), Some(6));
    // A failed borrow must not close the original process handle.
    let token = open_process_token_query(process.as_handle()).unwrap();
    drop(token);
    drop(process);
    let original = oracle.command("anonymous");
    let mut caller = UserIdentity::current().unwrap();
    let caller_sid = caller.user_sid().unwrap();
    drop(caller);
    let observed = std::thread::spawn(move || {
        assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
        struct Revert;
        impl Drop for Revert {
            fn drop(&mut self) {
                assert_ne!(unsafe { RevertToSelf() }, 0);
            }
        }
        let _revert = Revert;
        UserIdentity::current()
            .map(|mut identity| {
                let sid = identity.user_sid().unwrap();
                (sid.is_some(), sid == caller_sid)
            })
            .map_err(|error| error.raw_os_error())
    })
    .join()
    .unwrap();
    oracle.check(
        "anonymous-effective-identity-result",
        json!(observed.is_ok()),
        original["original"]["ok"].clone(),
    );
    if let Ok((has_user, same_user)) = observed {
        oracle.check(
            "anonymous-has-user",
            json!(has_user),
            json!(!original["original"]["value"].is_null()),
        );
        oracle.check(
            "anonymous-does-not-infer-process-user",
            json!(same_user),
            json!(original["original"]["value"] == hello["sid"]),
        );
    } else {
        oracle.check(
            "anonymous-native-error-retained",
            json!(observed.err().unwrap()),
            original["probeError"].clone(),
        );
    }
    let mut identity = UserIdentity::current().unwrap();
    let sid = identity.user_sid().unwrap();
    drop(identity);
    let mut before = 0;
    assert_ne!(
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut before) },
        0
    );
    for _ in 0..128 {
        let process = open_process_query_limited(pid).unwrap();
        let token = open_process_token_query(process.as_handle()).unwrap();
        let mut identity = UserIdentity::from_token(token.as_handle()).unwrap();
        drop(token);
        drop(process);
        assert_eq!(identity.user_sid().unwrap(), sid);
    }
    let mut after = 0;
    assert_ne!(
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut after) },
        0
    );
    assert_eq!(
        after, before,
        "Owned process/token/duplicate handles must retire exactly."
    );
    if let Some(path) = std::env::var_os("IVY_PROCESS_INSPECTION_OBSERVATION") {
        let path = std::path::PathBuf::from(path).with_extension("tokens.json");
        std::fs::write(path,serde_json::to_vec_pretty(&json!({"hello":hello,"comparisons":oracle.rows,"handleCountBefore":before,"handleCountAfter":after,"identityLifetimes":128})).unwrap()).unwrap();
    }
    oracle.command("done");
    assert!(oracle.child.wait().unwrap().success());
}
impl Drop for Oracle {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(directory) = &self.build {
            assert!(directory.starts_with(std::env::temp_dir()));
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}
fn value(row: &Value, key: &str) -> Value {
    assert_eq!(row[key]["ok"], true, "{key}: {row}");
    row[key]["value"].clone()
}

#[test]
fn original_metadata_cache_token_and_owned_window_boundaries_agree() {
    let _fixture = serial_fixture();
    let mut oracle = Oracle::start();
    let hello = oracle.read();
    let pid = hello["pid"].as_u64().unwrap() as u32;
    let child_pid = hello["childPid"].as_u64().unwrap() as u32;
    let mut observation = ProcessObservation::by_id(pid).unwrap().unwrap();
    let mut child_observation = ProcessObservation::by_id(child_pid).unwrap().unwrap();
    oracle.check("pid", json!(observation.pid()), json!(pid));
    oracle.check(
        "alive",
        json!(observation.has_exited().unwrap()),
        value(&hello["initial"], "exited"),
    );
    oracle.check(
        "session",
        json!(observation.session_id().unwrap()),
        value(&hello["initial"], "session"),
    );
    oracle.check(
        "created",
        json!(observation.created_filetime().unwrap().to_string()),
        value(&hello["initial"], "created"),
    );
    oracle.check(
        "first-module",
        json!(observation.main_module_file_name().unwrap()),
        value(&hello["initial"], "module"),
    );
    oracle.check(
        "owned-popup-is-not-main",
        json!(observation.main_window_handle().unwrap()),
        value(&hello["initial"], "window"),
    );
    assert_eq!(
        value(&hello["initial"], "window"),
        0,
        "Owned-window evidence unavailable: a preexisting eligible window exists."
    );
    assert!(same_current_session(pid).unwrap());
    let mut current = ProcessObservation::current();
    assert_eq!(current.pid(), std::process::id());
    assert_eq!(
        current.session_id().unwrap(),
        observation.session_id().unwrap()
    );
    let process = open_process_query_limited(pid).unwrap();
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetApplicationUserModelId(
            process: *mut std::ffi::c_void,
            length: *mut u32,
            text: *mut u16,
        ) -> i32;
    }
    let mut app_length = 0;
    let app_status = unsafe {
        GetApplicationUserModelId(
            process.as_handle().as_raw_handle(),
            &mut app_length,
            std::ptr::null_mut(),
        )
    };
    oracle.check(
        "borrowed-query-handle-application-status",
        json!(app_status),
        hello["appStatus"].clone(),
    );
    oracle.check(
        "borrowed-query-handle-application-length",
        json!(app_length),
        hello["appLength"].clone(),
    );
    let token = open_process_token_query(process.as_handle()).unwrap();
    let mut identity = UserIdentity::from_token(token.as_handle()).unwrap();
    drop(token);
    drop(process);
    let mut user = UserIdentity::current().unwrap();
    let sid = user.user_sid().unwrap();
    assert!(sid.is_some());
    assert_eq!(identity.user_sid().unwrap(), sid);
    let child_handle = open_process_query_limited(child_pid).unwrap();
    let child_token = open_process_token_query(child_handle.as_handle()).unwrap();
    let mut retained_child_identity = UserIdentity::from_token(child_token.as_handle()).unwrap();
    drop(child_token);
    drop(child_handle);
    let child_created = child_observation.created_filetime().unwrap();
    oracle.check(
        "child-created",
        json!(child_created.to_string()),
        value(&hello["child"], "created"),
    );
    let name = hello["name"].as_str().unwrap();
    for variant in [
        name.to_owned(),
        name.to_uppercase(),
        name.to_lowercase(),
        format!("{name}.exe"),
        "IvyNoSuchOwnedInspectionFixture".to_owned(),
    ] {
        let original = oracle.command(&format!("name {variant}"));
        let actual: Vec<u32> = ProcessObservation::by_name(&variant)
            .unwrap()
            .iter()
            .map(ProcessObservation::pid)
            .collect();
        oracle.check(&format!("name:{variant}"), json!(actual), original);
    }
    oracle.command("show");
    let shown = oracle.command("retained");
    let window = observation.main_window_handle().unwrap();
    assert_ne!(window, 0, "Owned visible-window evidence unavailable.");
    oracle.check(
        "zero-window-is-reenumerated",
        json!(window),
        value(&shown, "window"),
    );
    oracle.command("hide");
    let hidden = oracle.command("retained");
    oracle.check(
        "positive-window-is-cached",
        json!(observation.main_window_handle().unwrap()),
        value(&hidden, "window"),
    );
    let fresh = oracle.command("snapshot");
    let mut fresh_observation = ProcessObservation::by_id(pid).unwrap().unwrap();
    oracle.check(
        "fresh-object-has-no-window-cache",
        json!(fresh_observation.main_window_handle().unwrap()),
        value(&fresh, "window"),
    );
    oracle.command("deny-module");
    let denied = oracle.command("snapshot");
    let module_error = observation.main_module_file_name().unwrap_err();
    assert_eq!(denied["module"]["ok"], false);
    oracle.check(
        "late-module-denial",
        json!(module_error.raw_os_error()),
        denied["module"]["native"].clone(),
    );
    assert_eq!(
        module_error.raw_os_error(),
        Some(5),
        "Owned denial evidence unavailable."
    );
    oracle.check(
        "limited-created-survives-module-denial",
        json!(fresh_observation.created_filetime().unwrap().to_string()),
        value(&denied, "created"),
    );
    let limited = open_process_query_limited(pid).unwrap();
    drop(limited);
    oracle.command("restore");
    let restored = oracle.command("snapshot");
    oracle.check(
        "module-is-read-again-after-restore",
        json!(observation.main_module_file_name().unwrap()),
        value(&restored, "module"),
    );
    oracle.command("exit-child");
    let exited = oracle.command("child");
    oracle.check(
        "child-exit",
        json!(child_observation.has_exited().unwrap()),
        value(&exited, "exited"),
    );
    oracle.check(
        "creation-cache-outlives-exit",
        json!(child_observation.created_filetime().unwrap().to_string()),
        value(&exited, "created"),
    );
    assert_eq!(retained_child_identity.user_sid().unwrap(), sid);
    let absent = oracle.command(&format!("by-id {child_pid}"));
    oracle.check(
        "by-id-rejects-signaled-retained-pid",
        json!(ProcessObservation::by_id(child_pid).unwrap().is_some()),
        absent["ok"].clone(),
    );
    assert!(ProcessObservation::by_id(u32::MAX).unwrap().is_none());
    assert_eq!(
        open_process_query_limited(u32::MAX)
            .unwrap_err()
            .raw_os_error(),
        Some(87)
    );
    oracle.command("done");
    assert!(oracle.child.wait().unwrap().success());
    if let Some(path) = std::env::var_os("IVY_PROCESS_INSPECTION_OBSERVATION") {
        std::fs::write(path,serde_json::to_vec_pretty(&json!({"hello":hello,"comparisons":oracle.rows,"sameSid":true,"tokenRetainedAfterExit":true,"crossSessionEvidence":"unavailable","scope":"owned processes/windows only"})).unwrap()).unwrap();
    }
}
