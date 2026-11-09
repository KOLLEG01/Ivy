//! Native owner preparation, registered SDK calls and shutdown with packaged
//! current inputs. These cases exercise an empty workflow and a workspace call.
use super::*;
use ivy_contracts::json::stringify;
use ivy_task_board::native_runtime::{NativeRuntime, NativeRuntimeInputs};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[path = "task-board-native-process.rs"]
mod process;

struct PromptOwner {
    alive: Arc<AtomicBool>,
    drop_thread: Arc<std::sync::Mutex<Option<std::thread::ThreadId>>>,
}
impl Drop for PromptOwner {
    fn drop(&mut self) {
        *self.drop_thread.lock().unwrap() = Some(std::thread::current().id());
        self.alive.store(false, Ordering::Release);
    }
}
impl TaskPrompts for PromptOwner {
    fn task_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        flag: Option<&Json>,
    ) -> IvyResult<JsString> {
        ivy_task_board::prompts::CurrentTaskPrompts.task_prompt(task, owner, flag)
    }
    fn task_update_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        changed: bool,
    ) -> IvyResult<JsString> {
        ivy_task_board::prompts::CurrentTaskPrompts.task_update_prompt(task, owner, changed)
    }
}

fn instance(root: &Root, matching_build: bool, component: &str) -> (PathBuf, Json) {
    let artifact = root.0.join("configured artifact");
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
        ("componentId", component.into()),
        ("publicBaseUrl", "http://127.0.0.1:1/".into()),
        (
            "dataRoot",
            root.0
                .join("configured data")
                .to_string_lossy()
                .as_ref()
                .into(),
        ),
        ("artifactRoot", artifact.to_string_lossy().as_ref().into()),
        ("buildId", build.required("buildId").unwrap().clone()),
        (
            "version",
            if matching_build { "1.0.0" } else { "mismatch" }.into(),
        ),
        ("credential", "fixture-token".into()),
        ("settings", settings()),
    ]);
    let path = root.0.join("instance.json");
    std::fs::write(&path, stringify(&config, 32768).unwrap()).unwrap();
    (path, config)
}

fn inputs(root: &Root, prompts: PromptOwner) -> NativeRuntimeInputs<PromptOwner> {
    let packaged = ivy_task_board::runtime_inputs::packaged(
        root.0.join("configured artifact"),
        LocaleComparator::system().unwrap(),
    )
    .unwrap();
    NativeRuntimeInputs {
        contracts: packaged.contracts,
        catalogs: packaged.catalogs,
        registry: packaged.registry,
        locale: packaged.locale,
        prompts,
        chat_readable_versions: packaged.chat_readable_versions,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite_write_wait_keeps_actual_sdk_readiness_and_heartbeats_responsive() {
    let peer = Peer::start_mode(Some("--native-policies")).await;
    let root = Root::new();
    let (path, mut config) = instance(&root, true, "task-board");
    config.insert("publicBaseUrl", peer.url.clone().into());
    std::fs::write(&path, stringify(&config, 32768).unwrap()).unwrap();
    let alive = Arc::new(AtomicBool::new(true));
    let mut inputs = inputs(
        &root,
        PromptOwner {
            alive: alive.clone(),
            drop_thread: Arc::new(std::sync::Mutex::new(None)),
        },
    );
    inputs.catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let catalog = inputs
        .catalogs
        .checked_native_contract(&"0.159.2".into(), None)
        .unwrap();
    let draft = Json::object([
        ("nativeVersion", "0.159.2".into()),
        (
            "catalogSourceHash",
            catalog.catalog().required("sourceHash").unwrap().clone(),
        ),
        (
            "definitions",
            Json::object(
                [
                    ("threadStart", "thread/start"),
                    ("threadResume", "thread/resume"),
                    ("turnStart", "turn/start"),
                    ("turnInterrupt", "turn/interrupt"),
                ]
                .into_iter()
                .map(|(key, method)| {
                    (
                        key,
                        Json::from(catalog.definition_hash(&method.into()).unwrap()),
                    )
                }),
            ),
        ),
        ("threadStart", Json::Object(vec![])),
        ("threadResume", Json::Object(vec![])),
        ("turnStart", Json::object([("input", Json::Array(vec![]))])),
    ]);
    let runtime = NativeRuntime::prepare(&path, inputs).await.unwrap();
    let client = runtime.host.service.client.clone();
    let control = runtime.host.service.control.clone();
    let data = runtime.host.config.data_root().to_owned();
    let shutdown = Notify::new();
    let script = async {
        let connection = bounded(client.wait_ready(None)).await.unwrap();
        let blocker = Connection::open(data.join("task-board.sqlite")).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let heartbeats = || {
            peer.methods(1)
                .iter()
                .filter(|method| method.as_str() == "service.heartbeat")
                .count()
        };
        let before = heartbeats();
        let request = Json::object([
            ("name", "savePlan".into()),
            ("operationId", "sqlite-busy-plan".into()),
            (
                "arguments",
                Json::object([
                    ("action", "savePlan".into()),
                    ("operationId", "sqlite-busy-plan".into()),
                    ("plan", draft),
                ]),
            ),
        ]);
        let mut reply = Box::pin(connection.request(
            "test.invokeRegistered",
            request.clone(),
            RequestOptions::default(),
        ));
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(2400)) => {},
            result = &mut reply => panic!("The original SQLite write must remain blocked: {result:?}"),
        }
        let readiness =
            tokio::time::timeout(Duration::from_millis(250), control.readiness(&connection))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(readiness.get("ready"), Some(&true.into()));
        tokio::time::timeout(
            Duration::from_millis(500),
            connection.request("system.status", Json::Null, RequestOptions::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            heartbeats() > before,
            "Actual SDK heartbeats must continue during synchronous SQLite work"
        );
        blocker.execute_batch("COMMIT;").unwrap();
        let original = bounded(reply).await.unwrap();
        assert!(
            original.get("error").is_none(),
            "Original write: {original:?}"
        );
        let replay = bounded(connection.request(
            "test.invokeRegistered",
            request,
            RequestOptions::default(),
        ))
        .await
        .unwrap();
        assert_eq!(
            original.required("result").unwrap(),
            replay.required("result").unwrap()
        );
        shutdown.notify_one();
    };
    let (result, ()) = tokio::join!(runtime.run(shutdown.notified()), script);
    result.unwrap();
    assert!(!alive.load(Ordering::Acquire));
    let database = Connection::open(data.join("task-board.sqlite")).unwrap();
    database
        .execute_batch("BEGIN EXCLUSIVE; ROLLBACK;")
        .unwrap();
    peer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_native_runtime_observer_closes_and_drains_its_original_worker() {
    let peer = Peer::start_mode(Some("--native-policies")).await;
    let root = Root::new();
    let (path, mut config) = instance(&root, true, "task-board");
    config.insert("publicBaseUrl", peer.url.clone().into());
    std::fs::write(&path, stringify(&config, 32768).unwrap()).unwrap();
    let alive = Arc::new(AtomicBool::new(true));
    let drop_thread = Arc::new(std::sync::Mutex::new(None));
    let runtime = NativeRuntime::prepare(
        &path,
        inputs(
            &root,
            PromptOwner {
                alive: alive.clone(),
                drop_thread: drop_thread.clone(),
            },
        ),
    )
    .await
    .unwrap();
    let client = runtime.host.service.client.clone();
    let data = runtime.host.config.data_root().to_owned();
    let mut work = Box::pin(runtime.run(std::future::pending()));
    tokio::select! {
        result = work.as_mut() => panic!("Native owner ended before observation was cancelled: {result:?}"),
        ready = client.wait_ready(None) => { ready.unwrap(); },
    }
    drop(work);
    until(|| !alive.load(Ordering::Acquire)).await;
    assert!(!client.ready());
    assert_ne!(
        *drop_thread.lock().unwrap(),
        Some(std::thread::current().id())
    );
    let database = Connection::open(data.join("task-board.sqlite")).unwrap();
    database
        .execute_batch("BEGIN EXCLUSIVE; ROLLBACK;")
        .unwrap();
    peer.finish().await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_runtime_owns_configured_journal_and_resources_until_shutdown() {
    for open in [false, true] {
        let root = Root::new();
        let (path, config) = instance(&root, true, "task-board");
        let alive = Arc::new(AtomicBool::new(true));
        let drop_thread = Arc::new(std::sync::Mutex::new(None));
        let inputs = inputs(
            &root,
            PromptOwner {
                alive: alive.clone(),
                drop_thread: drop_thread.clone(),
            },
        );
        let runtime = NativeRuntime::prepare(&path, inputs).await.unwrap();
        let data = root.0.join("configured data");
        assert_eq!(runtime.host.config.json(), &config);
        assert_eq!(runtime.host.config.data_root(), data);
        assert!(!runtime.host.service.client.ready());
        assert!(!data.join("task-board.sqlite").exists());
        assert!(!data.join("health.json").exists());
        assert_eq!(
            runtime.policies.chat_readable_versions,
            vec![JsString::from("1.2.0"), JsString::from("1.3.0")]
        );
        assert!(runtime.policies.journal.free_bytes().unwrap().unwrap() > 0.0);
        if open {
            let journal = runtime.policies.journal.open().unwrap();
            journal.connection().execute_batch("CREATE TABLE startup_marker(value TEXT); INSERT INTO startup_marker VALUES('retained');").unwrap();
            drop(journal);
        }
        let shutdown = async {
            assert!(alive.load(Ordering::Acquire));
        };
        bounded(runtime.run(shutdown)).await.unwrap();
        assert!(
            !alive.load(Ordering::Acquire),
            "Prompt owner must release after shutdown"
        );
        assert_ne!(
            *drop_thread.lock().unwrap(),
            Some(std::thread::current().id())
        );
        let health = json(&std::fs::read_to_string(data.join("health.json")).unwrap());
        assert_eq!(health.get("instanceId"), config.get("instanceId"));
        assert_eq!(health.get("buildId"), config.get("buildId"));
        assert_eq!(health.get("ready"), Some(&Json::Bool(false)));
        assert_eq!(health.get("generation"), Some(&Json::Null));
        assert_eq!(health.get("details"), Some(&"TaskBoard stopped.".into()));
        assert_eq!(data.join("task-board.sqlite").exists(), open);
        assert!(
            !root
                .0
                .join("configured artifact/task-board.sqlite")
                .exists()
        );
        if open {
            let database = Connection::open(data.join("task-board.sqlite")).unwrap();
            database
                .execute_batch("BEGIN EXCLUSIVE; ROLLBACK;")
                .unwrap();
            assert_eq!(
                database
                    .query_row("SELECT value FROM startup_marker", [], |row| row
                        .get::<_, String>(0))
                    .unwrap(),
                "retained"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_runtime_rejects_build_and_service_identity_before_opening_journal() {
    for (matching, component, error) in [
        (false, "task-board", "build_mismatch"),
        (true, "other-service", "invalid_arguments"),
    ] {
        let root = Root::new();
        let (path, _) = instance(&root, matching, component);
        let alive = Arc::new(AtomicBool::new(true));
        let drop_thread = Arc::new(std::sync::Mutex::new(None));
        let inputs = inputs(
            &root,
            PromptOwner {
                alive: alive.clone(),
                drop_thread: drop_thread.clone(),
            },
        );
        let result = NativeRuntime::prepare(path, inputs).await;
        assert_eq!(result.err().unwrap().code, error);
        assert!(!alive.load(Ordering::Acquire));
        assert_eq!(
            *drop_thread.lock().unwrap(),
            Some(std::thread::current().id())
        );
        let data = root.0.join("configured data");
        assert!(!data.join("task-board.sqlite").exists());
        assert!(!data.join("health.json").exists());
        if !matching {
            assert!(!data.exists());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_runtime_registers_the_full_catalog_and_serves_an_sdk_workspace_call() {
    let peer = Peer::start_mode(Some("--native-policies")).await;
    let root = Root::new();
    let (path, mut config) = instance(&root, true, "task-board");
    config.insert("publicBaseUrl", peer.url.clone().into());
    std::fs::write(&path, stringify(&config, 32768).unwrap()).unwrap();
    let alive = Arc::new(AtomicBool::new(true));
    let drop_thread = Arc::new(std::sync::Mutex::new(None));
    let inputs = inputs(
        &root,
        PromptOwner {
            alive: alive.clone(),
            drop_thread: drop_thread.clone(),
        },
    );
    let advertisement = inputs.registry.value().clone();
    let runtime = NativeRuntime::prepare(&path, inputs).await.unwrap();
    let client = runtime.host.service.client.clone();
    let data = runtime.host.config.data_root().to_owned();
    let shutdown = Notify::new();
    let script = async {
        let connection = bounded(client.wait_ready(None)).await.unwrap();
        let response = bounded(connection.request(
            "test.invokeRegistered",
            Json::object([
                ("name", "workspace".into()),
                ("arguments", Json::Object(vec![])),
                ("operationId", "native-runtime-workspace".into()),
            ]),
            RequestOptions::default(),
        ))
        .await
        .unwrap();
        let workspace = response.required("result").unwrap();
        assert_eq!(workspace.get("serviceNodeId"), config.get("serviceNodeId"));
        assert_eq!(workspace.get("generation"), Some(&Json::Number(1.0)));
        let mut caller = JsString::from("caller-");
        caller.0.push(0xd800);
        assert_eq!(
            workspace.get("callerPrincipalId"),
            Some(&Json::String(caller))
        );
        assert_eq!(workspace.get("role"), Some(&"user".into()));
        assert_eq!(
            workspace.get("scheduler"),
            config.required("settings").unwrap().get("scheduler")
        );
        chrono::DateTime::parse_from_rfc3339(
            &workspace.required("observedAt").unwrap().as_str().unwrap(),
        )
        .unwrap();
        until(|| {
            std::fs::read_to_string(data.join("health.json"))
                .ok()
                .is_some_and(|value| json(&value).get("ready") == Some(&Json::Bool(true)))
        })
        .await;
        assert!(alive.load(Ordering::Acquire));
        shutdown.notify_one();
    };
    let (result, ()) =
        bounded(async { tokio::join!(runtime.run(shutdown.notified()), script) }).await;
    result.unwrap();
    assert!(!client.ready());
    assert!(!alive.load(Ordering::Acquire));
    assert_ne!(
        *drop_thread.lock().unwrap(),
        Some(std::thread::current().id())
    );
    let registrations = peer
        .frames
        .lock()
        .unwrap()
        .iter()
        .filter(|frame| frame.get("method") == Some(&"registry.sync".into()))
        .map(|frame| frame.required("params").unwrap().clone())
        .collect::<Vec<_>>();
    assert_eq!(registrations.len(), 1);
    assert_eq!(
        ivy_contracts::hash::hash_json(&registrations[0]).unwrap(),
        ivy_contracts::hash::hash_json(&advertisement).unwrap()
    );
    assert_eq!(
        registrations[0]
            .required("contracts")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        58
    );
    let health = json(&std::fs::read_to_string(data.join("health.json")).unwrap());
    assert_eq!(health.get("ready"), Some(&Json::Bool(false)));
    assert_eq!(health.get("generation"), Some(&Json::Null));
    assert_eq!(health.get("details"), Some(&"TaskBoard stopped.".into()));
    assert!(data.join("task-board.sqlite").exists());
    let database = Connection::open(data.join("task-board.sqlite")).unwrap();
    database
        .execute_batch("BEGIN EXCLUSIVE; ROLLBACK;")
        .unwrap();
    let _: i64 = database
        .query_row("SELECT count(*) FROM documents", [], |row| row.get(0))
        .unwrap();
    drop(database);
    peer.finish().await;
}
