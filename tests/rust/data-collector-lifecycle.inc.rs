use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_data_collector::{
    lifecycle::{self, StartOptions},
    model,
    runtime::Runtime,
    schema,
    store::{CollectorStore, Limits},
};
use ivy_host_runtime::config::atomic_json;
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

#[path = "data-collector-entrypoint.inc.rs"]
mod entrypoint;

struct Fixture {
    root: PathBuf,
    config: IvyJson,
    port: u16,
}
impl Fixture {
    async fn new(seed: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy collector lifecycle {}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("artifact/dist/native")).unwrap();
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let offline = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/ivy/",
            offline.local_addr().unwrap().port()
        );
        drop(offline);
        let config = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("componentId", "data-collector".into()),
            ("hostId", "host".into()),
            ("instanceId", "collector".into()),
            ("serviceNodeId", "host.collector".into()),
            ("publicBaseUrl", url.into()),
            ("credential", "synthetic-only".into()),
            (
                "dataRoot",
                root.join("data").to_string_lossy().to_string().into(),
            ),
            (
                "artifactRoot",
                root.join("artifact").to_string_lossy().to_string().into(),
            ),
            (
                "workRoot",
                root.join("work").to_string_lossy().to_string().into(),
            ),
            ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
            ("version", "1.0.0".into()),
            (
                "settings",
                IvyJson::object([(
                    "ingress",
                    IvyJson::object([
                        ("host", "127.0.0.1".into()),
                        ("port", IvyJson::Number(port as f64)),
                    ]),
                )]),
            ),
        ]);
        atomic_json(root.join("config.json"), config.clone())
            .await
            .unwrap();
        atomic_json(
            root.join("artifact/dist/build-info.json"),
            IvyJson::object([
                ("version", config.required("version").unwrap().clone()),
                ("buildId", config.required("buildId").unwrap().clone()),
            ]),
        )
        .await
        .unwrap();
        if cfg!(windows) {
            std::fs::copy(
                std::env::var_os("IVY_TEST_JOB_LAUNCHER").expect("qualified helper"),
                root.join("artifact/dist/native/ivy-job.exe"),
            )
            .unwrap();
        }
        if seed {
            let store = CollectorStore::open(
                &root.join("data/data-collector.sqlite"),
                Limits::default(),
                "2026-01-01T00:00:00.000Z",
            )
            .unwrap();
            let settings =
                schema::parse_schema("settingsSchema", config.required("settings").unwrap())
                    .unwrap();
            let mut task=parse(r#"{"id":"fixture","name":"Fixture","enabled":true,"intervalSeconds":3600,"timeoutSeconds":10,"memoryMb":64,"dependencies":{},"config":{},"secretNames":[],"allowUnauthenticatedInput":true,"script":"import {appendFileSync} from 'node:fs'; export default()=>{appendFileSync('marker','once\\n');return {data:{offline:true},state:7};}"}"#,usize::MAX).unwrap();
            task.insert("retention", settings.required("retention").unwrap().clone());
            Runtime {
                store: &store,
                settings: &settings,
                root_id: "",
            }
            .update(
                &IvyJson::object([
                    ("action", "save".into()),
                    ("operationId", "seed".into()),
                    ("expectedRevision", IvyJson::Number(0.0)),
                    ("task", task),
                ]),
                None,
                0,
                "2026-01-01T00:00:00.000Z",
                |_| false,
                |_| Ok(()),
            )
            .unwrap();
        }
        Self { root, config, port }
    }
    fn options(&self) -> StartOptions {
        StartOptions {
            node_executable: Some(node()),
            job_launcher: None,
        }
    }
    fn health(&self) -> Option<IvyJson> {
        std::fs::read_to_string(self.root.join("data/health.json"))
            .ok()
            .and_then(|s| parse(&s, usize::MAX).ok())
    }
    async fn wait_health(&self, stopped: bool) -> IvyJson {
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                if let Some(v) = self.health() {
                    if (v.get("details") == Some(&"DataCollector stopped.".into())) == stopped
                        && (stopped
                            || model::text(&v, "details")
                                .unwrap()
                                .starts_with("Hive unavailable"))
                    {
                        return v;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn control(&self, health: &IvyJson, correct: bool) {
        let mut value = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("instanceId", "collector".into()),
            (
                "bootId",
                if correct {
                    health.required("bootId").unwrap().clone()
                } else {
                    "foreign".into()
                },
            ),
            ("action", "shutdown".into()),
            ("requestedAt", "2026-01-01T00:00:00.000Z".into()),
        ]);
        if let Some(id) = health.get("launchId") {
            value.insert("launchId", id.clone());
        }
        atomic_json(self.root.join("data/control.json"), value)
            .await
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn node() -> PathBuf {
    let result = std::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(result.status.success());
    String::from_utf8(result.stdout).unwrap().trim().into()
}
async fn check_lifetime(fixture: &Fixture) -> IvyJson {
    let health = fixture.wait_health(false).await;
    assert_eq!(health.get("ready"), Some(&true.into()));
    assert_eq!(health.get("generation"), Some(&IvyJson::Null));
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if fixture.root.join("work/tasks/fixture/marker").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    // An old owner's control must not close this boot or its listener.
    fixture.control(&health, false).await;
    tokio::time::sleep(Duration::from_millis(650)).await;
    assert_ne!(
        fixture.health().unwrap().get("details"),
        Some(&"DataCollector stopped.".into())
    );
    let connection = tokio::net::TcpStream::connect(("127.0.0.1", fixture.port))
        .await
        .unwrap();
    fixture.control(&health, true).await;
    let stopped = fixture.wait_health(true).await;
    drop(connection);
    assert_eq!(stopped.get("ready"), Some(&false.into()));
    assert_eq!(stopped.get("bootId"), health.get("bootId"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("work/tasks/fixture/marker")).unwrap(),
        "once\n"
    );
    stopped
}
#[tokio::test]
async fn lifecycle_matches_original_offline_cache_worker_health_control_and_restart() {
    // Control files require the launch identity normally supplied by the host.
    // Isolate that environment from the other concurrently running test cases.
    if std::env::var_os("IVY_TEST_COLLECTOR_LIFETIME_CHILD").is_none() {
        let status = tokio::time::timeout(
            Duration::from_secs(60),
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "lifecycle::lifecycle_matches_original_offline_cache_worker_health_control_and_restart",
                    "--nocapture",
                ])
                .env("IVY_TEST_COLLECTOR_LIFETIME_CHILD", "1")
                .env("IVY_LAUNCH_ID", uuid::Uuid::new_v4().to_string())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(status.success());
        return;
    }
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut outcomes = Vec::new();
        for original in [true, false] {
            let fixture = Fixture::new(true).await;
            let mut process = None;
            let mut runtime = None;
            if original {
                let mut child = Command::new("node")
                    .args([
                        "--experimental-transform-types",
                        "--disable-warning=ExperimentalWarning",
                        "tests/fixtures/data-collector-lifecycle-original.mjs",
                    ])
                    .arg(fixture.root.join("config.json"))
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
                assert_eq!(line.trim(), r#"{"started":true}"#);
                process = Some(child);
            } else {
                runtime = Some(
                    lifecycle::start(fixture.root.join("config.json"), fixture.options())
                        .await
                        .unwrap(),
                );
            }
            let stopped = check_lifetime(&fixture).await;
            if let Some(mut process) = process {
                assert!(process.wait().await.unwrap().success());
            }
            if let Some(runtime) = runtime {
                runtime.wait_closed().await.unwrap();
                let (a, b) = tokio::join!(runtime.close(), runtime.close());
                a.unwrap();
                b.unwrap();
                assert_eq!(
                    runtime
                        .engine()
                        .store()
                        .call(|s| s.list(false))
                        .await
                        .unwrap_err()
                        .code,
                    "service_unavailable"
                );
            }
            // SQLite reopens under the old schema, with a durable offline result and cursor.
            let store = CollectorStore::open(
                &fixture.root.join("data/data-collector.sqlite"),
                Limits::default(),
                "2026-01-01T00:00:00.000Z",
            )
            .unwrap();
            let row = store.get("fixture", false).unwrap().unwrap();
            let pending = store.runs("publishing").unwrap();
            assert_eq!(pending.len(), 1);
            assert!(model::number(&row, "nextAt").unwrap() > 0.0);
            outcomes.push(IvyJson::object([
                ("state", row.required("state").unwrap().clone()),
                ("output", pending[0].required("output").unwrap().clone()),
                ("ready", stopped.required("ready").unwrap().clone()),
            ]));
            drop(store);
            // The prior boot control is ignored after restart; the durable cursor
            // prevents a second execution while still disconnected.
            let restarted = lifecycle::start(fixture.root.join("config.json"), fixture.options())
                .await
                .unwrap();
            let health = fixture.wait_health(false).await;
            assert_ne!(health.get("bootId"), stopped.get("bootId"));
            tokio::time::sleep(Duration::from_millis(650)).await;
            assert_eq!(
                std::fs::read_to_string(fixture.root.join("work/tasks/fixture/marker")).unwrap(),
                "once\n"
            );
            restarted.close().await.unwrap();
        }
        assert_eq!(
            stringify(&outcomes[0], usize::MAX).unwrap(),
            stringify(&outcomes[1], usize::MAX).unwrap()
        );
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn native_lifecycle_starts_without_a_workload_runtime() {
    let fixture = Fixture::new(false).await;
    let options = StartOptions {
        node_executable: Some(fixture.root.join("missing-node.exe")),
        job_launcher: None,
    };
    let collector = lifecycle::start(fixture.root.join("config.json"), options)
        .await
        .unwrap();
    fixture.wait_health(false).await;
    assert!(fixture.root.join("data/data-collector.sqlite").is_file());
    collector.close().await.unwrap();
    assert_eq!(
        fixture.wait_health(true).await.get("ready"),
        Some(&false.into())
    );
}

#[tokio::test]
async fn lifecycle_failed_health_cleanup_retires_listener_and_database() {
    let fixture = Fixture::new(false).await;
    let runtime = lifecycle::start(fixture.root.join("config.json"), fixture.options())
        .await
        .unwrap();
    let health = fixture.wait_health(false).await;
    assert_eq!(health.get("ready"), Some(&false.into()));
    // A real filesystem error must not prevent the remaining cleanup.
    std::fs::remove_file(fixture.root.join("data/health.json")).unwrap();
    std::fs::create_dir(fixture.root.join("data/health.json")).unwrap();
    let mut closing = Box::pin(runtime.close());
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(closing.as_mut().poll(cx).is_pending()))
            .await
    );
    drop(closing);
    assert!(runtime.wait_closed().await.is_err());
    assert_eq!(
        runtime
            .engine()
            .store()
            .call(|s| s.list(false))
            .await
            .unwrap_err()
            .code,
        "service_unavailable"
    );
    let listener = std::net::TcpListener::bind(("127.0.0.1", fixture.port)).unwrap();
    drop(listener);
    // Startup identity rejection happens before opening the listener.
    let mut wrong = fixture.config.clone();
    wrong.insert("componentId", "other".into());
    atomic_json(fixture.root.join("config.json"), wrong)
        .await
        .unwrap();
    assert_eq!(
        lifecycle::start(fixture.root.join("config.json"), fixture.options())
            .await
            .err()
            .unwrap()
            .code,
        "invalid_arguments"
    );
}

#[tokio::test]
async fn lifecycle_reconciles_real_backend_before_health_and_recovers_connection() {
    use ivy_sdk::rpc::{RequestOptions, RpcClient};
    tokio::time::timeout(Duration::from_secs(25), async {
        let mut peer = Command::new("node")
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
        BufReader::new(peer.stdout.take().unwrap())
            .read_line(&mut line)
            .await
            .unwrap();
        let info = parse(&line, usize::MAX).unwrap();
        let fixture = Fixture::new(false).await;
        let mut config = fixture.config.clone();
        config.insert("publicBaseUrl", info.required("url").unwrap().clone());
        atomic_json(fixture.root.join("config.json"), config)
            .await
            .unwrap();
        let runtime = lifecycle::start(fixture.root.join("config.json"), fixture.options())
            .await
            .unwrap();
        let connection = runtime.service().wait_ready(None).await.unwrap();
        let health = fixture.health().unwrap();
        assert_eq!(health.get("ready"), Some(&true.into()));
        assert_eq!(
            health.get("generation"),
            Some(&IvyJson::Number(connection.generation() as f64))
        );
        assert_eq!(
            health.get("details"),
            Some(&"Collector storage, scripts and result routing are ready.".into())
        );
        let empty = IvyJson::object(Vec::<(String, IvyJson)>::new());
        let trace = connection
            .request("test.info", empty.clone(), RequestOptions::default())
            .await
            .unwrap();
        let calls = trace.required("trace").unwrap().as_array().unwrap();
        let first = |name: &str| {
            calls
                .iter()
                .position(|v| v == &IvyJson::from(name))
                .unwrap()
        };
        assert!(first("objects.query") < first("service.heartbeat"));
        let result = connection
            .request(
                "test.invoke",
                parse(r#"{"tool":"read","input":{"view":"tasks"}}"#, usize::MAX).unwrap(),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            result
                .required("result")
                .unwrap()
                .required("items")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            0
        );
        connection
            .request("test.close", empty, RequestOptions::default())
            .await
            .unwrap();
        loop {
            if runtime
                .service()
                .connection()
                .is_ok_and(|c| c.generation() > connection.generation())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            fixture
                .health()
                .unwrap()
                .required("generation")
                .unwrap()
                .as_f64()
                .unwrap()
                > connection.generation() as f64
        );
        runtime.close().await.unwrap();
        peer.kill().await.unwrap();
        peer.wait().await.unwrap();
    })
    .await
    .unwrap();
}
