use ivy_agent_manager::{
    native_launch::NativeHomeInputs, runtime_reset::apply_agent_runtime_reset_with_clock,
};
use ivy_contracts::{
    IvyJson,
    json::{canonical, parse},
};
use ivy_host_runtime::{
    filesystem,
    node_path::{self, Context},
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-agent-reset-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-agent-reset-")
        );
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn inputs(root: &Path) -> NativeHomeInputs {
    NativeHomeInputs {
        paths: Context::current().unwrap(),
        environment: BTreeMap::new(),
        user_home: None,
        cwd: node_path::from_native(root),
        account_home: node_path::from_native(&root.join("account")),
    }
}
fn marker() -> IvyJson {
    parse(r#"{"schemaVersion":1,"resetId":"reset","runtimeEpoch":"epoch","completedAt":"2026-10-19T00:00:00.000Z","extra":{"retained":true}}"#, 4096).unwrap()
}
fn settings(home: &Path) -> IvyJson {
    IvyJson::object([("codexHome", IvyJson::String(node_path::from_native(home)))])
}
fn save(path: &Path, value: &IvyJson) {
    fs::write(path, canonical(value, 1024 * 1024).unwrap()).unwrap();
}
async fn apply(root: &Root, settings: IvyJson) -> ivy_contracts::IvyResult<()> {
    apply_agent_runtime_reset_with_clock(
        root.0.join("data"),
        settings,
        inputs(&root.0),
        Arc::new(|| "2026-10-19T01:00:00.000Z".into()),
    )
    .await
}

#[tokio::test]
async fn native_reset_markers_preserve_scope_files_and_idempotent_acknowledgements() {
    let root = Root::new();
    let data = root.0.join("data");
    let home = root.0.join("home");
    fs::create_dir(&data).unwrap();
    fs::create_dir(&home).unwrap();
    apply(&root, settings(&home)).await.unwrap();
    for name in ["sessions", "archived_sessions", "shell_snapshots", "tmp"] {
        fs::create_dir(home.join(name)).unwrap();
        fs::write(home.join(name).join("leaf"), b"transient").unwrap();
    }
    for name in [
        "history.jsonl",
        "state_5.sqlite",
        "state_5.sqlite-wal",
        "state_5.sqlite-shm",
    ] {
        fs::write(home.join(name), b"transient").unwrap();
    }
    for name in ["config.toml", "AGENTS.md", "state_6.sqlite", "foreign"] {
        fs::write(home.join(name), b"preserved").unwrap();
    }
    // Removing a fixed inner directory link must preserve its target.
    fs::remove_dir_all(home.join("tmp")).unwrap();
    let outside = root.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("leaf"), b"outside").unwrap();
    filesystem::directory_link(&outside, &home.join("tmp")).unwrap();
    let original = marker();
    save(&data.join("runtime-reset.json"), &original);
    let reads = Arc::new(AtomicUsize::new(0));
    let clock: Arc<ivy_agent_manager::runtime_reset::ResetClock> = Arc::new({
        let reads = reads.clone();
        move || {
            reads.fetch_add(1, Ordering::SeqCst);
            "2026-10-19T01:00:00.000Z".into()
        }
    });
    apply_agent_runtime_reset_with_clock(
        data.clone(),
        settings(&home),
        inputs(&root.0),
        clock.clone(),
    )
    .await
    .unwrap();
    for name in [
        "sessions",
        "archived_sessions",
        "history.jsonl",
        "state_5.sqlite",
        "state_5.sqlite-wal",
        "state_5.sqlite-shm",
        "shell_snapshots",
        "tmp",
    ] {
        assert!(!home.join(name).try_exists().unwrap());
    }
    for name in ["config.toml", "AGENTS.md", "state_6.sqlite", "foreign"] {
        assert_eq!(fs::read(home.join(name)).unwrap(), b"preserved");
    }
    assert_eq!(fs::read(outside.join("leaf")).unwrap(), b"outside");
    let mut expected = original.clone();
    expected.insert("appliedAt", "2026-10-19T01:00:00.000Z".into());
    let applied = data.join("runtime-reset-applied.json");
    assert_eq!(
        canonical(
            &parse(&fs::read_to_string(&applied).unwrap(), 4096).unwrap(),
            4096
        )
        .unwrap(),
        canonical(&expected, 4096).unwrap()
    );
    assert_eq!(
        canonical(
            &parse(
                &fs::read_to_string(data.join("runtime-reset.json")).unwrap(),
                4096
            )
            .unwrap(),
            4096
        )
        .unwrap(),
        canonical(&original, 4096).unwrap()
    );
    fs::create_dir(home.join("sessions")).unwrap();
    fs::write(home.join("sessions/leaf"), b"new history").unwrap();
    apply_agent_runtime_reset_with_clock(data, IvyJson::Null, inputs(&root.0), clock)
        .await
        .unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        fs::read(home.join("sessions/leaf")).unwrap(),
        b"new history"
    );
}

#[tokio::test]
async fn native_reset_rejects_malformed_markers_and_unsafe_homes_before_removal() {
    let root = Root::new();
    let data = root.0.join("data");
    let home = root.0.join("home");
    fs::create_dir(&data).unwrap();
    fs::create_dir(&home).unwrap();
    fs::write(home.join("history.jsonl"), b"preserved").unwrap();
    let path = data.join("runtime-reset.json");
    for date in [
        "invalid",
        "",
        "2026-10-19",
        "2026-10-19T00:00:00Z",
        "2026-10-19T02:00:00.000+02:00",
        "2026-02-30T00:00:00.000Z",
        "2026-10-19T00:00:00.000Z\0",
    ] {
        let mut invalid = marker();
        invalid.insert("completedAt", date.into());
        save(&path, &invalid);
        assert_eq!(
            apply(&root, settings(&home)).await.unwrap_err().code,
            "invalid_arguments"
        );
        assert!(!data.join("runtime-reset-applied.json").exists());
        assert_eq!(fs::read(home.join("history.jsonl")).unwrap(), b"preserved");
    }
    for key in ["schemaVersion", "resetId", "runtimeEpoch", "completedAt"] {
        for value in [IvyJson::Null, false.into()] {
            let mut invalid = marker();
            invalid.insert(key, value);
            save(&path, &invalid);
            assert_eq!(
                apply(&root, settings(&home)).await.unwrap_err().code,
                "invalid_arguments"
            );
            assert_eq!(fs::read(home.join("history.jsonl")).unwrap(), b"preserved");
        }
    }
    fs::write(&path, "{").unwrap();
    assert!(apply(&root, settings(&home)).await.is_err());
    fs::write(
        &path,
        format!(
            "{}{}",
            canonical(&marker(), 4096).unwrap(),
            " ".repeat(1024 * 1024)
        ),
    )
    .unwrap();
    assert!(apply(&root, settings(&home)).await.is_err());
    save(&path, &marker());
    assert_eq!(
        apply(&root, IvyJson::object([] as [(&str, IvyJson); 0]))
            .await
            .unwrap_err()
            .code,
        "unsupported_storage"
    );
    let mut drive = home.as_path();
    while let Some(parent) = drive.parent() {
        drive = parent;
    }
    assert_eq!(
        apply(&root, settings(drive)).await.unwrap_err().code,
        "target_conflict"
    );
    let file = root.0.join("file");
    fs::write(&file, b"file").unwrap();
    assert_eq!(
        apply(&root, settings(&file)).await.unwrap_err().code,
        "target_conflict"
    );
    let link = root.0.join("linked-home");
    filesystem::directory_link(&home, &link).unwrap();
    assert_eq!(
        apply(&root, settings(&link)).await.unwrap_err().code,
        "target_conflict"
    );
    assert_eq!(fs::read(home.join("history.jsonl")).unwrap(), b"preserved");
    assert!(!data.join("runtime-reset-applied.json").exists());
}

#[tokio::test]
async fn an_unpolled_reset_waiter_keeps_the_accepted_acknowledgement_write() {
    let root = Root::new();
    let data = root.0.join("data");
    let home = root.0.join("home");
    fs::create_dir(&data).unwrap();
    fs::create_dir(&home).unwrap();
    fs::create_dir(home.join("sessions")).unwrap();
    fs::write(home.join("sessions/leaf"), "transient").unwrap();
    let marker=parse(r#"{"schemaVersion":1,"resetId":"reset","runtimeEpoch":"epoch","completedAt":"2026-10-19T00:00:00.000Z"}"#,4096).unwrap();
    fs::write(
        data.join("runtime-reset.json"),
        canonical(&marker, 4096).unwrap(),
    )
    .unwrap();
    let observed = Arc::new(tokio::sync::Notify::new());
    drop(apply_agent_runtime_reset_with_clock(
        data.clone(),
        IvyJson::object([("codexHome", IvyJson::String(node_path::from_native(&home)))]),
        inputs(&root.0),
        Arc::new({
            let observed = observed.clone();
            move || {
                observed.notify_one();
                "2026-10-19T01:00:00.000Z".into()
            }
        }),
    ));
    tokio::time::timeout(Duration::from_secs(5), observed.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if data.join("runtime-reset-applied.json").is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!home.join("sessions").exists());
    assert_eq!(
        parse(
            &fs::read_to_string(data.join("runtime-reset-applied.json")).unwrap(),
            4096
        )
        .unwrap()
        .get("appliedAt"),
        Some(&"2026-10-19T01:00:00.000Z".into())
    );
}
