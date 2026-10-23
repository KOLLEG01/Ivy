use super::*;
use crate::{
    native_pipe::{NativeShutdown, NativeShutdownError},
    native_rpc::{NativeHandler, NativeInvocation, NativeOutcome, NativeRequest},
};
use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use std::{
    fs,
    io::{Read, Write},
    os::windows::{fs::OpenOptionsExt, process::CommandExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
const EPOCH: &str = "00112233-4455-6677-8899-aabbccddeeff";
struct Runtime {
    mode: String,
    shutdowns: Arc<AtomicUsize>,
}
impl NativeHandler for Runtime {
    fn cleanup_slots(&self) -> usize {
        4
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        assert!(matches!(request.method(), "status" | "heartbeat"));
        Box::pin(async { NativeOutcome::result("{\"configured\":false}".into()) })
    }
}
impl NativeRuntime for Runtime {
    fn shutdown(&self) -> NativeShutdown {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.clone();
        Box::pin(async move {
            match mode.as_str() {
                "shutdown-error" => Err(NativeShutdownError::OutcomeUnknown),
                "shutdown-pending" => std::future::pending().await,
                _ => Ok(()),
            }
        })
    }
}
#[test]
fn bootstrap_fixture() {
    let Some(arguments) = std::env::var_os("IVY_PHONE_BOOTSTRAP_ARGUMENTS") else {
        return;
    };
    let args: Vec<OsString> = parse(arguments.to_str().unwrap(), 8192)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().into())
        .collect();
    let mode = std::env::var("IVY_PHONE_BOOTSTRAP_MODE").unwrap();
    let factories = Arc::new(AtomicUsize::new(0));
    let calls = factories.clone();
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let cleaned = shutdowns.clone();
    let in_runtime = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered = in_runtime.clone();
    let code = run_owned_native_entry(&args, move || {
        calls.fetch_add(1, Ordering::SeqCst);
        entered.store(
            tokio::runtime::Handle::try_current().is_ok(),
            Ordering::SeqCst,
        );
        match mode.as_str() {
            "factory-error" => return Err(()),
            "factory-panic" => {
                // The entry must return without waiting again for partial
                // constructor work. Process exit owns this inert worker.
                let (entered, wait) = std::sync::mpsc::sync_channel(1);
                tokio::task::spawn_blocking(move || {
                    entered.send(()).unwrap();
                    std::thread::park();
                });
                wait.recv_timeout(Duration::from_secs(1)).unwrap();
                panic!("private-bootstrap-constructor-payload");
            }
            _ => (),
        }
        Ok(Arc::new(Runtime {
            mode,
            shutdowns: cleaned,
        }) as Arc<dyn NativeRuntime>)
    });
    let result = IvyJson::object([
        ("exitCode", IvyJson::Number(code.into())),
        (
            "factories",
            IvyJson::Number(factories.load(Ordering::SeqCst) as f64),
        ),
        (
            "shutdowns",
            IvyJson::Number(shutdowns.load(Ordering::SeqCst) as f64),
        ),
        ("factoryInRuntime", in_runtime.load(Ordering::SeqCst).into()),
    ]);
    fs::write(
        std::env::var_os("IVY_PHONE_BOOTSTRAP_RESULT").unwrap(),
        stringify(&result, 4096).unwrap(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !std::path::Path::new(&std::env::var_os("IVY_PHONE_BOOTSTRAP_RELEASE").unwrap()).exists()
    {
        assert!(
            Instant::now() < deadline,
            "Owned bootstrap release unavailable"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    std::process::exit(code);
}
struct Root {
    path: PathBuf,
    parent: PathBuf,
}
impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!(
            "ivy-phone-bootstrap-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        Self { path, parent }
    }
    fn dir(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::create_dir(&path).unwrap();
        path
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let path = fs::canonicalize(&self.path).unwrap();
        assert_eq!(path.parent(), Some(self.parent.as_path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-phone-bootstrap-")
        );
        fs::remove_dir_all(path).unwrap();
    }
}
struct Worker {
    child: Child,
    result: PathBuf,
    release: PathBuf,
}
impl Worker {
    fn start(args: &[OsString], mode: &str, root: &Root, label: &str, input: &[u8]) -> Self {
        let result = root.path.join(format!("{label}-result.json"));
        let release = root.path.join(format!("{label}-release"));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "native_entry::bootstrap_tests::bootstrap_fixture",
            "--nocapture",
            "--format=terse",
        ]);
        let arguments = IvyJson::Array(args.iter().map(|s| s.to_str().unwrap().into()).collect());
        command
            .env(
                "IVY_PHONE_BOOTSTRAP_ARGUMENTS",
                stringify(&arguments, 8192).unwrap(),
            )
            .env("IVY_PHONE_BOOTSTRAP_RESULT", &result)
            .env("IVY_PHONE_BOOTSTRAP_RELEASE", &release)
            .env("IVY_PHONE_BOOTSTRAP_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        Self {
            child,
            result,
            release,
        }
    }
    fn result(&mut self) -> IvyJson {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if let Ok(text) = fs::read_to_string(&self.result) {
                if let Ok(value) = parse(&text, 4096) {
                    return value;
                }
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                let mut error = String::new();
                self.child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut error)
                    .unwrap();
                panic!("Bootstrap fixture exited before result: {status} {error}");
            }
            assert!(Instant::now() < deadline, "Bootstrap result unavailable");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn exit(&mut self, code: i32) -> String {
        fs::write(&self.release, b"release").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(code));
                break;
            }
            assert!(Instant::now() < deadline, "Owned bootstrap did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut output = String::new();
        self.child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        let expected = "\nrunning 1 test\n";
        assert_eq!(output.replace('\r', ""), expected);
        let mut error = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        error.replace('\r', "")
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn args(root: &std::path::Path) -> Vec<OsString> {
    vec![
        "--epoch".into(),
        EPOCH.into(),
        "--owner-root".into(),
        root.into(),
    ]
}
fn locked(root: &std::path::Path) -> bool {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(root.join("phone-native.lock"))
        .is_err()
}
fn trace(name: &str, rows: &[IvyJson]) {
    if let Some(root) = std::env::var_os("IVY_PHONE_BOOTSTRAP_TRACE") {
        fs::write(
            std::path::Path::new(&root).join(format!("{name}.json")),
            stringify(&IvyJson::Array(rows.to_vec()), 131072).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn native_entry_phase_exit_and_process_lifetime_fence_agree() {
    let mut rows = Vec::new();
    {
        for case in [
            "usage",
            "relative",
            "missing",
            "record",
            "epoch-path",
            "eof",
            "partial",
            "invalid",
        ] {
            let root = Root::new();
            let owner = root.dir("owner");
            let mut arguments = args(&owner);
            match case {
                "usage" => arguments[0] = "--Epoch".into(),
                "relative" => arguments[3] = "relative-owner-root".into(),
                "missing" => arguments[3] = root.path.join("absent").into(),
                "record" => fs::write(owner.join("phone-native-owner.json"), b"{}").unwrap(),
                "epoch-path" => arguments[1] = format!("\t{EPOCH}\r\n").into(),
                _ => (),
            }
            let input: &[u8] = match case {
                "partial" => b"{",
                "invalid" => b"{bad}\n",
                _ => b"",
            };
            let code = match case {
                "usage" => 64,
                "relative" | "missing" | "record" | "epoch-path" | "invalid" => 1,
                _ => 0,
            };
            let admitted = matches!(case, "eof" | "partial" | "invalid");
            let mut worker = Worker::start(&arguments, "normal", &root, "worker", input);
            let result = worker.result();
            assert_eq!(
                result.get("exitCode").unwrap().as_f64(),
                Some(f64::from(code))
            );
            if admitted {
                assert!(locked(&owner));
                let record = crate::native_owner::NativeOwner::parse(
                    &fs::read(owner.join("phone-native-owner.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(record.pid(), worker.child.id());
                assert!(record.ensure_predecessor_exited().is_err());
            } else {
                assert_eq!(
                    owner.join("phone-native-owner.json").exists(),
                    case == "record"
                );
                if case == "record" {
                    assert_eq!(
                        fs::read(owner.join("phone-native-owner.json")).unwrap(),
                        b"{}"
                    );
                }
                assert_eq!(
                    owner.join("phone-native.lock").exists(),
                    matches!(case, "record" | "epoch-path")
                );
                assert!(
                    fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .share_mode(0)
                        .create(true)
                        .truncate(false)
                        .open(owner.join("phone-native.lock"))
                        .is_ok()
                );
            }
            {
                assert_eq!(
                    result.get("factories").unwrap().as_f64(),
                    Some(if admitted { 1.0 } else { 0.0 })
                );
                assert_eq!(
                    result.get("shutdowns").unwrap().as_f64(),
                    Some(if admitted { 1.0 } else { 0.0 })
                );
                assert_eq!(
                    result.get("factoryInRuntime").unwrap().as_bool(),
                    Some(admitted)
                );
            }
            let stderr = worker.exit(code);
            assert_eq!(
                stderr,
                if code == 1 {
                    "phone_runtime_failed\n"
                } else {
                    ""
                }
            );
            rows.push(IvyJson::object([
                ("case", case.into()),
                ("exitCode", IvyJson::Number(code.into())),
                ("leaseRetainedAfterReturn", admitted.into()),
                ("stderr", stderr.into()),
            ]));
            trace("entry-phases", &rows);
        }
        let root = Root::new();
        let owner = root.dir("owner");
        let arguments = args(&owner);
        let mut first = Worker::start(&arguments, "normal", &root, "first", b"");
        assert_eq!(first.result().get("exitCode").unwrap().as_f64(), Some(0.0));
        let saved = fs::read(owner.join("phone-native-owner.json")).unwrap();
        let identity = crate::native_owner::NativeOwner::parse(&saved).unwrap();
        let mut collision = Worker::start(&arguments, "normal", &root, "collision", b"");
        assert_eq!(
            collision.result().get("exitCode").unwrap().as_f64(),
            Some(1.0)
        );
        assert_eq!(collision.exit(1), "phone_runtime_failed\n");
        assert_eq!(
            fs::read(owner.join("phone-native-owner.json")).unwrap(),
            saved
        );
        assert!(locked(&owner));
        assert_eq!(first.exit(0), "");
        assert!(identity.ensure_predecessor_exited().is_ok());
        let mut replacement = Worker::start(&arguments, "normal", &root, "replacement", b"");
        assert_eq!(
            replacement.result().get("exitCode").unwrap().as_f64(),
            Some(0.0)
        );
        assert_eq!(replacement.exit(0), "");
        rows.push(IvyJson::object([
            ("case", "collision-and-replacement".into()),
            ("leaseRetainedAfterReturn", true.into()),
            ("predecessorExitVerified", true.into()),
            ("recordPreservedOnCollision", true.into()),
        ]));
        trace("entry-phases", &rows);
    }
    assert_eq!(rows.len(), 9);
}
#[test]
fn failed_factory_and_unconfirmed_shutdown_keep_the_process_fence_without_duplicate_cleanup() {
    let mut rows = Vec::new();
    for mode in [
        "factory-error",
        "factory-panic",
        "shutdown-error",
        "shutdown-pending",
    ] {
        let root = Root::new();
        let owner = root.dir("owner");
        let mut worker = Worker::start(&args(&owner), mode, &root, "worker", b"");
        let result = worker.result();
        assert_eq!(result.get("exitCode").unwrap().as_f64(), Some(1.0));
        assert_eq!(result.get("factories").unwrap().as_f64(), Some(1.0));
        assert_eq!(
            result.get("factoryInRuntime").unwrap().as_bool(),
            Some(true)
        );
        assert_eq!(
            result.get("shutdowns").unwrap().as_f64(),
            Some(if mode.starts_with("factory-") {
                0.0
            } else {
                1.0
            })
        );
        assert!(locked(&owner));
        let record = crate::native_owner::NativeOwner::parse(
            &fs::read(owner.join("phone-native-owner.json")).unwrap(),
        )
        .unwrap();
        assert!(record.ensure_predecessor_exited().is_err());
        assert_eq!(worker.exit(1), "phone_runtime_failed\n");
        assert!(record.ensure_predecessor_exited().is_ok());
        rows.push(IvyJson::object([
            ("mode", mode.into()),
            ("exitCode", IvyJson::Number(1.0)),
            ("leaseRetainedAfterReturn", true.into()),
            ("redacted", true.into()),
            ("shutdowns", result.get("shutdowns").unwrap().clone()),
        ]));
        trace("entry-failures", &rows);
    }
}
