use super::*;
use ivy_host_runtime::process::{self, Command as HostCommand, LaunchContext, OwnedProcess};
use ivy_sdk::rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient};
use std::{collections::BTreeMap, ffi::OsString, process::ExitCode};

const DRIVER: &str = "lifecycle::entrypoint::entrypoint_fixture_process";

#[path = "data-collector-transfer.inc.rs"]
mod transfer;

#[tokio::test]
async fn entrypoint_fixture_process() {
    if std::env::var_os("IVY_TEST_COLLECTOR_ENTRYPOINT").is_none() {
        return;
    }
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if let Ok(path) =
        ivy_host_runtime::config::configuration_path(&args, std::env::var_os("IVY_INSTANCE_CONFIG"))
    {
        atomic_json(
            path.parent().unwrap().join("entry-identity.json"),
            IvyJson::object([
                ("pid", IvyJson::Number(std::process::id() as f64)),
                (
                    "node",
                    std::env::var("IVY_NODE_EXECUTABLE")
                        .unwrap_or_default()
                        .into(),
                ),
                (
                    "executable",
                    std::env::current_exe()
                        .unwrap()
                        .to_string_lossy()
                        .to_string()
                        .into(),
                ),
            ]),
        )
        .await
        .unwrap();
    }
    // This executable is a test driver, not a production default-locale binding.
    let result = ivy_data_collector::entrypoint::main(Arc::new(|a, b| a.0.cmp(&b.0))).await;
    std::process::exit(if result == ExitCode::SUCCESS { 0 } else { 1 });
}

fn platform() -> &'static str {
    if cfg!(windows) { "win32" } else { "linux" }
}
fn binary(name: &str) -> String {
    format!("{name}{}", if cfg!(windows) { ".exe" } else { "" })
}
fn payload(fixture: &Fixture) -> PathBuf {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    fixture
        .root
        .join(format!("artifact/dist/native/{}-{arch}", platform()))
}
async fn stage(fixture: &Fixture) {
    let status = Command::new(node())
        .arg("tests/fixtures/data-collector-dispatchers.mjs")
        .arg(fixture.root.join("artifact/dist"))
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .status()
        .await
        .unwrap();
    assert!(status.success());
    let target = payload(fixture);
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(
        std::env::current_exe().unwrap(),
        target.join(binary("ivy-data-collector")),
    )
    .unwrap();
    std::fs::copy(
        std::env::var_os("IVY_TEST_HEALTH_CHECKER").expect("compiled Host health helper"),
        target.join(binary("ivy-health")),
    )
    .unwrap();
    if cfg!(windows) {
        std::fs::copy(
            std::env::var_os("IVY_TEST_JOB_LAUNCHER").unwrap(),
            target.join("ivy-job.exe"),
        )
        .unwrap();
        // Workloads must use the current platform payload, not the old helper location.
        std::fs::remove_file(fixture.root.join("artifact/dist/native/ivy-job.exe")).unwrap();
    }
}
async fn launch(fixture: &Fixture, health: bool, environment_config: bool) -> OwnedProcess {
    launch_bounded(fixture, health, environment_config, 30_000).await
}
async fn launch_bounded(
    fixture: &Fixture,
    health: bool,
    environment_config: bool,
    timeout_ms: u64,
) -> OwnedProcess {
    let entry = if health {
        "packages/host-runtime/src/health.js"
    } else {
        "services/data-collector/src/main.js"
    };
    let mut args: Vec<OsString> = vec![fixture.root.join("artifact/dist").join(entry).into()];
    if !health {
        args.extend(["--exact", DRIVER, "--nocapture", "--"].map(OsString::from));
    }
    if !environment_config {
        args.extend(["--config".into(), fixture.root.join("config.json").into()]);
    }
    let environment = BTreeMap::from([
        ("IVY_TEST_COLLECTOR_ENTRYPOINT".into(), "1".into()),
        ("IVY_LAUNCH_ID".into(), "collector-dispatch-fixture".into()),
        (
            "IVY_NODE_EXECUTABLE".into(),
            fixture.root.join("untrusted-node.exe").into(),
        ),
        (
            "IVY_INSTANCE_CONFIG".into(),
            fixture
                .root
                .join(if environment_config {
                    "config.json"
                } else {
                    "unused.json"
                })
                .into(),
        ),
    ]);
    process::spawn(
        &HostCommand {
            executable: "node".into(),
            args,
            cwd: None,
            timeout_ms,
        },
        &fixture.root.join("artifact"),
        &BTreeMap::from([("node".into(), node())]),
        LaunchContext {
            environment,
            job_launcher: std::env::var_os("IVY_TEST_JOB_LAUNCHER").map(PathBuf::from),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
async fn complete(process: &OwnedProcess) -> Arc<process::ProcessResult> {
    tokio::time::timeout(Duration::from_secs(12), process.completion.wait())
        .await
        .unwrap()
}
async fn health(fixture: &Fixture, ok: bool) -> IvyJson {
    let child = launch(fixture, true, false).await;
    let result = complete(&child).await;
    assert_eq!(result.exit_code, Some(if ok { 0 } else { 1 }), "{result:?}");
    assert_eq!(result.error_code, None, "{result:?}");
    assert_eq!(result.stderr, "", "{result:?}");
    let value = parse(result.stdout.trim(), 1024).unwrap();
    assert_eq!(value.get("ok"), Some(&ok.into()));
    value
}
async fn ready(
    fixture: &Fixture,
    child: &OwnedProcess,
    previous_boot: Option<&IvyJson>,
) -> IvyJson {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(result) = child.completion.current() {
                panic!("entry process exited: {result:?}");
            }
            if let Some(value) = fixture.health() {
                if value.get("ready") == Some(&true.into())
                    && value.get("bootId") != previous_boot
                    && value.get("details") != Some(&"DataCollector stopped.".into())
                {
                    return value;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn native_dispatcher_composes_process_entry_health_control_and_workload_helpers() {
    tokio::time::timeout(Duration::from_secs(90), async {
        for offline in [false, true] {
            let mut peer = None;
            let fixture = Fixture::new(offline).await;
            let mut config = fixture.config.clone();
            if !offline {
                let mut child = Command::new(node())
                    .args([
                        "--experimental-transform-types",
                        "--disable-warning=ExperimentalWarning",
                        "tests/fixtures/data-collector-provider-peer.mjs",
                        "--lifecycle",
                    ])
                    .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                let mut line = String::new();
                BufReader::new(child.stdout.take().unwrap())
                    .read_line(&mut line)
                    .await
                    .unwrap();
                config.insert(
                    "publicBaseUrl",
                    parse(&line, usize::MAX)
                        .unwrap()
                        .required("url")
                        .unwrap()
                        .clone(),
                );
                peer = Some(child);
            }
            stage(&fixture).await;
            // Exercise real startup failure and argument-over-environment precedence.
            let mut invalid = config.clone();
            invalid.insert("componentId", "other".into());
            atomic_json(fixture.root.join("config.json"), invalid)
                .await
                .unwrap();
            let rejected = launch(&fixture, false, false).await;
            let result = complete(&rejected).await;
            assert_eq!(result.exit_code, Some(1), "{result:?}");
            assert_eq!(
                parse(result.stderr.trim(), 1024).unwrap(),
                parse(
                    r#"{"code":"invalid_arguments","message":"DataCollector failed to start."}"#,
                    1024
                )
                .unwrap()
            );
            assert!(fixture.health().is_none());
            atomic_json(fixture.root.join("config.json"), config.clone())
                .await
                .unwrap();
            let mut previous = None;
            for environment_config in [false, true] {
                let child = launch(&fixture, false, environment_config).await;
                let value = ready(&fixture, &child, previous.as_ref()).await;
                let identity = parse(
                    &std::fs::read_to_string(fixture.root.join("entry-identity.json")).unwrap(),
                    usize::MAX,
                )
                .unwrap();
                assert_eq!(value.get("pid"), identity.get("pid"));
                assert_ne!(model::number(&value, "pid").unwrap() as u32, child.pid);
                assert_eq!(
                    value.get("launchId"),
                    Some(&"collector-dispatch-fixture".into())
                );
                assert_eq!(
                    std::fs::canonicalize(model::text(&identity, "node").unwrap()).unwrap(),
                    std::fs::canonicalize(node()).unwrap()
                );
                assert_eq!(
                    std::fs::canonicalize(model::text(&identity, "executable").unwrap()).unwrap(),
                    std::fs::canonicalize(payload(&fixture).join(binary("ivy-data-collector")))
                        .unwrap()
                );
                if offline {
                    assert_eq!(value.get("generation"), Some(&IvyJson::Null));
                    assert_eq!(
                        health(&fixture, false).await.get("code"),
                        Some(&"outcome_unknown".into())
                    );
                } else {
                    let checked = health(&fixture, true).await;
                    assert_eq!(checked.get("buildId"), value.get("buildId"));
                    assert_eq!(checked.get("generation"), value.get("generation"));
                    let client = HiveClient::new(
                        &model::text(&config, "publicBaseUrl").unwrap(),
                        ClientOptions {
                            credential: Some("synthetic-only".into()),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    client
                        .request(
                            "test.registration",
                            IvyJson::object([("ready", false.into())]),
                            RequestOptions::default(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        health(&fixture, false).await.get("code"),
                        Some(&"service_not_ready".into())
                    );
                    client
                        .request(
                            "test.registration",
                            IvyJson::object([("ready", true.into())]),
                            RequestOptions::default(),
                        )
                        .await
                        .unwrap();
                    let mut stale = value.clone();
                    stale.insert("observedAt", "2020-01-01T00:00:00.000Z".into());
                    atomic_json(fixture.root.join("data/health.json"), stale)
                        .await
                        .unwrap();
                    assert_eq!(
                        health(&fixture, false).await.get("code"),
                        Some(&"service_not_ready".into())
                    );
                    atomic_json(fixture.root.join("data/health.json"), value.clone())
                        .await
                        .unwrap();
                    health(&fixture, true).await;
                }
                fixture.control(&value, false).await;
                tokio::time::sleep(Duration::from_millis(650)).await;
                assert!(child.completion.current().is_none());
                fixture.control(&value, true).await;
                let result = complete(&child).await;
                assert_eq!(result.exit_code, Some(0), "{result:?}");
                assert_eq!(result.error_code, None, "{result:?}");
                assert_eq!(result.stderr, "", "{result:?}");
                let stopped = fixture.wait_health(true).await;
                assert_eq!(stopped.get("pid"), value.get("pid"));
                assert_eq!(stopped.get("ready"), Some(&false.into()));
                assert_eq!(stopped.get("bootId"), value.get("bootId"));
                previous = stopped.get("bootId").cloned();
                drop(std::net::TcpListener::bind(("127.0.0.1", fixture.port)).unwrap());
            }
            if offline {
                assert_eq!(
                    std::fs::read_to_string(fixture.root.join("work/tasks/fixture/marker"))
                        .unwrap(),
                    "once\n"
                );
                let store = CollectorStore::open(
                    &fixture.root.join("data/data-collector.sqlite"),
                    Limits::default(),
                    "2026-01-01T00:00:00.000Z",
                )
                .unwrap();
                assert_eq!(store.runs("publishing").unwrap().len(), 1);
                assert_eq!(
                    store.get("fixture", false).unwrap().unwrap().get("state"),
                    Some(&IvyJson::Number(7.0))
                );
            }
            std::fs::remove_file(payload(&fixture).join(binary("ivy-health"))).unwrap();
            assert_eq!(
                health(&fixture, false).await.get("code"),
                Some(&"artifact_missing".into())
            );
            if let Some(mut peer) = peer {
                peer.kill().await.unwrap();
                peer.wait().await.unwrap();
            }
        }
    })
    .await
    .unwrap();
}
