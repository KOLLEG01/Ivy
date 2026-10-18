use super::Fixture;
use ivy_contracts::{
    IvyJson,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::{
    config::atomic_json,
    journal::HostJournal,
    node_runtime::NodeRuntime,
    runtime_owner::{RuntimeOwner, TargetPolicyResolver},
};
use std::{path::PathBuf, sync::Arc, time::Duration};

fn json(value: &str) -> IvyJson {
    parse(value, MANAGEMENT_FRAME_BYTES).unwrap()
}
fn encoded(value: &IvyJson) -> String {
    canonical(value, MANAGEMENT_FRAME_BYTES).unwrap()
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn hash() -> IvyJson {
    format!("sha256:{}", "a".repeat(64)).into()
}
fn host(root: &Fixture) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", "owner-host".into()),
        (
            "runtimeRoot",
            root.0.join("state").to_string_lossy().to_string().into(),
        ),
        (
            "artifactRoot",
            root.0
                .join("artifacts")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        (
            "stagingRoot",
            root.0.join("staging").to_string_lossy().to_string().into(),
        ),
        ("publicBaseUrl", "http://127.0.0.1:39081/ivy".into()),
        (
            "executables",
            IvyJson::object([(
                "fixture",
                std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
                    .into(),
            )]),
        ),
        (
            "instances",
            json(
                r#"[{"instanceId":"fixture","serviceNodeId":"owner.fixture","componentId":"fixture","engine":"process","enabled":true,"settings":{}}]"#,
            ),
        ),
    ])
}
fn resolver(host: &IvyJson) -> TargetPolicyResolver {
    let host = host.clone();
    Arc::new(move |_, _| {
        let host = host.clone();
        Box::pin(async move { Ok(host) })
    })
}
fn node() -> NodeRuntime {
    NodeRuntime {
        executable: PathBuf::from("fixture-node"),
        version: "24.18.0".into(),
    }
}
fn bootstrap(root: &Fixture) -> PathBuf {
    let bootstrap = root.0.join("bootstrap");
    std::fs::create_dir_all(bootstrap.join("dist/native")).unwrap();
    let exe = std::env::current_exe().unwrap();
    std::fs::copy(
        exe.parent().unwrap().parent().unwrap().join("ivy-job.exe"),
        bootstrap.join("dist/native/ivy-job.exe"),
    )
    .unwrap();
    bootstrap
}
async fn owner(root: &Fixture, host: &IvyJson) -> RuntimeOwner {
    RuntimeOwner::for_host(host.clone(), "fixture".into(), bootstrap(root), node())
        .await
        .unwrap()
}

#[tokio::test]
async fn production_policy_resolver_uses_association_instead_of_latest_desired_mapping() {
    let root = Fixture::new();
    let accepted = host(&root);
    let journal = HostJournal::open(accepted.clone()).await.unwrap();
    let mut selected = target(&root, &journal, "associated-launch").await;
    let request = IvyJson::object([
        ("action", "deploy".into()),
        ("operationId", "associated-deploy".into()),
        ("instanceId", "fixture".into()),
        ("candidateId", hash()),
    ]);
    let entry = journal.accept(request, None).await.unwrap();
    let id = entry
        .required("record")
        .unwrap()
        .required("deploymentId")
        .unwrap()
        .as_str()
        .unwrap();
    journal
        .advance(&id, "prepared", "checking", json("{}"))
        .await
        .unwrap();
    journal
        .advance(&id, "checking", "draining", json("{}"))
        .await
        .unwrap();
    selected.insert("revision", "associated-policy".into());
    journal
        .advance(
            &id,
            "draining",
            "activating",
            IvyJson::object([("target", selected)]),
        )
        .await
        .unwrap();
    let mut desired = accepted.clone();
    desired.insert(
        "executables",
        IvyJson::object([(
            "fixture",
            root.0
                .join("missing-executable.exe")
                .to_string_lossy()
                .to_string()
                .into(),
        )]),
    );
    journal.use_configuration(desired.clone()).await.unwrap();
    let owner = owner(&root, &desired).await;
    owner.start().unwrap();
    let ready = observation(&journal, "ready").await;
    assert_eq!(
        ready.required("targetRevision").unwrap(),
        &IvyJson::from("associated-policy")
    );
    owner.close().await.unwrap();
    journal.close().await.unwrap();
}

#[tokio::test]
async fn owner_topology_preserves_existing_owners_and_adds_disabled_instances() {
    use ivy_service_manager::topology::Owners;
    let root = Fixture::new();
    let initial = host(&root);
    let journal = HostJournal::open(initial.clone()).await.unwrap();
    let mut owners = Owners::new(root.0.clone(), node());
    owners.reconcile("win32", &initial).await.unwrap();
    let original = observation(&journal, "idle").await;
    let mut updated = initial.clone();
    let mut instances = initial
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    instances.push(json(r#"{"instanceId":"disabled","serviceNodeId":"owner.disabled","componentId":"fixture","engine":"process","enabled":false,"settings":{}}"#));
    updated.insert("instances", IvyJson::Array(instances.clone()));
    journal.use_configuration(updated.clone()).await.unwrap();
    owners.reconcile("win32", &updated).await.unwrap();
    assert_eq!(owners.ids(), vec!["fixture".into(), "disabled".into()]);
    assert_eq!(
        journal
            .observations()
            .await
            .unwrap()
            .required("fixture")
            .unwrap()
            .required("ownerBootId")
            .unwrap(),
        original.required("ownerBootId").unwrap()
    );
    instances.remove(0);
    updated.insert("instances", IvyJson::Array(instances));
    journal.use_configuration(updated.clone()).await.unwrap();
    owners.reconcile("win32", &updated).await.unwrap();
    assert_eq!(owners.ids(), vec!["disabled".into()]);
    let retired = ivy_host_runtime::executor_lock::ExecutorLock::acquire(
        root.0.join("state/owners/fixture"),
        false,
    )
    .await
    .unwrap();
    retired.close().await.unwrap();
    owners.close().await.unwrap();
    assert_eq!(
        owners.reconcile("win32", &updated).await.unwrap_err().code,
        "service_unavailable"
    );
    journal.close().await.unwrap();
}

#[tokio::test]
async fn partial_topology_startup_releases_new_locks_and_preserves_existing_owner() {
    use ivy_host_runtime::executor_lock::ExecutorLock;
    use ivy_service_manager::topology::Owners;
    let root = Fixture::new();
    let initial = host(&root);
    let journal = HostJournal::open(initial.clone()).await.unwrap();
    let mut owners = Owners::new(root.0.clone(), node());
    owners.reconcile("win32", &initial).await.unwrap();
    let mut updated = initial.clone();
    let mut instances = initial
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    for id in ["added", "locked"] {
        let mut instance = instances[0].clone();
        instance.insert("instanceId", id.into());
        instance.insert("serviceNodeId", format!("owner.{id}").into());
        instances.push(instance);
    }
    updated.insert("instances", IvyJson::Array(instances));
    journal.use_configuration(updated.clone()).await.unwrap();
    let locked = ExecutorLock::acquire(root.0.join("state/owners/locked"), false)
        .await
        .unwrap();
    assert_eq!(
        owners.reconcile("win32", &updated).await.unwrap_err().code,
        "executor_already_running"
    );
    assert_eq!(owners.ids(), vec!["fixture".into()]);
    let released = ExecutorLock::acquire(root.0.join("state/owners/added"), false)
        .await
        .unwrap();
    released.close().await.unwrap();
    locked.close().await.unwrap();
    owners.close().await.unwrap();
    journal.close().await.unwrap();
}

fn manager_config(root: &Fixture, host: &IvyJson) -> ivy_host_runtime::config::InstanceConfig {
    ivy_host_runtime::config::InstanceConfig::from_json(IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", "owner-host".into()),
        ("instanceId", "manager".into()),
        ("serviceNodeId", "owner.manager".into()),
        ("componentId", "service-manager".into()),
        ("version", "1.0.0".into()),
        ("buildId", hash()),
        (
            "publicBaseUrl",
            host.required("publicBaseUrl").unwrap().clone(),
        ),
        ("credential", "fixture".into()),
        (
            "dataRoot",
            root.0
                .join("manager-data")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        (
            "artifactRoot",
            root.0
                .join("artifacts")
                .join("release")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        (
            "settings",
            IvyJson::object([(
                "hostConfigPath",
                root.0
                    .join("state")
                    .join("host.json")
                    .to_string_lossy()
                    .to_string()
                    .into(),
            )]),
        ),
    ]))
    .unwrap()
}
#[tokio::test]
async fn manager_keeps_local_child_alive_offline_and_finishes_with_unready_health() {
    use ivy_service_manager::manager::Manager;
    let root = Fixture::new();
    let mut accepted = host(&root);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); // Own the unavailable Hive endpoint.
    accepted.insert(
        "publicBaseUrl",
        format!("http://{}/ivy", listener.local_addr().unwrap()).into(),
    );
    let config = manager_config(&root, &accepted);
    let mut instances = accepted
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    instances.push(IvyJson::object([
        ("instanceId", "manager".into()),
        ("serviceNodeId", "owner.manager".into()),
        ("componentId", "service-manager".into()),
        ("engine", "process".into()),
        ("enabled", true.into()),
        (
            "settings",
            config.json().required("settings").unwrap().clone(),
        ),
    ]));
    accepted.insert("instances", IvyJson::Array(instances));
    let journal = HostJournal::open(accepted.clone()).await.unwrap();
    target(&root, &journal, "offline-child").await;
    let manager = Manager::start(config, accepted, false, node(), bootstrap(&root))
        .await
        .unwrap();
    observation(&journal, "ready").await;
    assert!(!manager.service.ready());
    tokio::time::timeout(Duration::from_secs(10), async {
        let (first, second) = tokio::join!(manager.close(), manager.close());
        first.unwrap();
        second.unwrap();
    })
    .await
    .unwrap();
    assert!(
        root.0
            .join("state/instances/fixture/data/cooperatively-stopped")
            .exists()
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    let health = json(&std::fs::read_to_string(root.0.join("manager-data/health.json")).unwrap());
    assert_eq!(health.required("ready").unwrap(), &IvyJson::Bool(false));
    assert_eq!(
        health.required("details").unwrap(),
        &IvyJson::from("ServiceManager stopped.")
    );
    journal.close().await.unwrap();
}

#[tokio::test]
async fn manager_maintenance_refusal_precedes_journal_or_owner_effects() {
    use ivy_service_manager::manager::Manager;
    let root = Fixture::new();
    let accepted = host(&root);
    let config = manager_config(&root, &accepted);
    let error = Manager::start(config, accepted, true, node(), root.0.clone())
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "maintenance_active");
    assert!(!root.0.join("state").exists());
}
async fn observation(journal: &HostJournal, expected: &str) -> IvyJson {
    let last = std::sync::Mutex::new(IvyJson::Null);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let values = journal.observations().await.unwrap();
            *last.lock().unwrap() = values.clone();
            if let Some(value) = values
                .get("fixture")
                .filter(|v| v.get("state") == Some(&IvyJson::from(expected)))
            {
                return value.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "No {expected} observation; last={}",
            encoded(&last.into_inner().unwrap())
        )
    })
}
async fn target(root: &Fixture, journal: &HostJournal, revision: &str) -> IvyJson {
    target_with_policy(
        root,
        journal,
        revision,
        json("{}"),
        json(r#"{"policy":"always","minimumDelayMs":60000,"maximumDelayMs":60000}"#),
        3000,
    )
    .await
}
async fn target_with_policy(
    root: &Fixture,
    journal: &HostJournal,
    revision: &str,
    settings: IvyJson,
    restart: IvyJson,
    deadline: u64,
) -> IvyJson {
    let artifact = root.0.join("artifacts/release");
    std::fs::create_dir_all(artifact.join("dist")).unwrap();
    let stamp = IvyJson::object([("version", "1.0.0".into()), ("buildId", hash())]);
    atomic_json(artifact.join("dist/build-info.json"), stamp)
        .await
        .unwrap();
    let entry = json(
        r#"{"executable":"fixture","args":["--exact","owner_cases::owner_child_fixture","--nocapture"],"timeoutMs":10000}"#,
    );
    let mut probe = entry.clone();
    probe.insert(
        "args",
        json(r#"["--exact","owner_cases::owner_probe_fixture","--nocapture"]"#),
    );
    let manifest = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("componentId", "fixture".into()),
        ("kind", "service".into()),
        ("version", "1.0.0".into()),
        ("buildId", hash()),
        ("connectsToHive", false.into()),
        (
            "requirements",
            json(r#"{"node":">=24.18.0 <25.0.0","hiveProtocol":null,"contracts":[]}"#),
        ),
        ("entrypoint", entry),
        (
            "readiness",
            IvyJson::object([
                ("command", probe),
                ("timeoutMs", IvyJson::Number(deadline as f64)),
            ]),
        ),
        ("shutdown", json(r#"{"timeoutMs":1000}"#)),
        ("restart", restart),
        (
            "storage",
            json(r#"{"writeFormat":1,"minReadableFormat":1,"maxReadableFormat":1}"#),
        ),
    ]);
    let candidate = IvyJson::object([
        ("candidateId", hash()),
        ("componentId", "fixture".into()),
        (
            "artifactRoot",
            artifact.to_string_lossy().to_string().into(),
        ),
        (
            "manifestPath",
            artifact
                .join("manifest.json")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        ("createdAt", now().into()),
        (
            "platform",
            json(r#"{"os":"win32","arch":"x64","node":"24.18.0"}"#),
        ),
        ("buildId", hash()),
    ]);
    journal.save_candidate(candidate, manifest).await.unwrap();
    let config_path = root.0.join("state").join(format!("{revision}.json"));
    let config = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", "owner-host".into()),
        ("instanceId", "fixture".into()),
        ("serviceNodeId", "owner.fixture".into()),
        ("componentId", "fixture".into()),
        ("version", "1.0.0".into()),
        ("buildId", hash()),
        ("publicBaseUrl", "http://127.0.0.1:39081/ivy".into()),
        (
            "dataRoot",
            root.0
                .join("state")
                .join("instances")
                .join("fixture")
                .join("data")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        (
            "artifactRoot",
            artifact.to_string_lossy().to_string().into(),
        ),
        ("settings", settings),
    ]);
    atomic_json(config_path.clone(), config).await.unwrap();
    let target = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("instanceId", "fixture".into()),
        ("revision", revision.into()),
        ("candidateId", hash()),
        ("desired", "running".into()),
        (
            "configPath",
            config_path.to_string_lossy().to_string().into(),
        ),
        ("requestedAt", now().into()),
    ]);
    journal.restore_target(target.clone()).await.unwrap();
    target
}

#[test]
fn owner_probe_fixture() {}
#[test]
fn owner_child_fixture() {
    use std::io::Write;
    let Some(config_path) = std::env::var_os("IVY_INSTANCE_CONFIG") else {
        return;
    };
    let config = json(&std::fs::read_to_string(config_path).unwrap());
    let data = PathBuf::from(config.required("dataRoot").unwrap().as_str().unwrap());
    let launch = std::env::var("IVY_LAUNCH_ID").unwrap();
    let settings = config.required("settings").unwrap();
    writeln!(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data.join("launches"))
            .unwrap(),
        "{}",
        std::process::id()
    )
    .unwrap();
    let mut health = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("instanceId", "fixture".into()),
        ("componentId", "fixture".into()),
        ("buildId", hash()),
        ("pid", IvyJson::Number(std::process::id() as f64)),
        ("bootId", "child-boot".into()),
        ("launchId", launch.clone().into()),
        ("startedAt", now().into()),
        ("observedAt", now().into()),
        ("ready", true.into()),
        ("generation", IvyJson::Null),
        ("details", "".into()),
    ]);
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        if let Some(code) = settings.get("exitCode").and_then(IvyJson::as_f64) {
            if started.elapsed() >= Duration::from_millis(200) {
                if code == 0.0 {
                    return;
                }
                std::process::exit(code as i32);
            }
        }
        let ready = settings
            .get("unreadyAfterMs")
            .and_then(IvyJson::as_f64)
            .is_none_or(|after| started.elapsed().as_millis() < after as u128);
        health.insert("ready", ready.into());
        health.insert("observedAt", now().into());
        std::fs::write(data.join("health.json"), encoded(&health)).unwrap();
        if let Ok(value) = std::fs::read_to_string(data.join("control.json")) {
            let control = json(&value);
            if control.get("launchId") == Some(&IvyJson::from(launch.clone()))
                && control.get("bootId") == Some(&IvyJson::from("child-boot"))
            {
                std::thread::sleep(Duration::from_millis(200));
                std::fs::write(data.join("cooperatively-stopped"), launch).unwrap();
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("Owner child watchdog expired");
}

#[tokio::test]
async fn real_process_exit_uses_each_restart_policy_without_duplicate_ownership() {
    for (policy, code, restarts) in [
        ("never", 1, false),
        ("on-failure", 0, false),
        ("always", 0, true),
        ("on-failure", 1, true),
    ] {
        let root = Fixture::new();
        let accepted = host(&root);
        let journal = HostJournal::open(accepted.clone()).await.unwrap();
        target_with_policy(
            &root,
            &journal,
            "exit-policy",
            IvyJson::object([("exitCode", IvyJson::Number(code as f64))]),
            IvyJson::object([
                ("policy", policy.into()),
                ("minimumDelayMs", IvyJson::Number(100.0)),
                ("maximumDelayMs", IvyJson::Number(1000.0)),
            ]),
            3000,
        )
        .await;
        let owner = owner(&root, &accepted).await;
        owner.start().unwrap();
        let launches = root.0.join("state/instances/fixture/data/launches");
        let count = || {
            std::fs::read_to_string(&launches)
                .map(|v| v.lines().count())
                .unwrap_or(0)
        };
        if restarts {
            tokio::time::timeout(Duration::from_secs(8), async {
                while count() < 2 {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .unwrap();
        } else {
            observation(&journal, "exited").await;
            tokio::time::sleep(Duration::from_millis(1100)).await;
            assert_eq!(count(), 1, "{policy}/{code} relaunched an exited target");
        }
        owner.close().await.unwrap();
        journal.close().await.unwrap();
    }
}

#[tokio::test]
async fn real_readiness_loss_retires_the_child_then_records_restart_backoff() {
    let root = Fixture::new();
    let accepted = host(&root);
    let journal = HostJournal::open(accepted.clone()).await.unwrap();
    target_with_policy(
        &root,
        &journal,
        "loses-readiness",
        json(r#"{"unreadyAfterMs":2500}"#),
        json(r#"{"policy":"always","minimumDelayMs":60000,"maximumDelayMs":60000}"#),
        1500,
    )
    .await;
    let owner = owner(&root, &accepted).await;
    owner.start().unwrap();
    observation(&journal, "ready").await;
    let failed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let value = journal
                .observations()
                .await
                .unwrap()
                .required("fixture")
                .unwrap()
                .clone();
            if value.get("code") == Some(&"readiness_deadline".into()) {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(failed.get("restartCount"), Some(&IvyJson::Number(1.0)));
    assert!(
        failed
            .required("nextRestartAt")
            .unwrap()
            .as_js_string()
            .is_some()
    );
    assert!(
        root.0
            .join("state/instances/fixture/data/cooperatively-stopped")
            .exists()
    );
    owner.close().await.unwrap();
    journal.close().await.unwrap();
}

#[tokio::test]
async fn owner_close_is_shared_and_releases_the_exact_lock_after_cooperative_retirement() {
    let root = Fixture::new();
    let host = host(&root);
    let journal = HostJournal::open(host.clone()).await.unwrap();
    target(&root, &journal, "launch-1").await;
    let owner = owner(&root, &host).await;
    owner.start().unwrap();
    owner.start().unwrap();
    observation(&journal, "ready").await;
    let competing = RuntimeOwner::open(
        host.clone(),
        "fixture".into(),
        root.0.clone(),
        node(),
        resolver(&host),
    )
    .await;
    assert_eq!(competing.err().unwrap().code, "executor_already_running");
    let cancelled = tokio::time::timeout(Duration::from_millis(1), owner.close()).await;
    assert!(cancelled.is_err());
    let (first, second) = tokio::join!(owner.close(), owner.close());
    first.unwrap();
    second.unwrap();
    assert!(
        root.0
            .join("state/instances/fixture/data/cooperatively-stopped")
            .exists()
    );
    observation(&journal, "stopped").await;
    let replacement = RuntimeOwner::open(
        host.clone(),
        "fixture".into(),
        root.0.clone(),
        node(),
        resolver(&host),
    )
    .await
    .unwrap();
    replacement.close().await.unwrap();
    journal.close().await.unwrap();
}

#[tokio::test]
async fn same_manager_unknown_fence_survives_an_absent_target_and_close() {
    let root = Fixture::new();
    let host = host(&root);
    let journal = HostJournal::open(host.clone()).await.unwrap();
    let prior = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("instanceId", "fixture".into()),
        ("ownerPid", IvyJson::Number(std::process::id() as f64)),
        ("ownerBootId", "old-boot".into()),
        ("observedAt", now().into()),
        ("targetRevision", IvyJson::Null),
        ("candidateId", IvyJson::Null),
        ("buildId", IvyJson::Null),
        ("state", "unknown".into()),
        ("health", IvyJson::Null),
        ("restartCount", IvyJson::Number(0.0)),
        ("nextRestartAt", IvyJson::Null),
        ("code", "outcome_unknown".into()),
        ("message", "unknown process".into()),
    ]);
    journal.record_observation("fixture", prior).await.unwrap();
    let owner = owner(&root, &host).await;
    owner.start().unwrap();
    tokio::time::sleep(Duration::from_millis(650)).await;
    let before = journal
        .observations()
        .await
        .unwrap()
        .required("fixture")
        .unwrap()
        .clone();
    assert_eq!(before.required("state").unwrap(), &IvyJson::from("unknown"));
    owner.close().await.unwrap();
    let after = journal
        .observations()
        .await
        .unwrap()
        .required("fixture")
        .unwrap()
        .clone();
    assert_eq!(before, after);
    journal.close().await.unwrap();
}

#[tokio::test]
async fn invalid_replacement_does_not_drain_the_ready_child() {
    let root = Fixture::new();
    let host = host(&root);
    let journal = HostJournal::open(host.clone()).await.unwrap();
    target(&root, &journal, "launch-1").await;
    let owner = owner(&root, &host).await;
    owner.start().unwrap();
    observation(&journal, "ready").await;
    let mut target = journal.target("fixture").await.unwrap().unwrap();
    target.insert("revision", "launch-2".into());
    target.insert(
        "configPath",
        root.0
            .join("state/missing.json")
            .to_string_lossy()
            .to_string()
            .into(),
    );
    journal.restore_target(target).await.unwrap();
    observation(&journal, "failed").await;
    assert!(
        !root
            .0
            .join("state/instances/fixture/data/control.json")
            .exists()
    );
    let health = json(
        &std::fs::read_to_string(root.0.join("state/instances/fixture/data/health.json")).unwrap(),
    );
    assert_eq!(
        health.required("launchId").unwrap(),
        &IvyJson::from("launch-1")
    );
    owner.close().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(
            root.0
                .join("state/instances/fixture/data/cooperatively-stopped")
        )
        .unwrap(),
        "launch-1"
    );
    journal.close().await.unwrap();
}
