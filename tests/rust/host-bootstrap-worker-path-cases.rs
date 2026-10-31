use super::*;
use ivy_host_runtime::bootstrap_auto_worker::AutomaticBootstrapWorker;

#[cfg(windows)]
#[tokio::test]
async fn bootstrap_windows_pause_retains_original_jobs_past_the_scheduler_deadline_and_a_dropped_observer()
 {
    use ivy_host_runtime::bootstrap_os::WindowsBootstrapOs;
    use std::os::windows::process::CommandExt;
    let t = Test::new().await;
    t.seed().await;
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../assets/bootstrap-maintenance-windows.ps1");
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/host-bootstrap-drain.ps1");
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let distribution = t.f.root.0.join("pause adapter");
    let assets = distribution.join("dist/packages/host-runtime/assets");
    fs::create_dir_all(&assets).unwrap();
    for name in ["bootstrap-maintenance-windows.ps1", "windows-principal.ps1"] {
        fs::copy(source.parent().unwrap().join(name), assets.join(name)).unwrap();
    }
    let mut plan = t.old.clone();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let installation = format!("ivy-next-{}", &nonce[..12]);
    plan.insert("installationId", installation.clone().into());
    plan.insert("hostId", "cooperative-bootstrap-fixture".into());
    let artifact = node_path::to_native(
        plan.required("artifactRoot")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    fs::create_dir_all(artifact.join("dist/native")).unwrap();
    fs::copy(job(), artifact.join("dist/native/ivy-host-job.exe")).unwrap();
    let mut processes = array(plan.required("processes").unwrap()).to_vec();
    let mut roots = Vec::new();
    for (index, process) in processes.iter_mut().enumerate() {
        process.insert("name", format!("{installation}-{index:012x}").into());
        let config_path = node_path::to_native(
            process
                .required("configPath")
                .unwrap()
                .as_js_string()
                .unwrap(),
        );
        let configuration = config::json_file(&config_path, config::CONFIGURATION_BYTES)
            .await
            .unwrap();
        let root = node_path::to_native(
            configuration
                .required("dataRoot")
                .unwrap()
                .as_js_string()
                .unwrap(),
        );
        fs::create_dir_all(&root).unwrap();
        roots.push((id(process, "componentId"), root));
        process.insert(
            "entrypoint",
            J::object([
                ("executable", p(&powershell)),
                (
                    "args",
                    J::Array(vec![
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-ExecutionPolicy".into(),
                        "Bypass".into(),
                        "-File".into(),
                        p(&script),
                        "-Mode".into(),
                        "child".into(),
                        "-FixtureConfiguration".into(),
                        p(&config_path),
                    ]),
                ),
                ("timeoutMs", J::Number(30000.0)),
            ]),
        );
    }
    plan.insert("processes", J::Array(processes));
    let input = J::object([("plan", plan.clone()), ("source", p(&source))]);
    struct Tasks {
        powershell: PathBuf,
        script: PathBuf,
        input: J,
    }
    impl Tasks {
        fn run(&self, mode: &str) {
            let mut child = Command::new(&self.powershell)
                .creation_flags(0x08000000)
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(&self.script)
                .args(["-Mode", mode])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(canonical(&self.input).as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    impl Drop for Tasks {
        fn drop(&mut self) {
            self.run("cleanup");
        }
    }
    let tasks = Tasks {
        powershell,
        script,
        input,
    };
    tasks.run("prepare");
    async fn marker(path: &Path) {
        tokio::time::timeout(Duration::from_secs(20), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("Missing fixture marker: {}", path.display()));
    }
    for (_, root) in &roots {
        marker(&root.join("health.json")).await;
    }
    let os = WindowsBootstrapOs::new(distribution, job());
    let owners = os.inspect(vec![plan.clone()]).await.unwrap();
    assert!(
        owners
            .iter()
            .all(|owner| owner.get("enabled") == Some(&true.into())
                && owner.get("running") == Some(&true.into()))
    );
    let snapshot = J::object([
        ("hostId", plan.required("hostId").unwrap().clone()),
        ("installationId", installation.into()),
        ("owners", J::Array(owners.clone())),
    ]);
    let mut changed = snapshot.clone();
    let mut changed_owners = owners.clone();
    changed_owners[0].insert("definitionHash", "sha256:changed".into());
    changed.insert("owners", J::Array(changed_owners));
    assert_eq!(
        os.pause(changed, vec![plan.clone()])
            .unwrap()
            .await
            .unwrap_err()
            .code,
        "target_conflict"
    );
    assert_json_eq(
        &J::Array(os.inspect(vec![plan.clone()]).await.unwrap()),
        &J::Array(owners),
    );
    let observer = tokio::spawn({
        let os = os.clone();
        let plan = plan.clone();
        let snapshot = snapshot.clone();
        async move { os.pause(snapshot, vec![plan]).unwrap().await }
    });
    let manager = &roots
        .iter()
        .find(|(component, _)| component == "service-manager")
        .unwrap()
        .1;
    let executor = &roots
        .iter()
        .find(|(component, _)| component == "host-executor")
        .unwrap()
        .1;
    marker(&manager.join("draining")).await;
    let original_health =
        config::json_file(manager.join("health.json"), config::CONFIGURATION_BYTES)
            .await
            .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    // Cross the former outer 60-second deadline with real retained work.
    tokio::time::sleep(Duration::from_secs(61)).await;
    let during = os.inspect(vec![plan.clone()]).await.unwrap();
    assert!(
        during
            .iter()
            .all(|owner| owner.get("enabled") == Some(&false.into())
                && owner.get("running") == Some(&true.into()))
    );
    assert!(!manager.join("completed").exists());
    assert_json_eq(
        &config::json_file(manager.join("health.json"), config::CONFIGURATION_BYTES)
            .await
            .unwrap(),
        &original_health,
    );
    assert!(
        !executor.join("control.json").exists(),
        "HostExecutor remains available while services drain"
    );
    fs::write(manager.join("release"), b"work finished").unwrap();
    marker(&manager.join("completed")).await;
    marker(&executor.join("draining")).await;
    fs::write(executor.join("release"), b"work finished").unwrap();
    marker(&executor.join("completed")).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if os
                .inspect(vec![plan.clone()])
                .await
                .unwrap()
                .iter()
                .all(|owner| owner.get("running") == Some(&false.into()))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    // A configuration-only restart uses the same cooperative process wait.
    for (_, root) in &roots {
        for name in [
            "health.json",
            "control.json",
            "draining",
            "release",
            "completed",
        ] {
            let path = root.join(name);
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }
    }
    os.resume(snapshot, vec![plan.clone()])
        .unwrap()
        .await
        .unwrap();
    for (_, root) in &roots {
        marker(&root.join("health.json")).await;
    }
    use ivy_host_runtime::bootstrap_restart::{
        BootstrapRestartOs, NativeBootstrapRestartOs, restart_windows_owner,
    };
    let native = NativeBootstrapRestartOs::new(job());
    let manager_owner = os
        .inspect(vec![plan.clone()])
        .await
        .unwrap()
        .into_iter()
        .find(|owner| owner.get("componentId") == Some(&"service-manager".into()))
        .unwrap();
    let name = manager_owner
        .required("name")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    assert_eq!(
        native
            .command("/End", name.clone(), t.f.root.0.join("pause adapter"))
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let mut changed = manager_owner;
    changed.insert("definitionHash", "sha256:changed".into());
    assert_eq!(
        native
            .shutdown(
                changed,
                vec![plan.clone()],
                t.f.root.0.join("pause adapter")
            )
            .await
            .unwrap_err()
            .code,
        "target_conflict"
    );
    assert!(!manager.join("control.json").exists());
    let original = config::json_file(manager.join("health.json"), config::CONFIGURATION_BYTES)
        .await
        .unwrap();
    let restart = tokio::spawn({
        let plan = plan.clone();
        let distribution = t.f.root.0.join("pause adapter");
        async move {
            restart_windows_owner(
                vec![plan],
                name,
                distribution,
                &ivy_sdk::clock::SystemClock,
                &native,
            )
            .await
        }
    });
    marker(&manager.join("draining")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!restart.is_finished());
    assert!(!manager.join("completed").exists());
    fs::write(manager.join("release"), b"work finished").unwrap();
    tokio::time::timeout(Duration::from_secs(20), restart)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current =
                config::json_file(manager.join("health.json"), config::CONFIGURATION_BYTES)
                    .await
                    .unwrap();
            if current.get("bootId") != original.get("bootId") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    drop(tasks);
    assert!(
        os.inspect(vec![plan]).await.is_err(),
        "Own scheduled tasks were removed"
    );
    t.f.journal.close().await.unwrap();
}

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
