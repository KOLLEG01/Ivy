use ivy_agent_manager::{
    native_launch::NativeHomeInputs, runtime_reset::apply_agent_runtime_reset_with_clock,
};
use ivy_contracts::{
    IvyJson,
    hash::digest,
    json::{canonical, parse, stringify},
};
use ivy_host_runtime::node_path::{self, Context};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn value<'a>(v: &'a IvyJson, key: &str) -> &'a IvyJson {
    v.required(key).unwrap()
}
fn string(v: &IvyJson, key: &str) -> String {
    value(v, key).as_str().unwrap()
}
fn same(actual: &IvyJson, expected: &IvyJson, name: &str) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap(),
        "{name}"
    );
}
fn bytes(value: &IvyJson, key: &str) -> Vec<u8> {
    string(value, key)
        .as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
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
fn link(target: &Path, path: &Path) {
    #[cfg(windows)]
    {
        let result = Command::new("node")
            .args([
                "-e",
                "require('node:fs').symlinkSync(process.argv[1],process.argv[2],'junction')",
                "--",
            ])
            .arg(target)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(target, path).unwrap();
}
fn snapshot(root: &Path) -> IvyJson {
    fn walk(root: &Path, prefix: &str, rows: &mut Vec<IvyJson>) {
        let mut entries: Vec<_> = fs::read_dir(root).unwrap().map(Result::unwrap).collect();
        entries.sort_by_key(|entry| node_path::from_native(Path::new(&entry.file_name())));
        for entry in entries {
            let name = entry.file_name().into_string().unwrap();
            // UUID values differ across real atomic writers. Preserve the
            // exact temporary-file effect/count/bytes while comparing its name
            // independently from that intentionally random identity.
            let temporary = name
                .strip_prefix("runtime-reset-applied.json.")
                .and_then(|s| s.strip_suffix(".tmp"))
                .is_some_and(|s| {
                    uuid::Uuid::parse_str(s).is_ok_and(|id| {
                        id.to_string() == s && id.get_version() == Some(uuid::Version::Random)
                    })
                });
            let path = format!(
                "{prefix}{}",
                if temporary {
                    "runtime-reset-applied.json.<uuid>.tmp"
                } else {
                    &name
                }
            );
            let info = fs::symlink_metadata(entry.path()).unwrap();
            let kind = if info.file_type().is_symlink() {
                "link"
            } else if info.is_dir() {
                "directory"
            } else {
                "file"
            };
            let mut row = IvyJson::object([("path", path.clone().into()), ("kind", kind.into())]);
            if kind == "directory" {
                rows.push(row);
                walk(&entry.path(), &format!("{path}/"), rows);
            } else {
                if kind == "file" {
                    let bytes = fs::read(entry.path()).unwrap();
                    row.insert("bytes", IvyJson::Number(bytes.len() as f64));
                    row.insert("hash", digest(bytes).into());
                }
                rows.push(row);
            }
        }
    }
    let mut rows = Vec::new();
    walk(root, "", &mut rows);
    IvyJson::Array(rows)
}
fn selected(v: &IvyJson, root: &Path, home: &Path) -> IvyJson {
    let Some(fields) = v.as_object() else {
        return v.clone();
    };
    IvyJson::object(fields.iter().map(|(key, v)| {
        let path = match v.as_js_string().and_then(|s| s.to_utf8().ok()).as_deref() {
            Some("$home") => Some(node_path::from_native(home)),
            Some("$other") => Some(node_path::from_native(&root.join("other"))),
            Some("$root") => {
                let mut path = home;
                while let Some(parent) = path.parent() {
                    path = parent;
                }
                Some(node_path::from_native(path))
            }
            _ => None,
        };
        (
            key.clone(),
            path.map(IvyJson::String).unwrap_or_else(|| v.clone()),
        )
    }))
}
#[tokio::test]
async fn original_reset_marker_date_idempotency_scope_and_file_effects_are_preserved() {
    let root = Root::new();
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/agent-runtime-reset-oracle.mjs",
        ])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            stringify(
                &IvyJson::object([(
                    "root",
                    IvyJson::String(node_path::from_native(&root.0.join("original"))),
                )]),
                usize::MAX,
            )
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let isolation = value(&oracle, "isolation");
    assert_eq!(value(isolation, "selections").as_array().unwrap().len(), 5);
    assert_eq!(value(isolation, "prevented").as_array().unwrap().len(), 3);
    assert_eq!(value(isolation, "effects").as_array().unwrap().len(), 1);
    assert_eq!(value(isolation, "sentinelPreserved"), &true.into());
    eprintln!(
        "Reset fixture isolation: {}",
        canonical(isolation, usize::MAX).unwrap()
    );
    let cases = value(&oracle, "cases").as_array().unwrap();
    assert!(cases.len() > 80);
    for case in cases {
        let name = string(case, "name");
        let directory = root.0.join("rust").join(&name);
        let data = directory.join("data");
        let home = directory.join("home");
        fs::create_dir_all(&data).unwrap();
        let marker = data.join("runtime-reset.json");
        let applied = data.join("runtime-reset-applied.json");
        match string(case, "markerKind").as_str() {
            "missing" => {}
            "directory" => fs::create_dir(&marker).unwrap(),
            "hard-link" => {
                fs::write(data.join("marker-source"), bytes(case, "markerBytes")).unwrap();
                fs::hard_link(data.join("marker-source"), &marker).unwrap();
            }
            _ => fs::write(&marker, bytes(case, "markerBytes")).unwrap(),
        }
        match string(case, "appliedKind").as_str() {
            "directory" => fs::create_dir(&applied).unwrap(),
            "file" => fs::write(&applied, bytes(case, "appliedBytes")).unwrap(),
            _ => {}
        }
        let kind = string(case, "homeKind");
        if kind == "file" || kind == "ancestor-file" {
            fs::write(&home, "not a directory").unwrap();
        } else if kind != "missing" {
            let actual = if kind == "junction" {
                directory.join("actual")
            } else {
                home.clone()
            };
            fs::create_dir(&actual).unwrap();
            for name in ["sessions", "archived_sessions", "shell_snapshots", "tmp"] {
                fs::create_dir(actual.join(name)).unwrap();
                fs::write(actual.join(name).join("leaf"), format!("removed {name}")).unwrap();
            }
            for name in [
                "history.jsonl",
                "state_5.sqlite",
                "state_5.sqlite-wal",
                "state_5.sqlite-shm",
            ] {
                fs::write(actual.join(name), format!("removed {name}")).unwrap();
            }
            for name in ["config.toml", "AGENTS.md", "state_6.sqlite", "foreign"] {
                fs::write(actual.join(name), format!("preserved {name}")).unwrap();
            }
            if kind == "junction" {
                link(&actual, &home);
            }
        }
        if name == "inner-junction" {
            fs::remove_dir_all(home.join("sessions")).unwrap();
            fs::create_dir(directory.join("external")).unwrap();
            fs::write(directory.join("external/leaf"), "preserve link target").unwrap();
            link(&directory.join("external"), &home.join("sessions"));
        }
        if name == "readonly" {
            let path = home.join("sessions/leaf");
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(path, permissions).unwrap();
        }
        if name == "settings-null-codex" {
            let sessions = directory.join("account/.codex/sessions");
            fs::create_dir_all(&sessions).unwrap();
            fs::write(sessions.join("leaf"), "fixture fallback history").unwrap();
        }
        let home = if kind == "ancestor-file" {
            home.join("nested")
        } else {
            home
        };
        let settings = selected(value(case, "settings"), &directory, &home);
        let reads = Arc::new(AtomicUsize::new(0));
        let clock: Arc<ivy_agent_manager::runtime_reset::ResetClock> = Arc::new({
            let reads = reads.clone();
            let now = value(&oracle, "now").as_js_string().unwrap().clone();
            move || {
                reads.fetch_add(1, Ordering::SeqCst);
                now.clone()
            }
        });
        let mut result = Ok(());
        for _ in 0..value(case, "repeat").as_f64().unwrap() as usize {
            result = apply_agent_runtime_reset_with_clock(
                data.clone(),
                settings.clone(),
                inputs(&directory),
                clock.clone(),
            )
            .await;
            if result.is_err() {
                break;
            }
        }
        let reply = match result {
            Ok(()) => IvyJson::object([("ok", true.into())]),
            Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
        };
        same(
            &IvyJson::object([
                ("reply", reply),
                (
                    "clockReads",
                    IvyJson::Number(reads.load(Ordering::SeqCst) as f64),
                ),
                ("files", snapshot(&directory)),
            ]),
            value(case, "expected"),
            &name,
        );
    }
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
