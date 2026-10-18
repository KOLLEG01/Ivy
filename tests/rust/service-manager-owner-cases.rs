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
async fn owner(root: &Fixture, host: &IvyJson) -> RuntimeOwner {
    let bootstrap = root.0.join("bootstrap");
    std::fs::create_dir_all(bootstrap.join("dist/native")).unwrap();
    let exe = std::env::current_exe().unwrap();
    std::fs::copy(
        exe.parent().unwrap().parent().unwrap().join("ivy-job.exe"),
        bootstrap.join("dist/native/ivy-job.exe"),
    )
    .unwrap();
    RuntimeOwner::open(
        host.clone(),
        "fixture".into(),
        bootstrap,
        node(),
        resolver(host),
    )
    .await
    .unwrap()
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
            IvyJson::object([("command", probe), ("timeoutMs", IvyJson::Number(3000.0))]),
        ),
        ("shutdown", json(r#"{"timeoutMs":1000}"#)),
        (
            "restart",
            json(r#"{"policy":"always","minimumDelayMs":60000,"maximumDelayMs":60000}"#),
        ),
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
        ("settings", json("{}")),
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
    let Some(config_path) = std::env::var_os("IVY_INSTANCE_CONFIG") else {
        return;
    };
    let config = json(&std::fs::read_to_string(config_path).unwrap());
    let data = PathBuf::from(config.required("dataRoot").unwrap().as_str().unwrap());
    let launch = std::env::var("IVY_LAUNCH_ID").unwrap();
    let health = IvyJson::object([
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
