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

    let mut variants = vec![
        "unicode-dot",
        "canonical",
        "linked",
        "parent-missing",
        "parent-link",
    ];
    #[cfg(windows)]
    variants.extend(["opaque-high", "opaque-low", "trailing"]);
    for variant in variants {
        let base = root.0.join(variant);
        fs::create_dir(&base).unwrap();
        let mut data = base.join(if variant == "unicode-dot" {
            "目录 space"
        } else {
            "data"
        });
        let mut alias = None;
        #[cfg(windows)]
        if variant.starts_with("opaque") || variant == "trailing" {
            use std::{ffi::OsString, os::windows::ffi::OsStringExt};
            let canonical = fs::canonicalize(&base).unwrap();
            let retained = canonical.join(if variant == "trailing" {
                "data"
            } else {
                "data�"
            });
            fs::create_dir(&retained).unwrap();
            let sentinel = retained.join("authority");
            fs::write(&sentinel, b"unrelated record authority").unwrap();
            alias = Some(sentinel);
            let mut name: Vec<u16> = "data".encode_utf16().collect();
            if variant == "trailing" {
                name.extend(". ".encode_utf16());
            } else {
                name.push(if variant == "opaque-high" {
                    0xd800
                } else {
                    0xdfff
                });
            }
            data = canonical.join(OsString::from_wide(&name));
        }
        fs::create_dir(&data).unwrap();
        let mut configured = data.clone();
        let mut link = None;
        if variant == "unicode-dot" {
            configured.push(".");
        } else if variant == "canonical" {
            configured = fs::canonicalize(&data).unwrap();
        } else if variant == "linked" {
            let path = base.join("data-link");
            filesystem::directory_link(Path::new("data"), &path).unwrap();
            link = Some((path.clone(), fs::read_link(&path).unwrap()));
            configured = path;
        } else if variant == "parent-missing" {
            configured = base.join("missing").join("..").join("data");
        } else if variant == "parent-link" {
            fs::create_dir_all(base.join("alternate/child")).unwrap();
            fs::create_dir(base.join("alternate/data")).unwrap();
            let sentinel = base.join("alternate/data/authority");
            fs::write(&sentinel, b"unrelated record authority").unwrap();
            alias = Some(sentinel);
            let path = base.join("redirect");
            filesystem::directory_link(Path::new("alternate/child"), &path).unwrap();
            link = Some((path.clone(), fs::read_link(&path).unwrap()));
            configured = path.join("..").join("data");
        }
        let home = base.join("home");
        fs::create_dir(&home).unwrap();
        let history = home.join("history.jsonl");
        fs::write(&history, b"transient history").unwrap();
        let intent = marker();
        let marker_path = data.join("runtime-reset.json");
        save(&marker_path, &intent);
        let original = fs::read(&marker_path).unwrap();
        apply_agent_runtime_reset_with_clock(
            configured.clone(),
            settings(&home),
            inputs(&root.0),
            Arc::new(|| "2026-10-19T01:00:00.000Z".into()),
        )
        .await
        .unwrap_or_else(|error| panic!("{variant}: {error:?}"));
        assert!(!history.exists(), "{variant}");
        assert_eq!(fs::read(&marker_path).unwrap(), original, "{variant}");
        let applied_path = data.join("runtime-reset-applied.json");
        let acknowledgement = fs::read(&applied_path).unwrap();
        let actual = parse(std::str::from_utf8(&acknowledgement).unwrap(), 4096).unwrap();
        let mut expected = intent;
        expected.insert("appliedAt", "2026-10-19T01:00:00.000Z".into());
        assert_eq!(
            canonical(&actual, 4096).unwrap(),
            canonical(&expected, 4096).unwrap(),
            "{variant}"
        );
        fs::write(&history, b"new retained history").unwrap();
        apply_agent_runtime_reset_with_clock(
            configured,
            IvyJson::Null,
            inputs(&root.0),
            Arc::new(|| panic!("Acknowledged reset must precede settings/clock admission")),
        )
        .await
        .unwrap_or_else(|error| panic!("{variant}: {error:?}"));
        assert_eq!(
            fs::read(&history).unwrap(),
            b"new retained history",
            "{variant}"
        );
        assert_eq!(
            fs::read(&applied_path).unwrap(),
            acknowledgement,
            "{variant}"
        );
        assert_eq!(fs::read(&marker_path).unwrap(), original, "{variant}");
        if let Some(alias) = alias {
            assert_eq!(
                fs::read(alias).unwrap(),
                b"unrelated record authority",
                "{variant}"
            );
        }
        if let Some((path, target)) = link {
            assert_eq!(fs::read_link(path).unwrap(), target, "{variant}");
        }
    }
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
