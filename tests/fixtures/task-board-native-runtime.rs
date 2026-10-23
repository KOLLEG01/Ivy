//! Native owner preparation and shutdown before connection. Complete registry
//! admission/current producer execution require their separate owner deliveries.
use super::*;
use ivy_contracts::json::stringify;
use ivy_task_board::native_runtime::{NativeRuntime, NativeRuntimeInputs};
use std::rc::Rc;

struct PromptOwner(Rc<Cell<bool>>);
impl Drop for PromptOwner {
    fn drop(&mut self) {
        self.0.set(false);
    }
}
impl TaskPrompts for PromptOwner {
    fn task_prompt(&self, _: &Json, _: &JsString, _: Option<&Json>) -> IvyResult<JsString> {
        panic!("No native execution before connection")
    }
    fn task_update_prompt(&self, _: &Json, _: &JsString, _: bool) -> IvyResult<JsString> {
        panic!("No native execution before connection")
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

fn inputs(
    root: &Root,
    prompts: PromptOwner,
) -> (
    NativeRuntimeInputs<PromptOwner>,
    std::sync::Weak<LocaleRuntime>,
) {
    let runtime = LocaleRuntime::load(&PathBuf::from(
        std::env::var_os("IVY_TEST_LOCALE_RUNTIME").expect("Supply the packaged ICU runtime"),
    ))
    .unwrap();
    let weak = Arc::downgrade(&runtime);
    let contracts = contracts();
    let registry = registry::fixture(&contracts);
    (
        NativeRuntimeInputs {
            contracts,
            catalogs: CheckedNativeContracts::new(root.0.join("configured artifact")),
            registry,
            locale: runtime.executing_default().unwrap(),
            prompts,
            chat_readable_versions: vec!["explicit-fixture-chat-reader".into()],
        },
        weak,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn native_runtime_owns_configured_journal_and_resources_until_shutdown() {
    for open in [false, true] {
        let root = Root::new();
        let (path, config) = instance(&root, true, "task-board");
        let alive = Rc::new(Cell::new(true));
        let (inputs, locale) = inputs(&root, PromptOwner(alive.clone()));
        let runtime = NativeRuntime::prepare(&path, inputs).await.unwrap();
        let data = root.0.join("configured data");
        assert_eq!(runtime.host.config.json(), &config);
        assert_eq!(runtime.host.config.data_root(), data);
        assert!(!runtime.host.service.client.ready());
        assert!(!data.join("task-board.sqlite").exists());
        assert!(!data.join("health.json").exists());
        assert_eq!(
            runtime.policies.chat_readable_versions,
            vec![JsString::from("explicit-fixture-chat-reader")]
        );
        assert!(runtime.policies.journal.free_bytes().unwrap().unwrap() > 0.0);
        if open {
            let journal = runtime.policies.journal.open().unwrap();
            journal.connection().execute_batch("CREATE TABLE startup_marker(value TEXT); INSERT INTO startup_marker VALUES('retained');").unwrap();
            drop(journal);
        }
        let shutdown = async {
            assert!(alive.get());
            assert!(locale.upgrade().is_some());
        };
        bounded(runtime.run(shutdown)).await.unwrap();
        assert!(!alive.get(), "Prompt owner must release after shutdown");
        assert!(
            locale.upgrade().is_none(),
            "The actual ICU owner must release"
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
        let alive = Rc::new(Cell::new(true));
        let (inputs, locale) = inputs(&root, PromptOwner(alive.clone()));
        let result = NativeRuntime::prepare(path, inputs).await;
        assert_eq!(result.err().unwrap().code, error);
        assert!(!alive.get());
        assert!(locale.upgrade().is_none());
        let data = root.0.join("configured data");
        assert!(!data.join("task-board.sqlite").exists());
        assert!(!data.join("health.json").exists());
        if !matching {
            assert!(!data.exists());
        }
    }
}
