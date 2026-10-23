//! Host lifecycle checks use a finite handler on the real SDK/generation/SQLite
//! path. Business policies and native agents are outside this fixture's scope.
use ivy_contracts::{
    IvyJson as Json, IvyResult, JsString,
    json::{parse, stringify},
};
use ivy_host_runtime::{config::InstanceConfig, health::HealthFile};
use ivy_sdk::{
    clock::SystemClock,
    rpc::{RequestOptions, RpcClient},
    service::{InvocationContext, ServiceConnection},
};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    generation::GenerationRuntime,
    host_service::PreparedTaskBoard,
    journal_store::Contracts,
    local_journal::{LocalJournal, initialize},
    schema::RuntimeContracts,
    service::{GenerationFactory, GenerationLaunch, ServiceSetup, service as make_service},
    service_bridge::LocalHandler,
};
use std::{
    cell::Cell,
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::Notify,
};

fn json(text: &str) -> Json {
    parse(text, 32 * 1024 * 1024).unwrap()
}
fn clock() -> JsString {
    "2026-01-02T03:04:05.006Z".into()
}
async fn bounded<T>(work: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), work)
        .await
        .expect("Host lifecycle did not settle")
}
async fn until(mut condition: impl FnMut() -> bool) {
    bounded(async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
}
fn contracts() -> RuntimeContracts {
    let root = PathBuf::from(
        std::env::var_os("IVY_TASK_BOARD_CURRENT_CONTRACTS")
            .expect("Supply the current packaged canonical schema directory"),
    );
    let documents = [
        "task-board",
        "chat",
        "agent",
        "hive-wire",
        "hive-operations",
    ]
    .iter()
    .map(|name| json(&std::fs::read_to_string(root.join(format!("{name}.schema.json"))).unwrap()))
    .collect();
    RuntimeContracts::from_documents(documents).unwrap()
}
fn settings() -> Json {
    json(
        r#"{"principalId":"fixture-principal","rootObjectId":null,"scheduler":{"enabled":false,"intervalMs":1000,"pageSize":50},"phoneTarget":null}"#,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn native_preparation_uses_host_configuration_without_a_node_runtime() {
    let contracts = contracts();
    for matches_build in [true, false] {
        let root = HostRoot::new();
        let artifact = root.0.join("artifact");
        std::fs::create_dir_all(artifact.join("dist")).unwrap();
        let build = Json::object([
            ("version", "1.0.0".into()),
            ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
        ]);
        std::fs::write(
            artifact.join("dist/build-info.json"),
            stringify(&build, 1024).unwrap(),
        )
        .unwrap();
        let config = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("hostId", "fixture-host".into()),
            ("instanceId", "fixture-task-board".into()),
            ("serviceNodeId", "fixture-task-board".into()),
            ("componentId", "task-board".into()),
            ("publicBaseUrl", "http://127.0.0.1:1/".into()),
            (
                "dataRoot",
                root.0.join("data").to_string_lossy().as_ref().into(),
            ),
            ("artifactRoot", artifact.to_string_lossy().as_ref().into()),
            ("buildId", build.required("buildId").unwrap().clone()),
            (
                "version",
                if matches_build {
                    "1.0.0".into()
                } else {
                    "different".into()
                },
            ),
            ("credential", "fixture-token".into()),
            ("settings", settings()),
        ]);
        let path = root.0.join("instance.json");
        std::fs::write(&path, stringify(&config, 32768).unwrap()).unwrap();
        let registry: ivy_sdk::service::Dependency<Json> =
            Arc::new(|| Box::pin(async { Ok(Json::Object(vec![])) }));
        let prepared =
            ivy_task_board::host_service::prepare(&path, &contracts, registry, vec![]).await;
        if matches_build {
            let prepared =
                prepared.unwrap_or_else(|error| panic!("Native preparation failed: {error:?}"));
            assert_eq!(prepared.config.json(), &config);
            assert!(!prepared.service.client.ready());
            assert!(prepared.config.data_root().is_dir());
        } else {
            assert_eq!(prepared.err().unwrap().code, "build_mismatch");
            assert!(!root.0.join("data").exists());
        }
    }
}

struct HostRoot(PathBuf);
impl HostRoot {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("task-board-runtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn health(&self) -> Option<Json> {
        std::fs::read_to_string(self.0.join("health.json"))
            .ok()
            .map(|s| json(&s))
    }
}
impl Drop for HostRoot {
    fn drop(&mut self) {
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("task-board-runtime-")
        );
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Factory {
    held: Cell<bool>,
    entered: Cell<bool>,
    release: Notify,
    stopping: Cell<bool>,
    finished: Cell<bool>,
    writes: Cell<usize>,
}
struct Runtime<'a> {
    factory: &'a Factory,
    connection: &'a ServiceConnection,
}
impl GenerationRuntime for Runtime<'_> {
    fn recovered(&self) -> bool {
        true
    }
    fn diagnostics(&self) -> Vec<Json> {
        vec![]
    }
    fn task_changed(&self) {}
    fn notification(&self, _: &Json) -> bool {
        false
    }
    async fn verify_owner(&self) -> IvyResult<()> {
        let owner = self
            .connection
            .request(
                "serviceNodes.get",
                Json::object([("serviceNodeId", "fixture-task-board".into())]),
                RequestOptions::default(),
            )
            .await?;
        assert_eq!(owner.get("principalId"), Some(&"fixture-principal".into()));
        Ok(())
    }
    async fn tick(&self) {}
    fn stop_admissions(&self) {
        self.factory.stopping.set(true);
    }
    async fn stop(&self) {
        assert!(self.factory.stopping.get());
    }
}
struct Public<'a> {
    factory: &'a Factory,
    journal: LocalJournal<'a>,
}
impl LocalHandler for Public<'_> {
    async fn handle(&self, tool: &str, args: Json, _: InvocationContext) -> IvyResult<Json> {
        assert_eq!(tool, "workspace");
        if self.factory.held.get() {
            self.factory.entered.set(true);
            self.factory.release.notified().await;
        }
        let name: JsString = format!("call-{}", self.factory.writes.get()).into();
        let pin = self.journal.create(
            "host-runtime/operation",
            &name,
            None,
            &args,
            &clock(),
            &uuid::Uuid::new_v4().to_string().into(),
        )?;
        let saved = self.journal.read("host-runtime/operation", &pin, true)?;
        self.factory.writes.set(self.factory.writes.get() + 1);
        Ok(saved.value)
    }
}
impl GenerationFactory for Factory {
    async fn run(&self, launch: GenerationLaunch) -> IvyResult<()> {
        let GenerationLaunch {
            connection,
            control,
            generation,
            activation,
        } = launch;
        let database = Connection::open_in_memory().unwrap();
        initialize(&database)?;
        let public = Public {
            factory: self,
            journal: LocalJournal {
                connection: &database,
                root_object_id: None,
            },
        };
        let runtime = Runtime {
            factory: self,
            connection: &connection,
        };
        let result = activation
            .run(
                &control,
                generation,
                &public,
                &runtime,
                &SystemClock,
                async { Ok(()) },
            )
            .await;
        drop(public);
        database.close().unwrap();
        self.finished.set(true);
        result
    }
}

#[derive(Clone, Copy)]
enum Stop {
    Signal,
    Control,
}
async fn exercise(stop: Stop) {
    // Reuse the existing bounded loopback peer; no account or live Hive state.
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-service-peer.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let peer = json(&bounded(lines.next_line()).await.unwrap().unwrap());
    let reader = tokio::spawn(async move { while lines.next_line().await.unwrap().is_some() {} });
    let registry = peer.required("registry").unwrap().clone();
    let root = HostRoot::new();
    let contracts = contracts();
    let setup = ServiceSetup {
        component_id: "task-board".into(),
        public_base_url: peer.required("url").unwrap().as_str().unwrap(),
        credential: "fixture-token".into(),
        service_node_id: "fixture-task-board".into(),
        host_id: Some("fixture-host".into()),
        version: "1.0.0".into(),
        build_id: format!("sha256:{}", "a".repeat(64)).into(),
        settings: settings(),
        registry: Arc::new(move || {
            let value = registry.clone();
            Box::pin(async move { Ok(value) })
        }),
        tools: vec!["workspace".into()],
    };
    let config = InstanceConfig::from_json(Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("hostId", "fixture-host".into()),
        ("instanceId", "fixture-task-board".into()),
        ("serviceNodeId", Json::String(setup.service_node_id.clone())),
        ("componentId", Json::String(setup.component_id.clone())),
        ("publicBaseUrl", setup.public_base_url.clone().into()),
        ("dataRoot", root.0.to_string_lossy().as_ref().into()),
        (
            "artifactRoot",
            root.0.join("artifact").to_string_lossy().as_ref().into(),
        ),
        ("buildId", Json::String(setup.build_id.clone())),
        ("version", Json::String(setup.version.clone())),
        ("credential", Json::String(setup.credential.clone())),
        ("settings", setup.settings.clone()),
    ]))
    .unwrap();
    let health = Arc::new(HealthFile::with_launch_id(
        config.clone(),
        Some("fixture-launch".into()),
    ));
    let (service, owner) = make_service(setup, &contracts).unwrap();
    let client = service.client.clone();
    let control = service.control.clone();
    let host = PreparedTaskBoard {
        config,
        health: health.clone(),
        service,
        owner,
    };
    let factory = Factory::default();
    let shutdown = Notify::new();
    let completed = Cell::new(false);
    let run = async {
        let result = host.run(&factory, shutdown.notified()).await;
        completed.set(true);
        result
    };
    let script = async {
        let connection = bounded(client.wait_ready(None)).await.unwrap();
        until(|| {
            root.health()
                .is_some_and(|v| v.get("ready") == Some(&Json::Bool(true)))
        })
        .await;
        let running = root.health().unwrap();
        assert_eq!(running.get("generation"), Some(&Json::Number(1.0)));
        assert_eq!(running.get("bootId"), Some(&health.boot_id().into()));
        assert_eq!(running.get("launchId"), Some(&"fixture-launch".into()));
        let response = connection
            .request(
                "test.invoke",
                Json::object([("value", "SDK to SQLite".into())]),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.required("result").unwrap(),
            &Json::object([("value", "SDK to SQLite".into())])
        );
        assert_eq!(factory.writes.get(), 1);

        factory.held.set(true);
        let call = control.handler("workspace")(
            Json::Null,
            InvocationContext::detached(
                "caller".into(),
                Some("held-call".into()),
                1,
                RequestOptions::default().cancellation,
                ivy_sdk::IvyError::new("service_unavailable", "Service connection is stopping.")
                    .with_outcome(ivy_sdk::Outcome::Unknown),
            ),
        );
        let stop_and_release = async {
            until(|| factory.entered.get()).await;
            match stop {
                Stop::Signal => shutdown.notify_one(),
                Stop::Control => {
                    let path = root.0.join("control.json");
                    let command = |boot: &str| {
                        Json::object([
                            ("schemaVersion", Json::Number(1.0)),
                            ("instanceId", "fixture-task-board".into()),
                            ("bootId", boot.into()),
                            ("launchId", "fixture-launch".into()),
                            ("action", "shutdown".into()),
                            ("requestedAt", Json::String(clock())),
                        ])
                    };
                    std::fs::write(&path, "{").unwrap();
                    tokio::time::sleep(Duration::from_millis(650)).await;
                    assert!(client.ready(), "Malformed control must not stop this owner");
                    std::fs::write(&path, stringify(&command("another-boot"), 4096).unwrap())
                        .unwrap();
                    tokio::time::sleep(Duration::from_millis(650)).await;
                    assert!(client.ready(), "Another boot cannot stop this owner");
                    std::fs::write(&path, stringify(&command(health.boot_id()), 4096).unwrap())
                        .unwrap();
                }
            }
            until(|| !client.ready()).await;
            assert!(!completed.get());
            assert!(!factory.finished.get());
            factory.release.notify_one();
        };
        let (value, ()) = tokio::join!(call, stop_and_release);
        assert!(value.unwrap().is_null());
    };
    let (result, ()) = tokio::join!(run, script);
    result.unwrap();
    assert!(completed.get());
    assert!(factory.finished.get());
    assert!(factory.stopping.get());
    assert_eq!(factory.writes.get(), 2);
    let stopped = root.health().unwrap();
    assert_eq!(stopped.get("ready"), Some(&Json::Bool(false)));
    assert_eq!(stopped.get("generation"), Some(&Json::Null));
    assert_eq!(stopped.get("details"), Some(&"TaskBoard stopped.".into()));
    assert_eq!(stopped.get("bootId"), Some(&health.boot_id().into()));
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    bounded(reader).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn host_signal_drains_accepted_work_before_releasing_sqlite() {
    exercise(Stop::Signal).await;
}
#[tokio::test(flavor = "current_thread")]
async fn host_control_ignores_invalid_and_foreign_commands_then_drains() {
    exercise(Stop::Control).await;
}

#[test]
fn native_contracts_keep_public_versions_and_local_workflow_routing() {
    let contracts = contracts();
    let versions = contracts.readable_versions("task-board/task").unwrap();
    let expected: Vec<JsString> = std::iter::once("1.0.0".into())
        .chain((1..=12).map(|minor| format!("1.{minor}.0").into()))
        .chain((0..=3).map(|minor| format!("2.{minor}.0").into()))
        .collect();
    assert_eq!(versions, expected);
    for version in versions {
        assert!(
            contracts
                .readable_version("task-board/task", version)
                .unwrap()
        );
    }
    assert_eq!(
        contracts
            .readable_versions("task-board/native-call")
            .unwrap(),
        &[JsString::from("1.0.0")]
    );
    assert_eq!(
        contracts.readable_versions("task-board/comments").unwrap(),
        &[
            "1.0.0".into(),
            "1.1.0".into(),
            "2.0.0".into(),
            "2.1.0".into()
        ]
    );
    assert!(contracts.readable_versions("unknown").is_err());
    assert_eq!(contracts.version("task-board/task").unwrap(), "2.3.0");
    assert_eq!(contracts.version("task-board/run").unwrap(), "2.2.0");
    assert_eq!(contracts.version("task-board/result").unwrap(), "2.2.0");
    assert_eq!(contracts.version("task-board/comments").unwrap(), "1.0.0");
    assert!(
        contracts
            .readable_version("task-board/comments", &"2.1.0".into())
            .unwrap()
    );
    assert!(!contracts.local("task-board/task").unwrap());
    assert!(contracts.local("task-board/operation").unwrap());
    assert!(
        contracts
            .readable_version("task-board/task", &"1.0.0".into())
            .unwrap()
    );
    assert!(
        !contracts
            .readable_version("task-board/task", &"3.0.0".into())
            .unwrap()
    );
    contracts
        .validate_definition("Settings", &settings())
        .unwrap();
    assert_eq!(
        contracts
            .validate_definition("Settings", &Json::Null)
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let sequence =
        json(r#"{"schemaVersion":1,"nextValue":2,"updatedAt":"2026-01-02T03:04:05.006Z"}"#);
    contracts
        .validate_record("task-board/task-key-sequence", &"1.0.0".into(), &sequence)
        .unwrap();
    assert!(
        contracts
            .validate_record("task-board/task-key-sequence", &"2.0.0".into(), &sequence)
            .is_err()
    );
    assert!(
        contracts
            .validate_record("task-board/operation", &"1.0.0".into(), &sequence)
            .is_err()
    );
    let mut task = json(
        r#"{
        "schemaVersion":1,"taskKey":"TASK-0001",
        "fields":{"title":"Manual task","description":"Manual work","acceptanceCriteria":[],"category":null,
            "control":"user","priority":2,"executionRequirement":null,"workspaceRequirement":{"kind":"directory_path","path":"/fixture"},"userContact":"ticket","dueAt":null},
        "workflowState":"todo","publication":null,"primaryResourceRef":null,"claim":null,"attemptCount":0,
        "lastRun":null,"lastHistory":null,"latestResult":null,"acceptedReview":null,"comments":[],"commentDeliveries":[],
        "agentCommentCount":0,"workRevision":0,"attachments":[],"createdAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z",
        "nextSteps":[],"runtimeWait":null
    }"#,
    );
    contracts
        .validate_record("task-board/task", &"2.3.0".into(), &task)
        .unwrap();
    task.insert("taskKey", "invalid".into());
    assert!(
        contracts
            .validate_record("task-board/task", &"2.3.0".into(), &task)
            .is_err()
    );
    task.insert("taskKey", "TASK-0001".into());
    task.remove("nextSteps");
    assert!(
        contracts
            .validate_record("task-board/task", &"2.3.0".into(), &task)
            .is_err()
    );
}
