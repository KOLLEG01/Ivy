use super::*;
use ivy_host_runtime::bootstrap_auto_worker::AutomaticBootstrapWorker;

struct Ready {
    events: Mutex<Vec<J>>,
    next: J,
    old: J,
    fail: bool,
    fail_prior: bool,
    mutate: Option<(PathBuf, J)>,
}
impl BootstrapReadiness for Ready {
    fn wait(
        &self,
        _: J,
        plan: J,
        states: J,
        not_before: i64,
        _: i64,
    ) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async move {
            self.events.lock().unwrap().push(J::object([
                ("plan", plan.required("candidateId")?.clone()),
                ("states", states),
                ("notBefore", J::Number(not_before as f64)),
            ]));
            if let Some((path, request)) = &self.mutate {
                let mut request = request.clone();
                let mut previous = request.required("previous")?.clone();
                let mut owners = array(previous.required("owners")?).to_vec();
                for owner in &mut owners {
                    owner.insert("enabled", false.into());
                    owner.insert("running", false.into());
                }
                previous.insert("owners", J::Array(owners));
                request.insert("previous", previous);
                config::atomic_json(path.clone(), request).await?;
            }
            if self.fail && plan.get("candidateId") == Some(&self.next) {
                return Err(IvyError::new(
                    "readiness_failed",
                    "Simulated updated bootstrap readiness failure.",
                ));
            }
            if self.fail_prior && plan.get("candidateId") == Some(&self.old) {
                return Err(IvyError::new(
                    "readiness_failed",
                    "Simulated prior bootstrap readiness failure.",
                ));
            }
            Ok(())
        })
    }
}

struct WorkerFixture {
    t: Test,
    config_path: PathBuf,
    request_path: PathBuf,
    record_path: PathBuf,
}
impl WorkerFixture {
    async fn new(config_name: &std::ffi::OsStr, request_name: &std::ffi::OsStr) -> Self {
        let executable = std::env::var("IVY_TEST_NODE").expect("Pinned Node path required");
        let t = Test::with_configuration(
            config_name,
            &executable,
            Arc::new(|left, right| {
                assert_eq!(left, right, "Only equal fixture timestamps need comparison");
                std::cmp::Ordering::Equal
            }),
        )
        .await;
        t.seed().await;
        let config_path = t.f.root.0.join(config_name);
        let root =
            bootstrap_automatic::automatic_bootstrap_root(&t.f.config, &"maintenance-1".into())
                .unwrap();
        let request_path = root.join(request_name);
        config::atomic_json(&request_path, t.request.clone())
            .await
            .unwrap();
        let result = Self {
            t,
            config_path,
            request_path,
            record_path: root.join("record.json"),
        };
        result.phase("launched").await;
        result
    }
    async fn plain() -> Self {
        Self::new("host.json".as_ref(), "request.json".as_ref()).await
    }
    async fn phase(&self, phase: &str) -> J {
        let mut record = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("operationId", "maintenance-1".into()),
            ("requestHash", hash_json(&self.t.request).unwrap().into()),
            ("phase", phase.into()),
            ("errorCode", J::Null),
            ("createdAt", DATE.into()),
            ("updatedAt", DATE.into()),
        ]);
        bootstrap_automatic::save_automatic_bootstrap_record(&self.t.f.config, &mut record, &Fixed)
            .await
            .unwrap();
        record
    }
    fn readiness(&self, fail: bool, fail_prior: bool, mutate: bool) -> Arc<Ready> {
        Arc::new(Ready {
            events: Mutex::new(vec![]),
            next: self.t.next.required("candidateId").unwrap().clone(),
            old: self.t.old.required("candidateId").unwrap().clone(),
            fail,
            fail_prior,
            mutate: mutate.then(|| (self.request_path.clone(), self.t.request.clone())),
        })
    }
    fn worker(&self, readiness: Arc<Ready>) -> AutomaticBootstrapWorker {
        AutomaticBootstrapWorker::with_readiness(self.t.maintenance.clone(), readiness)
    }
    async fn record(&self) -> J {
        config::json_file(&self.record_path, config::CONFIGURATION_BYTES)
            .await
            .unwrap()
    }
    fn assert_no_os(&self, ready: &Ready) {
        let state = self.t.os.state.lock().unwrap();
        assert_eq!(
            (
                state.inspections,
                state.writes,
                state.reloads,
                state.pauses,
                state.resumes
            ),
            (0, 0, 0, 0, 0),
        );
        assert!(ready.events.lock().unwrap().is_empty());
    }
    async fn close(&self) {
        self.t.f.journal.close().await.unwrap();
    }
}
async fn worker_case(
    case: &str,
    starting: &str,
    fail: bool,
    fail_pause: bool,
    third: bool,
    mutate: bool,
    fail_prior: bool,
    repeat: bool,
) {
    let fixture = WorkerFixture::plain().await;
    let t = &fixture.t;
    let mut expected_record = fixture.phase(starting).await;
    let config_bytes = fs::read(&fixture.config_path).unwrap();
    {
        let mut s = t.os.state.lock().unwrap();
        s.fail_pause = fail_pause;
        s.pause_third = third;
    }
    let readiness = fixture.readiness(fail, fail_prior, mutate);
    let worker = fixture.worker(readiness.clone());
    let result = worker
        .run(fixture.config_path.clone(), fixture.request_path.clone())
        .await;
    let attention = third || fail_prior;
    if attention {
        assert_eq!(
            result.unwrap_err().code,
            if third {
                "target_conflict"
            } else {
                "readiness_failed"
            },
            "{case}",
        );
    } else {
        result.unwrap();
    }
    let interrupted = ["updating", "verifying", "rolling_back"].contains(&starting);
    let terminal = starting == "rolled_back";
    let rolled_back = fail || fail_pause || interrupted || terminal;
    let phase = if attention {
        "needs_attention"
    } else if rolled_back {
        "rolled_back"
    } else {
        "succeeded"
    };
    let record = fixture.record().await;
    expected_record.insert("phase", phase.into());
    expected_record.insert(
        "errorCode",
        match () {
            _ if third => "target_conflict".into(),
            _ if fail => "readiness_failed".into(),
            _ if fail_pause => "bootstrap_os_unavailable".into(),
            _ if interrupted => "outcome_unknown".into(),
            _ => J::Null,
        },
    );
    assert_json_eq(&record, &expected_record);
    let (pauses, resumes, writes, events) = if terminal {
        (0, 0, 0, 0)
    } else if third {
        (1, 0, 0, 0)
    } else if fail {
        (2, 2, replacements(&t.old, &t.next) * 2, 2)
    } else if fail_pause || interrupted {
        (1, 1, 0, 1)
    } else {
        (1, 1, replacements(&t.old, &t.next), 1)
    };
    {
        let s = t.os.state.lock().unwrap();
        assert_eq!(
            (s.pauses, s.resumes, s.writes),
            (pauses, resumes, writes),
            "{case}"
        );
        for (owner, prior) in s.owners.iter().zip(array(
            t.request
                .required("previous")
                .unwrap()
                .required("owners")
                .unwrap(),
        )) {
            for key in ["enabled", "running"] {
                assert_eq!(
                    owner.get(key),
                    Some(&J::from(if third { false } else { true })),
                    "{case}/{key}"
                );
            }
            assert_eq!(owner.get("instanceId"), prior.get("instanceId"));
            if !third {
                let plan = if rolled_back { &t.old } else { &t.next };
                let process = array(plan.required("processes").unwrap())
                    .iter()
                    .find(|p| p.get("instanceId") == owner.get("instanceId"))
                    .unwrap();
                assert_eq!(owner.get("candidateId"), process.get("candidateId"));
                let retained = json::parse(
                    &id(owner.required("definition").unwrap(), "content"),
                    usize::MAX,
                )
                .unwrap();
                assert_eq!(
                    retained.get("preserved"),
                    Some(&"principal, triggers and settings".into())
                );
            }
        }
    }
    let ready = readiness.events.lock().unwrap().clone();
    assert_eq!(ready.len(), events, "{case}");
    for (index, event) in ready.iter().enumerate() {
        let plan = if fail && index == 0 || !rolled_back {
            &t.next
        } else {
            &t.old
        };
        assert_eq!(event.get("plan"), plan.get("candidateId"));
        assert_json_eq(
            event.required("states").unwrap(),
            t.request.required("previous").unwrap(),
        );
        assert_eq!(event.get("notBefore"), Some(&J::Number(AT as f64)));
    }
    let selected = config::json_file(
        t.f.root
            .0
            .join("state/bootstrap")
            .join(format!("{}.json", id(&t.old, "installationId"))),
        config::CONFIGURATION_BYTES,
    )
    .await
    .unwrap();
    assert_json_eq(
        &selected,
        if rolled_back || attention {
            &t.old
        } else {
            &t.next
        },
    );
    assert_eq!(fs::read(&fixture.config_path).unwrap(), config_bytes);
    if mutate {
        let on_disk = config::json_file(&fixture.request_path, config::CONFIGURATION_BYTES)
            .await
            .unwrap();
        assert_ne!(hash_json(&on_disk).unwrap(), hash_json(&t.request).unwrap());
    }
    if repeat {
        let before = (t.os.trace(), ready, fs::read(&fixture.record_path).unwrap());
        t.os.state.lock().unwrap().forbid = true;
        worker
            .run(fixture.config_path.clone(), fixture.request_path.clone())
            .await
            .unwrap();
        assert_json_eq(&t.os.trace(), &before.0);
        assert_eq!(readiness.events.lock().unwrap().len(), before.1.len());
        assert_eq!(fs::read(&fixture.record_path).unwrap(), before.2);
    }
    fixture.close().await;
}
fn relative(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let mut base = cwd.as_path();
    let mut result = PathBuf::new();
    while !path.starts_with(base) {
        base = base
            .parent()
            .expect("Fixture and CWD must share a native root");
        result.push("..");
    }
    result.push(path.strip_prefix(base).unwrap());
    result
}
fn filename(prefix: &str, opaque: Option<u16>) -> std::ffi::OsString {
    let name = std::ffi::OsString::from(format!("{prefix}-中-🚀"));
    #[cfg(windows)]
    let name = {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut units: Vec<_> = name.encode_wide().collect();
        if let Some(unit) = opaque {
            units.push(unit);
        }
        std::ffi::OsString::from_wide(&units)
    };
    #[cfg(not(windows))]
    let _ = opaque;
    let mut name = name;
    name.push(".json");
    name
}
async fn input_paths() {
    let mut variants = vec![
        "absolute",
        "relative",
        "missing-parent",
        "junction-parent",
        "canonical",
        "trailing",
        "unicode",
    ];
    #[cfg(windows)]
    variants.extend([
        "root-relative",
        "drive-relative",
        "opaque-high",
        "opaque-low",
    ]);
    for variant in variants {
        let opaque = match variant {
            "opaque-high" => Some(0xd800),
            "opaque-low" => Some(0xdfff),
            _ => None,
        };
        let config_name = filename("host", opaque);
        let request_name = filename("request", opaque);
        let fixture = WorkerFixture::new(&config_name, &request_name).await;
        fixture.phase("rolled_back").await;
        fixture.t.os.state.lock().unwrap().forbid = true;
        let bytes = (
            fs::read(&fixture.config_path).unwrap(),
            fs::read(&fixture.request_path).unwrap(),
            fs::read(&fixture.record_path).unwrap(),
        );
        let transform = |path: &Path| {
            let parent = path.parent().unwrap();
            match variant {
                "relative" => relative(path),
                "missing-parent" => parent
                    .join("uncreated")
                    .join("..")
                    .join(".")
                    .join(path.file_name().unwrap()),
                "junction-parent" => {
                    let target = parent.join("physical/target");
                    fs::create_dir_all(&target).unwrap();
                    fs::write(
                        parent.join("physical").join(path.file_name().unwrap()),
                        b"wrong physical parent",
                    )
                    .unwrap();
                    let link = parent.join("link");
                    ivy_host_runtime::filesystem::directory_link(&target, &link).unwrap();
                    link.join("..").join(path.file_name().unwrap())
                }
                "canonical" => fs::canonicalize(path).unwrap(),
                "trailing" => {
                    let mut p = path.as_os_str().to_owned();
                    p.push(std::path::MAIN_SEPARATOR_STR);
                    PathBuf::from(p)
                }
                #[cfg(windows)]
                "root-relative" => {
                    use std::os::windows::ffi::{OsStrExt, OsStringExt};
                    let units: Vec<_> = path.as_os_str().encode_wide().skip(2).collect();
                    PathBuf::from(std::ffi::OsString::from_wide(&units))
                }
                #[cfg(windows)]
                "drive-relative" => {
                    use std::os::windows::ffi::{OsStrExt, OsStringExt};
                    let prefix: Vec<_> = path.as_os_str().encode_wide().take(2).collect();
                    let mut p = std::ffi::OsString::from_wide(&prefix);
                    p.push(relative(path));
                    PathBuf::from(p)
                }
                _ => path.to_owned(),
            }
        };
        let config = transform(&fixture.config_path);
        let request = transform(&fixture.request_path);
        let ready = fixture.readiness(false, false, false);
        fixture
            .worker(ready.clone())
            .run(config, request)
            .await
            .unwrap_or_else(|e| panic!("{variant}: {}", e.code));
        fixture.assert_no_os(&ready);
        assert_eq!(fs::read(&fixture.config_path).unwrap(), bytes.0);
        assert_eq!(fs::read(&fixture.request_path).unwrap(), bytes.1);
        assert_eq!(fs::read(&fixture.record_path).unwrap(), bytes.2);
        fixture.close().await;
    }
}
async fn refusal_order() {
    for variant in [
        "missing-request",
        "syntax-request",
        "schema-before-config",
        "directory-request",
        "empty-request",
        "nul-request",
        "missing-config",
        "directory-config",
        "nul-config",
        "missing-record",
        "hash-mismatch",
        "needs-attention",
    ] {
        let fixture = WorkerFixture::plain().await;
        fixture.t.os.state.lock().unwrap().forbid = true;
        let mut config = fixture.config_path.clone();
        let mut request = fixture.request_path.clone();
        let expected = match variant {
            "missing-request" => {
                request.set_file_name("missing.json");
                config = fixture.t.f.root.0.clone();
                "internal_error"
            }
            "syntax-request" => {
                fs::write(&request, b"{").unwrap();
                config = fixture.t.f.root.0.clone();
                "internal_error"
            }
            "schema-before-config" => {
                fs::write(&request, b"{}").unwrap();
                config.set_file_name("missing.json");
                "invalid_arguments"
            }
            "directory-request" => {
                request = request.parent().unwrap().to_owned();
                "invalid_arguments"
            }
            "empty-request" => {
                request = PathBuf::new();
                "invalid_arguments"
            }
            "nul-request" => {
                request = PathBuf::from("invalid\0request");
                "internal_error"
            }
            "missing-config" => {
                config.set_file_name("missing.json");
                "internal_error"
            }
            "directory-config" => {
                config = fixture.t.f.root.0.clone();
                "invalid_arguments"
            }
            "nul-config" => {
                config = PathBuf::from("invalid\0config");
                "internal_error"
            }
            "missing-record" => {
                fs::remove_file(&fixture.record_path).unwrap();
                "storage_invalid"
            }
            "hash-mismatch" => {
                let mut record = fixture.record().await;
                record.insert("requestHash", digest(b"other request").into());
                config::atomic_json(&fixture.record_path, record)
                    .await
                    .unwrap();
                "storage_invalid"
            }
            "needs-attention" => {
                fixture.phase("needs_attention").await;
                "bootstrap_update_failed"
            }
            _ => unreachable!(),
        };
        let before = (
            fs::read(&fixture.config_path).unwrap(),
            fs::read(&fixture.request_path).unwrap(),
            fs::read(&fixture.record_path).ok(),
        );
        let ready = fixture.readiness(false, false, false);
        let error = fixture
            .worker(ready.clone())
            .run(config, request)
            .await
            .unwrap_err();
        assert_eq!(error.code, expected, "{variant}");
        fixture.assert_no_os(&ready);
        assert_eq!(fs::read(&fixture.config_path).unwrap(), before.0);
        assert_eq!(fs::read(&fixture.request_path).unwrap(), before.1);
        assert_eq!(fs::read(&fixture.record_path).ok(), before.2);
        fixture.close().await;
    }
}
async fn detached_owner() {
    let fixture = WorkerFixture::plain().await;
    let ready = fixture.readiness(false, false, false);
    let worker = fixture.worker(ready.clone());
    fixture.t.os.block.store(true, Ordering::SeqCst);
    let config = fixture.config_path.clone();
    let request = fixture.request_path.clone();
    let waiter = tokio::spawn(async move { worker.run(config, request).await });
    tokio::time::timeout(Duration::from_secs(5), fixture.t.os.entered.notified())
        .await
        .unwrap();
    waiter.abort();
    let _ = waiter.await;
    for root in [
        "executor",
        "owners/executor-renamed",
        "bootstrap-maintenance-lock",
    ] {
        assert_eq!(
            ExecutorLock::acquire(fixture.t.f.root.0.join("state").join(root), false)
                .await
                .err()
                .unwrap()
                .code,
            "executor_already_running",
        );
    }
    fixture.t.os.released.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if fixture.record().await.get("phase") == Some(&"succeeded".into()) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(ready.events.lock().unwrap().len(), 1);
    assert!(
        array(&fixture.t.os.owners())
            .iter()
            .all(|o| o.get("running") == Some(&true.into()))
    );
    let lock = ExecutorLock::acquire(fixture.t.f.root.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
    fixture.close().await;
}
#[tokio::test]
async fn bootstrap_worker_success_and_terminal_replay_preserve_original_scheduling() {
    input_paths().await;
    refusal_order().await;
    detached_owner().await;
    worker_case(
        "worker-success-replay",
        "launched",
        false,
        false,
        false,
        false,
        false,
        true,
    )
    .await;
    worker_case(
        "worker-terminal-rollback-replay",
        "rolled_back",
        false,
        false,
        false,
        false,
        false,
        true,
    )
    .await;
}
#[tokio::test]
async fn bootstrap_worker_failed_readiness_rolls_back_exact_candidates_and_held_snapshot() {
    worker_case(
        "worker-readiness-rollback-held-request",
        "launched",
        true,
        false,
        false,
        true,
        false,
        false,
    )
    .await;
}
#[tokio::test]
async fn bootstrap_worker_interrupted_phases_roll_back_before_resuming() {
    for phase in ["updating", "verifying", "rolling_back"] {
        worker_case(
            &format!("worker-interrupted-{phase}"),
            phase,
            false,
            false,
            false,
            false,
            false,
            true,
        )
        .await;
    }
}
#[tokio::test]
async fn bootstrap_worker_partial_pause_recovers_only_unchanged_definitions() {
    worker_case(
        "worker-partial-pause-recovered",
        "launched",
        false,
        true,
        false,
        false,
        false,
        false,
    )
    .await;
    worker_case(
        "worker-partial-pause-third-state",
        "launched",
        false,
        true,
        true,
        false,
        false,
        false,
    )
    .await;
    worker_case(
        "worker-rollback-readiness-failed",
        "launched",
        true,
        false,
        false,
        false,
        true,
        false,
    )
    .await;
}
