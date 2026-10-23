use super::*;
use crate::native_config::NativeMicroValue;
use ivy_host_runtime::process::{self, Command as HostCommand, LaunchContext, OwnedProcess};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Read,
    os::windows::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

#[path = "phone_micro_client_process.rs"]
mod root_process;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn trace(name: &str, value: &Value) {
    if let Ok(root) = std::env::var("IVY_PHONE_MICRO_CLIENT_TRACE") {
        std::fs::write(
            Path::new(&root).join(name),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ivy-phone-client-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        assert!(self.0.is_absolute());
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-phone-client-")
        );
        // A failing test retains its private scratch if an owned child still has
        // a handle. The process owner stops it when the last handle is dropped.
        if !std::thread::panicking() {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}
fn settings(executable: &Path, hash: &str) -> NativeMicroSettings {
    let raw =
        json!({"usbipExecutable":executable.to_str().unwrap(),"executableHash":hash,"port":3249})
            .to_string();
    let value = ivy_contracts::json::parse(&raw, 65536).unwrap();
    NativeMicroValue::read(&value, &raw)
        .unwrap()
        .validate()
        .unwrap()
}
fn result(value: &MicroClientResult) -> Value {
    json!({"started":value.started,"completed":value.completed,"exitCode":value.exit_code,"output":value.output,"errorCode":value.error_code})
}
fn admission_fixture(cases: &[Value]) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-micro-client-cases.json");
    let expected: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(cases.len(), expected.as_array().unwrap().len());
    for (input, row) in cases.iter().zip(expected.as_array().unwrap()) {
        assert_eq!(input["name"], row["name"]);
    }
    expected
}
#[test]
fn admission_hash_file_and_cancellation_precedence_prevent_launch() {
    let mut cases = Vec::new();
    for file in [
        "missing-directory",
        "missing",
        "directory",
        "wrong",
        "locked",
    ] {
        for hash in ["wrong", "release", "release-lf"] {
            for cancelled in [false, true] {
                cases.push(json!({"name":format!("{file}-{hash}-{cancelled}"),"file":file,"hash":hash,"cancelled":cancelled}));
            }
        }
    }
    let actual = runtime().block_on(async {
        let calls = Arc::new(AtomicU64::new(0));
        let mut out = Vec::new();
        for row in &cases {
            let root = Temp::new();
            let state = row["file"].as_str().unwrap();
            let directory = if state == "missing-directory" {
                root.0.join("missing")
            } else {
                root.0.clone()
            };
            let executable = directory.join("usbip.exe");
            if state == "directory" {
                std::fs::create_dir(&executable).unwrap();
            }
            if matches!(state, "wrong" | "locked") {
                std::fs::write(&executable, b"inert fixture bytes, never executable").unwrap();
            }
            let _blocked = (state == "locked").then(|| {
                OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&executable)
                    .unwrap()
            });
            let pins = MicroClientPins::release().unwrap();
            let hash = match row["hash"].as_str().unwrap() {
                "wrong" => format!("sha256:{}", "0".repeat(64)),
                "release-lf" => format!("sha256:{}\n", pins.get("usbip.exe").unwrap()),
                _ => format!("sha256:{}", pins.get("usbip.exe").unwrap()),
            };
            let count = calls.clone();
            let client =
                MicroUsbClient::with_executor(settings(&executable, &hash), move |_, _| {
                    count.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async {
                        panic!("Immutable release pins must prevent this fixture launching.")
                    })
                });
            let stop = CancellationToken::new();
            if row["cancelled"].as_bool().unwrap() {
                stop.cancel();
            }
            let value = client
                .run(vec!["port".into()], stop)
                .await
                .unwrap_or_else(|_| panic!("Unexpected admission exception"));
            let mut value = result(&value);
            value["name"] = row["name"].clone();
            out.push(value);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        Value::Array(out)
    });
    trace("admission-rust.json", &actual);
    let expected = admission_fixture(&cases);
    let differences: Vec<_> = actual
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.as_array().unwrap())
        .filter(|(a, b)| a != b)
        .map(|(a, b)| json!({"rust":a,"original":b}))
        .collect();
    trace(
        "admission-comparisons.json",
        &json!({"cases":cases.len(),"differences":differences,"rust":actual,"original":expected}),
    );
    assert_eq!(actual, expected);
}

fn hash_file(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        use std::fmt::Write;
        write!(output, "{byte:02x}").unwrap();
    }
    output
}
struct Fixture {
    root: Temp,
    pins: Vec<(String, String)>,
    next: AtomicU64,
}
impl Fixture {
    fn new() -> Self {
        let root = Temp::new();
        // Private copy: no hardlink or mutation/lease of a shared tool executable.
        std::fs::copy(
            std::env::var("IVY_PHONE_MICRO_CLIENT_NODE").unwrap(),
            root.0.join("usbip.exe"),
        )
        .unwrap();
        for name in ["libusbip.dll", "resources.dll"] {
            std::fs::write(root.0.join(name), b"inert sibling lease fixture").unwrap();
        }
        let pins = ["usbip.exe", "libusbip.dll", "resources.dll"]
            .map(|name| (name.into(), hash_file(&root.0.join(name))))
            .to_vec();
        Self {
            root,
            pins,
            next: AtomicU64::new(0),
        }
    }
    fn probe(&self) -> Value {
        Value::Array(["libusbip.dll", "resources.dll"].into_iter().map(|name| {
            let path = self.root.0.join(name);
            json!({"name":name,"read":OpenOptions::new().read(true).share_mode(1).open(&path).is_ok(),
                "write":OpenOptions::new().write(true).share_mode(7).open(&path).is_ok(),
                "delete":OpenOptions::new().access_mode(0x10000).share_mode(7).open(&path).is_ok()})
        }).collect())
    }
    fn assert_leased(&self, held: bool) -> Value {
        let value = self.probe();
        for file in value.as_array().unwrap() {
            assert_eq!(file["read"], true);
            assert_eq!(file["write"], !held);
            assert_eq!(file["delete"], !held);
        }
        value
    }
    fn start(&self, mode: &str, args: Vec<String>, slow_drain: bool) -> Running {
        let number = self.next.fetch_add(1, Ordering::SeqCst);
        let ready = self.root.0.join(format!("ready-{number}.json"));
        let finish = self.root.0.join(format!("finish-{number}"));
        let process = Arc::new(Mutex::new(None));
        let completed = Arc::new(Mutex::new(None));
        let calls = Arc::new(AtomicU64::new(0));
        let root = self.root.0.clone();
        let observe = process.clone();
        let record = completed.clone();
        let count = calls.clone();
        let marker = format!("owned-marker-{number}");
        let expected_marker = marker.clone();
        let client = MicroUsbClient::with_executor(
            settings(
                &root.join("usbip.exe"),
                &format!("sha256:{}", self.pins[0].1),
            ),
            move |arguments, stop| {
                count.fetch_add(1, Ordering::SeqCst);
                let root = root.clone();
                let observe = observe.clone();
                let record = record.clone();
                let marker = marker.clone();
                Box::pin(async move {
                    let mut environment: BTreeMap<_, _> = std::env::vars_os().collect();
                    environment.insert("IVY_PHONE_CLIENT_MARKER".into(), marker.into());
                    let command = HostCommand {
                        executable: "usbip.exe".into(),
                        args: arguments.into_iter().map(Into::into).collect(),
                        cwd: None,
                        timeout_ms: 60000,
                    };
                    // Test-only leaf adapter. Generic Job ownership is intentionally
                    // not the production Micro compatibility API (parent decision238).
                    let mut child = match process::spawn_exact_environment(
                        &command,
                        &root,
                        &BTreeMap::new(),
                        LaunchContext {
                            environment,
                            raw_output: true,
                            job_launcher: Some(
                                std::env::var("IVY_PHONE_MICRO_CLIENT_JOB").unwrap().into(),
                            ),
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        Ok(child) => child,
                        Err(_) => return Ok(failure(false, "micro_client_unavailable")),
                    };
                    drop(child.stdin.take());
                    let stdout = child.stdout.take().unwrap();
                    let stderr = child.stderr.take().unwrap();
                    let child = Arc::new(child);
                    *observe.lock().unwrap() = Some(child.clone());
                    let completion = child.completion.clone();
                    let drain = child.clone();
                    let value = run_started_micro_command(
                        stdout,
                        stderr,
                        async move {
                            let value = completion.wait().await;
                            if value.error_code.is_some() {
                                return Err(io::Error::other("Unproved owned leaf exit"));
                            }
                            value
                                .exit_code
                                .ok_or_else(|| io::Error::other("Missing owned leaf exit"))
                        },
                        move || async move {
                            if slow_drain {
                                std::future::pending::<()>().await;
                            }
                            let _ = drain.stop(Duration::ZERO).await;
                        },
                        &stop,
                        [ivy_host_runtime::process_output::console_output_code_page(); 2],
                    )
                    .await;
                    *record.lock().unwrap() = Some(value.clone());
                    Ok(value)
                })
            },
        );
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/fixtures/phone-micro-client-child.mjs");
        let mut arguments = vec![
            script.to_str().unwrap().into(),
            mode.into(),
            ready.to_str().unwrap().into(),
            finish.to_str().unwrap().into(),
        ];
        arguments.extend(args);
        let stop = CancellationToken::new();
        let work = client.run_pinned(
            arguments,
            stop.clone(),
            Ok(MicroClientPins::fixture(self.pins.clone())),
        );
        Running {
            work: Some(work),
            ready,
            finish,
            process,
            completed,
            calls,
            stop,
            marker: expected_marker,
        }
    }
}
struct Running {
    work: Option<MicroCommand>,
    ready: PathBuf,
    finish: PathBuf,
    process: Arc<Mutex<Option<Arc<OwnedProcess>>>>,
    completed: Arc<Mutex<Option<MicroClientResult>>>,
    calls: Arc<AtomicU64>,
    stop: CancellationToken,
    marker: String,
}
impl Running {
    async fn ready(&self) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Ok(bytes) = std::fs::read(&self.ready) {
                    if let Ok(value) = serde_json::from_slice(&bytes) {
                        return value;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("Owned leaf readiness")
    }
    fn child(&self) -> Arc<OwnedProcess> {
        self.process.lock().unwrap().as_ref().unwrap().clone()
    }
    async fn finish(&mut self) -> MicroClientResult {
        let value = tokio::time::timeout(Duration::from_secs(15), self.work.take().unwrap())
            .await
            .expect("Bounded retained command")
            .unwrap_or_else(|_| panic!("Unexpected command exception"));
        assert_eq!(self.calls.load(Ordering::SeqCst), 1);
        value
    }
    async fn retired(&self) -> bool {
        process::owned_process_tree_stopped(&self.child())
            .await
            .unwrap()
    }
}
#[test]
fn inert_leaf_output_argv_environment_cancellation_and_deadline_follow_command_policy() {
    runtime().block_on(async {
        let fixture = Fixture::new(); let mut rows = Vec::new();
        let args = vec!["".into(),"space value".into(),"quote\"value".into(),"é𐐷".into(),"tail\\".into()];
        for (mode, code) in [("ok",0),("exit7",7),("stderr-bound",0)] {
            let mut run = fixture.start(mode,args.clone(),false); let ready = run.ready().await;
            let value = run.finish().await;
            assert!(value.started && value.completed); assert_eq!(value.exit_code,Some(code)); assert!(value.error_code.is_none());
            if mode=="stderr-bound" {assert!(value.output.is_empty());} else {
                let output: Value = serde_json::from_str(&value.output).unwrap();
                assert_eq!(output["args"],json!(args)); assert_eq!(output["marker"],run.marker);
                assert_eq!(Path::new(output["cwd"].as_str().unwrap()),fixture.root.0.as_path());
            }
            assert!(run.retired().await); let leases=fixture.assert_leased(false);
            rows.push(json!({"mode":mode,"ready":ready,"result":result(&value),"retired":true,"after":leases}));
        }
        for mode in ["stdout-over","stderr-over","over-hang","hang","deadline"] {
            let mut run = fixture.start(if mode=="deadline" {"hang"} else {mode},vec![],false); let ready = run.ready().await;
            let clock = Instant::now();
            let held = if matches!(mode,"over-hang"|"hang"|"deadline") {fixture.assert_leased(true)} else {Value::Null};
            if mode=="over-hang" {
                // A pipe fault does not short-circuit Task.WhenAll.
                assert!(tokio::time::timeout(Duration::from_millis(100),run.work.as_mut().unwrap()).await.is_err());
                assert!(run.child().completion.current().is_none()); run.stop.cancel();
            } else if mode=="hang" { run.stop.cancel(); }
            let value = run.finish().await; let elapsed = clock.elapsed().as_millis();
            assert_eq!(value,failure(true,"micro_attach_unknown")); assert!(run.retired().await);
            if mode=="deadline" {assert!((9000..=13000).contains(&elapsed),"Ten-second work deadline: {elapsed}ms");}
            if mode=="hang" {assert!(elapsed<2500);}
            let after=fixture.assert_leased(false);
            rows.push(json!({"mode":mode,"ready":ready,"result":result(&value),"held":held,"after":after,"elapsedMs":elapsed,"retired":true}));
        }
        trace("leaf-policy.json",&json!({"scope":"Injected generic Host owner with owned leaf children only; not the production Micro process API or original positive-run differential.","rows":rows}));
    });
}
#[test]
fn dropped_observer_keeps_one_command_and_unknown_releases_using_var_leases_after_two_seconds() {
    runtime().block_on(async {
        let fixture=Fixture::new(); let mut run=fixture.start("release",vec![],false); let ready=run.ready().await;
        let held=fixture.assert_leased(true); drop(run.work.take());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(run.completed.lock().unwrap().is_none()); fixture.assert_leased(true);
        std::fs::write(&run.finish,b"finish this owned leaf").unwrap();
        tokio::time::timeout(Duration::from_secs(3),async {
            while run.completed.lock().unwrap().is_none() {tokio::time::sleep(Duration::from_millis(5)).await;}
        }).await.unwrap();
        // The record is written just before the retained task drops its lease.
        tokio::time::timeout(Duration::from_secs(1),async {
            while fixture.probe().as_array().unwrap().iter().any(|f|f["write"]!=true) {tokio::time::sleep(Duration::from_millis(5)).await;}
        }).await.unwrap();
        assert_eq!(run.calls.load(Ordering::SeqCst),1); assert!(run.retired().await);
        let released=fixture.assert_leased(false); let completed=run.completed.lock().unwrap().clone().unwrap(); assert!(completed.completed);
        let mut unknown=fixture.start("hang",vec![],true); let unknown_ready=unknown.ready().await; let before=fixture.assert_leased(true);
        unknown.stop.cancel(); let clock=Instant::now(); let value=unknown.finish().await; let elapsed=clock.elapsed().as_millis();
        assert_eq!(value,failure(true,"micro_attach_unknown")); assert!((1900..=3500).contains(&elapsed));
        // Deliberately unconfirmed kill/root drain: preserve original bounded
        // unknown lease release, then explicitly retire the inert test leaf.
        assert!(unknown.child().completion.current().is_none()); let after=fixture.assert_leased(false);
        unknown.child().stop(Duration::ZERO).await.unwrap(); assert!(unknown.retired().await);
        trace("retention-policy.json",&json!({"droppedObserver":{"ready":ready,"held":held,"result":result(&completed),"after":released,"calls":1,"retired":true},
            "unconfirmedDrain":{"ready":unknown_ready,"held":before,"result":result(&value),"elapsedMs":elapsed,"afterUnknownBeforeFixtureCleanup":after,"rootStillPendingAtUnknown":true,"fixtureCleanupRetired":true},
            "scope":"Injected owned leaf and artificial unconfirmed drain only; no production process API, descendant parity or actual USB/IP invocation."}));
    });
}
