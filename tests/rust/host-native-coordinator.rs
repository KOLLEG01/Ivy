//! Real native preparation/coordinator lifecycle; the fixture builder copies
//! Cargo-built binaries instead of invoking the distribution release compiler.
#[path = "host-service-fixture.rs"]
#[allow(dead_code)]
mod fixture;
use fixture::{ServiceFixture, host_configuration};
use ivy_contracts::{IvyJson as J, JsString, core_schema::validate_host};
use ivy_host_runtime::{
    artifact, config,
    executor_lock::ExecutorLock,
    health::HealthFile,
    journal::HostJournal,
    node_path,
    portable_http::PortableHttp,
    prepare::{PreparationRuntime, prepare_candidate},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

fn value(path: &Path) -> J {
    J::String(node_path::from_native(path))
}
fn native(value: &J) -> PathBuf {
    node_path::to_native(value.as_js_string().unwrap())
}
fn platform() -> String {
    format!(
        "{}-{}",
        if cfg!(windows) { "win32" } else { "linux" },
        if cfg!(target_arch = "x86_64") {
            "x64"
        } else {
            "arm64"
        }
    )
}
fn binary(name: &str) -> String {
    format!("{name}{}", if cfg!(windows) { ".exe" } else { "" })
}
fn helpers() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ivy-host-executor"))
        .parent()
        .unwrap()
        .to_owned()
}
async fn build(marker: PathBuf) {
    let count = fs::read_to_string(&marker)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
        + 1;
    fs::write(&marker, count.to_string()).unwrap();
    config::atomic_json(
        marker.with_extension("args.json"),
        J::Array(
            std::env::args_os()
                .skip(2)
                .map(|arg| value(Path::new(&arg)))
                .collect(),
        ),
    )
    .await
    .unwrap();
    let root = std::env::current_dir().unwrap();
    let dir = root.join("dist/native").join(platform());
    fs::create_dir_all(&dir).unwrap();
    fs::copy(
        std::env::current_exe().unwrap(),
        dir.join(binary("ivy-fixture")),
    )
    .unwrap();
    fs::copy(
        helpers().join(binary("ivy-health")),
        dir.join(binary("ivy-health")),
    )
    .unwrap();
    config::atomic_json(
        dir.join("build-info.json"),
        J::object([
            ("schemaVersion", J::Number(1.0)),
            ("componentId", "fixture".into()),
            ("version", "1.0.0".into()),
            ("buildId", std::env::var("IVY_BUILD_ID").unwrap().into()),
        ]),
    )
    .await
    .unwrap();
}
async fn service() {
    let runtime = config::native_instance_config(PathBuf::from(
        std::env::var_os("IVY_INSTANCE_CONFIG").unwrap(),
    ))
    .await
    .unwrap();
    let health = HealthFile::new(runtime.clone());
    loop {
        health.write(true, "".into(), None).await.unwrap();
        if health.control().await.unwrap().is_some() {
            config::atomic_json(
                runtime.data_root().join("cooperative-stop.json"),
                true.into(),
            )
            .await
            .unwrap();
            health.write(false, "Stopped".into(), None).await.unwrap();
            return;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}
struct Running {
    child: Child,
    log: PathBuf,
}
impl Running {
    fn start(f: &ServiceFixture, own: &Path) -> Self {
        let log = f.0.join("executor.log");
        let output = fs::File::create(&log).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_ivy-host-executor"))
            .args([
                "--config".as_ref(),
                f.0.join("host.json").as_os_str(),
                "--instance-config".as_ref(),
                own.as_os_str(),
                "--job-launcher".as_ref(),
                helpers().join(binary("ivy-job")).as_os_str(),
            ])
            .env_remove("IVY_NODE_EXECUTABLE")
            .env_remove("IVY_INSTANCE_CONFIG")
            .env_remove("IVY_LAUNCH_ID")
            .env(
                "PATH",
                PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"),
            )
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap();
        Self { child, log }
    }
    fn live(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "{}",
            fs::read_to_string(&self.log).unwrap()
        );
    }
    async fn stopped(&mut self) {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "{}",
                        fs::read_to_string(&self.log).unwrap()
                    );
                    return;
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        })
        .await
        .expect("native Host must drain owned children and exit");
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
async fn ready(run: &mut Running, path: &Path) -> J {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            run.live();
            if let Ok(value) = config::json_file(path.to_path_buf(), 65536).await {
                if value.get("ready") == Some(&true.into()) {
                    return value;
                }
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("native process must publish fresh health")
}
async fn accepted(run: &mut Running, journal: &HostJournal, operation: &str) -> J {
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            run.live();
            let entry = journal.operation(operation).await.unwrap().unwrap();
            let phase = entry
                .required("record")
                .unwrap()
                .required("phase")
                .unwrap()
                .as_str()
                .unwrap();
            if ["succeeded", "failed", "needs_attention", "rolled_back"].contains(&phase.as_str()) {
                assert_eq!(phase, "succeeded", "{entry:?}");
                return entry;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("accepted native deployment must reach its durable result")
}
async fn stop(run: &mut Running, health_path: &Path) {
    let health = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let health = ready(run, health_path).await;
            if health.get("pid") == Some(&J::Number(run.child.id() as f64)) {
                break health;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap();
    let control = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("instanceId", health.required("instanceId").unwrap().clone()),
        ("bootId", health.required("bootId").unwrap().clone()),
        ("launchId", health.required("launchId").unwrap().clone()),
        ("action", "shutdown".into()),
        (
            "requestedAt",
            chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                .into(),
        ),
    ]);
    config::atomic_json(health_path.with_file_name("control.json"), control)
        .await
        .unwrap();
    run.stopped().await;
}
async fn lifecycle() {
    let f = ServiceFixture::new();
    let mut host = host_configuration(&f);
    let marker_name = JsString(
        "build-"
            .encode_utf16()
            .chain([0xd800])
            .chain("-count".encode_utf16())
            .collect(),
    );
    let build_marker = f.0.join(node_path::to_native(&marker_name));
    let replacement_marker = f.0.join("build-\u{fffd}-count");
    fs::write(&replacement_marker, b"unrelated").unwrap();
    let build_args = vec![
        "--build".into(),
        value(&build_marker),
        "".into(),
        "space and \"quotes\"".into(),
        "trailing\\".into(),
        "Grüße-🙂".into(),
        "literal-\u{fffd}".into(),
    ];
    let own_root = f.0.join("artifact");
    let own_path = f.0.join("own.json");
    let host_path = f.0.join("host.json");
    let native_dir = own_root.join("dist/native").join(platform());
    fs::create_dir_all(&native_dir).unwrap();
    fs::copy(
        helpers().join(binary("ivy-job")),
        native_dir.join(binary("ivy-job")),
    )
    .unwrap();
    host.insert(
        "executables",
        J::object([
            (
                "tar",
                value(
                    &PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                        .join("System32/tar.exe"),
                ),
            ),
            ("fixture-builder", value(&std::env::current_exe().unwrap())),
        ]),
    );
    let mut instances = host
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    instances.push(J::object([
        ("instanceId", "native-host".into()),
        ("serviceNodeId", "fixture-host.executor".into()),
        ("componentId", "host-executor".into()),
        ("engine", "process".into()),
        ("enabled", true.into()),
        (
            "settings",
            J::object([("hostConfigPath", value(&host_path))]),
        ),
    ]));
    host.insert("instances", J::Array(instances));
    config::atomic_json(host_path.clone(), host.clone())
        .await
        .unwrap();
    let mut own = f.value();
    own.remove("credential");
    for (key, v) in [
        ("instanceId", "native-host"),
        ("hostId", "fixture-host"),
        ("componentId", "host-executor"),
        ("serviceNodeId", "fixture-host.executor"),
    ] {
        own.insert(key, v.into());
    }
    for (key, dir) in [
        ("dataRoot", "host-data"),
        ("workRoot", "host-work"),
        ("logsRoot", "host-logs"),
    ] {
        own.insert(key, value(&f.0.join(dir)));
    }
    own.insert(
        "settings",
        J::object([("hostConfigPath", value(&host_path))]),
    );
    f.build(&own).await;
    let mut invalid_own = own.clone();
    invalid_own.insert("hostId", "another-host".into());
    config::atomic_json(own_path.clone(), invalid_own)
        .await
        .unwrap();
    let refused = ivy_host_executor::entry::run_native(
        &[
            "--config".into(),
            host_path.clone().into_os_string(),
            "--instance-config".into(),
            own_path.clone().into_os_string(),
        ],
        tokio_util::sync::CancellationToken::new(),
    )
    .await;
    assert_eq!(refused.unwrap_err().code, "invalid_arguments");
    assert!(!f.0.join("state/executor").exists());
    config::atomic_json(own_path.clone(), own).await.unwrap();
    let staging = native(host.required("stagingRoot").unwrap());
    let source = staging.join("snapshot/source");
    let snapshot = J::object([
        ("snapshotId", "native-fixed-source".into()),
        ("sourceRoot", value(&source)),
        ("originalRoot", value(&f.0.join("checkout"))),
        (
            "manifestPath",
            value(&staging.join("snapshot/snapshot.json")),
        ),
        ("capturedAt", "2026-01-01T00:00:00.000Z".into()),
    ]);
    config::atomic_json(
        staging.join("snapshot/snapshot.json"),
        J::object([
            ("schemaVersion", J::Number(1.0)),
            ("snapshot", snapshot.clone()),
        ]),
    )
    .await
    .unwrap();
    config::atomic_json(
        source.join("package.json"),
        J::object([("packageManager", "npm@11.16.0".into())]),
    )
    .await
    .unwrap();
    config::atomic_json(
        source.join("package-lock.json"),
        J::object([
            ("lockfileVersion", J::Number(3.0)),
            ("packages", J::object([] as [(&str, J); 0])),
        ]),
    )
    .await
    .unwrap();
    let command = |executable: &str, args: Vec<J>| {
        J::object([
            ("executable", executable.into()),
            ("args", J::Array(args)),
            ("timeoutMs", J::Number(10000.0)),
        ])
    };
    let plan = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("componentId", "fixture".into()),
        ("kind", "native".into()),
        ("version", "1.0.0".into()),
        ("description", "Native lifecycle fixture".into()),
        ("connectsToHive", false.into()),
        (
            "requirements",
            J::object([
                ("node", ">=24.18.0 <25.0.0".into()),
                ("hiveProtocol", J::Null),
                ("contracts", J::Array(vec![])),
            ]),
        ),
        (
            "prepare",
            J::Array(vec![command("fixture-builder", build_args.clone())]),
        ),
        ("checks", J::Array(vec![])),
        (
            "entrypoint",
            command("dist/native/ivy-fixture", vec!["--child".into()]),
        ),
        (
            "readiness",
            J::object([
                ("timeoutMs", J::Number(10000.0)),
                ("command", command("dist/native/ivy-health", vec![])),
            ]),
        ),
        ("shutdown", J::object([("timeoutMs", J::Number(3000.0))])),
        (
            "restart",
            J::object([
                ("policy", "always".into()),
                ("minimumDelayMs", J::Number(100.0)),
                ("maximumDelayMs", J::Number(1000.0)),
            ]),
        ),
    ]);
    validate_host("BuildPlan", &plan).unwrap();
    config::atomic_json(source.join("services/fixture/deploy.json"), plan)
        .await
        .unwrap();
    let runtime = PreparationRuntime {
        node: None,
        compare: artifact::native_file_compare().unwrap(),
        http: PortableHttp::new(Default::default()).unwrap(),
    };
    let candidate = prepare_candidate(
        snapshot.clone(),
        "fixture".into(),
        host.clone(),
        own_root.clone(),
        runtime.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        candidate.required("platform").unwrap().get("node"),
        Some(&"unavailable".into())
    );
    let reused = prepare_candidate(snapshot, "fixture".into(), host.clone(), own_root, runtime)
        .await
        .unwrap();
    assert_eq!(
        ivy_contracts::hash::hash_json(&candidate).unwrap(),
        ivy_contracts::hash::hash_json(&reused).unwrap()
    );
    assert_eq!(fs::read_to_string(&build_marker).unwrap(), "1");
    assert_eq!(fs::read(&replacement_marker).unwrap(), b"unrelated");
    assert_eq!(
        config::json_file(build_marker.with_extension("args.json"), 65536)
            .await
            .unwrap(),
        J::Array(build_args[1..].to_vec()),
    );
    artifact::verify_native_candidate(
        candidate.clone(),
        native(host.required("artifactRoot").unwrap()),
    )
    .await
    .unwrap();
    // The explicit launcher must work without a packaged default at startup.
    fs::remove_file(native_dir.join(binary("ivy-job"))).unwrap();
    let manifest =
        artifact::read_release_manifest(native(candidate.required("manifestPath").unwrap()))
            .await
            .unwrap();
    let authorization = J::object([
        ("componentId", "fixture".into()),
        ("version", "1.0.0".into()),
        ("buildId", candidate.required("buildId").unwrap().clone()),
        (
            "archiveHash",
            candidate.required("archiveHash").unwrap().clone(),
        ),
        (
            "bytes",
            J::Number(
                fs::metadata(native(candidate.required("archivePath").unwrap()))
                    .unwrap()
                    .len() as f64,
            ),
        ),
        ("manifest", manifest),
    ]);
    ivy_host_runtime::package_validation::validate_package_authorization(&authorization).unwrap();
    for version in [
        J::Null,
        true.into(),
        J::Number(1.0),
        J::Array(vec!["1.0.0".into()]),
        J::object([("toString", "1.0.0".into())]),
    ] {
        let mut malformed = authorization.clone();
        malformed.insert("version", version);
        assert_eq!(
            ivy_host_runtime::package_validation::validate_package_authorization(&malformed)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    let journal = HostJournal::open(host.clone()).await.unwrap();
    journal
        .accept(
            J::object([
                ("operationId", "native-deploy".into()),
                ("instanceId", "fixture".into()),
                ("action", "deploy".into()),
                (
                    "candidateId",
                    candidate.required("candidateId").unwrap().clone(),
                ),
            ]),
            None,
        )
        .await
        .unwrap();
    let mut run = Running::start(&f, &own_path);
    accepted(&mut run, &journal, "native-deploy").await;
    let target = journal.target("fixture").await.unwrap().unwrap();
    let data = native(
        config::json_file(native(target.required("configPath").unwrap()), 65536)
            .await
            .unwrap()
            .required("dataRoot")
            .unwrap(),
    );
    let first = ready(&mut run, &data.join("health.json")).await;
    assert_eq!(first.get("launchId"), target.get("revision"));
    assert!(
        ExecutorLock::acquire(f.0.join("state/executor"), false)
            .await
            .is_err()
    );
    for (operation, action) in [
        ("native-restart", "restart"),
        ("native-disable", "disable"),
        ("native-enable", "enable"),
    ] {
        journal
            .accept(
                J::object([
                    ("operationId", operation.into()),
                    ("instanceId", "fixture".into()),
                    ("action", action.into()),
                ]),
                None,
            )
            .await
            .unwrap();
        accepted(&mut run, &journal, operation).await;
    }
    let last = ready(&mut run, &data.join("health.json")).await;
    assert_ne!(first.get("bootId"), last.get("bootId"));
    assert!(data.join("cooperative-stop.json").is_file());
    let host_health = f.0.join("host-data/health.json");
    stop(&mut run, &host_health).await;
    let lock = ExecutorLock::acquire(f.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
    // A fresh native Host must adopt a retained target without an unfinished job.
    let mut resumed = Running::start(&f, &own_path);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let next = ready(&mut resumed, &data.join("health.json")).await;
            if next.get("bootId") != last.get("bootId") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap();
    stop(&mut resumed, &host_health).await;
    journal.close().await.unwrap();
}
#[tokio::main]
async fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--build") {
        build(PathBuf::from(&args[1])).await;
        return;
    }
    if args.first().is_some_and(|arg| arg == "--child") {
        service().await;
        return;
    }
    if args.iter().any(|arg| arg == "--list") {
        println!("native_preparation_and_coordinator_lifecycle: test");
        return;
    }
    if cfg!(windows) {
        lifecycle().await;
    }
}
