use super::Root;
use ivy_contracts::{IvyJson, json};
use ivy_host_runtime::{exec_file::{self, SpawnCommand, SpawnOptions}, portable_native_files::NativeHelperLayout,
    process::{self, OwnedProcess, SpawnFailure}};
use std::{collections::BTreeMap, ffi::{OsStr, OsString}, fs, io::Write, path::{Path, PathBuf}, process::{Command, Stdio}, sync::Mutex, time::Duration};
static CORPUS: Mutex<()> = Mutex::new(());
fn value(value: &OsStr) -> IvyJson { IvyJson::String(ivy_host_runtime::node_path::from_native(Path::new(value))) }
fn record(name: &str, input: IvyJson, expected: IvyJson, actual: IvyJson) {
    if let Some(path) = std::env::var_os("IVY_HOST_IGNORED_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(fs::OpenOptions::new().create(true).append(true).open(path).unwrap(), "{}",
            json::stringify(&IvyJson::object([("case", name.into()), ("input", input), ("original", expected), ("native", actual)]), usize::MAX).unwrap()).unwrap();
    }
}
fn node() -> PathBuf {
    let out = Command::new("node").args(["-p", "process.execPath"]).output().unwrap();
    assert!(out.status.success()); PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
}
fn root() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..") }
fn command(receipt: &Path, mode: &str) -> SpawnCommand {
    SpawnCommand { executable: node().into_os_string(), args: vec![root().join("tests/fixtures/host-ignored-stdio-child.mjs").into_os_string(),
        receipt.as_os_str().to_owned(), mode.into()], cwd: None }
}
fn options() -> SpawnOptions {
    SpawnOptions { #[cfg(windows)] job_launcher: Some(PathBuf::from(env!("CARGO_BIN_EXE_ivy-job"))), ..Default::default() }
}
fn environment() -> BTreeMap<OsString, OsString> {
    // Explicit-env probes use only OS lookup keys and fixture values. Ambient
    // inheritance is exercised separately without copying private environment
    // values into the differential corpus.
    std::env::vars_os().filter(|(key,_)| ["PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "COMSPEC", "TEMP", "TMP", "LANG", "LC_ALL", "TZ", "NODE_NO_WARNINGS"]
        .iter().any(|name| key.to_string_lossy().eq_ignore_ascii_case(name))).collect()
}
fn oracle(command: &SpawnCommand, environment: Option<&BTreeMap<OsString, OsString>>) -> (IvyJson, IvyJson) {
    let mut input = IvyJson::object([("file", value(&command.executable)),
        ("args", IvyJson::Array(command.args.iter().map(|s| value(s)).collect()))]);
    if let Some(cwd) = &command.cwd { input.insert("cwd", value(cwd.as_os_str())); }
    if let Some(environment) = environment { input.insert("env", IvyJson::object(environment.iter().map(|(k,v)|
        (ivy_host_runtime::node_path::from_native(Path::new(k)), value(v))))); }
    let mut child = Command::new("node").arg(root().join("tests/fixtures/host-ignored-stdio-oracle.mjs"))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(json::stringify(&input, usize::MAX).unwrap().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap(); assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    (input, json::parse(&String::from_utf8(out.stdout).unwrap(), usize::MAX).unwrap())
}
async fn terminal(result: Result<OwnedProcess, SpawnFailure>) -> IvyJson {
    match result {
        Err(failure) => {
            assert_eq!(failure.error.code, "spawn_failed");
            assert_eq!(failure.error.outcome, ivy_contracts::Outcome::NotExecuted);
            let source = failure.source.unwrap();
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            IvyJson::object([("kind", "spawn".into()), ("code", "ENOENT".into())])
        }
        Ok(child) => {
            assert!(child.stdin.is_none() && child.stdout.is_none() && child.stderr.is_none());
            let result = tokio::time::timeout(Duration::from_secs(5), child.completion.wait()).await.unwrap();
            assert!(result.error_code.is_none(), "{result:?}"); assert!(!result.truncated);
            assert_eq!((&*result.stdout, &*result.stderr), ("", ""));
            assert!(process::owned_process_tree_stopped(&child).await.unwrap());
            IvyJson::object([("kind", "exit".into()), ("code", result.exit_code.map(|n| IvyJson::Number(n as f64)).unwrap_or(IvyJson::Null)),
                ("signal", result.signal.clone().map(Into::into).unwrap_or(IvyJson::Null)), ("ignored", true.into())])
        }
    }
}
fn read(path: &Path) -> IvyJson { json::parse(&fs::read_to_string(path).unwrap(), usize::MAX).unwrap() }
fn equal(a: &IvyJson, b: &IvyJson) { assert_eq!(json::canonical(a, usize::MAX).unwrap(), json::canonical(b, usize::MAX).unwrap()); }

#[tokio::test]
async fn ignored_stdio_matches_original_ambient_exact_environment_lookup_and_exit() {
    for mode in ["ambient", "explicit", "path", "relative", "exit", "missing", "missing-cwd"] {
        let scratch = Root::new(); let receipt = scratch.0.join("receipt.json");
        let mut command = command(&receipt, mode); let mut launch = options();
        command.args.extend(["".into(), "spaces and \"quotes\"".into(), "trailing\\".into(), "€😀".into()]);
        if mode == "ambient" { command.executable = "node".into(); }
        if matches!(mode, "explicit" | "path" | "relative") {
            let mut env = environment();
            env.insert("IVY_HOST_IGNORED_INHERIT".into(), "child exact override".into());
            if matches!(mode, "path" | "relative") {
                let directory = scratch.0.join("bin; quoted"); fs::create_dir(&directory).unwrap();
                let basename = if cfg!(windows) { "fixture-spawn.exe" } else { "fixture-spawn" };
                fs::copy(node(), directory.join(basename)).unwrap();
                command.executable = if mode == "path" { "fixture-spawn".into() } else { PathBuf::from("bin; quoted").join(basename).into_os_string() };
                command.cwd = Some(scratch.0.clone());
                env.retain(|key,_| !key.to_string_lossy().eq_ignore_ascii_case("PATH"));
                env.insert("PATH".into(), if cfg!(windows) { format!("\"{}\"", directory.display()).into() } else { directory.into_os_string() });
            }
            launch.environment = Some(env);
        }
        if mode == "missing" { command.executable = scratch.0.join("missing-executable").into_os_string(); }
        if mode == "missing-cwd" { command.cwd = Some(scratch.0.join("missing-cwd")); }
        let (input, expected) = oracle(&command, launch.environment.as_ref());
        let snapshot = receipt.is_file().then(|| read(&receipt));
        let actual = terminal(exec_file::spawn_ignored(command, root(), launch).await).await;
        record(mode, input, expected.clone(), actual.clone()); equal(&actual, &expected);
        if let Some(expected) = snapshot {
            let actual = read(&receipt); equal(&actual, &expected);
            assert_eq!(actual.get("stdinBytes"), Some(&IvyJson::Number(0.0)));
            for device in actual.required("devices").unwrap().as_array().unwrap() {
                assert_eq!(device.get("pipe"), Some(&IvyJson::Bool(false)));
                assert_eq!(device.get("character"), Some(&IvyJson::Bool(true)));
            }
            if mode == "ambient" { equal(actual.required("inherited").unwrap(), &std::env::var("IVY_HOST_IGNORED_INHERIT").ok().map(Into::into).unwrap_or(IvyJson::Null)); }
            record(&format!("{mode}-effects"), IvyJson::Null, expected, actual);
        }
    }
}

#[cfg(windows)]
#[test]
fn ignored_stdio_child_fixture() {
    let Some(file) = std::env::var_os("IVY_HOST_IGNORED_PROOF") else { return };
    use windows_sys::Win32::{Storage::FileSystem::GetFileType, System::{Console::{GetConsoleWindow, GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE},
        Threading::{GetStartupInfoW, STARTF_USESHOWWINDOW, STARTUPINFOW}}};
    use std::io::Read;
    let mut input = Vec::new(); std::io::stdin().read_to_end(&mut input).unwrap();
    let mut info: STARTUPINFOW = unsafe { std::mem::zeroed() }; unsafe { GetStartupInfoW(&mut info) };
    let devices = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].into_iter().map(|id|
        IvyJson::Number(unsafe { GetFileType(GetStdHandle(id)) } as f64)).collect();
    let result = IvyJson::object([("stdinBytes", IvyJson::Number(input.len() as f64)), ("devices", IvyJson::Array(devices)),
        ("hidden", (info.dwFlags & STARTF_USESHOWWINDOW != 0 && info.wShowWindow == 0).into()),
        ("console", (!unsafe { GetConsoleWindow() }.is_null()).into())]);
    fs::write(file, json::stringify(&result, usize::MAX).unwrap()).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn ignored_stdio_component_helper_is_admitted_and_windows_handles_are_hidden_null_devices() {
    let scratch = Root::new(); let root = ivy_host_runtime::paths::realpath(&scratch.0).unwrap();
    let native = root.join("dist/native"); fs::create_dir_all(&native).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_ivy-job"), native.join("ivy-job.exe")).unwrap();
    assert_eq!(SpawnOptions::for_component(&root, NativeHelperLayout::Target).err().unwrap().code, "artifact_missing");
    let target = native.join(if cfg!(target_arch="x86_64") { "win32-x64" } else { "win32-arm64" }); fs::create_dir(&target).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_ivy-job"), target.join("ivy-job.exe")).unwrap();
    let mut options = SpawnOptions::for_component(&root, NativeHelperLayout::Target).unwrap();
    assert_eq!(options.job_launcher, Some(target.join("ivy-job.exe")));
    let file = root.join("proof.json"); let mut env = environment();
    env.insert("IVY_HOST_IGNORED_PROOF".into(), file.as_os_str().to_owned()); options.environment = Some(env);
    let command = SpawnCommand { executable: std::env::current_exe().unwrap().into_os_string(), cwd: None,
        args: vec!["--exact".into(), "ignored_stdio_cases::ignored_stdio_child_fixture".into(), "--nocapture".into()] };
    let (input, expected) = oracle(&command, options.environment.as_ref()); let expected_effects = read(&file);
    let actual = terminal(exec_file::spawn_ignored(command, root, options).await).await;
    let effects = read(&file); equal(&actual, &expected); equal(&effects, &expected_effects);
    assert_eq!(effects.get("hidden"), Some(&IvyJson::Bool(true))); assert_eq!(effects.get("console"), Some(&IvyJson::Bool(false)));
    assert_eq!(effects.get("stdinBytes"), Some(&IvyJson::Number(0.0)));
    assert!(effects.required("devices").unwrap().as_array().unwrap().iter().all(|v|v == &IvyJson::Number(2.0)));
    record("windows-handle-status", input, expected, actual); record("windows-handle-effects", IvyJson::Null, expected_effects, effects);
}

async fn identities(file: &Path) -> Vec<u32> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = fs::read_to_string(file) { if let Ok(value) = json::parse(&text, usize::MAX) {
                if let (Some(pid), Some(descendant)) = (value.get("pid").and_then(IvyJson::as_f64), value.get("descendant").and_then(IvyJson::as_f64)) { return vec![pid as u32, descendant as u32] }
            } }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap()
}
#[cfg(windows)]
struct Handles(Vec<windows_sys::Win32::Foundation::HANDLE>);
#[cfg(windows)]
impl Handles {
    fn open(pids: &[u32]) -> Self { use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        Self(pids.iter().map(|pid| { let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, *pid) }; assert!(!h.is_null()); h }).collect()) }
    fn stopped(&self) -> bool { self.0.iter().all(|h| unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(*h, 0) } == 0) }
}
#[cfg(windows)]
impl Drop for Handles { fn drop(&mut self) { for h in &self.0 { unsafe { windows_sys::Win32::Foundation::CloseHandle(*h); } } } }

#[tokio::test]
async fn ignored_stdio_observers_deadlines_exit_and_drop_keep_tree_retirement_owned() {
    for phase in ["normal-exit", "deadline", "drop-owner"] {
        let scratch = Root::new(); let file = scratch.0.join("identities.json");
        let child = exec_file::spawn_ignored(command(&file, "tree"), root(), options()).await.unwrap();
        let completion = child.completion.clone(); let mut pids = identities(&file).await; pids.push(child.pid);
        #[cfg(windows)] let handles = Handles::open(&pids);
        let observer = tokio::spawn({ let completion = completion.clone(); async move { completion.wait().await } }); observer.abort();
        assert!(tokio::time::timeout(Duration::from_millis(40), completion.wait()).await.is_err());
        #[cfg(windows)] assert!(!handles.stopped());
        if phase == "drop-owner" { drop(child); } else {
            if phase == "normal-exit" { fs::write(file.with_extension("json.release"), b"release").unwrap(); }
            else { child.stop(Duration::ZERO).await.unwrap(); }
            let result = tokio::time::timeout(Duration::from_secs(5), completion.wait()).await.unwrap();
            assert!(result.error_code.is_none(), "{phase}: {result:?}");
            assert!(process::owned_process_tree_stopped(&child).await.unwrap());
            if phase == "normal-exit" { assert_eq!(result.exit_code, Some(0)); }
        }
        let result = tokio::time::timeout(Duration::from_secs(5), completion.wait()).await.unwrap(); assert!(result.error_code.is_none(), "{phase}: {result:?}");
        #[cfg(windows)] let retired = handles.stopped();
        #[cfg(unix)] let retired = pids.iter().all(|pid| unsafe { libc::kill(*pid as i32, 0) } == -1);
        assert!(retired, "{phase}");
        record(phase, IvyJson::Array(pids.into_iter().map(|p|IvyJson::Number(p as f64)).collect()), IvyJson::Null,
            IvyJson::object([("retired", retired.into()), ("exitCode", result.exit_code.map(|n|IvyJson::Number(n as f64)).unwrap_or(IvyJson::Null)),
                ("errorCode", result.error_code.clone().map(Into::into).unwrap_or(IvyJson::Null))]));
    }
}
