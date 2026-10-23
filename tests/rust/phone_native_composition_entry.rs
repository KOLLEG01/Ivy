use super::*;
use ivy_phone_bridge::{native_entry::run_owned_native_entry, native_owner::NativeOwner};
use std::{
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Read},
    os::windows::{fs::OpenOptionsExt, process::CommandExt},
    process::{Child, ChildStdin},
    sync::mpsc,
    time::Instant,
};
const CHILD: &str = "call_audio::session::native_operations::commands::composition::entry::owned_composition_fixture";
#[test]
#[ignore = "Owned child process fixture; invoked only by the bounded entry check"]
fn owned_composition_fixture() {
    let root = PathBuf::from(std::env::var_os("IVY_PHONE_COMPOSED_ENTRY_ROOT").unwrap());
    let owner = PathBuf::from(std::env::var_os("IVY_PHONE_COMPOSED_ENTRY_OWNER").unwrap());
    let failed = std::env::var("IVY_PHONE_COMPOSED_ENTRY_FAILURE").unwrap() == "true";
    let args: Vec<OsString> = vec![
        "--epoch".into(),
        CALL.into(),
        "--owner-root".into(),
        owner.into(),
    ];
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    remaining
        .close_mode
        .store(usize::from(failed), Ordering::SeqCst);
    let media = media_factory();
    let loads = Arc::new(AtomicUsize::new(0));
    let loaded = loads.clone();
    let factories = AtomicUsize::new(0);
    let code = run_owned_native_entry(&args, || {
        factories.fetch_add(1, Ordering::SeqCst);
        let commands = NativeSipCommands::windows_with_diagnostics(
            media.clone(),
            remaining.clone(),
            |_| Ok("null".into()),
            move || {
                loaded.fetch_add(1, Ordering::SeqCst);
                panic!("No admitted codec load in entry guard check")
            },
        );
        fs::write(root.join("ready"), b"ready").unwrap();
        Ok::<_, ()>(Arc::new(commands) as Arc<dyn NativeRuntime>)
    });
    fs::write(root.join("result.json"),serde_json::to_vec(&json!({"exitCode":code,"runtimeFactories":factories.load(Ordering::SeqCst),"mediaFactories":media.starts.load(Ordering::SeqCst),"codecLoads":loads.load(Ordering::SeqCst),"remainingCloses":remaining.closes.load(Ordering::SeqCst),"remainingMethods":remaining.methods.lock().unwrap().len()})).unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !root.join("exit-release").exists() {
        assert!(Instant::now() < deadline);
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
            "ivy-phone-composed-entry-{}-{}",
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
                .starts_with("ivy-phone-composed-entry-")
        );
        fs::remove_dir_all(path).unwrap();
    }
}
struct Worker {
    child: Child,
    input: Option<ChildStdin>,
    root: PathBuf,
    frames: mpsc::Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}
impl Worker {
    fn start(root: PathBuf, owner: &Path, failed: bool) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                CHILD,
                "--nocapture",
                "--format=terse",
            ])
            .env("IVY_PHONE_COMPOSED_ENTRY_ROOT", &root)
            .env("IVY_PHONE_COMPOSED_ENTRY_OWNER", owner)
            .env("IVY_PHONE_COMPOSED_ENTRY_FAILURE", failed.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, frames) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                if line.starts_with('{') {
                    let v = serde_json::from_str(&line).unwrap();
                    if tx.send(v).is_err() {
                        return;
                    }
                } else {
                    assert!(
                        line.is_empty() || line == "running 1 test",
                        "Unexpected fixture output: {line}"
                    );
                }
            }
        });
        Self {
            child,
            input,
            root,
            frames,
            reader: Some(reader),
        }
    }
    fn file(&mut self, name: &str) -> PathBuf {
        let deadline = Instant::now() + Duration::from_secs(12);
        let file = self.root.join(name);
        while !file.exists() {
            if let Some(code) = self.child.try_wait().unwrap() {
                let mut error = String::new();
                self.child
                    .stderr
                    .as_mut()
                    .unwrap()
                    .read_to_string(&mut error)
                    .unwrap();
                panic!("Entry exited before {name}: {code} {error}");
            }
            assert!(Instant::now() < deadline, "Entry file unavailable: {name}");
            std::thread::sleep(Duration::from_millis(5));
        }
        file
    }
    fn send(&mut self, method: &str, params: &str, sequence: u64) -> Value {
        let operation = if ivy_phone_bridge::ipc::is_read_only(method) {
            "null".into()
        } else {
            format!("\"00000000-0000-4000-8000-{sequence:012x}\"")
        };
        let frame = format!(
            r#"{{"version":1,"epoch":"{CALL}","requestId":{sequence},"operationId":{operation},"method":"{method}","params":{params}}}"#
        );
        self.input
            .as_mut()
            .unwrap()
            .write_all(format!("{frame}\n").as_bytes())
            .unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        loop {
            let reply = self
                .frames
                .recv_timeout(Duration::from_secs(5))
                .expect("Owned native reply unavailable");
            if reply["requestId"] == sequence {
                return reply;
            }
        }
    }
    fn result(&mut self) -> Value {
        let file = self.file("result.json");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(value) = serde_json::from_slice(&fs::read(&file).unwrap()) {
                return value;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn exit(&mut self, code: i32) -> String {
        fs::write(self.root.join("exit-release"), b"release").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(self.child.wait().unwrap().code(), Some(code));
        let mut error = String::new();
        self.child
            .stderr
            .as_mut()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        self.reader.take().unwrap().join().unwrap();
        error.replace('\r', "")
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn locked(root: &Path) -> bool {
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(root.join("phone-native.lock"))
    {
        Ok(_) => false,
        Err(e) if e.raw_os_error() == Some(32) => true,
        Err(e) => panic!("Lease observation unavailable: {e}"),
    }
}
#[test]
fn composed_entry_keeps_one_lease_through_confirmed_and_failed_shutdown() {
    let mut rows = Vec::new();
    for failed in [false, true] {
        let root = Root::new();
        let owner = root.dir("owner");
        let mut worker = Worker::start(root.dir("worker"), &owner, failed);
        worker.file("ready");
        assert!(locked(&owner));
        let record = fs::read(owner.join("phone-native-owner.json")).unwrap();
        let parsed = NativeOwner::parse(&record).unwrap();
        assert_eq!(parsed.pid(), worker.child.id());
        assert_eq!(parsed.epoch(), CALL);
        let mut replies = Vec::new();
        for (i, (method, params)) in [
            ("status", "{}".to_owned()),
            ("configure", BASE.to_owned()),
            ("call.prepare", json!({"callId":CALL}).to_string()),
            ("codec.test", "{}".into()),
            ("inventory", r#"{"extra":null}"#.into()),
            ("call.release", json!({"callId":CALL}).to_string()),
        ]
        .into_iter()
        .enumerate()
        {
            let r = worker.send(method, &params, i as u64 + 1);
            if method == "codec.test" {
                assert_eq!(r["error"], "runtime_not_ready");
            } else if method == "inventory" {
                assert_eq!(r["error"], "invalid_request");
            } else {
                assert_eq!(r["ok"], true, "{r}");
            }
            replies.push(json!({"method":method,"reply":r}));
        }
        worker.input.take();
        let result = worker.result();
        assert_eq!(
            result,
            json!({"exitCode":i32::from(failed),"runtimeFactories":1,"mediaFactories":1,"codecLoads":0,"remainingCloses":1,"remainingMethods":0})
        );
        assert!(locked(&owner));
        assert_eq!(
            fs::read(owner.join("phone-native-owner.json")).unwrap(),
            record
        );
        let error = worker.exit(i32::from(failed));
        assert_eq!(error, if failed { "phone_runtime_failed\n" } else { "" });
        assert!(!locked(&owner));
        rows.push(json!({"remainingFailure":failed,"result":result,"leaseRetainedAfterRuntimeResult":true,"leaseReleasedOnlyAtProcessExit":true,"replies":replies}));
    }
    save("composition-entry.json", &json!(rows));
}
