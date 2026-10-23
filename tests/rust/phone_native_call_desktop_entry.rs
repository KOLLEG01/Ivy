//! Actual inherited-stdio/entry/lease composition with inert call providers.
use super::*;
use ivy_phone_bridge::{
    native_desktop::NativeDesktopCommands, native_entry::run_owned_native_entry,
    native_owner::NativeOwner, native_rpc::NativeInvocationStart,
};
use std::os::windows::process::CommandExt;
use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Read},
    os::windows::fs::OpenOptionsExt,
    process::{Child, ChildStdin},
    sync::{Weak, mpsc},
    time::Instant,
};
const CHILD: &str = "call_audio::session::native_operations::commands::desktop::exception_policy::entry::owned_entry_fixture";

struct Observed {
    owner: NativeSipOperations,
    context: Arc<Context>,
    route: Arc<Route>,
    control: Arc<FaultControl>,
    remaining: Arc<Remaining>,
    media: Arc<CountedFactory>,
}
impl Observed {
    fn json(&self) -> IvyJson {
        IvyJson::object([
            ("calls", n(self.owner.observation().calls.len())),
            ("chords", n(self.context.chords.load(Ordering::SeqCst))),
            (
                "applications",
                n(self.context.applications.load(Ordering::SeqCst)),
            ),
            ("suspends", n(self.control.suspends.load(Ordering::SeqCst))),
            (
                "routeDisposals",
                n(self.route.state.lock().unwrap().disposals),
            ),
            (
                "remainingCloses",
                n(self.remaining.closes.load(Ordering::SeqCst)),
            ),
            (
                "mediaFactories",
                n(self.media.starts.load(Ordering::SeqCst)),
            ),
        ])
    }
}
struct Instrumented {
    commands: Arc<NativeSipCommands>,
    observed: Arc<Observed>,
    mode: String,
    state_file: PathBuf,
    shutdowns: Arc<AtomicUsize>,
}
impl NativeHandler for Instrumented {
    fn cleanup_slots(&self) -> usize {
        self.commands.cleanup_slots()
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        let start = self.start(request);
        Box::pin(async move { start.await.await })
    }
    fn start(&self, request: NativeRequest) -> NativeInvocationStart {
        if request.method() == "call.desktop.pauseVoice" && self.mode.starts_with("clear-") {
            self.observed.control.at.store(
                self.observed.control.suspends.load(Ordering::SeqCst) + 2,
                Ordering::SeqCst,
            );
        }
        let started = self.commands.start(request);
        let observed = self.observed.clone();
        let file = self.state_file.clone();
        Box::pin(async move {
            let work = started.await;
            Box::pin(async move {
                let result = work.await;
                fs::write(file, stringify(&observed.json(), 4096).unwrap()).unwrap();
                result
            }) as NativeInvocation
        })
    }
}
impl NativeRuntime for Instrumented {
    fn shutdown(&self) -> NativeShutdown {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        self.commands.shutdown()
    }
}

#[test]
fn owned_entry_fixture() {
    let Some(root) = std::env::var_os("IVY_PHONE_CALL_ENTRY_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mode = std::env::var("IVY_PHONE_CALL_ENTRY_MODE").unwrap();
    let owner_root = PathBuf::from(std::env::var_os("IVY_PHONE_CALL_ENTRY_OWNER").unwrap());
    let args: Vec<OsString> = vec![
        "--epoch".into(),
        CALL.into(),
        "--owner-root".into(),
        owner_root.into(),
    ];
    let factories = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let weak = Arc::new(Mutex::new(None::<Weak<Instrumented>>));
    let counters = Arc::new(Mutex::new(
        None::<(Arc<Context>, Arc<Route>, Arc<FaultControl>, Arc<Remaining>)>,
    ));
    let code = run_owned_native_entry(&args, || {
        factories.fetch_add(1, Ordering::SeqCst);
        assert!(tokio::runtime::Handle::try_current().is_ok());
        if mode == "lease-probe" {
            return Err(());
        }
        let pending = mode.starts_with("pending-");
        let gate = pending.then(|| Arc::new(CaptureGate::default()));
        if let Some(gate) = &gate {
            let gate = gate.clone();
            let root = root.clone();
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(20);
                while !gate.entered.load(Ordering::SeqCst) {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
                fs::write(root.join("launch-entered"), b"entered").unwrap();
                while !root.join("launch-release").exists() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
                gate.release();
            });
        }
        let context = Arc::new(Context {
            hold: gate,
            ..Default::default()
        });
        if mode == "revoke-panic" {
            context.capture.store(true, Ordering::SeqCst);
        }
        let (desktop, voice) = runtime(context.clone());
        let route = Arc::new(Route::new());
        let control = Arc::new(FaultControl {
            at: AtomicUsize::new(usize::MAX),
            suspends: AtomicUsize::new(0),
            panic: mode.ends_with("panic"),
            gate: None,
        });
        let factory = Arc::new(CountedFactory {
            inner: NativeSipSessionFactory::new(
                Arc::new(FaultFactory {
                    inner: Arc::new(Factory::routes(vec![route.clone()])),
                    control: control.clone(),
                }),
                None,
                Arc::new(Clock),
                |_| Arc::new(Events::default()),
            ),
            starts: AtomicUsize::new(0),
        });
        let owner =
            NativeSipOperations::with_desktop_runtime(factory.clone(), desktop.clone(), voice);
        let endpoint = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let endpoint = owner
                    .configure(&configuration(BASE), |_| async { Ok(()) })
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                let call = owner.prepare(CALL.into()).unwrap();
                call.prepared().await.unwrap();
                let mut audio = settings();
                audio.source_mode = "desktop_process".into();
                call.media()
                    .unwrap()
                    .prepare_audio(audio, Some(identity("")), CancellationToken::new())
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                endpoint
            })
        });
        assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
        if mode == "revoke-panic" {
            control.at.store(
                control.suspends.load(Ordering::SeqCst) + 1,
                Ordering::SeqCst,
            );
        }
        let remaining = Arc::new(Remaining::default());
        remaining.close_gate.release();
        let observed = Arc::new(Observed {
            owner: owner.clone(),
            context: context.clone(),
            route: route.clone(),
            control: control.clone(),
            remaining: remaining.clone(),
            media: factory,
        });
        let platform = Arc::new(NativeDesktopCommands::with_runtime(
            owner.clone(),
            desktop,
            remaining.clone(),
        ));
        let commands = Arc::new(NativeSipCommands::new(
            owner,
            platform,
            |_| Box::pin(async { Ok(()) }),
            closed_platform_views(),
        ));
        let commands = Arc::new(Instrumented {
            commands,
            observed,
            mode: mode.clone(),
            state_file: root.join("native-state.json"),
            shutdowns: shutdowns.clone(),
        });
        *weak.lock().unwrap() = Some(Arc::downgrade(&commands));
        *counters.lock().unwrap() = Some((context, route, control, remaining));
        fs::write(root.join("endpoint"), endpoint.to_string()).unwrap();
        fs::write(root.join("ready"), b"ready").unwrap();
        Ok(commands as Arc<dyn NativeRuntime>)
    });
    let mut fields = vec![
        ("exitCode".into(), n(code as usize)),
        ("factories".into(), n(factories.load(Ordering::SeqCst))),
        ("shutdowns".into(), n(shutdowns.load(Ordering::SeqCst))),
    ];
    if let Some((ctx, route, control, remaining)) = counters.lock().unwrap().as_ref() {
        assert!(remaining.methods.lock().unwrap().is_empty());
        fields.extend([
            ("chords".into(), n(ctx.chords.load(Ordering::SeqCst))),
            (
                "suspends".into(),
                n(control.suspends.load(Ordering::SeqCst)),
            ),
            (
                "routeDisposals".into(),
                n(route.state.lock().unwrap().disposals),
            ),
            (
                "remainingCloses".into(),
                n(remaining.closes.load(Ordering::SeqCst)),
            ),
        ]);
    }
    fields.push((
        "runtimeStillOwned".into(),
        weak.lock()
            .unwrap()
            .as_ref()
            .is_some_and(|w| w.upgrade().is_some())
            .into(),
    ));
    fs::write(
        root.join("result.json"),
        stringify(&IvyJson::Object(fields), 4096).unwrap(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !root.join("exit-release").exists() {
        assert!(
            Instant::now() < deadline,
            "Owned entry exit release unavailable"
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
            "ivy-phone-call-entry-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        Self { path, parent }
    }
    fn dir(&self, name: &str) -> PathBuf {
        let p = self.path.join(name);
        fs::create_dir(&p).unwrap();
        p
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let p = fs::canonicalize(&self.path).unwrap();
        assert_eq!(p.parent(), Some(self.parent.as_path()));
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-phone-call-entry-")
        );
        fs::remove_dir_all(p).unwrap();
    }
}
struct Worker {
    child: Child,
    input: Option<ChildStdin>,
    root: PathBuf,
    frames: mpsc::Receiver<IvyJson>,
    output: Option<std::thread::JoinHandle<String>>,
}
impl Worker {
    fn start(owner: &Path, root: PathBuf, mode: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--nocapture", "--format=terse"])
            .env("IVY_PHONE_CALL_ENTRY_ROOT", &root)
            .env("IVY_PHONE_CALL_ENTRY_OWNER", owner)
            .env("IVY_PHONE_CALL_ENTRY_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, frames) = mpsc::channel();
        let output = std::thread::spawn(move || {
            let mut all = String::new();
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                all.push_str(&line);
                all.push('\n');
                if line.starts_with('{') {
                    tx.send(parse(&line, FRAME_BYTES).unwrap()).unwrap();
                }
            }
            all
        });
        Self {
            child,
            input,
            root,
            frames,
            output: Some(output),
        }
    }
    fn wait_file(&mut self, file: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.root.join(file).exists() {
            if let Some(exit) = self.child.try_wait().unwrap() {
                let mut error = String::new();
                self.child
                    .stderr
                    .as_mut()
                    .unwrap()
                    .read_to_string(&mut error)
                    .unwrap();
                panic!("Owned entry exited before {file}: {exit}: {error}");
            }
            assert!(
                Instant::now() < deadline,
                "Owned entry file unavailable: {file}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn json(&mut self, file: &str) -> IvyJson {
        self.wait_file(file);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(b) = fs::read_to_string(self.root.join(file)) {
                if let Ok(v) = parse(&b, 4096) {
                    return v;
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn send(&mut self, method: &str, params: &str, operation: usize, request: usize) {
        let frame = format!(
            "{{\"version\":1,\"epoch\":\"{CALL}\",\"requestId\":{request},\"operationId\":\"00000000-0000-4000-8000-{operation:012x}\",\"method\":\"{method}\",\"params\":{params}}}\n"
        );
        self.input
            .as_mut()
            .unwrap()
            .write_all(frame.as_bytes())
            .unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
    }
    fn response(&mut self, id: usize) -> IvyJson {
        loop {
            let r = self
                .frames
                .recv_timeout(Duration::from_secs(4))
                .expect("Native response unavailable");
            if r.get("requestId").and_then(IvyJson::as_f64) == Some(id as f64) {
                return r;
            }
        }
    }
    fn eof(&mut self) {
        self.input.take();
    }
    fn exit(mut self, code: i32) -> String {
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
        let output = self.output.take().unwrap().join().unwrap();
        assert!(output.starts_with("\nrunning 1 test\n"));
        error.replace('\r', "")
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn locked(owner: &Path) -> bool {
    let result = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(owner.join("phone-native.lock"));
    match result {
        Err(e) if e.raw_os_error() == Some(32) => true,
        Ok(_) => false,
        Err(e) => panic!("Lease observation unavailable: {e}"),
    }
}
fn ok(r: &IvyJson) -> bool {
    r.get("ok").unwrap().as_bool().unwrap()
}
fn error(r: &IvyJson) -> String {
    r.get("error").unwrap().as_str().unwrap()
}

#[test]
fn actual_call_owner_keeps_entry_lease_until_process_exit() {
    let mut rows = vec![];
    let cases = [
        "healthy",
        "clear-error",
        "clear-panic",
        "revoke-panic",
        "pending-drain",
        "pending-unconfirmed",
    ];
    let selected = std::env::var("IVY_PHONE_CALL_ENTRY_CASE").ok();
    assert!(
        selected
            .as_ref()
            .is_none_or(|s| cases.contains(&s.as_str()))
    );
    for mode in cases
        .into_iter()
        .filter(|m| selected.as_ref().is_none_or(|s| s == m))
    {
        let root = Root::new();
        let owner = root.dir("owner");
        let mut worker = Worker::start(&owner, root.dir("original"), mode);
        worker.wait_file("ready");
        assert!(locked(&owner));
        let original = fs::read(owner.join("phone-native-owner.json")).unwrap();
        let record = NativeOwner::parse(&original).unwrap();
        assert_eq!(record.pid(), worker.child.id());
        assert_eq!(record.epoch(), CALL);
        assert!(record.ensure_predecessor_exited().is_err());
        let pending = mode.starts_with("pending-");
        let failed = mode != "healthy" && mode != "pending-drain";
        let mut before = None;
        if pending {
            worker.send("call.desktop.launch", &params("call.desktop.launch"), 1, 1);
            worker.wait_file("launch-entered");
            worker.eof();
            std::thread::sleep(Duration::from_millis(50));
            assert!(!worker.root.join("result.json").exists());
        } else {
            worker.send(
                "call.desktop.startVoice",
                &params("call.desktop.startVoice"),
                1,
                1,
            );
            let started = worker.response(1);
            assert_eq!(ok(&started), mode != "revoke-panic");
            if mode != "revoke-panic" {
                let method = if failed {
                    "call.desktop.pauseVoice"
                } else {
                    "call.desktop.stopVoice"
                };
                worker.send(method, &params(method), 2, 2);
                let result = worker.response(2);
                assert_eq!(ok(&result), !failed);
            }
            let id = format!("{{\"callId\":\"{CALL}\"}}");
            worker.send("call.release", &id, 3, 3);
            let released = worker.response(3);
            assert_eq!(ok(&released), !failed);
            if failed {
                for (request, id) in [(4, CALL), (5, "22222222-2222-4222-8222-222222222222")] {
                    worker.send(
                        "call.prepare",
                        &format!("{{\"callId\":\"{id}\"}}"),
                        request,
                        request,
                    );
                    assert_eq!(error(&worker.response(request)), "operation_conflict");
                }
                worker.send(
                    "call.desktop.startVoice",
                    &params("call.desktop.startVoice"),
                    1,
                    6,
                );
                let replay = worker.response(6);
                assert_eq!(replay.get("ok"), started.get("ok"));
                assert_eq!(replay.get("result"), started.get("result"));
                assert_eq!(replay.get("error"), started.get("error"));
            }
            before = Some(worker.json("native-state.json"));
            if failed {
                assert_eq!(
                    before.as_ref().unwrap().get("calls").unwrap().as_f64(),
                    Some(1.0)
                );
                assert_eq!(
                    before
                        .as_ref()
                        .unwrap()
                        .get("mediaFactories")
                        .unwrap()
                        .as_f64(),
                    Some(1.0)
                );
            }
            worker.eof();
        }
        let mut contender = Worker::start(&owner, root.dir("contender"), "lease-probe");
        contender.eof();
        let blocked = contender.json("result.json");
        assert_eq!(blocked.get("factories").unwrap().as_f64(), Some(0.0));
        assert_eq!(contender.exit(1), "phone_runtime_failed\n");
        assert_eq!(
            fs::read(owner.join("phone-native-owner.json")).unwrap(),
            original
        );
        if mode == "pending-drain" {
            fs::write(worker.root.join("launch-release"), b"release").unwrap();
        }
        let result = worker.json("result.json");
        assert_eq!(
            result.get("exitCode").unwrap().as_f64(),
            Some(if failed { 1.0 } else { 0.0 })
        );
        assert_eq!(result.get("factories").unwrap().as_f64(), Some(1.0));
        assert_eq!(result.get("shutdowns").unwrap().as_f64(), Some(1.0));
        // The original finally reaches platform disposal only after SIP close
        // returns. A still-pending drain must not report that cleanup occurred.
        assert_eq!(
            result.get("remainingCloses").unwrap().as_f64(),
            Some(if mode == "pending-unconfirmed" {
                0.0
            } else {
                1.0
            })
        );
        assert!(locked(&owner));
        assert!(record.ensure_predecessor_exited().is_err());
        if mode.starts_with("clear-") || mode == "revoke-panic" {
            let before = before.as_ref().unwrap();
            assert_eq!(result.get("chords"), before.get("chords"));
            assert_eq!(result.get("suspends"), before.get("suspends"));
            assert_eq!(result.get("routeDisposals").unwrap().as_f64(), Some(0.0));
        }
        if pending {
            fs::write(worker.root.join("launch-release"), b"release").unwrap();
        }
        let stderr = worker.exit(if failed { 1 } else { 0 });
        if failed {
            assert_eq!(
                stderr
                    .lines()
                    .filter(|s| *s == "phone_runtime_failed")
                    .count(),
                1
            );
            assert!(
                stderr
                    .lines()
                    .all(|s| s == "phone_native_panic" || s == "phone_runtime_failed")
            );
        } else {
            assert!(stderr.is_empty());
        }
        assert!(record.ensure_predecessor_exited().is_ok());
        let mut successor = Worker::start(&owner, root.dir("successor"), "lease-probe");
        successor.eof();
        let admitted = successor.json("result.json");
        assert_eq!(admitted.get("factories").unwrap().as_f64(), Some(1.0));
        assert!(locked(&owner));
        assert_eq!(successor.exit(1), "phone_runtime_failed\n");
        rows.push(IvyJson::object([
            ("mode", mode.into()),
            ("result", result),
            ("leaseRetainedAfterEntry", true.into()),
            ("liveContenderFactories", n(0)),
            ("successorFactoriesAfterExit", n(1)),
            ("redacted", true.into()),
        ]));
        save(
            "native-call-desktop-entry.json",
            &IvyJson::Array(rows.clone()),
        );
    }
    assert_eq!(rows.len(), if selected.is_some() { 1 } else { 6 });
}
