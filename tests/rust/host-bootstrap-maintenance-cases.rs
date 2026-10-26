use super::{
    assert_json_eq,
    package_updater_cases::{Fixture, comparator, instance, job, node},
};
use futures_util::future::BoxFuture;
use ivy_contracts::{
    IvyJson as J,
    core_schema::validate_host,
    hash::{digest, hash_json},
    json,
};
use ivy_host_runtime::{
    IvyError, IvyResult, Outcome, bootstrap,
    bootstrap_auto_worker::{BootstrapReadiness, NativeBootstrapReadiness},
    bootstrap_automatic,
    bootstrap_maintenance::{self, BootstrapMaintenance},
    bootstrap_os::BootstrapOs,
    config,
    executor_lock::ExecutorLock,
    host_config, layout, node_path,
};
use ivy_sdk::clock::Clock;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
const AT: i64 = 1760000000000;
const DATE: &str = "2025-10-09T08:53:20.000Z";
fn p(path: &Path) -> J {
    J::String(node_path::from_native(path))
}
fn id(value: &J, key: &str) -> String {
    value.required(key).unwrap().as_str().unwrap()
}
fn array(value: &J) -> &[J] {
    value.as_array().unwrap()
}
fn canonical(value: &J) -> String {
    json::canonical(value, usize::MAX).unwrap()
}
fn wrap(value: IvyResult<J>) -> J {
    match value {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
struct Fixed;
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        AT
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
#[derive(Default)]
struct State {
    owners: Vec<J>,
    writes: usize,
    inspections: usize,
    reloads: usize,
    pauses: usize,
    resumes: usize,
    fail: bool,
    forbid: bool,
    change_config: bool,
    fail_pause: bool,
    pause_third: bool,
}
struct Os {
    state: Mutex<State>,
    config: J,
    path: PathBuf,
    entered: tokio::sync::Notify,
    released: tokio::sync::Notify,
    block: AtomicBool,
}
impl Os {
    fn new(owners: Vec<J>, config: J, path: PathBuf) -> Self {
        Self {
            state: Mutex::new(State {
                owners,
                ..Default::default()
            }),
            config,
            path,
            entered: tokio::sync::Notify::new(),
            released: tokio::sync::Notify::new(),
            block: AtomicBool::new(false),
        }
    }
    fn owners(&self) -> J {
        J::Array(self.state.lock().unwrap().owners.clone())
    }
    fn trace(&self) -> J {
        let s = self.state.lock().unwrap();
        J::object([
            ("owners", J::Array(s.owners.clone())),
            ("writes", J::Number(s.writes as f64)),
            ("inspections", J::Number(s.inspections as f64)),
            ("reloads", J::Number(s.reloads as f64)),
        ])
    }
}
fn definition(content: String) -> J {
    J::object([
        ("hash", digest(content.as_bytes()).into()),
        ("content", content.into()),
    ])
}
impl BootstrapOs for Os {
    fn inspect(&self, _: Vec<J>) -> BoxFuture<'_, IvyResult<Vec<J>>> {
        Box::pin(async move {
            let mut s = self.state.lock().unwrap();
            if s.forbid {
                return Err(IvyError::new(
                    "fixture_os_forbidden",
                    "Historical replay accessed the OS.",
                ));
            }
            s.inspections += 1;
            Ok(s.owners.clone())
        })
    }
    fn render(&self, owner: J, next: J) -> BoxFuture<'_, IvyResult<J>> {
        Box::pin(async move {
            let process = array(next.required("processes")?)
                .iter()
                .find(|p| p.get("instanceId") == owner.get("instanceId"))
                .unwrap();
            let mut content =
                json::parse(&id(owner.required("definition")?, "content"), usize::MAX)?;
            content.insert("candidateId", process.required("candidateId")?.clone());
            Ok(definition(canonical(&content)))
        })
    }
    fn check(&self, value: J) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async move {
            if value.get("hash") != Some(&digest(id(&value, "content").as_bytes()).into()) {
                return Err(IvyError::new(
                    "storage_invalid",
                    "Changed simulated definition.",
                ));
            }
            Ok(())
        })
    }
    fn apply(&self, owner: J, desired: J, _: Vec<J>) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async move {
            self.entered.notify_one();
            if self.block.swap(false, Ordering::SeqCst) {
                self.released.notified().await;
            }
            let (change, fail) = {
                let mut s = self.state.lock().unwrap();
                let current = s
                    .owners
                    .iter_mut()
                    .find(|p| p.get("instanceId") == owner.get("instanceId"))
                    .unwrap();
                if current.get("running") == Some(&true.into())
                    || current.get("enabled") == Some(&true.into())
                    || ![owner.get("definitionHash"), desired.get("hash")]
                        .contains(&current.get("definitionHash"))
                {
                    return Err(IvyError::new(
                        "target_conflict",
                        "Simulated definition changed.",
                    ));
                }
                if current.get("definitionHash") == desired.get("hash") {
                    return Ok(());
                }
                current.insert("definition", desired.clone());
                current.insert("definitionHash", desired.required("hash")?.clone());
                current.insert(
                    "candidateId",
                    json::parse(&id(&desired, "content"), usize::MAX)?
                        .required("candidateId")?
                        .clone(),
                );
                s.writes += 1;
                let flags = (s.change_config, s.fail);
                s.change_config = false;
                s.fail = false;
                flags
            };
            if change {
                let mut config = self.config.clone();
                config.insert("publicBaseUrl", "http://127.0.0.1:39081/changed".into());
                config::atomic_json(self.path.clone(), config).await?;
            }
            if fail {
                return Err(IvyError::new(
                    "outcome_unknown",
                    "Simulated process loss after the OS write and before its recorded outcome.",
                )
                .with_outcome(Outcome::Unknown));
            }
            Ok(())
        })
    }
    fn reload(&self) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async move {
            self.state.lock().unwrap().reloads += 1;
            Ok(())
        })
    }
    fn pause(&self, _: J, _: Vec<J>) -> Option<BoxFuture<'_, IvyResult<()>>> {
        Some(Box::pin(async move {
            let mut s = self.state.lock().unwrap();
            s.pauses += 1;
            for owner in &mut s.owners {
                owner.insert("running", false.into());
                owner.insert("enabled", false.into());
            }
            if s.fail_pause {
                s.fail_pause = false;
                if s.pause_third {
                    let owner = &mut s.owners[0];
                    let d = definition(format!(
                        "{}third",
                        id(owner.required("definition")?, "content")
                    ));
                    owner.insert("definitionHash", d.required("hash")?.clone());
                    owner.insert("definition", d);
                }
                return Err(IvyError::new(
                    "bootstrap_os_unavailable",
                    "Simulated partial pause.",
                ));
            }
            Ok(())
        }))
    }
    fn resume(&self, snapshot: J, _: Vec<J>) -> Option<BoxFuture<'_, IvyResult<()>>> {
        Some(Box::pin(async move {
            let mut s = self.state.lock().unwrap();
            s.resumes += 1;
            for owner in &mut s.owners {
                let prior = array(snapshot.required("owners")?)
                    .iter()
                    .find(|p| p.get("instanceId") == owner.get("instanceId"))
                    .unwrap();
                for key in ["enabled", "running"] {
                    owner.insert(key, prior.required(key)?.clone());
                }
            }
            Ok(())
        }))
    }
    fn can_pause_and_resume(&self) -> bool {
        true
    }
}
fn owners(plan: &J) -> Vec<J> {
    array(plan.required("processes").unwrap())
        .iter()
        .map(|process| {
            let mut owner = process.clone();
            let d = definition(canonical(&J::object([
                (
                    "candidateId",
                    process.required("candidateId").unwrap().clone(),
                ),
                ("instance", process.clone()),
                ("preserved", "principal, triggers and settings".into()),
            ])));
            owner.insert("definitionHash", d.required("hash").unwrap().clone());
            owner.insert("definition", d);
            owner.insert("running", true.into());
            owner.insert("enabled", true.into());
            owner
        })
        .collect()
}
fn replacements(old: &J, next: &J) -> usize {
    array(next.required("processes").unwrap())
        .iter()
        .filter(|process| {
            array(old.required("processes").unwrap())
                .iter()
                .find(|prior| prior.get("instanceId") == process.get("instanceId"))
                .is_none_or(|prior| prior.get("candidateId") != process.get("candidateId"))
        })
        .count()
}
struct Test {
    f: Fixture,
    old: J,
    next: J,
    request: J,
    os: Arc<Os>,
    maintenance: BootstrapMaintenance,
}
impl Test {
    async fn new() -> Self {
        let output = Command::new("node")
            .args(["-p", "process.execPath"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let executable = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        Self::with_configuration(
            "host.json".as_ref(),
            &executable,
            comparator(vec![
                DATE.into(),
                "executor-renamed".into(),
                "manager-renamed".into(),
            ]),
        )
        .await
    }
    async fn with_configuration(
        name: &std::ffi::OsStr,
        executable: &str,
        compare: ivy_host_runtime::artifact::FileCompare,
    ) -> Self {
        let mut f = Fixture::with_node(
            vec![
                instance("executor-renamed", "host-executor", true),
                instance("manager-renamed", "service-manager", true),
                instance("domain", "fixture-domain", true),
            ],
            executable,
        )
        .await;
        let config_path = f.root.0.join(name);
        f.config = host_config::checked_host_config(f.config.clone(), &config_path)
            .await
            .unwrap();
        config::atomic_json(&config_path, f.config.clone())
            .await
            .unwrap();
        f.journal.use_configuration(f.config.clone()).await.unwrap();
        let old = f.release("host-executor", "1.0.0", "maintenance-old").await;
        let next = f
            .release("host-executor", "2.0.0", "maintenance-next")
            .await;
        let manager = f
            .release("service-manager", "1.0.0", "maintenance-manager")
            .await;
        f.install("executor-renamed", &old).await;
        f.install("manager-renamed", &manager).await;
        let mut plans = Vec::new();
        for pair in [&old, &next] {
            let candidate = pair.required("candidate").unwrap();
            let components =
                bootstrap::bootstrap_components(&f.config, &f.journal, candidate, &[], &compare)
                    .await
                    .unwrap();
            plans.push(
                bootstrap::configuration_bootstrap_plan(
                    &f.config,
                    &node_path::from_native(&config_path),
                    candidate,
                    None,
                    &components,
                )
                .unwrap(),
            );
        }
        let old = plans.remove(0);
        let next = plans.remove(0);
        assert!(!array(next.required("processes").unwrap()).is_empty());
        let os = Arc::new(Os::new(owners(&old), f.config.clone(), config_path.clone()));
        let maintenance = BootstrapMaintenance::with_clock(
            node(),
            compare.clone(),
            os.clone(),
            job(),
            Arc::new(Fixed),
        );
        let request = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("operationId", "maintenance-1".into()),
            ("previous", maintenance.inspect(config_path).await.unwrap()),
            ("next", next.clone()),
        ]);
        validate_host("BootstrapMaintenanceRequest", &request).unwrap();
        os.state.lock().unwrap().inspections = 0;
        Self {
            f,
            old,
            next,
            request,
            os,
            maintenance,
        }
    }
    fn input(&self, mode: &str, steps: Vec<&str>) -> J {
        J::object([
            ("mode", mode.into()),
            ("steps", J::Array(steps.into_iter().map(J::from).collect())),
            ("at", J::Number(AT as f64)),
            ("config", self.f.config.clone()),
            ("configPath", p(&self.f.config_path())),
            ("distribution", p(&self.f.root.0)),
            ("pairs", J::Array(self.f.pairs.clone())),
            ("installed", J::Array(self.f.installed.clone())),
            ("oldPlan", self.old.clone()),
            ("nextPlan", self.next.clone()),
            ("request", self.request.clone()),
            ("owners", J::Array(owners(&self.old))),
        ])
    }
    async fn seed(&self) {
        bootstrap::retain_bootstrap_plan(&self.old, job())
            .await
            .unwrap();
        bootstrap::publish_bootstrap_instance_configurations(
            &self.old,
            &self.f.config,
            &self.f.journal,
        )
        .await
        .unwrap();
        config::atomic_json(
            self.f
                .root
                .0
                .join("state/bootstrap")
                .join(format!("{}.json", id(&self.old, "installationId"))),
            self.old.clone(),
        )
        .await
        .unwrap();
        self.f
            .journal
            .sync_bootstrap(self.old.clone(), false)
            .await
            .unwrap();
    }

    async fn output(&self, initial: J, results: Vec<J>, trace: Vec<J>, readiness: J) -> J {
        let mut files = Vec::new();
        for process in array(self.next.required("processes").unwrap()) {
            let path = node_path::to_native(
                process
                    .required("configPath")
                    .unwrap()
                    .as_js_string()
                    .unwrap(),
            );
            files.push(if path.exists() {
                config::json_file(path, config::CONFIGURATION_BYTES)
                    .await
                    .unwrap()
            } else {
                J::Null
            });
        }
        let mut installed = Vec::new();
        for instance in array(self.f.config.required("instances").unwrap()) {
            installed.push(
                self.f
                    .journal
                    .installed(&id(instance, "instanceId"))
                    .await
                    .unwrap()
                    .unwrap_or(J::Null),
            );
        }
        let selected = config::json_file(
            self.f
                .root
                .0
                .join("state/bootstrap")
                .join(format!("{}.json", id(&self.old, "installationId"))),
            config::CONFIGURATION_BYTES,
        )
        .await
        .unwrap();
        let s = self.os.state.lock().unwrap();
        J::object([
            ("initial", initial),
            ("results", J::Array(results)),
            ("trace", J::Array(trace)),
            ("readiness", readiness),
            ("files", J::Array(files)),
            ("installed", J::Array(installed)),
            ("selected", selected),
            ("writes", J::Number(s.writes as f64)),
            ("pauses", J::Number(s.pauses as f64)),
            ("resumes", J::Number(s.resumes as f64)),
        ])
    }
    async fn run(&self, input: &J) -> J {
        self.seed().await;
        let initial = self
            .maintenance
            .inspect(self.f.config_path())
            .await
            .unwrap();
        let mut results = Vec::new();
        let mut trace = Vec::new();
        let mut saved_owner = None;
        let mut saved_evidence = None;
        let evidence =
            bootstrap_maintenance::operation_root(&self.f.config, &"maintenance-1".into())
                .unwrap()
                .join(format!(
                    "{}.desired.json",
                    id(&array(input.required("owners").unwrap())[0], "instanceId")
                ));
        for step in array(input.required("steps").unwrap()) {
            match step.as_str().unwrap().as_str() {
                "pause" => {
                    self.os.pause(J::Null, vec![]).unwrap().await.unwrap();
                    continue;
                }
                "fail" => {
                    self.os.state.lock().unwrap().fail = true;
                    continue;
                }
                "forbid" => {
                    self.os.state.lock().unwrap().forbid = true;
                    continue;
                }
                "third" => {
                    let mut s = self.os.state.lock().unwrap();
                    let owner = s.owners.last_mut().unwrap();
                    saved_owner = Some(owner.clone());
                    let d = definition(format!(
                        "{}third",
                        id(owner.required("definition").unwrap(), "content")
                    ));
                    owner.insert("definitionHash", d.required("hash").unwrap().clone());
                    owner.insert("definition", d);
                    continue;
                }
                "repair-owner" => {
                    *self.os.state.lock().unwrap().owners.last_mut().unwrap() =
                        saved_owner.take().unwrap();
                    continue;
                }
                "tamper" => {
                    let text = fs::read_to_string(&evidence).unwrap();
                    fs::write(
                        &evidence,
                        text.replace("principal, triggers and settings", "tampered"),
                    )
                    .unwrap();
                    saved_evidence = Some(text);
                    continue;
                }
                "repair-evidence" => {
                    fs::write(&evidence, saved_evidence.take().unwrap()).unwrap();
                    continue;
                }
                "change-config" => {
                    self.os.state.lock().unwrap().change_config = true;
                    continue;
                }
                "restore-config" => {
                    config::atomic_json(self.f.config_path(), self.f.config.clone())
                        .await
                        .unwrap();
                    continue;
                }
                _ => {}
            }
            let result = match step.as_str().unwrap().as_str() {
                "update" => {
                    self.maintenance
                        .update(self.f.config_path(), self.request.clone())
                        .await
                }
                "conflict" => {
                    let mut r = self.request.clone();
                    r.insert("next", self.old.clone());
                    self.maintenance.update(self.f.config_path(), r).await
                }
                "later" => {
                    let mut r = self.request.clone();
                    r.insert("operationId", "maintenance-later".into());
                    r.insert(
                        "previous",
                        self.maintenance
                            .inspect(self.f.config_path())
                            .await
                            .unwrap(),
                    );
                    r.insert("next", self.old.clone());
                    self.maintenance.update(self.f.config_path(), r).await
                }
                "status" => {
                    bootstrap_maintenance::bootstrap_status(&self.f.config, &"maintenance-1".into())
                        .await
                }
                _ => panic!("Unknown test step"),
            };
            results.push(wrap(result));
            trace.push(self.os.trace());
        }
        self.output(initial, results, trace, J::Array(vec![])).await
    }
}
#[path = "host-bootstrap-worker-path-cases.rs"]
mod worker_path_cases;

#[tokio::test]
async fn bootstrap_maintenance_dropped_waiter_retains_transaction_locks_and_unrelated_owner() {
    let name = std::ffi::OsString::from("host-中-🚀");
    #[cfg(windows)]
    let name = {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut units: Vec<_> = name.encode_wide().collect();
        units.push(0xd800);
        std::ffi::OsString::from_wide(&units)
    };
    let mut name = name;
    name.push(".json");
    let executable = std::env::var("IVY_TEST_NODE").expect("Pinned Node path required");
    let t = Test::with_configuration(
        &name,
        &executable,
        Arc::new(|left, right| {
            assert_eq!(
                left, right,
                "Only equal fixture timestamps require comparison"
            );
            std::cmp::Ordering::Equal
        }),
    )
    .await;
    let config_path = t.f.root.0.join(&name);
    let config_bytes = fs::read(&config_path).unwrap();
    let context = node_path::Context::current().unwrap();
    let relative = context.relative(&context.cwd, &node_path::from_native(&t.f.root.0));
    let path = node_path::to_native(&relative)
        .join("uncreated/..")
        .join(&name);
    t.seed().await;
    let snapshot = t.maintenance.inspect(path.clone()).await.unwrap();
    assert_json_eq(&snapshot, t.request.required("previous").unwrap());
    assert_eq!(snapshot.get("configPath"), Some(&p(&config_path)));
    assert_eq!(
        snapshot.get("configurationHash"),
        Some(&hash_json(&t.f.config).unwrap().into())
    );
    t.os.pause(J::Null, vec![]).unwrap().await.unwrap();
    t.os.block.store(true, Ordering::SeqCst);
    let unrelated = ExecutorLock::acquire(t.f.root.0.join("state/owners/domain"), false)
        .await
        .unwrap();
    let m = t.maintenance.clone();
    let request = t.request.clone();
    let waiter = tokio::spawn(async move { m.update(path, request).await });
    tokio::time::timeout(Duration::from_secs(5), t.os.entered.notified())
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
            ExecutorLock::acquire(t.f.root.0.join("state").join(root), false)
                .await
                .err()
                .unwrap()
                .code,
            "executor_already_running"
        );
    }
    let db =
        ivy_store::rusqlite::Connection::open(t.f.root.0.join("state/deployments.sqlite")).unwrap();
    db.busy_timeout(Duration::from_millis(1)).unwrap();
    assert!(db.execute_batch("BEGIN IMMEDIATE").is_err());
    drop(db);
    t.os.released.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if bootstrap_maintenance::bootstrap_status(&t.f.config, &"maintenance-1".into())
                .await
                .unwrap()
                .get("phase")
                == Some(&"succeeded".into())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    unrelated.close().await.unwrap();
    let record = bootstrap_maintenance::bootstrap_status(&t.f.config, &"maintenance-1".into())
        .await
        .unwrap();
    validate_host("BootstrapMaintenanceRecord", &record).unwrap();
    assert_eq!(
        record.get("requestHash"),
        Some(&hash_json(&t.request).unwrap().into())
    );
    assert_json_eq(record.required("request").unwrap(), &t.request);
    assert_eq!(fs::read(&config_path).unwrap(), config_bytes);
    let lock = ExecutorLock::acquire(t.f.root.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
    t.f.journal.close().await.unwrap();
}
#[tokio::test]
async fn bootstrap_native_readiness_checks_build_timestamp_and_only_previously_running_owners() {
    let t = Test::new().await;
    t.seed().await;
    let readiness = NativeBootstrapReadiness::new(Arc::new(Fixed));
    let mut states = t.request.required("previous").unwrap().clone();
    let mut owners = array(states.required("owners").unwrap()).to_vec();
    for owner in &mut owners {
        owner.insert("running", false.into());
    }
    states.insert("owners", J::Array(owners));
    readiness
        .wait(t.f.config.clone(), t.old.clone(), states, AT, 0)
        .await
        .unwrap();
    let original = t.request.required("previous").unwrap().clone();
    assert_eq!(
        readiness
            .wait(t.f.config.clone(), t.old.clone(), original.clone(), AT, 0)
            .await
            .unwrap_err()
            .code,
        "readiness_failed"
    );
    for variant in ["wrong-build", "stale", "before-resume", "ready"] {
        for owner in array(original.required("owners").unwrap()) {
            let instance = array(t.f.config.required("instances").unwrap())
                .iter()
                .find(|p| p.get("instanceId") == owner.get("instanceId"))
                .unwrap();
            let paths = layout::service_paths(&t.f.config, instance).unwrap();
            let candidate =
                t.f.journal
                    .candidate(&id(owner, "candidateId"))
                    .await
                    .unwrap();
            let value = J::object([
                ("schemaVersion", J::Number(1.0)),
                (
                    "componentId",
                    owner.required("componentId").unwrap().clone(),
                ),
                ("instanceId", owner.required("instanceId").unwrap().clone()),
                (
                    "buildId",
                    if variant == "wrong-build" {
                        digest(b"wrong-build").into()
                    } else {
                        candidate.required("buildId").unwrap().clone()
                    },
                ),
                ("bootId", "boot-fixture".into()),
                ("pid", J::Number(1.0)),
                ("startedAt", DATE.into()),
                (
                    "observedAt",
                    if variant == "stale" {
                        "2025-10-09T08:53:00.000Z"
                    } else if variant == "before-resume" {
                        "2025-10-09T08:53:19.999Z"
                    } else {
                        DATE
                    }
                    .into(),
                ),
                ("ready", true.into()),
                ("generation", J::Null),
                ("details", "fixture health".into()),
            ]);
            validate_host("Health", &value).unwrap();
            config::atomic_json(
                node_path::to_native(paths.required("data").unwrap().as_js_string().unwrap())
                    .join("health.json"),
                value,
            )
            .await
            .unwrap();
        }
        let result = readiness
            .wait(t.f.config.clone(), t.old.clone(), original.clone(), AT, 0)
            .await;
        assert_eq!(result.is_ok(), variant == "ready");
    }
    t.f.journal.close().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn bootstrap_windows_adapter_renders_and_checks_retained_xml_without_scheduler_access() {
    use ivy_host_runtime::bootstrap_os::WindowsBootstrapOs;
    let t = Test::new().await;
    let assets = t.f.root.0.join("dist/packages/host-runtime/assets");
    fs::create_dir_all(&assets).unwrap();
    for name in ["bootstrap-maintenance-windows.ps1", "windows-principal.ps1"] {
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../assets")
                .join(name),
            assets.join(name),
        )
        .unwrap();
    }
    let xml = "<Task><RegistrationInfo><Description>prior</Description></RegistrationInfo><Principals><Principal><UserId>preserved-owner</UserId></Principal></Principals><Triggers><BootTrigger /></Triggers><Settings><Enabled>false</Enabled><ExecutionTimeLimit>PT0S</ExecutionTimeLimit></Settings><Actions><Exec><Command>old</Command><Arguments>old</Arguments><WorkingDirectory>old</WorkingDirectory></Exec></Actions></Task>";
    let prior = J::object([
        ("content", xml.into()),
        (
            "hash",
            digest(xml.replace("<Enabled>false</Enabled>", "").as_bytes()).into(),
        ),
    ]);
    let os = WindowsBootstrapOs::new(t.f.root.0.clone(), job());
    os.check(prior.clone()).await.unwrap();
    let owner = J::object([
        ("instanceId", "executor-renamed".into()),
        ("definition", prior),
    ]);
    let rendered = os.render(owner, t.next.clone()).await.unwrap();
    os.check(rendered.clone()).await.unwrap();
    let content = id(&rendered, "content");
    for retained in [
        "<UserId>preserved-owner</UserId>",
        "<BootTrigger />",
        "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
        "<Enabled>false</Enabled>",
        "--instance-config",
    ] {
        assert!(content.contains(retained), "{retained}");
    }
    assert!(content.contains(&id(&t.next, "candidateId")));
    let mut changed = rendered.clone();
    changed.insert(
        "content",
        content.replace("preserved-owner", "changed-owner").into(),
    );
    assert_eq!(os.check(changed).await.unwrap_err().code, "target_conflict");
    let mut enabled = rendered;
    enabled.insert(
        "content",
        content
            .replace("<Enabled>false</Enabled>", "<Enabled>true</Enabled>")
            .into(),
    );
    os.check(enabled.clone()).await.unwrap();
    let owner = J::object([
        ("instanceId", "executor-renamed".into()),
        ("definition", enabled),
    ]);
    assert_eq!(
        os.render(owner, t.next.clone()).await.unwrap_err().code,
        "target_conflict"
    );
    t.f.journal.close().await.unwrap();
}

#[tokio::test]
async fn bootstrap_offline_ownership_waits_for_exit_and_preserves_stop_fences() {
    use ivy_host_runtime::{private_backup::offline_bootstrap_owners, process_fence};
    let t = Test::new().await;
    let root = t.f.root.0.join("state");
    let ids = array(t.next.required("processes").unwrap())
        .iter()
        .map(|p| {
            p.required("instanceId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone()
        })
        .collect::<Vec<_>>();
    let prior = ExecutorLock::acquire(root.join("executor"), false)
        .await
        .unwrap();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        prior.close().await.unwrap();
    });
    let mut held =
        offline_bootstrap_owners(&t.f.config, &ids, Arc::new(ivy_sdk::clock::SystemClock))
            .await
            .unwrap();
    release.await.unwrap();
    assert_eq!(
        ExecutorLock::acquire(root.join("executor"), false)
            .await
            .err()
            .unwrap()
            .code,
        "executor_already_running"
    );
    held.close().await.unwrap();
    let instance = &array(t.f.config.required("instances").unwrap())[0];
    let data = node_path::to_native(
        layout::service_paths(&t.f.config, instance)
            .unwrap()
            .required("data")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    let fence = super::stop_fence();
    process_fence::record_process_stop_fence(&data, fence.clone())
        .await
        .unwrap();
    assert_eq!(
        offline_bootstrap_owners(&t.f.config, &ids, Arc::new(Fixed))
            .await
            .err()
            .unwrap()
            .code,
        "outcome_unknown"
    );
    assert_json_eq(
        &process_fence::process_stop_fence(data)
            .await
            .unwrap()
            .unwrap(),
        &fence,
    );
    for path in std::iter::once(root.join("executor")).chain(
        ids.iter()
            .map(|id| root.join("owners").join(node_path::to_native(id))),
    ) {
        let lock = ExecutorLock::acquire(path, false).await.unwrap();
        lock.close().await.unwrap();
    }
    t.f.journal.close().await.unwrap();
}
