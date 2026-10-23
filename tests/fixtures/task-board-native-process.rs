//! Execute the service binary with its actual packaged input and process owners.
use super::*;

fn executable(root: &Root, locale: bool) -> PathBuf {
    let directory = root.0.join("configured artifact/dist/native");
    std::fs::create_dir_all(&directory).unwrap();
    let executable = directory.join(format!("ivy-task-board{}", std::env::consts::EXE_SUFFIX));
    let packaged = std::env::var_os("IVY_TEST_TASK_BOARD_ARTIFACT").map(PathBuf::from);
    let binary = packaged.as_ref().map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_ivy-task-board")),
        |artifact| ivy_host_runtime::native_plan::native_binary_path(artifact, "ivy-task-board"),
    );
    std::fs::copy(&binary, &executable).unwrap();
    if locale {
        let source = if packaged.is_some() {
            binary.parent().unwrap().to_owned()
        } else {
            PathBuf::from(std::env::var_os("IVY_TEST_LOCALE_RUNTIME").unwrap())
        };
        #[cfg(windows)]
        let names = ["icudt78.dll", "icuuc78.dll", "icuin78.dll"];
        #[cfg(not(windows))]
        let names = ["libicudata.so.78", "libicuuc.so.78", "libicui18n.so.78"];
        for name in names {
            std::fs::copy(source.join(name), directory.join(name)).unwrap();
        }
    }
    executable
}

fn command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .kill_on_drop(true)
        .env_remove("IVY_INSTANCE_CONFIG")
        .env("IVY_LAUNCH_ID", "process-launch")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[tokio::test(flavor = "current_thread")]
async fn executable_refuses_configuration_before_locale_loading_without_leaking_credentials() {
    for variant in 0..4 {
        let root = Root::new();
        let (path, _) = instance(&root, false, "task-board");
        let executable = executable(&root, false);
        let mut child = command(&executable);
        match variant {
            0 => {}
            1 => {
                child.env("IVY_INSTANCE_CONFIG", "relative.json");
            }
            2 => {
                child.arg("--config").env("IVY_INSTANCE_CONFIG", &path);
            }
            _ => {
                child
                    .arg("--config")
                    .arg(&path)
                    .env("IVY_INSTANCE_CONFIG", "relative.json");
            }
        }
        let output = bounded(child.output()).await.unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert_eq!(diagnostic.lines().count(), 1);
        let error = json(&diagnostic);
        assert_eq!(
            error.get("code"),
            Some(&Json::from(if variant == 3 {
                "build_mismatch"
            } else {
                "invalid_arguments"
            }))
        );
        assert_eq!(
            error.get("message"),
            Some(&"TaskBoard failed to start.".into())
        );
        assert!(!diagnostic.contains("fixture-token"));
        assert!(!diagnostic.contains(&path.to_string_lossy().to_string()));
        assert!(!root.0.join("configured data").exists());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn executable_registers_current_inputs_and_stops_offline_with_its_journal_released() {
    let peer = Peer::start_mode(Some("--native-process")).await;
    let root = Root::new();
    let (path, mut config) = instance(&root, true, "task-board");
    config.insert("publicBaseUrl", peer.url.clone().into());
    std::fs::write(&path, stringify(&config, 32768).unwrap()).unwrap();
    let executable = executable(&root, true);
    let mut child = command(&executable)
        .env("IVY_INSTANCE_CONFIG", &path)
        .env("PATH", "")
        .current_dir(&root.0)
        .spawn()
        .unwrap();
    until(|| {
        peer.frames.lock().unwrap().iter().any(|frame| {
            frame.get("frame").and_then(|value| value.get("id"))
                == Some(&"native-process-workspace".into())
        })
    })
    .await;
    let frames = peer.frames.lock().unwrap().clone();
    let registration = frames
        .iter()
        .find(|frame| frame.get("method") == Some(&"registry.sync".into()))
        .unwrap()
        .required("params")
        .unwrap();
    assert_eq!(
        ivy_contracts::hash::hash_json(registration).unwrap(),
        "sha256:3f615c081e17e00e5cf5aa0e9342dcde60af06a5380961b0fe75e78dcdc81893"
    );
    let response = frames
        .iter()
        .find_map(|frame| {
            frame
                .get("frame")
                .filter(|value| value.get("id") == Some(&"native-process-workspace".into()))
        })
        .unwrap();
    let workspace = response.required("result").unwrap();
    assert_eq!(workspace.get("serviceNodeId"), config.get("serviceNodeId"));
    assert_eq!(workspace.get("generation"), Some(&Json::Number(1.0)));
    let mut caller = JsString::from("process-caller-");
    caller.0.push(0xd800);
    assert_eq!(
        workspace.get("callerPrincipalId"),
        Some(&Json::String(caller))
    );
    let data = root.0.join("configured data");
    until(|| {
        std::fs::read_to_string(data.join("health.json"))
            .ok()
            .is_some_and(|value| json(&value).get("ready") == Some(&true.into()))
    })
    .await;
    let running = json(&std::fs::read_to_string(data.join("health.json")).unwrap());
    assert_eq!(
        running.get("pid"),
        Some(&Json::Number(child.id().unwrap() as f64))
    );
    assert_eq!(running.get("launchId"), Some(&"process-launch".into()));
    assert!(data.join("task-board.sqlite").exists());
    peer.finish().await;
    until(|| {
        std::fs::read_to_string(data.join("health.json"))
            .ok()
            .is_some_and(|value| json(&value).get("ready") == Some(&false.into()))
    })
    .await;
    std::fs::write(
        data.join("control.json"),
        stringify(
            &Json::object([
                ("schemaVersion", Json::Number(1.0)),
                ("instanceId", config.required("instanceId").unwrap().clone()),
                ("bootId", running.required("bootId").unwrap().clone()),
                ("launchId", "process-launch".into()),
                ("action", "shutdown".into()),
                ("requestedAt", "2026-01-02T03:04:05.006Z".into()),
            ]),
            32768,
        )
        .unwrap(),
    )
    .unwrap();
    let output = bounded(child.wait_with_output()).await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let stopped = json(&std::fs::read_to_string(data.join("health.json")).unwrap());
    assert_eq!(stopped.get("ready"), Some(&false.into()));
    assert_eq!(stopped.get("generation"), Some(&Json::Null));
    assert_eq!(stopped.get("details"), Some(&"TaskBoard stopped.".into()));
    let database = Connection::open(data.join("task-board.sqlite")).unwrap();
    database
        .execute_batch("BEGIN EXCLUSIVE; ROLLBACK;")
        .unwrap();
    let _: i64 = database
        .query_row("SELECT count(*) FROM documents", [], |row| row.get(0))
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn executable_saves_and_replays_catalog_checked_plans_and_refuses_a_wrong_definition() {
    use ivy_contracts::{checked_native_contract::CheckedNativeContracts, hash::hash_json};
    let root = Root::new();
    let (path, mut config) = instance(&root, true, "task-board");
    let executable = executable(&root, true);
    let catalog_directory = root
        .0
        .join("configured artifact/dist/specs/native/codex-0.159.2");
    std::fs::create_dir_all(&catalog_directory).unwrap();
    let distribution = std::env::var_os("IVY_TEST_TASK_BOARD_ARTIFACT").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        |artifact| PathBuf::from(artifact).join("dist"),
    );
    std::fs::copy(
        distribution.join("specs/native/codex-0.159.2/catalog.json"),
        catalog_directory.join("catalog.json"),
    )
    .unwrap();
    let catalogs = CheckedNativeContracts::new(root.0.join("configured artifact/dist"));
    let catalog = catalogs
        .checked_native_contract(&"0.159.2".into(), None)
        .unwrap();
    let definitions = Json::object(
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
    );
    let draft = Json::object([
        ("nativeVersion", "0.159.2".into()),
        (
            "catalogSourceHash",
            catalog.catalog().required("sourceHash").unwrap().clone(),
        ),
        ("definitions", definitions.clone()),
        ("threadStart", Json::Object(vec![])),
        ("threadResume", Json::Object(vec![])),
        ("turnStart", Json::object([("input", Json::Array(vec![]))])),
    ]);
    let request = Json::object([
        ("action", "savePlan".into()),
        ("operationId", "process-save-plan".into()),
        ("plan", draft.clone()),
    ]);
    let mut invalid = draft.clone();
    let mut wrong_definition = definitions;
    wrong_definition.insert("threadStart", format!("sha256:{}", "b".repeat(64)).into());
    invalid.insert("definitions", wrong_definition);
    let invalid = Json::object([
        ("action", "savePlan".into()),
        ("operationId", "process-wrong-plan".into()),
        ("plan", invalid),
    ]);
    let requests = Json::Array(
        vec![request.clone(), request, invalid]
            .into_iter()
            .map(|arguments| Json::object([("name", "savePlan".into()), ("arguments", arguments)]))
            .collect(),
    );
    let payload = root.0.join("process-requests.json");
    std::fs::write(&payload, stringify(&requests, 1024 * 1024).unwrap()).unwrap();
    let peer = Peer::start_mode(Some(&format!("--native-process={}", payload.display()))).await;
    config.insert("publicBaseUrl", peer.url.clone().into());
    std::fs::write(&path, stringify(&config, 32768).unwrap()).unwrap();
    let child = command(&executable)
        .arg("--config")
        .arg(&path)
        .env("PATH", "")
        .current_dir(&root.0)
        .spawn()
        .unwrap();
    until(|| {
        peer.frames.lock().unwrap().iter().any(|frame| {
            frame.get("frame").and_then(|value| value.get("id"))
                == Some(&"native-process-request-2".into())
        })
    })
    .await;
    let frames = peer.frames.lock().unwrap().clone();
    let response = |index| {
        frames
            .iter()
            .find_map(|frame| {
                frame.get("frame").filter(|value| {
                    value.get("id") == Some(&Json::from(format!("native-process-request-{index}")))
                })
            })
            .unwrap()
    };
    let outcome = response(0).required("result").unwrap();
    assert_eq!(
        outcome.get("operationId"),
        Some(&"process-save-plan".into())
    );
    assert_eq!(
        hash_json(response(0).required("result").unwrap()).unwrap(),
        hash_json(response(1).required("result").unwrap()).unwrap()
    );
    assert_eq!(
        response(2)
            .required("error")
            .unwrap()
            .required("data")
            .unwrap()
            .get("code"),
        Some(&"native_plan_definition_mismatch".into())
    );
    let data = root.0.join("configured data");
    let running = json(&std::fs::read_to_string(data.join("health.json")).unwrap());
    std::fs::write(
        data.join("control.json"),
        stringify(
            &Json::object([
                ("schemaVersion", Json::Number(1.0)),
                ("instanceId", config.required("instanceId").unwrap().clone()),
                ("bootId", running.required("bootId").unwrap().clone()),
                ("launchId", "process-launch".into()),
                ("action", "shutdown".into()),
                ("requestedAt", "2026-01-02T03:04:05.006Z".into()),
            ]),
            32768,
        )
        .unwrap(),
    )
    .unwrap();
    let output = bounded(child.wait_with_output()).await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let database = Connection::open(data.join("task-board.sqlite")).unwrap();
    database
        .execute_batch("BEGIN EXCLUSIVE; ROLLBACK;")
        .unwrap();
    let count: i64 = database
        .query_row(
            "SELECT count(*) FROM documents WHERE key='task-board/native-plan' AND parent_id IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "An exact replay and refused definition may not create additional plans"
    );
    let retained:String = database.query_row("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='task-board/native-plan' AND d.parent_id IS NULL",[],|row|row.get(0)).unwrap();
    let resolved = ivy_sdk::native_plan::read_native_plan(
        &catalog,
        &json(&retained),
        ivy_sdk::native_plan::NativePlanKind::TaskBoard,
        None,
    )
    .unwrap();
    assert_eq!(hash_json(&resolved).unwrap(), hash_json(&draft).unwrap());
    peer.finish().await;
}
