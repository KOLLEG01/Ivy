use ivy_agent_manager::{
    skills_files::{SkillsComparator, skills_paths, tree_hash},
    skills_manager::{SkillsManager, SkillsSettings},
    skills_migration::import_skills,
};
use ivy_contracts::{
    IvyError, IvyJson, JsString, Outcome,
    hash::{digest, hash_json},
    json::{canonical, parse, stringify},
};
use ivy_host_runtime::node_path::{self, Context};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Semaphore;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-skills-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn value(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap_or_else(|| panic!("missing {key}"))
}
fn text(value: &IvyJson, key: &str) -> JsString {
    field(value, key).as_js_string().unwrap().clone()
}
fn fixture(input: IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .arg("tests/fixtures/agent-skills-files-oracle.mjs")
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
        .write_all(stringify(&input, 8 * 1024 * 1024).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        8 * 1024 * 1024,
    )
    .unwrap()
}
fn catalog() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| fixture(IvyJson::object([("action", "catalog".into())])))
}
fn comparator() -> Arc<SkillsComparator> {
    let pairs = field(catalog(), "pairs").as_array().unwrap();
    // These original Node pairs isolate filesystem behavior while the actual
    // Shared locale API is pending. They do not qualify a production comparator.
    Arc::new(move |left, right| {
        let pair = pairs
            .iter()
            .find(|row| text(row, "left") == *left && text(row, "right") == *right)
            .expect("unexpected synthetic locale pair");
        field(pair, "result")
            .as_f64()
            .unwrap()
            .partial_cmp(&0.0)
            .unwrap()
    })
}
fn manager(prepared: &IvyJson) -> SkillsManager {
    SkillsManager::new(SkillsSettings {
        state_home: text(prepared, "home"),
        root: text(prepared, "target"),
        host_id: "fixture".into(),
        hive_url: "https://fixture.example".into(),
        defaults: field(prepared, "defaults").clone(),
        compare: comparator(),
    })
}
fn status(manager: &SkillsManager) -> IvyJson {
    let status = manager.status();
    assert_eq!(status.observed_at.len(), 24);
    assert!(chrono::DateTime::parse_from_rfc3339(&status.observed_at).is_ok());
    IvyJson::object(
        status
            .json()
            .as_object()
            .unwrap()
            .iter()
            .filter(|(key, _)| *key != JsString::from("observedAt"))
            .cloned(),
    )
}
fn compare(actual: &IvyJson, expected: &IvyJson, label: &str) {
    assert_eq!(
        canonical(actual, 8 * 1024 * 1024).unwrap(),
        canonical(expected, 8 * 1024 * 1024).unwrap(),
        "{label}"
    );
}
struct Missing;
impl RpcClient for Missing {
    fn request<'a>(&'a self, method: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(method, "objects.stat");
            Err(IvyError::new("not_found", "fixture missing"))
        })
    }
}
struct Failed;
impl RpcClient for Failed {
    fn request<'a>(&'a self, _: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async {
            Err(IvyError::new("service_unavailable", "fixture failure")
                .with_outcome(Outcome::Unknown))
        })
    }
}

#[tokio::test]
async fn real_skills_files_and_bootstrap_match_original_without_runtime_legacy_import() {
    let root = Root::new();
    for (bootstrap, key, count) in [(false, "applyModes", 20), (true, "bootstrapModes", 8)] {
        let rows = field(catalog(), key).as_array().unwrap();
        assert_eq!(rows.len(), count);
        for (index, mode) in rows.iter().enumerate() {
            let original = root.0.join(format!("original-{key}-{index}"));
            let actual_root = root.0.join(format!("rust-{key}-{index}"));
            let request = |action: &str, root: &Path| {
                IvyJson::object([
                    ("action", action.into()),
                    ("root", value(root)),
                    ("mode", mode.clone()),
                    ("bootstrap", bootstrap.into()),
                ])
            };
            let expected = fixture(request("run", &original));
            let prepared = fixture(request("prepare", &actual_root));
            let manager = manager(&prepared);
            let error = if bootstrap {
                manager
                    .bootstrap()
                    .await
                    .err()
                    .map(|e| e.to_wire())
                    .unwrap_or(IvyJson::Null)
            } else {
                manager
                    .synchronize(if mode == &IvyJson::from("read-failure") {
                        Arc::new(Failed) as Arc<dyn RpcClient>
                    } else {
                        Arc::new(Missing)
                    })
                    .wait()
                    .await;
                IvyJson::Null
            };
            manager.close().wait().await;
            let actual = fixture(IvyJson::object([
                ("action", "snapshot".into()),
                ("root", value(&actual_root)),
                (
                    "provided",
                    IvyJson::object([("status", status(&manager)), ("error", error)]),
                ),
            ]));
            compare(&actual, &expected, &format!("{key} {mode:?}"));
        }
    }
}

#[test]
fn tree_hash_matches_original_depth_first_locale_ties_and_observed_link_boundaries() {
    let root = Root::new();
    let compare_names = comparator();
    for (index, mode) in field(catalog(), "treeModes")
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let expected = fixture(IvyJson::object([
            ("action", "treeRun".into()),
            ("root", value(&root.0.join(format!("original-{index}")))),
            ("mode", mode.clone()),
        ]));
        let path = fixture(IvyJson::object([
            ("action", "treePrepare".into()),
            ("root", value(&root.0.join(format!("rust-{index}")))),
            ("mode", mode.clone()),
        ]));
        let actual = match tree_hash(path.as_js_string().unwrap(), compare_names.as_ref()) {
            Ok(hash) => {
                IvyJson::object([("result", hash.map(IvyJson::from).unwrap_or(IvyJson::Null))])
            }
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        compare(&actual, &expected, &format!("tree {mode:?}"));
    }
}
struct Held {
    calls: Mutex<Vec<String>>,
    started: Semaphore,
    release: Semaphore,
}
impl RpcClient for Held {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let first = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(method.into());
                calls.len() == 1
            };
            if first {
                self.started.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            Missing.request(method, params, options).await
        })
    }
}
struct Unused(AtomicUsize);
impl RpcClient for Unused {
    fn request<'a>(&'a self, _: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { panic!("coalesced connection must not run") })
    }
}
#[tokio::test]
async fn skills_lifetime_coalesces_drains_and_retains_success_after_errors() {
    let root = Root::new();
    let original = root.0.join("original");
    let actual_root = root.0.join("rust");
    let expected = fixture(IvyJson::object([
        ("action", "lifecycle".into()),
        ("root", value(&original)),
    ]));
    let prepared = fixture(IvyJson::object([
        ("action", "prepare".into()),
        ("mode", "fresh".into()),
        ("root", value(&actual_root)),
    ]));
    let manager = manager(&prepared);
    let mut states = vec![status(&manager)];
    manager.synchronize(Arc::new(Missing)).wait().await;
    states.push(status(&manager));
    manager.synchronize(Arc::new(Failed)).wait().await;
    states.push(status(&manager));
    let path = actual_root.join("skills/alpha/SKILL.md");
    let good = fs::read(&path).unwrap();
    fs::write(&path, "local edit").unwrap();
    manager.synchronize(Arc::new(Missing)).wait().await;
    states.push(status(&manager));
    fs::write(&path, good).unwrap();
    manager.synchronize(Arc::new(Missing)).wait().await;
    manager.close().wait().await;
    states.push(status(&manager));
    let held = Arc::new(Held {
        calls: Mutex::new(vec![]),
        started: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    let first = manager.synchronize(held.clone());
    held.started.acquire().await.unwrap().forget();
    let unused = Arc::new(Unused(AtomicUsize::new(0)));
    let second = manager.synchronize(unused.clone());
    let mut close = Box::pin(manager.close().wait());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), close.as_mut())
            .await
            .is_err()
    );
    let during = status(&manager);
    held.release.add_permits(1);
    first.wait().await;
    second.wait().await;
    close.await;
    assert_eq!(unused.0.load(Ordering::SeqCst), 0);
    drop(manager.synchronize(Arc::new(Missing)));
    manager.close().wait().await;
    let actual = fixture(IvyJson::object([
        ("action", "snapshot".into()),
        ("root", value(&actual_root)),
        (
            "provided",
            IvyJson::object([
                ("states", IvyJson::Array(states)),
                ("during", during),
                ("after", status(&manager)),
                (
                    "calls",
                    IvyJson::Array(
                        held.calls
                            .lock()
                            .unwrap()
                            .iter()
                            .map(|s| s.clone().into())
                            .collect(),
                    ),
                ),
                ("secondCalls", IvyJson::Number(0.0)),
                ("closed", true.into()),
            ]),
        ),
    ]));
    let projected = IvyJson::object(
        expected
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, _)| (k.clone(), actual.get_key(k).unwrap().clone())),
    );
    compare(&projected, &expected, "lifecycle");
}

#[tokio::test]
async fn explicit_skills_import_retains_source_and_metadata_then_uses_current_runtime() {
    let root = Root::new();
    for (version, in_place) in [(1, false), (1, true), (2, false), (2, true)] {
        let base = root.0.join(format!("v{version}-{in_place}"));
        let prepared = fixture(IvyJson::object([
            ("action", "migrationPrepare".into()),
            ("root", value(&base)),
            ("version", IvyJson::Number(version as f64)),
            ("inPlace", in_place.into()),
        ]));
        let source = node_path::filesystem_path(&text(&prepared, "source"));
        let original = fs::read(&source).unwrap();
        let state = field(&prepared, "state");
        let authority = field(state, "authority").as_str().unwrap();
        let result = import_skills(
            text(&prepared, "source"),
            text(&prepared, "target"),
            authority.clone(),
            comparator(),
        )
        .await
        .unwrap();
        assert_eq!(result.unchanged, version == 2 && in_place);
        assert_eq!(result.copied_skills, if version == 1 { 2 } else { 0 });
        if let Some(backup) = result.source_backup {
            assert_eq!(
                fs::read(node_path::filesystem_path(&backup)).unwrap(),
                original
            );
        }
        if !in_place {
            assert_eq!(fs::read(&source).unwrap(), original);
        }
        assert_eq!(
            digest(&original),
            field(&prepared, "sourceDigest").as_str().unwrap()
        );
        let current = parse(
            &fs::read_to_string(node_path::filesystem_path(&result.state_path)).unwrap(),
            4 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(current.get("schemaVersion"), Some(&IvyJson::Number(2.0)));
        assert_eq!(current.get("extra"), state.get("extra"));
        assert_eq!(current.get("appliedVersion"), state.get("appliedVersion"));
        for name in ["alpha", "beta"] {
            let path = node_path::from_native(&base.join("skills").join(name));
            let hash = tree_hash(&path, comparator().as_ref()).unwrap().unwrap();
            assert_eq!(
                field(field(&current, "applied"), name),
                &IvyJson::from(hash)
            );
            assert_eq!(
                fs::read_to_string(base.join("skills").join(name).join("SKILL.md")).unwrap(),
                format!("retained {name}")
            );
        }
        if version == 1 {
            for name in ["alpha", "beta"] {
                assert_eq!(
                    fs::read_to_string(
                        base.join("skills")
                            .join(format!("ivy-managed-{name}"))
                            .join("SKILL.md")
                    )
                    .unwrap(),
                    format!("retained {name}")
                );
            }
        }
        let repeated = import_skills(
            text(&prepared, "source"),
            text(&prepared, "target"),
            authority,
            comparator(),
        )
        .await
        .unwrap();
        assert!(repeated.unchanged);
        assert_eq!(repeated.copied_skills, 0);
        let manager = manager(&prepared);
        manager.bootstrap().await.unwrap();
        assert_eq!(manager.status().state, "pending");
        manager.synchronize(Arc::new(Missing)).wait().await;
        assert_eq!(manager.status().state, "applied");
        assert_eq!(
            fs::read_to_string(base.join("skills/alpha/SKILL.md")).unwrap(),
            "default\né"
        );
        if !in_place {
            assert_eq!(fs::read(&source).unwrap(), original);
        }
    }
}

#[tokio::test]
async fn import_conflicts_preserve_source_and_unowned_targets() {
    let root = Root::new();
    for mode in [
        "local-edit",
        "missing-tree",
        "target-collision",
        "wrong-authority",
        "existing-owner",
    ] {
        let base = root.0.join(mode);
        let prepared = fixture(IvyJson::object([
            ("action", "migrationPrepare".into()),
            ("root", value(&base)),
        ]));
        let source = node_path::filesystem_path(&text(&prepared, "source"));
        let source_bytes = fs::read(&source).unwrap();
        let mut authority = field(field(&prepared, "state"), "authority")
            .as_str()
            .unwrap();
        match mode {
            "local-edit" => {
                fs::write(base.join("skills/ivy-managed-alpha/SKILL.md"), "local edit").unwrap()
            }
            "missing-tree" => fs::remove_dir_all(base.join("skills/ivy-managed-beta")).unwrap(),
            "target-collision" => {
                fs::create_dir(base.join("skills/beta")).unwrap();
                fs::write(base.join("skills/beta/SKILL.md"), "foreign").unwrap();
            }
            "wrong-authority" => authority = "other".into(),
            "existing-owner" => {
                let paths =
                    skills_paths(&text(&prepared, "target"), &Context::current().unwrap()).unwrap();
                fs::create_dir(node_path::filesystem_path(&paths.state_root)).unwrap();
                fs::write(node_path::filesystem_path(&paths.state_path), "{}").unwrap();
            }
            _ => unreachable!(),
        }
        let error = import_skills(
            text(&prepared, "source"),
            text(&prepared, "target"),
            authority,
            comparator(),
        )
        .await
        .err()
        .expect("must refuse unsafe import");
        assert_eq!(error.code, "target_conflict", "{mode}");
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        assert!(!base.join("skills/alpha").exists());
        if mode == "target-collision" {
            assert_eq!(
                fs::read_to_string(base.join("skills/beta/SKILL.md")).unwrap(),
                "foreign"
            );
        }
    }
}

#[tokio::test]
async fn normal_runtime_does_not_read_legacy_state_or_convert_version_one() {
    let root = Root::new();
    for in_place in [false, true] {
        let base = root.0.join(format!("legacy-{in_place}"));
        let prepared = fixture(IvyJson::object([
            ("action", "migrationPrepare".into()),
            ("root", value(&base)),
            ("inPlace", in_place.into()),
        ]));
        let source = node_path::filesystem_path(&text(&prepared, "source"));
        let before = fs::read(&source).unwrap();
        let manager = manager(&prepared);
        manager.synchronize(Arc::new(Missing)).wait().await;
        assert_eq!(
            manager.status().state,
            if in_place { "conflict" } else { "applied" }
        );
        assert_eq!(fs::read(&source).unwrap(), before);
        assert_eq!(base.join("skills/alpha").exists(), !in_place);
        assert!(base.join("skills/ivy-managed-alpha").exists());
    }
}
