use super::*;
use crate::{
    journal::{PhoneJournalLimits, PhoneJournalOptions, PhoneJournalOwner},
    service_test_support::{equal, fixture, observed, timed, until},
};
use ivy_contracts::json::{canonical, parse, stringify};
use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::Semaphore,
};

fn object<const N: usize>(items: [(&str, IvyJson); N]) -> IvyJson {
    IvyJson::object(items)
}
fn id(n: u32) -> JsString {
    format!("50000000-0000-4000-8000-{n:012}").into()
}
fn protocol_cases() -> &'static IvyJson {
    static VALUE: OnceLock<IvyJson> = OnceLock::new();
    VALUE.get_or_init(|| fixture("process"))
}
fn fence() -> IvyJson {
    object([
        ("schemaVersion", 1.0.into()),
        ("owner", "native".into()),
        ("ownerId", id(1).into()),
        ("processId", 123.0.into()),
        ("observedAt", "2026-08-25T12:00:00.000Z".into()),
        ("code", "outcome_unknown".into()),
    ])
}
fn owner() -> IvyJson {
    object([
        ("schemaVersion", 1.0.into()),
        ("epoch", id(1).into()),
        ("pid", 456.0.into()),
        ("creationFileTime", "123456789".into()),
    ])
}
fn yes(row: &IvyJson, key: &str) -> bool {
    row.get(key) == Some(&true.into())
}
fn text(row: &IvyJson, key: &str) -> String {
    row.get(key)
        .unwrap()
        .as_js_string()
        .unwrap()
        .to_utf8()
        .unwrap()
}
fn root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ivy-phone-process-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    root
}
fn remove(root: PathBuf) {
    assert!(root.starts_with(std::env::temp_dir()));
    assert!(
        root.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ivy-phone-process-")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn original_exact_fence_record_and_lease_reconciliation_agree() {
    for row in protocol_cases().get("fences").unwrap().as_array().unwrap() {
        let root = root();
        let mut retained = fence();
        if yes(row, "foreign") {
            retained.insert("owner", "runtime-owner".into());
        }
        if !row.get("fence").is_some_and(IvyJson::is_null) {
            record_process_stop_fence(root.clone(), retained.clone())
                .await
                .unwrap();
            if yes(row, "noncanonical") {
                std::fs::write(
                    root.join("process-stop-fence.json"),
                    canonical(&retained, 4096).unwrap().replacen(':', ": ", 1),
                )
                .unwrap();
            }
        }
        std::fs::write(
            root.join("phone-native-owner.json"),
            stringify(row.get("owner").unwrap_or(&owner()), 4096).unwrap(),
        )
        .unwrap();
        if !yes(row, "missingLease") {
            std::fs::write(root.join("phone-native.lock"), b"").unwrap();
        }
        let probes = Arc::new(AtomicUsize::new(0));
        let result = if yes(row, "actualProbe") {
            reconcile_phone_process_stop_fence(root.clone()).await
        } else {
            let count = probes.clone();
            let row = row.clone();
            let directory = root.clone();
            reconcile_with(root.clone(), move |path| async move {
                assert_eq!(path, directory.join("phone-native.lock"));
                count.fetch_add(1, Ordering::SeqCst);
                if yes(&row, "replaceFence") {
                    let mut changed = fence();
                    changed.insert("ownerId", id(2).into());
                    std::fs::write(
                        directory.join("process-stop-fence.json"),
                        canonical(&changed, 4096).unwrap() + "\n",
                    )
                    .unwrap();
                }
                if yes(&row, "probeError") {
                    return Err(IvyError::internal());
                }
                Ok(row.get("probe") != Some(&false.into()))
            })
            .await
        };
        let actual = object([
            ("result", observed(result.map(IvyJson::from))),
            ("probes", (probes.load(Ordering::SeqCst) as f64).into()),
            (
                "retained",
                process_stop_fence(root.clone())
                    .await
                    .unwrap()
                    .unwrap_or(IvyJson::Null),
            ),
        ]);
        equal(&actual, row.get("expected").unwrap(), &text(row, "name"));
        remove(root);
    }
}

#[cfg(windows)]
#[tokio::test]
async fn native_lifetime_lock_blocks_fence_clear_until_its_exact_owner_releases_it() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = root();
    record_process_stop_fence(root.clone(), fence())
        .await
        .unwrap();
    std::fs::write(
        root.join("phone-native-owner.json"),
        stringify(&owner(), 4096).unwrap(),
    )
    .unwrap();
    let lease = std::fs::OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .share_mode(0)
        .open(root.join("phone-native.lock"))
        .unwrap();
    assert!(
        !reconcile_phone_process_stop_fence(root.clone())
            .await
            .unwrap()
    );
    equal(
        &process_stop_fence(root.clone()).await.unwrap().unwrap(),
        &fence(),
        "retained exact fence",
    );
    drop(lease);
    assert!(
        reconcile_phone_process_stop_fence(root.clone())
            .await
            .unwrap()
    );
    assert_eq!(process_stop_fence(root.clone()).await.unwrap(), None);
    remove(root);
}

#[derive(Default)]
struct Capture {
    launches: usize,
    stops: usize,
    command: Option<IvyJson>,
    epoch: Option<JsString>,
    status_requests: usize,
    stderr_drained: bool,
}
struct FakeChild {
    capture: Arc<Mutex<Capture>>,
    completed: watch::Sender<Option<Arc<ProcessResult>>>,
    result: Arc<ProcessResult>,
}
impl ChildOwner for FakeChild {
    fn pid(&self) -> Option<u32> {
        Some(123)
    }
    fn completion(&self) -> Work<Arc<ProcessResult>> {
        let mut receive = self.completed.subscribe();
        Box::pin(async move {
            loop {
                if let Some(result) = receive.borrow_and_update().clone() {
                    return result;
                }
                receive.changed().await.unwrap();
            }
        })
    }
    fn stop(&self, grace: Duration) -> Work<IvyResult<Arc<ProcessResult>>> {
        assert_eq!(grace, Duration::from_millis(250));
        self.capture.lock().unwrap().stops += 1;
        self.completed.send_replace(Some(self.result.clone()));
        let result = self.result.clone();
        Box::pin(async move { Ok(result) })
    }
}
struct FakeLauncher {
    capture: Arc<Mutex<Capture>>,
    journal: PhoneJournal,
    artifact: PathBuf,
    name: String,
    status_gate: Option<Arc<Semaphore>>,
    require_stop: bool,
}
impl Launcher for FakeLauncher {
    fn launch(
        &self,
        command: Command,
        root: PathBuf,
        context: LaunchContext,
    ) -> Work<IvyResult<Launched>> {
        let capture = self.capture.clone();
        let journal = self.journal.clone();
        let artifact = self.artifact.clone();
        let name = self.name.clone();
        let status_gate = self.status_gate.clone();
        let require_stop = self.require_stop;
        Box::pin(async move {
            capture.lock().unwrap().launches += 1;
            if name == "launch-rejected" {
                return Err(IvyError::new("fixture_launch_refused", "fixture"));
            }
            let epoch = node_path::from_native(Path::new(&command.args[1]));
            capture.lock().unwrap().epoch = Some(epoch.clone());
            capture.lock().unwrap().command = Some(object([
                ("executable", command.executable.into()),
                (
                    "args",
                    IvyJson::Array(vec![
                        "--epoch".into(),
                        "$epoch".into(),
                        "--owner-root".into(),
                        "$data".into(),
                    ]),
                ),
                ("timeoutMs", (command.timeout_ms as f64).into()),
                ("rootMatches", (root == artifact).into()),
                (
                    "ownerRootMatches",
                    (Path::new(&command.args[3]) == journal.data_root()).into(),
                ),
                (
                    "jobMatches",
                    (context.job_launcher == Some(root.join("dist/native/ivy-job.exe"))).into(),
                ),
                ("rawOutput", context.raw_output.into()),
            ]));
            assert_eq!(command.args[0], "--epoch");
            assert_eq!(command.args[2], "--owner-root");
            assert_eq!(command.args.len(), 4);
            assert!(!context.without_job);
            assert!(context.environment.is_empty());
            let unknown = name.contains("unknown-stop");
            let result = Arc::new(ProcessResult {
                exit_code: Some(0),
                signal: None,
                error_code: unknown.then(|| "outcome_unknown".into()),
                stdout: String::new(),
                stderr: String::new(),
                truncated: unknown,
            });
            let (completed, _) = watch::channel(None);
            let child = Arc::new(FakeChild {
                capture: capture.clone(),
                completed,
                result,
            });
            let (input, peer_input) = tokio::io::duplex(65536);
            let (mut output, read) = tokio::io::duplex(65536);
            let (mut error_output, stderr) = tokio::io::duplex(1024);
            let drained = capture.clone();
            tokio::spawn(async move {
                // More than pipe capacity proves production's discard reader.
                if error_output.write_all(&vec![b'x'; 262144]).await.is_ok() {
                    drained.lock().unwrap().stderr_drained = true;
                }
            });
            let server = child.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(peer_input);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap() == 0 {
                        break;
                    }
                    let request = parse(&line, 65536).unwrap();
                    let status = request.get("method") == Some(&"status".into());
                    if status {
                        capture.lock().unwrap().status_requests += 1;
                        if let Some(gate) = &status_gate {
                            gate.acquire().await.unwrap().forget();
                        }
                        if name == "epoch-changed" {
                            journal.begin_epoch(id(4)).await.unwrap();
                        }
                    }
                    let rejected = [
                        "startup-rejected",
                        "startup-unknown-stop",
                        "malformed-startup",
                    ]
                    .contains(&name.as_str())
                        && !status;
                    let result = if status {
                        object([
                            ("configured", (name == "configured-hello").into()),
                            (
                                "endpoint",
                                if ["configured-hello", "invalid-hello"].contains(&name.as_str()) {
                                    "127.0.0.1:5060".into()
                                } else {
                                    IvyJson::Null
                                },
                            ),
                            ("call", IvyJson::Null),
                            ("media", IvyJson::Null),
                            ("registration", IvyJson::Null),
                        ])
                    } else {
                        object([])
                    };
                    let response = object([
                        ("version", 1.0.into()),
                        ("epoch", epoch.clone().into()),
                        ("requestId", request.get("requestId").unwrap().clone()),
                        ("ok", (!rejected).into()),
                        ("result", if rejected { IvyJson::Null } else { result }),
                        (
                            "error",
                            if rejected {
                                if name == "malformed-startup" {
                                    "fixture_rejected"
                                } else {
                                    "runtime_not_ready"
                                }
                                .into()
                            } else {
                                IvyJson::Null
                            },
                        ),
                    ]);
                    if output
                        .write_all((canonical(&response, 65536).unwrap() + "\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                if !require_stop {
                    server.completed.send_replace(Some(server.result.clone()));
                }
            });
            Ok(Launched {
                owner: child,
                input: Box::new(input),
                output: Box::new(read),
                stderr: Some(Box::new(stderr)),
            })
        })
    }
}
async fn journal(root: &Path) -> PhoneJournal {
    PhoneJournal::open(PhoneJournalOptions {
        data_root: root.join("data"),
        owner: PhoneJournalOwner {
            host_id: "phone-process-fixture".into(),
            service_node_id: "phone-process-fixture".into(),
        },
        limits: PhoneJournalLimits::default(),
        max_concurrent_calls: 1,
    })
    .await
    .unwrap()
}
async fn options(root: &Path, name: &str, journal: &PhoneJournal) -> PhoneProcessOptions {
    let artifact = root.join("artifact");
    std::fs::create_dir(&artifact).unwrap();
    let executable = artifact.join("fixture.exe");
    std::fs::write(&executable, b"inert verified Phone fixture\n").unwrap();
    std::fs::write(root.join("outside.exe"), b"outside fixture").unwrap();
    PhoneProcessOptions {
        artifact_root: if name == "relative-artifact" {
            ".".into()
        } else {
            artifact
        },
        executable: match name {
            "outside-artifact" => "../outside.exe".into(),
            "absolute-executable" => executable.clone(),
            _ => "fixture.exe".into(),
        },
        executable_hash: if name == "wrong-hash" {
            format!("sha256:{}", "0".repeat(64))
        } else {
            file_hash(&executable).await.unwrap()
        },
        journal: journal.clone(),
    }
}
fn fake(root: &Path, name: &str, journal: &PhoneJournal) -> Arc<FakeLauncher> {
    Arc::new(FakeLauncher {
        capture: Arc::new(Mutex::new(Capture::default())),
        journal: journal.clone(),
        artifact: root.join("artifact"),
        name: name.into(),
        status_gate: None,
        require_stop: false,
    })
}

#[tokio::test]
async fn original_verified_launch_hello_epoch_and_memoized_stop_agree() {
    for row in protocol_cases()
        .get("startups")
        .unwrap()
        .as_array()
        .unwrap()
    {
        let name = text(row, "name");
        let root = root();
        let journal = journal(&root).await;
        let previous = (name == "previous-epoch").then(|| id(1));
        if let Some(previous) = &previous {
            journal.begin_epoch(previous.clone()).await.unwrap();
        }
        if name.ends_with("fence") || name == "missing-native-lease" {
            let mut retained = fence();
            if name == "retained-foreign-fence" {
                retained.insert("owner", "runtime-owner".into());
            }
            record_process_stop_fence(journal.data_root().to_path_buf(), retained)
                .await
                .unwrap();
            let mut record = owner();
            if name == "unmatched-native-fence" {
                record.insert("epoch", id(2).into());
            }
            std::fs::write(
                journal.data_root().join("phone-native-owner.json"),
                stringify(&record, 4096).unwrap(),
            )
            .unwrap();
            if name != "missing-native-lease" {
                std::fs::write(journal.data_root().join("phone-native.lock"), b"").unwrap();
            }
            if name == "corrupt-stop-fence" {
                std::fs::write(journal.data_root().join("process-stop-fence.json"), b"{").unwrap();
            }
        }
        let mut options = options(&root, &name, &journal).await;
        if name == "corrupt-stop-fence" {
            options.artifact_root = options.artifact_root.join("absent");
        }
        let launcher = fake(&root, &name, &journal);
        let result = timed(start_with(
            options,
            launcher.clone(),
            name != "wrong-platform",
        ))
        .await;
        let (result, before_close, same_close, close, completion) = match result {
            Ok(process) => {
                let before = object([
                    (
                        "epochMatches",
                        (journal.epoch().await.unwrap() == Some(process.epoch.clone())).into(),
                    ),
                    (
                        "epochs",
                        journal
                            .status()
                            .await
                            .unwrap()
                            .get("epochs")
                            .unwrap()
                            .clone(),
                    ),
                ]);
                until(|| launcher.capture.lock().unwrap().stderr_drained).await;
                let first = process.close();
                let second = process.close();
                let same = first.0.same_channel(&second.0);
                let close = observed(timed(first.wait()).await.map(|_| IvyJson::Null));
                let completion = observed(timed(process.completion.wait()).await.map(|r| {
                    object([
                        (
                            "errorCode",
                            r.error_code.clone().map_or(IvyJson::Null, IvyJson::from),
                        ),
                        ("truncated", r.truncated.into()),
                    ])
                }));
                (
                    observed(Ok(IvyJson::Null)),
                    before,
                    same.into(),
                    close,
                    completion,
                )
            }
            Err(error) => (
                observed(Err(error)),
                IvyJson::Null,
                IvyJson::Null,
                IvyJson::Null,
                IvyJson::Null,
            ),
        };
        let mut fence = match process_stop_fence(journal.data_root().to_path_buf()).await {
            Ok(value) => value.unwrap_or(IvyJson::Null),
            Err(error) => observed(Err(error)),
        };
        if fence.get("ownerId").is_some() {
            fence.insert("observedAt", "$time".into());
            let current = launcher.capture.lock().unwrap().epoch.clone();
            let owned = fence.get("ownerId").and_then(IvyJson::as_js_string) == current.as_ref();
            fence.insert("ownerId", if owned { "$epoch" } else { "$previous" }.into());
        }
        let epoch = journal.epoch().await.unwrap();
        let epoch = if epoch == Some(id(4)) {
            "$changed".into()
        } else if epoch.is_some() && epoch == previous {
            "$previous".into()
        } else if epoch.is_none() {
            IvyJson::Null
        } else {
            "$epoch".into()
        };
        let actual = {
            let capture = launcher.capture.lock().unwrap();
            object([
                ("result", result),
                ("launches", (capture.launches as f64).into()),
                ("stops", (capture.stops as f64).into()),
                ("command", capture.command.clone().unwrap_or(IvyJson::Null)),
                ("beforeClose", before_close),
                ("sameClose", same_close),
                ("close", close),
                ("completion", completion),
                ("epoch", epoch),
                ("fence", fence),
            ])
        };
        equal(&actual, row.get("expected").unwrap(), &name);
        drop(launcher);
        journal.close().wait().await.unwrap();
        remove(root);
    }
}

#[tokio::test]
async fn startup_observer_loss_retains_original_completion_and_unknown_fence() {
    let root = root();
    let journal = journal(&root).await;
    let options = options(&root, "observer-unknown-stop", &journal).await;
    let gate = Arc::new(Semaphore::new(0));
    let mut launcher = fake(&root, "observer-unknown-stop", &journal);
    Arc::get_mut(&mut launcher).unwrap().status_gate = Some(gate.clone());
    let start_launcher = launcher.clone();
    let start = tokio::spawn(async move { start_with(options, start_launcher, true).await });
    until(|| launcher.capture.lock().unwrap().status_requests == 1).await;
    assert!(journal.epoch().await.unwrap().is_none());
    start.abort();
    let _ = start.await;
    gate.add_permits(1);
    timed(async {
        loop {
            let stops = launcher.capture.lock().unwrap().stops;
            if stops == 1
                && journal.epoch().await.unwrap().is_none()
                && process_stop_fence(journal.data_root().to_path_buf())
                    .await
                    .unwrap()
                    .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let retained = process_stop_fence(journal.data_root().to_path_buf())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        retained.get("ownerId").and_then(IvyJson::as_js_string),
        launcher.capture.lock().unwrap().epoch.as_ref()
    );
    assert_eq!(launcher.capture.lock().unwrap().status_requests, 1);
    assert_eq!(
        require_clear_process_stop_fence(journal.data_root().to_path_buf())
            .await
            .unwrap_err()
            .outcome,
        Outcome::Unknown
    );
    drop(launcher);
    journal.close().wait().await.unwrap();
    remove(root);
}

#[tokio::test]
async fn eof_deadline_still_uses_the_single_shared_stop_authority() {
    let root = root();
    let journal = journal(&root).await;
    let options = options(&root, "forced-unknown-stop", &journal).await;
    let mut launcher = fake(&root, "forced-unknown-stop", &journal);
    Arc::get_mut(&mut launcher).unwrap().require_stop = true;
    let process = timed(start_with(options, launcher.clone(), true))
        .await
        .unwrap();
    let began = tokio::time::Instant::now();
    let close = process.close();
    drop(process);
    let error = timed(close.wait()).await.unwrap_err();
    assert_eq!(error.code, "outcome_unknown");
    assert_eq!(error.outcome, Outcome::Unknown);
    assert!(began.elapsed() >= Duration::from_secs(5));
    assert!(began.elapsed() < Duration::from_secs(8));
    assert_eq!(launcher.capture.lock().unwrap().stops, 1);
    assert!(
        process_stop_fence(journal.data_root().to_path_buf())
            .await
            .unwrap()
            .is_some()
    );
    assert!(journal.epoch().await.unwrap().is_none());
    drop(launcher);
    journal.close().wait().await.unwrap();
    remove(root);
}
