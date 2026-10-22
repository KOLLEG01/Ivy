//! One executable serves as both the native Manager fixture and the integration
//! runner so the production --config child-launch contract is exercised directly.
#[path = "host-service-fixture.rs"]
#[allow(dead_code)]
mod fixture;
use fixture::{ServiceFixture, host_configuration};
use ivy_contracts::IvyJson as J;
use ivy_host_executor::entry::{self, Options};
use ivy_host_runtime::{config, executor_lock::ExecutorLock, health::HealthFile};
use std::{ffi::OsString, path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

async fn child(path: PathBuf) {
    let config = config::native_instance_config(path).await.unwrap();
    if config
        .json()
        .required("settings")
        .unwrap()
        .get("fixtureExit")
        == Some(&true.into())
    {
        std::process::exit(7);
    }
    let health = HealthFile::new(config.clone());
    loop {
        health.write(true, "".into(), None).await.unwrap();
        if health.control().await.unwrap().is_some() {
            config::atomic_json(config.data_root().join("stopped.json"), true.into())
                .await
                .unwrap();
            health.write(false, "Stopped".into(), None).await.unwrap();
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn files(f: &ServiceFixture) -> Options {
    let host_path = f.0.join("host.json");
    let mut manager = f.value();
    manager.insert("componentId", "service-manager".into());
    manager.insert("hostId", "fixture-host".into());
    manager.remove("credential");
    manager.insert(
        "settings",
        J::object([(
            "hostConfigPath",
            host_path.to_string_lossy().to_string().into(),
        )]),
    );
    f.build(&manager).await;
    f.save(manager.clone()).await;
    let mut own = manager.clone();
    own.insert("instanceId", "native-host".into());
    own.insert("componentId", "host-executor".into());
    own.insert("serviceNodeId", "fixture-host.executor".into());
    for (key, dir) in [
        ("dataRoot", "host-data"),
        ("logsRoot", "host-logs"),
        ("workRoot", "host-work"),
    ] {
        own.insert(key, f.0.join(dir).to_string_lossy().to_string().into());
    }
    config::atomic_json(f.0.join("host-instance.json"), own.clone())
        .await
        .unwrap();
    let mut host = host_configuration(f);
    // This native launch deliberately has no Node executable or Node discovery.
    host.insert(
        "executables",
        J::object([(
            "manager",
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .to_string()
                .into(),
        )]),
    );
    host.insert(
        "instances",
        J::Array(
            [&manager, &own]
                .iter()
                .map(|value| {
                    J::object([
                        ("instanceId", value.required("instanceId").unwrap().clone()),
                        (
                            "serviceNodeId",
                            value.required("serviceNodeId").unwrap().clone(),
                        ),
                        (
                            "componentId",
                            value.required("componentId").unwrap().clone(),
                        ),
                        ("engine", "process".into()),
                        ("enabled", true.into()),
                        ("settings", value.required("settings").unwrap().clone()),
                    ])
                })
                .collect(),
        ),
    );
    config::atomic_json(host_path.clone(), host).await.unwrap();
    let executable = std::env::current_exe().unwrap();
    Options {
        host_config: host_path,
        manager_executable: executable.clone(),
        manager_config: f.0.join("config.json"),
        instance_config: Some(f.0.join("host-instance.json")),
        job_launcher: cfg!(windows).then(|| {
            executable
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("ivy-job.exe")
        }),
    }
}
async fn read_ready(path: PathBuf) -> J {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(value) = config::json_file(path.clone(), 65536).await {
                if value.get("ready") == Some(&true.into()) {
                    return value;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("native host and manager should become ready")
}

#[tokio::main]
async fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--config") {
        child(PathBuf::from(&args[1])).await;
        return;
    }
    if args.iter().any(|arg| arg == "--list") {
        println!("native_host_lifecycle: test");
        return;
    }
    let f = ServiceFixture::new();
    let options = files(&f).await;
    let valid = vec![
        OsString::from("--manager-executable"),
        options.manager_executable.as_os_str().into(),
        "--manager-config".into(),
        options.manager_config.as_os_str().into(),
    ];
    assert!(Options::parse(&valid, Some(options.host_config.as_os_str().into())).is_ok());
    assert!(Options::parse(&["--config".into(), "relative.json".into()], None).is_err());
    let mut duplicate = valid.clone();
    duplicate.extend(valid.clone());
    assert!(Options::parse(&duplicate, Some(options.host_config.as_os_str().into())).is_err());

    let original = config::json_file(options.manager_config.clone(), 65536)
        .await
        .unwrap();
    let mut mismatched = original.clone();
    mismatched.insert("version", "9.9.9".into());
    config::atomic_json(options.manager_config.clone(), mismatched)
        .await
        .unwrap();
    assert_eq!(
        config::native_instance_config(options.manager_config.clone())
            .await
            .err()
            .unwrap()
            .code,
        "build_mismatch"
    );
    config::atomic_json(options.manager_config.clone(), original)
        .await
        .unwrap();

    let stop = CancellationToken::new();
    let mut run = tokio::spawn(entry::run(options, stop.clone()));
    let own = read_ready(f.0.join("host-data/health.json")).await;
    let manager = read_ready(f.0.join("data/health.json")).await;
    assert_ne!(manager.get("pid"), own.get("pid"));
    assert!(manager.get("launchId").is_some());
    assert!(own.get("launchId").is_some());
    let status = config::json_file(f.0.join("state/executor.json"), 65536)
        .await
        .unwrap();
    ivy_contracts::core_schema::validate_host("ExecutorStatus", &status).unwrap();
    assert_eq!(status.get("state"), Some(&"ready".into()));
    assert_eq!(status.get("pid"), own.get("pid"));
    assert_eq!(status.get("bootId"), own.get("bootId"));
    let mut control = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("instanceId", own.required("instanceId").unwrap().clone()),
        ("bootId", "wrong-boot".into()),
        ("action", "shutdown".into()),
        (
            "requestedAt",
            chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                .into(),
        ),
    ]);
    if let Some(launch_id) = own.get("launchId") {
        control.insert("launchId", launch_id.clone());
    }
    config::atomic_json(f.0.join("host-data/control.json"), control.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        !run.is_finished(),
        "a stale boot must not stop the current owner"
    );
    control.insert("bootId", own.required("bootId").unwrap().clone());
    config::atomic_json(f.0.join("host-data/control.json"), control)
        .await
        .unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(10), &mut run).await;
    if finished.is_err() {
        stop.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(35), &mut run).await;
    }
    finished.unwrap().unwrap().unwrap();
    assert_eq!(
        config::json_file(f.0.join("data/stopped.json"), 1024)
            .await
            .unwrap(),
        J::Bool(true)
    );
    let final_status = config::json_file(f.0.join("state/executor.json"), 65536)
        .await
        .unwrap();
    assert_eq!(final_status.get("state"), Some(&"stopping".into()));
    let lock = ExecutorLock::acquire(f.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();

    let failure = ServiceFixture::new();
    let options = files(&failure).await;
    let mut manager = config::json_file(options.manager_config.clone(), 65536)
        .await
        .unwrap();
    let mut settings = manager.required("settings").unwrap().clone();
    settings.insert("fixtureExit", true.into());
    manager.insert("settings", settings);
    config::atomic_json(options.manager_config.clone(), manager)
        .await
        .unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        entry::run(options, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code, "service_manager_exited");
    assert_eq!(
        error.outcome.literal(),
        Some(ivy_contracts::Outcome::Completed)
    );
    let final_status = config::json_file(failure.0.join("state/executor.json"), 65536)
        .await
        .unwrap();
    assert_eq!(final_status.get("state"), Some(&"failed".into()));
    assert_eq!(
        final_status.get("code"),
        Some(&"service_manager_exited".into())
    );
    println!(
        "native entry: launch, health, boot fencing, cooperative stop, lock release and child failure passed"
    );
}
