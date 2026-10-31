use super::{ServiceFixture, host_configuration};
use futures_util::future::BoxFuture;
use ivy_contracts::{IvyJson as J, JsString, hash::hash_json, json};
use ivy_host_runtime::{
    IvyError, IvyResult, Outcome, bootstrap,
    bootstrap_restart::{
        BootstrapConfigurationRestarts, BootstrapRestartOs, RestartContext, restart_windows_owner,
    },
    config, filesystem, layout,
    linux_resources::installation_identity,
    node_path,
};
use ivy_sdk::clock::Clock;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
const AT: i64 = 1760000000000;
fn assert_json_eq(actual: &J, expected: &J) {
    assert_eq!(
        json::canonical(actual, usize::MAX).unwrap(),
        json::canonical(expected, usize::MAX).unwrap()
    );
}
fn path(value: &J) -> PathBuf {
    node_path::to_native(value.as_js_string().unwrap())
}
fn file_value(path: &Path) -> J {
    J::String(node_path::from_native(path))
}
fn wrap(value: IvyResult<J>) -> J {
    match value {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
struct FixedClock(AtomicI64);
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct NoActions;
impl ivy_host_runtime::executor::ActivationRuntime for NoActions {
    fn prepare<'a>(
        &'a self,
        _: &'a ivy_host_runtime::executor::OperationContext,
        _: &'a J,
        _: &'a str,
    ) -> ivy_host_runtime::executor::ExecutorFuture<'a, J> {
        Box::pin(async { panic!("No source preparation admitted") })
    }
    fn compatibility<'a>(
        &'a self,
        _: &'a ivy_host_runtime::executor::OperationContext,
        _: &'a str,
        _: Option<J>,
    ) -> ivy_host_runtime::executor::ExecutorFuture<'a, ()> {
        Box::pin(async { panic!("No deployment admitted") })
    }
    fn reconcile<'a>(
        &'a self,
        _: &'a ivy_host_runtime::executor::OperationContext,
        _: Option<J>,
        _: bool,
    ) -> ivy_host_runtime::executor::ExecutorFuture<'a, ()> {
        Box::pin(async { panic!("No OS activation admitted") })
    }
    fn dispose(&self) -> ivy_host_runtime::executor::ExecutorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Os {
    states: Vec<J>,
    index: AtomicUsize,
    events: Arc<Mutex<Vec<J>>>,
    clock: Arc<FixedClock>,
    fail: Option<J>,
    entered: Arc<tokio::sync::Notify>,
    released: Arc<tokio::sync::Notify>,
    block: bool,
}
impl Os {
    fn new(input: &J, clock: Arc<FixedClock>) -> Self {
        Self {
            states: input
                .required("states")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec(),
            index: AtomicUsize::new(0),
            events: Arc::new(Mutex::new(Vec::new())),
            clock,
            fail: input.get("failAction").cloned(),
            entered: Arc::new(tokio::sync::Notify::new()),
            released: Arc::new(tokio::sync::Notify::new()),
            block: false,
        }
    }
}
impl BootstrapRestartOs for Os {
    fn inspect(&self, plans: Vec<J>, _: PathBuf) -> BoxFuture<'static, IvyResult<Vec<J>>> {
        self.events.lock().unwrap().push(J::object([
            ("kind", "inspect".into()),
            ("plans", J::Array(plans)),
        ]));
        let state = self.states[self
            .index
            .fetch_add(1, Ordering::SeqCst)
            .min(self.states.len() - 1)]
        .clone();
        self.clock.0.fetch_add(
            state.get("advance").and_then(J::as_f64).unwrap_or(0.0) as i64,
            Ordering::SeqCst,
        );
        Box::pin(async move {
            if let Some(code) = state.get("error") {
                return Err(IvyError::new(
                    &code.as_str()?,
                    "Fixture owner inspection failed.",
                ));
            }
            Ok(state.required("owners")?.as_array().unwrap().to_vec())
        })
    }
    fn shutdown(
        &self,
        owner: J,
        _: Vec<J>,
        distribution: PathBuf,
    ) -> BoxFuture<'static, IvyResult<()>> {
        self.command(
            "shutdown",
            owner
                .required("name")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            distribution,
        )
    }
    fn command(
        &self,
        action: &'static str,
        name: JsString,
        _: PathBuf,
    ) -> BoxFuture<'static, IvyResult<()>> {
        self.events.lock().unwrap().push(J::object([
            ("kind", "command".into()),
            ("action", action.into()),
            ("name", J::String(name)),
        ]));
        let failed = self.fail == Some(action.into());
        let (entered, released, block) = (self.entered.clone(), self.released.clone(), self.block);
        Box::pin(async move {
            entered.notify_one();
            if block {
                released.notified().await;
            }
            if failed {
                let mut error = IvyError::new(
                    "bootstrap_os_unavailable",
                    "The selected Windows bootstrap task could not be restarted.",
                );
                error.outcome = Outcome::Unknown.into();
                Err(error)
            } else {
                Ok(())
            }
        })
    }
}
fn owner_state(name: &J, enabled: bool, running: bool) -> J {
    J::object([(
        "owners",
        J::Array(vec![J::object([
            ("name", name.clone()),
            ("enabled", enabled.into()),
            ("running", running.into()),
        ])]),
    )])
}
fn stamp(at: i64) -> J {
    chrono::DateTime::from_timestamp_millis(at)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .into()
}
struct Fixture {
    root: ServiceFixture,
    host: J,
    plan: J,
    configs: Vec<J>,
    health: Vec<Option<J>>,
    seed_marker: Option<J>,
    marker_path: PathBuf,
    fractional_mtime: bool,
}
impl Fixture {
    fn new(manager: bool) -> Self {
        let root = ServiceFixture::new();
        let mut host = host_configuration(&root);
        host.insert("configPath", file_value(&root.0.join("host.json")));
        let mut instances = vec![J::object([
            ("instanceId", "renamed-executor".into()),
            ("componentId", "host-executor".into()),
            ("serviceNodeId", "own.executor".into()),
            ("enabled", true.into()),
            ("engine", "process".into()),
            ("credential", "old-fixture-token".into()),
            (
                "settings",
                J::object([(
                    "hostConfigPath",
                    host.required("configPath").unwrap().clone(),
                )]),
            ),
        ])];
        if manager {
            let mut instance = instances[0].clone();
            instance.insert("instanceId", "renamed-manager".into());
            instance.insert("componentId", "service-manager".into());
            instance.insert("serviceNodeId", "own.manager".into());
            instances.push(instance);
        }
        host.insert("instances", J::Array(instances.clone()));
        let identity = installation_identity(&host).unwrap();
        let mut owners = Vec::new();
        let mut configs = Vec::new();
        let mut health = Vec::new();
        for instance in instances {
            let id = instance.required("instanceId").unwrap();
            let paths = layout::service_paths(&host, &instance).unwrap();
            let build: J = format!(
                "sha256:{}",
                if instance.required("componentId").unwrap() == &J::from("host-executor") {
                    "a"
                } else {
                    "b"
                }
                .repeat(64)
            )
            .into();
            let artifact: J = file_value(&root.0.join(id.as_str().unwrap()).join("release"));
            owners.push(J::object([
                ("instanceId", id.clone()),
                (
                    "componentId",
                    instance.required("componentId").unwrap().clone(),
                ),
                (
                    "name",
                    format!("{identity}-{}", &hash_json(id).unwrap()[7..19]).into(),
                ),
                ("candidateId", build.clone()),
                ("artifactRoot", artifact.clone()),
                (
                    "configPath",
                    file_value(
                        &root
                            .0
                            .join("state/bootstrap")
                            .join(&identity)
                            .join("instances")
                            .join(format!("{}.json", id.as_str().unwrap())),
                    ),
                ),
            ]));
            configs.push(J::object([
                ("schemaVersion", J::Number(1.0)),
                ("hostId", host.required("hostId").unwrap().clone()),
                ("instanceId", id.clone()),
                (
                    "componentId",
                    instance.required("componentId").unwrap().clone(),
                ),
                (
                    "serviceNodeId",
                    instance.required("serviceNodeId").unwrap().clone(),
                ),
                (
                    "publicBaseUrl",
                    host.required("publicBaseUrl").unwrap().clone(),
                ),
                ("settings", instance.required("settings").unwrap().clone()),
                (
                    "credential",
                    instance.required("credential").unwrap().clone(),
                ),
                ("buildId", build.clone()),
                ("version", "1.2.3".into()),
                ("artifactRoot", artifact),
                ("dataRoot", paths.required("data").unwrap().clone()),
                ("workRoot", paths.required("work").unwrap().clone()),
                ("logsRoot", paths.required("logs").unwrap().clone()),
            ]));
            health.push(Some(J::object([
                ("schemaVersion", J::Number(1.0)),
                ("instanceId", id.clone()),
                (
                    "componentId",
                    instance.required("componentId").unwrap().clone(),
                ),
                ("buildId", build),
                ("pid", J::Number(123.0)),
                ("bootId", "old-boot".into()),
                ("ready", true.into()),
                ("startedAt", stamp(AT - 60000)),
                ("observedAt", stamp(AT)),
                ("generation", J::Null),
                ("details", "".into()),
            ])));
        }
        let plan = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("os", "win32".into()),
            ("hostId", host.required("hostId").unwrap().clone()),
            ("installationId", identity.into()),
            ("candidateId", format!("sha256:{}", "a".repeat(64)).into()),
            ("artifactRoot", file_value(&root.0.join("release"))),
            ("configPath", host.required("configPath").unwrap().clone()),
            ("configHash", hash_json(&host).unwrap().into()),
            (
                "nodeExecutable",
                host.required("executables")
                    .unwrap()
                    .required("node")
                    .unwrap()
                    .clone(),
            ),
            ("runtimeRoot", host.required("runtimeRoot").unwrap().clone()),
            ("processes", J::Array(owners)),
        ]);
        ivy_contracts::core_schema::validate_host("BootstrapPlan", &plan).unwrap();
        ivy_contracts::core_schema::validate_host("HostConfig", &host).unwrap();
        for value in &configs {
            ivy_contracts::core_schema::validate_host("InstanceConfig", value).unwrap();
        }
        for value in health.iter().flatten() {
            ivy_contracts::core_schema::validate_host("Health", value).unwrap();
        }
        let marker_path = root.0.join("state/bootstrap/restart-renamed-manager.json");
        Self {
            root,
            host,
            plan,
            configs,
            health,
            seed_marker: None,
            marker_path,
            fractional_mtime: false,
        }
    }
    fn selected(&self) -> PathBuf {
        self.root.0.join("state/bootstrap").join(format!(
            "{}.json",
            installation_identity(&self.host).unwrap()
        ))
    }
    fn config_paths(&self) -> Vec<PathBuf> {
        self.plan
            .required("processes")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|owner| owner.get("configPath").map(path))
            .collect()
    }
    fn marker(&self) -> PathBuf {
        self.marker_path.clone()
    }
    async fn setup(&self) {
        let _ = fs::remove_dir_all(self.root.0.join("state"));
        config::atomic_json(self.selected(), self.plan.clone())
            .await
            .unwrap();
        for (index, config_path) in self.config_paths().iter().enumerate() {
            config::atomic_json(config_path, self.configs[index].clone())
                .await
                .unwrap();
            filesystem::set_times_ms(config_path, AT - 1000, AT - 1000).unwrap();
            if self.fractional_mtime {
                fs::OpenOptions::new()
                    .write(true)
                    .open(config_path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(
                        std::time::UNIX_EPOCH
                            + Duration::from_millis((AT - 1000) as u64)
                            + Duration::from_nanos(123400),
                    ))
                    .unwrap();
            }
            let health_path =
                path(self.configs[index].required("dataRoot").unwrap()).join("health.json");
            if let Some(health) = &self.health[index] {
                config::atomic_json(health_path, health.clone())
                    .await
                    .unwrap();
            }
        }
        if let Some(marker) = &self.seed_marker {
            config::atomic_json(self.marker(), marker.clone())
                .await
                .unwrap();
        }
    }
    fn input(&self) -> J {
        let owners = self.plan.required("processes").unwrap().as_array().unwrap();
        let name = owners.last().unwrap().required("name").unwrap();
        J::object([
            ("kind", "reconcile".into()),
            ("config", self.host.clone()),
            ("own", "renamed-executor".into()),
            ("platform", "win32".into()),
            ("startedAt", J::Number((AT - 2000) as f64)),
            ("at", J::Number(AT as f64)),
            ("distribution", file_value(&self.root.0)),
            (
                "configFiles",
                J::Array(
                    self.config_paths()
                        .iter()
                        .map(|path| file_value(path))
                        .collect(),
                ),
            ),
            ("markerPath", file_value(&self.marker())),
            ("states", J::Array(vec![owner_state(name, true, false)])),
        ])
    }
}
async fn actual(f: &Fixture, input: &J, os: Arc<Os>, clock: Arc<FixedClock>) -> J {
    let restarts = BootstrapConfigurationRestarts::with_os(
        RestartContext {
            own_instance_id: input.get("own").and_then(J::as_js_string).cloned(),
            distribution: input.get("distribution").filter(|v| !v.is_null()).map(path),
            process_started_at: input.required("startedAt").unwrap().as_f64().unwrap(),
            platform: input.required("platform").unwrap().as_str().unwrap(),
        },
        clock,
        os.clone(),
    );
    let mut results = Vec::new();
    for _ in 0..input.get("times").and_then(J::as_f64).unwrap_or(1.0) as usize {
        results.push(wrap(restarts.reconcile(f.host.clone()).await.map(J::from)));
    }
    let mut files = Vec::new();
    for file in f.config_paths() {
        files.push(
            config::json_file(file, config::CONFIGURATION_BYTES)
                .await
                .unwrap(),
        );
    }
    wrap(Ok(J::object([
        ("results", J::Array(results)),
        ("files", J::Array(files)),
        (
            "marker",
            config::json_file_if_present(f.marker(), config::CONFIGURATION_BYTES)
                .await
                .unwrap()
                .unwrap_or(J::Null),
        ),
        ("events", J::Array(os.events.lock().unwrap().clone())),
    ])))
}

#[tokio::test]
async fn bootstrap_restarts_match_identity_health_markers_and_current_files() {
    for mode in [
        "own-request",
        "new-boot",
        "process-newer",
        "other-instance",
        "missing-health",
        "null-health",
        "false-health",
        "zero-health",
        "empty-health",
        "no-distribution",
        "linux",
        "stale-manager",
        "future-manager",
        "manager-once",
        "manager-running",
        "disabled-manager",
        "inspect-failure",
        "shutdown-failure",
        "run-failure",
        "changed-settings",
        "remove-credential",
        "immutable-mismatch",
        "health-mismatch",
        "missing-owner",
        "missing-release",
        "marker-same",
        "marker-expired",
        "marker-new-boot",
        "marker-changed-file",
        "marker-invalid-date",
        "marker-number-date",
        "marker-array-date",
        "marker-object-date",
        "fractional-mtime",
        "native-paths",
    ] {
        let mut f = Fixture::new(
            ![
                "own-request",
                "new-boot",
                "process-newer",
                "other-instance",
                "missing-health",
                "null-health",
                "false-health",
                "zero-health",
                "empty-health",
                "changed-settings",
                "remove-credential",
                "immutable-mismatch",
                "health-mismatch",
                "missing-owner",
                "missing-release",
            ]
            .contains(&mode),
        );
        let mut input = f.input();
        match mode {
            "native-paths" => {
                let name = std::ffi::OsString::from("manager-中-🚀-");
                #[cfg(windows)]
                let name = {
                    use std::os::windows::ffi::{OsStrExt, OsStringExt};
                    let mut units: Vec<_> = name.encode_wide().collect();
                    units.push(0xd800);
                    std::ffi::OsString::from_wide(&units)
                };
                let id = file_value(&PathBuf::from(&name));
                let mut instances = f
                    .host
                    .required("instances")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .to_vec();
                instances[1].insert("instanceId", id.clone());
                let paths = layout::service_paths(&f.host, &instances[1]).unwrap();
                f.host.insert("instances", J::Array(instances));
                f.configs[1].insert("instanceId", id.clone());
                for (key, area) in [
                    ("dataRoot", "data"),
                    ("workRoot", "work"),
                    ("logsRoot", "logs"),
                ] {
                    f.configs[1].insert(key, paths.required(area).unwrap().clone());
                }
                f.health[1]
                    .as_mut()
                    .unwrap()
                    .insert("instanceId", id.clone());
                let mut filename = name.clone();
                filename.push(".json");
                let config_path = f.config_paths()[1].parent().unwrap().join(filename);
                let mut owners = f
                    .plan
                    .required("processes")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .to_vec();
                owners[1].insert("instanceId", id.clone());
                owners[1].insert(
                    "name",
                    format!(
                        "{}-{}",
                        installation_identity(&f.host).unwrap(),
                        &hash_json(&id).unwrap()[7..19]
                    )
                    .into(),
                );
                owners[1].insert("configPath", file_value(&config_path));
                f.plan.insert("processes", J::Array(owners));
                f.plan
                    .insert("configHash", hash_json(&f.host).unwrap().into());
                let mut marker = std::ffi::OsString::from("restart-");
                marker.push(name);
                marker.push(".json");
                f.marker_path = f.root.0.join("state/bootstrap").join(marker);
                input = f.input();
                input.insert("times", J::Number(2.0));
            }
            "new-boot" => f.health[0].as_mut().unwrap().insert("startedAt", stamp(AT)),
            "process-newer" => input.insert("startedAt", J::Number(AT as f64)),
            "other-instance" => input.insert("own", "not-this-executor".into()),
            "missing-health" => f.health[0] = None,
            "null-health" => f.health[0] = Some(J::Null),
            "false-health" => f.health[0] = Some(false.into()),
            "zero-health" => f.health[0] = Some(J::Number(0.0)),
            "empty-health" => f.health[0] = Some("".into()),
            "no-distribution" => input.remove("distribution").map(|_| ()).unwrap(),
            "linux" => input.insert("platform", "linux".into()),
            "stale-manager" => f.health[1]
                .as_mut()
                .unwrap()
                .insert("observedAt", stamp(AT - 15000)),
            "future-manager" => f.health[1]
                .as_mut()
                .unwrap()
                .insert("observedAt", stamp(AT + 30000)),
            "manager-once" => input.insert("times", J::Number(2.0)),
            "manager-running" | "shutdown-failure" => {
                let name = f.plan.required("processes").unwrap().as_array().unwrap()[1]
                    .required("name")
                    .unwrap();
                input.insert(
                    "states",
                    J::Array(vec![
                        owner_state(name, true, true),
                        owner_state(name, true, false),
                    ]),
                );
                if mode == "shutdown-failure" {
                    input.insert("failAction", "shutdown".into());
                }
            }
            "disabled-manager" => {
                let name = f.plan.required("processes").unwrap().as_array().unwrap()[1]
                    .required("name")
                    .unwrap();
                input.insert("states", J::Array(vec![owner_state(name, false, true)]));
            }
            "inspect-failure" => input.insert(
                "states",
                J::Array(vec![J::object([("error", "target_conflict".into())])]),
            ),
            "run-failure" => input.insert("failAction", "/Run".into()),
            "changed-settings" | "remove-credential" => {
                let mut instances = f
                    .host
                    .required("instances")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .to_vec();
                if mode == "remove-credential" {
                    instances[0].remove("credential");
                } else {
                    instances[0].insert("credential", "rotated-fixture-token".into());
                    instances[0].insert(
                        "settings",
                        J::object([("text", J::String(JsString(vec![0xd800, 120])))]),
                    );
                }
                f.host.insert("instances", J::Array(instances));
                f.health[0] = None;
            }
            "immutable-mismatch" => {
                f.configs[0].insert("buildId", format!("sha256:{}", "c".repeat(64)).into())
            }
            "health-mismatch" => f.health[0]
                .as_mut()
                .unwrap()
                .insert("instanceId", "wrong-instance".into()),
            "missing-owner" => f.host.insert("instances", J::Array(Vec::new())),
            "missing-release" => {
                let mut owners = f
                    .plan
                    .required("processes")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .to_vec();
                owners[0].remove("artifactRoot");
                f.plan.insert("processes", J::Array(owners));
            }
            "fractional-mtime" => f.fractional_mtime = true,
            mode if mode.starts_with("marker-") => {
                let mut marker = J::object([
                    ("instanceId", "renamed-manager".into()),
                    ("configWrittenAt", J::Number((AT - 1000) as f64)),
                    ("priorBootId", "old-boot".into()),
                    ("requestedAt", stamp(AT - 1000)),
                ]);
                match mode {
                    "marker-expired" => marker.insert("requestedAt", stamp(AT - 60000)),
                    "marker-new-boot" => marker.insert("priorBootId", "other-boot".into()),
                    "marker-changed-file" => {
                        marker.insert("configWrittenAt", J::Number((AT - 1001) as f64))
                    }
                    "marker-invalid-date" => marker.insert("requestedAt", "not-a-date".into()),
                    "marker-number-date" => marker.insert("requestedAt", J::Number(2026.0)),
                    "marker-array-date" => {
                        marker.insert("requestedAt", J::Array(vec![J::Number(2026.0)]))
                    }
                    "marker-object-date" => {
                        marker.insert("requestedAt", J::object([("invalid", false.into())]))
                    }
                    _ => (),
                }
                f.seed_marker = Some(marker);
            }
            _ => (),
        }
        input.insert("config", f.host.clone());
        f.setup().await;
        let native_config =
            (mode == "native-paths").then(|| fs::read(&f.config_paths()[1]).unwrap());
        let clock = Arc::new(FixedClock(AtomicI64::new(AT)));
        let os = Arc::new(Os::new(&input, clock.clone()));
        let observed = actual(&f, &input, os, clock).await;
        let result = &observed
            .required("value")
            .unwrap()
            .required("results")
            .unwrap()
            .as_array()
            .unwrap()[0];
        let code = match mode {
            "immutable-mismatch" | "health-mismatch" | "missing-owner" | "missing-release"
            | "disabled-manager" | "inspect-failure" => Some("target_conflict"),
            "shutdown-failure" | "run-failure" => Some("bootstrap_os_unavailable"),
            "false-health" | "zero-health" | "empty-health" => Some("invalid_arguments"),
            _ => None,
        };
        if let Some(code) = code {
            assert_eq!(
                result
                    .required("error")
                    .unwrap()
                    .required("data")
                    .unwrap()
                    .required("code")
                    .unwrap(),
                &J::from(code),
                "{mode}"
            );
        } else {
            let expected = ![
                "new-boot",
                "process-newer",
                "other-instance",
                "missing-health",
                "null-health",
                "changed-settings",
                "remove-credential",
            ]
            .contains(&mode);
            assert_eq!(
                result.required("value").unwrap(),
                &J::Bool(expected),
                "{mode}"
            );
        }
        let commands = observed
            .required("value")
            .unwrap()
            .required("events")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event.get("kind") == Some(&J::from("command")))
            .count();
        let expected_commands = match mode {
            "manager-running" => 2,
            "future-manager"
            | "manager-once"
            | "shutdown-failure"
            | "run-failure"
            | "marker-expired"
            | "marker-new-boot"
            | "marker-changed-file"
            | "marker-invalid-date"
            | "marker-number-date"
            | "marker-array-date"
            | "marker-object-date"
            | "fractional-mtime"
            | "native-paths" => 1,
            _ => 0,
        };
        assert_eq!(commands, expected_commands, "{mode}");
        if let Some(bytes) = native_config {
            assert_eq!(fs::read(&f.config_paths()[1]).unwrap(), bytes);
            assert_json_eq(
                observed
                    .required("value")
                    .unwrap()
                    .required("marker")
                    .unwrap(),
                &J::object([
                    (
                        "instanceId",
                        f.configs[1].required("instanceId").unwrap().clone(),
                    ),
                    ("configWrittenAt", J::Number((AT - 1000) as f64)),
                    ("priorBootId", "old-boot".into()),
                    ("requestedAt", stamp(AT)),
                ]),
            );
            assert!(f.marker().is_file());
        }
        if mode == "changed-settings" {
            assert_eq!(
                observed
                    .required("value")
                    .unwrap()
                    .required("files")
                    .unwrap()
                    .as_array()
                    .unwrap()[0]
                    .required("credential")
                    .unwrap(),
                &J::from("rotated-fixture-token")
            );
        }
        if mode == "remove-credential" {
            assert!(
                observed
                    .required("value")
                    .unwrap()
                    .required("files")
                    .unwrap()
                    .as_array()
                    .unwrap()[0]
                    .get("credential")
                    .is_none()
            );
        }
    }
}

#[tokio::test]
async fn bootstrap_retained_plans_enforce_bounded_authority_and_pending_files() {
    for mode in [
        "selected",
        "none",
        "duplicate",
        "pending",
        "invalid-entry",
        "entry-directory",
        "wrong-selected-installation",
        "wrong-history-installation",
        "wrong-history-hash",
        "stale-schema",
        "malformed-json",
        "oversized-selected",
        "too-many",
    ] {
        let f = Fixture::new(false);
        f.setup().await;
        let root = f.root.0.join("state/bootstrap-plans");
        fs::create_dir(&root).unwrap();
        let historical = root.join(format!("{}.json", &hash_json(&f.plan).unwrap()[7..]));
        match mode {
            "none" => fs::remove_file(f.selected()).unwrap(),
            "duplicate" => {
                config::atomic_json(historical, f.plan.clone())
                    .await
                    .unwrap();
            }
            "pending" => {
                fs::write(
                    root.join(format!(
                        "{}.json.{}.tmp",
                        "a".repeat(64),
                        uuid::Uuid::new_v4()
                    )),
                    b"{partial",
                )
                .unwrap();
            }
            "invalid-entry" => fs::write(root.join("unexpected"), b"{}").unwrap(),
            "entry-directory" => fs::create_dir(historical).unwrap(),
            "wrong-selected-installation" | "wrong-history-installation" => {
                let mut plan = f.plan.clone();
                plan.insert("hostId", "different".into());
                if mode == "wrong-selected-installation" {
                    config::atomic_json(f.selected(), plan).await.unwrap();
                } else {
                    config::atomic_json(
                        root.join(format!("{}.json", &hash_json(&plan).unwrap()[7..])),
                        plan,
                    )
                    .await
                    .unwrap();
                }
            }
            "wrong-history-hash" => config::atomic_json(
                root.join(format!("{}.json", "c".repeat(64))),
                f.plan.clone(),
            )
            .await
            .unwrap(),
            "stale-schema" => config::atomic_json(f.selected(), J::Null).await.unwrap(),
            "malformed-json" => fs::write(f.selected(), b"{bad").unwrap(),
            "oversized-selected" => fs::write(f.selected(), vec![b' '; 65537]).unwrap(),
            "too-many" => {
                for n in 0..1001 {
                    fs::write(root.join(format!("{n:064x}.json")), b"null").unwrap();
                }
            }
            _ => (),
        }
        let observed = wrap(
            bootstrap::retained_bootstrap_plans(f.host.clone())
                .await
                .map(J::Array),
        );
        if [
            "selected",
            "duplicate",
            "pending",
            "wrong-history-installation",
            "wrong-history-hash",
        ]
        .contains(&mode)
        {
            assert_eq!(
                observed
                    .required("value")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                1,
                "{mode}"
            );
        } else if [
            "none",
            "wrong-selected-installation",
            "stale-schema",
            "oversized-selected",
        ]
        .contains(&mode)
        {
            assert!(
                observed
                    .required("value")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "{mode}"
            );
        } else {
            assert_eq!(observed.get("ok"), Some(&J::Bool(false)), "{mode}");
            if mode != "malformed-json" {
                assert_eq!(
                    observed
                        .required("error")
                        .unwrap()
                        .required("data")
                        .unwrap()
                        .get("code"),
                    Some(&J::from("storage_invalid")),
                    "{mode}"
                );
            }
        }
    }
}

#[tokio::test]
async fn bootstrap_windows_restart_orders_stop_observe_start_and_preserves_unknown_outcomes() {
    for mode in [
        "stopped",
        "running",
        "wait-once",
        "deadline",
        "missing",
        "disabled",
        "shutdown-failure",
        "run-failure",
        "inspect-failure",
        "invalid-enabled",
        "invalid-running",
        "invalid-stopped",
    ] {
        let f = Fixture::new(false);
        let name: J = "owned-task".into();
        let mut states = vec![
            owner_state(&name, true, true),
            owner_state(&name, true, false),
        ];
        if mode == "stopped" {
            states[0] = owner_state(&name, true, false);
        }
        if mode == "wait-once" {
            states.insert(1, owner_state(&name, true, true));
        }
        if mode == "deadline" {
            let mut state = owner_state(&name, true, true);
            state.insert("advance", J::Number(30000.0));
            states[1] = state;
        }
        if mode == "missing" {
            states[0] = J::object([("owners", J::Array(Vec::new()))]);
        }
        if mode == "disabled" {
            states[0] = owner_state(&name, false, true);
        }
        if mode == "inspect-failure" {
            states[0] = J::object([("error", "target_conflict".into())]);
        }
        if ["invalid-enabled", "invalid-running", "invalid-stopped"].contains(&mode) {
            let index = usize::from(mode == "invalid-stopped");
            let mut owners = states[index]
                .required("owners")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec();
            owners[0].insert(
                if mode == "invalid-enabled" {
                    "enabled"
                } else {
                    "running"
                },
                J::Number(if mode == "invalid-enabled" { 1.0 } else { 0.0 }),
            );
            states[index].insert("owners", J::Array(owners));
        }
        let mut input = J::object([
            ("kind", "windows".into()),
            ("at", J::Number(AT as f64)),
            ("plans", J::Array(vec![f.plan.clone()])),
            ("name", name.clone()),
            ("states", J::Array(states)),
            ("distribution", file_value(&f.root.0)),
        ]);
        if mode == "shutdown-failure" {
            input.insert("failAction", "shutdown".into());
        }
        if mode == "run-failure" {
            input.insert("failAction", "/Run".into());
        }
        let clock = Arc::new(FixedClock(AtomicI64::new(AT)));
        let os = Os::new(&input, clock.clone());
        let observed = wrap(
            restart_windows_owner(
                vec![f.plan.clone()],
                name.as_js_string().unwrap().clone(),
                f.root.0.clone(),
                clock.as_ref(),
                &os,
            )
            .await
            .map(|()| J::Array(os.events.lock().unwrap().clone())),
        );
        let code = match mode {
            "deadline" => Some("outcome_unknown"),
            "missing" | "disabled" | "inspect-failure" | "invalid-enabled" => {
                Some("target_conflict")
            }
            "invalid-running" | "invalid-stopped" | "shutdown-failure" | "run-failure" => {
                Some("bootstrap_os_unavailable")
            }
            _ => None,
        };
        if let Some(code) = code {
            assert_eq!(
                observed
                    .required("error")
                    .unwrap()
                    .required("data")
                    .unwrap()
                    .get("code"),
                Some(&J::from(code)),
                "{mode}"
            );
        } else {
            assert_eq!(observed.get("ok"), Some(&J::Bool(true)), "{mode}");
        }
        let events = os.events.lock().unwrap();
        let commands = events
            .iter()
            .filter_map(|event| event.get("action").cloned())
            .collect::<Vec<_>>();
        let expected: &[&str] = match mode {
            "stopped" => &["/Run"],
            "running" | "wait-once" | "run-failure" => &["shutdown", "/Run"],
            "deadline" | "shutdown-failure" | "invalid-stopped" => &["shutdown"],
            _ => &[],
        };
        assert_eq!(
            commands,
            expected
                .iter()
                .map(|value| J::from(*value))
                .collect::<Vec<_>>(),
            "{mode}"
        );
        let inspections = events
            .iter()
            .filter(|event| event.get("kind") == Some(&J::from("inspect")))
            .count();
        assert_eq!(
            inspections,
            match mode {
                "wait-once" => 3,
                "missing" | "disabled" | "inspect-failure" | "shutdown-failure"
                | "invalid-enabled" | "invalid-running" => 1,
                _ => 2,
            },
            "{mode}"
        );
    }
}

#[tokio::test]
async fn bootstrap_restart_marker_precedes_signal_and_dropped_waiter_keeps_owned_reconciliation() {
    let f = Fixture::new(true);
    f.setup().await;
    let input = f.input();
    let clock = Arc::new(FixedClock(AtomicI64::new(AT)));
    let mut os = Os::new(&input, clock.clone());
    os.block = true;
    let os = Arc::new(os);
    let owner = BootstrapConfigurationRestarts::with_os(
        RestartContext {
            own_instance_id: Some("renamed-executor".into()),
            distribution: Some(f.root.0.clone()),
            process_started_at: (AT - 2000) as f64,
            platform: "win32".into(),
        },
        clock,
        os.clone(),
    );
    let work = owner.clone();
    let host = f.host.clone();
    let waiter = tokio::spawn(async move { work.reconcile(host).await });
    tokio::time::timeout(Duration::from_secs(5), os.entered.notified())
        .await
        .unwrap();
    let marker = config::json_file(f.marker(), config::CONFIGURATION_BYTES)
        .await
        .unwrap();
    assert_eq!(
        marker.required("priorBootId").unwrap(),
        &J::from("old-boot")
    );
    assert_eq!(
        marker.required("configWrittenAt").unwrap(),
        &J::Number((AT - 1000) as f64)
    );
    waiter.abort();
    os.released.notify_one();
    let following = tokio::time::timeout(Duration::from_secs(5), owner.reconcile(f.host.clone()))
        .await
        .unwrap()
        .unwrap();
    assert!(following);
    assert_eq!(
        os.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.get("kind") == Some(&J::from("command")))
            .count(),
        1
    );
}

#[cfg(windows)]
#[tokio::test]
async fn bootstrap_restart_native_powershell_adapter_owns_utf8_stdin_eof_and_rejected_pipes() {
    use ivy_host_runtime::bootstrap_restart::NativeBootstrapRestartOs;
    let mut f = Fixture::new(false);
    let mut owners = f
        .plan
        .required("processes")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    owners[0].insert("name", "fixture-owner-é-📦".into());
    f.plan.insert("processes", J::Array(owners));
    let script = f
        .root
        .0
        .join("dist/packages/host-runtime/assets/bootstrap-maintenance-windows.ps1");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    let capture = script.parent().unwrap().join("request.json");
    let os = NativeBootstrapRestartOs::new(PathBuf::from(env!("CARGO_BIN_EXE_ivy-job")));
    for mode in [
        "request",
        "early-eof",
        "domain-error",
        "invalid-response",
        "string-success",
        "number-success",
        "missing-success",
        "exit-error",
        "buffer-limit",
    ] {
        let body = match mode {
            "request" => {
                r#"$request = [Console]::In.ReadToEnd()
[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'request.json'), $request, [Text.UTF8Encoding]::new($false))
$data = $request | ConvertFrom-Json
@{ok=$true;value=@(@{name=$data.data.plans[0].processes[0].name;enabled=$true;running=$false})} | ConvertTo-Json -Depth 100 -Compress"#
            }
            "early-eof" => r#"[Console]::In.Close(); '{"ok":true,"value":[]}'"#,
            "domain-error" => {
                r#"[Console]::In.ReadToEnd() | Out-Null; '{"ok":false,"code":"target_conflict","message":"Fixture owner differs."}'"#
            }
            "invalid-response" => r#"[Console]::In.ReadToEnd() | Out-Null; 'invalid-json'"#,
            "string-success" => {
                r#"[Console]::In.ReadToEnd() | Out-Null; '{"ok":"true","value":[]}'"#
            }
            "number-success" => r#"[Console]::In.ReadToEnd() | Out-Null; '{"ok":1,"value":[]}'"#,
            "missing-success" => r#"[Console]::In.ReadToEnd() | Out-Null; '{"value":[]}'"#,
            "exit-error" => "exit 1",
            _ => "[Console]::In.ReadToEnd() | Out-Null; [Console]::Out.Write(('x' * 2097153))",
        };
        let prefix = "[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)\n[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)\n";
        fs::write(&script, format!("{prefix}{body}\n")).unwrap();
        let mut plan = f.plan.clone();
        plan.insert("fixtureUtf16", J::String(JsString(vec![0xd800, 120])));
        if mode == "early-eof" {
            plan.insert("fixtureInput", "x".repeat(4 * 1024 * 1024).into());
        }
        let result = os.inspect(vec![plan.clone()], f.root.0.clone()).await;
        match mode {
            "request" => {
                assert_json_eq(
                    &J::Array(result.unwrap()),
                    &J::Array(vec![J::object([
                        (
                            "name",
                            f.plan.required("processes").unwrap().as_array().unwrap()[0]
                                .required("name")
                                .unwrap()
                                .clone(),
                        ),
                        ("enabled", true.into()),
                        ("running", false.into()),
                    ])]),
                );
                let written =
                    json::parse(&fs::read_to_string(&capture).unwrap(), usize::MAX).unwrap();
                assert_json_eq(
                    &written,
                    &J::object([
                        ("action", "inspect".into()),
                        ("data", J::object([("plans", J::Array(vec![plan]))])),
                    ]),
                );
            }
            "early-eof" => assert!(result.unwrap().is_empty()),
            "domain-error" => {
                let error = result.unwrap_err();
                assert_eq!(error.code, "target_conflict");
                assert_eq!(error.outcome, Outcome::NotExecuted);
            }
            _ => {
                let error = result.unwrap_err();
                assert_eq!(error.code, "bootstrap_os_unavailable");
                assert_eq!(error.outcome, Outcome::Unknown);
            }
        }
    }
}

async fn configuration_error_server() -> (String, tokio::task::JoinHandle<J>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/fixture", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            header.push(byte[0]);
            assert!(header.len() < 16384);
        }
        let header = String::from_utf8(header).unwrap().to_lowercase();
        assert!(header.starts_with("post /fixture/api/v1/rpc "));
        assert!(header.contains("authorization: bearer old-fixture-token\r\n"));
        let count = header
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap();
        let mut body = vec![0; count];
        socket.read_exact(&mut body).await.unwrap();
        let request = json::parse(std::str::from_utf8(&body).unwrap(), 65536).unwrap();
        assert_eq!(
            request.required("method").unwrap(),
            &J::from("hostConfigurations.get")
        );
        let response = J::object([
            ("jsonrpc", "2.0".into()),
            ("id", request.required("id").unwrap().clone()),
            (
                "error",
                IvyError::new(
                    "service_unavailable",
                    "Fixture configuration endpoint is unavailable.",
                )
                .to_wire(),
            ),
        ]);
        let body = json::stringify(&response, 65536).unwrap();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        request
    });
    (url, task)
}

#[tokio::test]
async fn bootstrap_restart_executor_self_restart_drains_and_releases_the_real_journal_lock() {
    use ivy_host_runtime::{
        executor::{ExecutorOptions, HostExecutor},
        executor_lock::ExecutorLock,
    };
    let mut f = Fixture::new(false);
    let (url, server) = configuration_error_server().await;
    f.host.insert("publicBaseUrl", url.clone().into());
    f.configs[0].insert("publicBaseUrl", url.into());
    f.setup().await;
    let input = f.input();
    let clock = Arc::new(FixedClock(AtomicI64::new(AT)));
    let os = Arc::new(Os::new(&input, clock.clone()));
    let restarts = BootstrapConfigurationRestarts::with_os(
        RestartContext {
            own_instance_id: Some("renamed-executor".into()),
            distribution: Some(f.root.0.clone()),
            process_started_at: (AT - 2000) as f64,
            platform: "win32".into(),
        },
        clock.clone(),
        os.clone(),
    );
    let options = ExecutorOptions {
        own_instance_id: Some("renamed-executor".into()),
        account_home: node_path::from_native(&f.root.0),
        build_id: None,
    };
    let owner = HostExecutor::open_with_configuration_and_restarts(
        f.host.clone(),
        f.root.0.join("host.json"),
        None,
        options,
        Arc::new(NoActions),
        clock,
        restarts,
    )
    .await
    .unwrap();
    owner.start().unwrap();
    tokio::time::timeout(Duration::from_secs(8), owner.completion())
        .await
        .unwrap()
        .unwrap();
    let request = server.await.unwrap();
    assert_eq!(
        request
            .required("params")
            .unwrap()
            .required("hostId")
            .unwrap(),
        f.host.required("hostId").unwrap()
    );
    assert!(os.events.lock().unwrap().is_empty());
    let lock = ExecutorLock::acquire(f.root.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
    let status = config::json_file(
        f.root.0.join("state/executor.json"),
        config::CONFIGURATION_BYTES,
    )
    .await
    .unwrap();
    assert_eq!(status.required("state").unwrap(), &J::from("stopping"));
    owner.close().await.unwrap();
}

#[tokio::test]
async fn bootstrap_restart_executor_heartbeat_and_close_retain_an_admitted_scheduler_operation() {
    use ivy_host_runtime::{
        executor::{ExecutorOptions, HostExecutor},
        executor_lock::ExecutorLock,
    };
    let mut f = Fixture::new(true);
    let (url, server) = configuration_error_server().await;
    f.host.insert("publicBaseUrl", url.clone().into());
    for config in &mut f.configs {
        config.insert("publicBaseUrl", url.clone().into());
    }
    f.health[0].as_mut().unwrap().insert("startedAt", stamp(AT));
    f.setup().await;
    let input = f.input();
    let clock = Arc::new(FixedClock(AtomicI64::new(AT)));
    let mut os = Os::new(&input, clock.clone());
    os.block = true;
    let os = Arc::new(os);
    let restarts = BootstrapConfigurationRestarts::with_os(
        RestartContext {
            own_instance_id: Some("renamed-executor".into()),
            distribution: Some(f.root.0.clone()),
            process_started_at: (AT - 2000) as f64,
            platform: "win32".into(),
        },
        clock.clone(),
        os.clone(),
    );
    let options = ExecutorOptions {
        own_instance_id: Some("renamed-executor".into()),
        account_home: node_path::from_native(&f.root.0),
        build_id: None,
    };
    let owner = HostExecutor::open_with_configuration_and_restarts(
        f.host.clone(),
        f.root.0.join("host.json"),
        None,
        options,
        Arc::new(NoActions),
        clock,
        restarts,
    )
    .await
    .unwrap();
    owner.start().unwrap();
    tokio::time::timeout(Duration::from_secs(8), os.entered.notified())
        .await
        .unwrap();
    let close = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.close().await })
    };
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!close.is_finished());
    let status = config::json_file(
        f.root.0.join("state/executor.json"),
        config::CONFIGURATION_BYTES,
    )
    .await
    .unwrap();
    assert_eq!(status.required("state").unwrap(), &J::from("ready"));
    assert!(
        ExecutorLock::acquire(f.root.0.join("state/executor"), false)
            .await
            .is_err()
    );
    os.released.notify_one();
    tokio::time::timeout(Duration::from_secs(8), close)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
    let lock = ExecutorLock::acquire(f.root.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
    assert_eq!(
        os.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.get("kind") == Some(&J::from("command")))
            .count(),
        1
    );
}
