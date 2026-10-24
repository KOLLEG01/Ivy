//! Native skills files, ownership, import and lifetime checks.
use ivy_agent_manager::{
    offline_import::OfflineImport,
    skills_files::{SkillsComparator, skills_paths, tree_hash},
    skills_manager::{SkillsManager, SkillsSettings},
    skills_migration::import_skills,
};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    hash::{digest, hash_json},
    json::{canonical, parse, stringify},
};
use ivy_host_runtime::node_path::{self, Context};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Semaphore;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!("ivy-skills-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("Retained skills fixture: {}", self.0.display());
            return;
        }
        let actual = fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            actual
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-skills-")
        );
        fs::remove_dir_all(actual).unwrap();
    }
}
fn value(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn opaque_name(prefix: &str) -> std::ffi::OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&prefix.encode_utf16().chain([0xd800]).collect::<Vec<_>>())
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(prefix.bytes().chain([0xff]).collect())
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.required(key).unwrap()
}
fn text(value: &IvyJson, key: &str) -> JsString {
    field(value, key).as_js_string().unwrap().clone()
}
fn comparator() -> Arc<SkillsComparator> {
    Arc::new(|a, b| ivy_agent_manager::runtime::compare_names(a, b, false))
}
fn compare(actual: &IvyJson, expected: &IvyJson, label: &str) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap(),
        "{label}"
    );
}
fn defaults() -> IvyJson {
    parse(r#"{"schemaVersion":1,"hostId":null,"enabled":true,"files":[{"path":"alpha/SKILL.md","content":"default\né"},{"path":"alpha/nested/notes.txt","content":"contents"},{"path":"beta/SKILL.md","content":"second"}]}"#, usize::MAX).unwrap()
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
fn prepared(base: &Path) -> IvyJson {
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    IvyJson::object([
        ("home", value(&home)),
        ("target", value(&base.join("skills"))),
        ("defaults", defaults()),
    ])
}
fn fixture(input: IvyJson) -> IvyJson {
    assert_eq!(field(&input, "action"), &IvyJson::from("migrationPrepare"));
    let base = node_path::filesystem_path(field(&input, "root").as_js_string().unwrap());
    let mut result = prepared(&base);
    let target = base.join("skills");
    let paths = skills_paths(
        &node_path::from_native(&target),
        &Context::current().unwrap(),
    )
    .unwrap();
    let version = input
        .get("version")
        .and_then(IvyJson::as_f64)
        .unwrap_or(1.0);
    let in_place = input
        .get("inPlace")
        .and_then(IvyJson::as_bool)
        .unwrap_or(false);
    let mut applied = Vec::new();
    for name in ["alpha", "beta"] {
        let directory = if version == 1.0 {
            format!("ivy-managed-{name}")
        } else {
            name.into()
        };
        let path = target.join(&directory);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), format!("retained {name}")).unwrap();
        let hash = tree_hash(&node_path::from_native(&path), comparator().as_ref())
            .unwrap()
            .unwrap();
        applied.push((JsString::from(directory), IvyJson::from(hash)));
    }
    let state = IvyJson::object([
        ("schemaVersion", IvyJson::Number(version)),
        (
            "authority",
            hash_json(&IvyJson::object([
                ("hive", "https://fixture.example".into()),
                ("hostId", "fixture".into()),
            ]))
            .unwrap()
            .into(),
        ),
        ("applied", IvyJson::object(applied)),
        ("appliedVersion", "prior".into()),
        (
            "extra",
            parse(r#"{"retained":[1,null,"metadata"]}"#, usize::MAX).unwrap(),
        ),
    ]);
    let source = if in_place {
        node_path::filesystem_path(&paths.state_path)
    } else {
        base.join("home/legacy-skills.json")
    };
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let bytes = stringify(&state, usize::MAX).unwrap();
    fs::write(&source, &bytes).unwrap();
    result.insert("source", value(&source));
    result.insert("sourceDigest", digest(&bytes).into());
    result.insert("state", state);
    result
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
        Box::pin(async { Err(IvyError::new("disconnected", "Fixture connection lost")) })
    }
}

#[tokio::test]
async fn skills_files_bootstrap_and_current_ownership_preserve_local_edits() {
    let root = Root::new();
    let input = prepared(&root.0);
    let owner = manager(&input);
    owner.synchronize(Arc::new(Missing)).wait().await;
    assert_eq!(owner.status().state, "applied");
    let applied = owner.status().applied_version.unwrap();
    let paths = skills_paths(&text(&input, "target"), &Context::current().unwrap()).unwrap();
    let state_path = node_path::filesystem_path(&paths.state_path);
    let state = parse(&fs::read_to_string(&state_path).unwrap(), usize::MAX).unwrap();
    assert_eq!(field(&state, "schemaVersion"), &IvyJson::Number(2.0));
    assert_eq!(
        field(&state, "appliedVersion"),
        &IvyJson::from(applied.clone())
    );
    let bootstrapped = manager(&input);
    bootstrapped.bootstrap().await.unwrap();
    assert_eq!(bootstrapped.status().state, "pending");
    assert!(bootstrapped.status().applied_version.is_none());
    assert_eq!(
        parse(&fs::read_to_string(&state_path).unwrap(), usize::MAX).unwrap(),
        state
    );
    let file = root.0.join("skills/alpha/SKILL.md");
    let original = fs::read(&file).unwrap();
    fs::write(&file, "local edit").unwrap();
    let state_before = fs::read(&state_path).unwrap();
    owner.synchronize(Arc::new(Missing)).wait().await;
    assert_eq!(owner.status().state, "conflict");
    assert_eq!(owner.status().code.as_deref(), Some("target_conflict"));
    assert_eq!(owner.status().applied_version, Some(applied));
    assert_eq!(fs::read(&file).unwrap(), b"local edit");
    assert_eq!(fs::read(&state_path).unwrap(), state_before);
    fs::write(&file, original).unwrap();
    owner.synchronize(Arc::new(Missing)).wait().await;
    assert_eq!(owner.status().state, "applied");
    owner.synchronize(Arc::new(Failed)).wait().await;
    assert_eq!(owner.status().state, "pending");
    assert!(owner.status().applied_version.is_some());
    owner.close().wait().await;

    let foreign = Root::new();
    let input = prepared(&foreign.0);
    fs::create_dir_all(foreign.0.join("skills/alpha")).unwrap();
    fs::write(foreign.0.join("skills/alpha/SKILL.md"), "foreign").unwrap();
    let owner = manager(&input);
    owner.synchronize(Arc::new(Missing)).wait().await;
    assert_eq!(owner.status().state, "conflict");
    assert_eq!(
        fs::read(foreign.0.join("skills/alpha/SKILL.md")).unwrap(),
        b"foreign"
    );
    owner.close().wait().await;
}

#[test]
fn tree_hash_keeps_the_published_path_digest_format_and_native_file_boundaries() {
    let root = Root::new();
    let target = root.0.join("tree");
    fs::create_dir_all(target.join("nested/empty")).unwrap();
    for (name, bytes) in [
        ("z.txt", "last"),
        ("nested/b.txt", "nested"),
        ("a.txt", "first"),
    ] {
        fs::write(target.join(name), bytes).unwrap();
    }
    let expected = hash_json(&IvyJson::Array(
        [
            ("a.txt", "first"),
            ("nested/b.txt", "nested"),
            ("z.txt", "last"),
        ]
        .into_iter()
        .map(|(name, bytes)| IvyJson::Array(vec![name.into(), digest(bytes).into()]))
        .collect(),
    ))
    .unwrap();
    assert_eq!(
        tree_hash(&node_path::from_native(&target), comparator().as_ref()).unwrap(),
        Some(expected)
    );
    assert_eq!(
        tree_hash(
            &node_path::from_native(&root.0.join("absent")),
            comparator().as_ref()
        )
        .unwrap(),
        None
    );
    let native = root.0.join("native-tree");
    let directory = opaque_name("directory-");
    let file = opaque_name("file-");
    fs::create_dir_all(native.join(&directory)).unwrap();
    fs::write(native.join(&directory).join(&file), b"native bytes").unwrap();
    let mut relative = node_path::node_utf8(&directory).0;
    relative.push(47);
    relative.extend(node_path::node_utf8(&file).0);
    let expected = hash_json(&IvyJson::Array(vec![IvyJson::Array(vec![
        IvyJson::String(JsString(relative)),
        digest(b"native bytes").into(),
    ])]))
    .unwrap();
    assert_eq!(
        tree_hash(&node_path::from_native(&native), comparator().as_ref()).unwrap(),
        Some(expected)
    );
    let alias = file.to_string_lossy().into_owned();
    fs::write(native.join(&directory).join(&alias), b"alias bytes").unwrap();
    let before = tree_hash(&node_path::from_native(&native), comparator().as_ref()).unwrap();
    fs::write(native.join(&directory).join(&file), b"edited native bytes").unwrap();
    assert_ne!(
        tree_hash(&node_path::from_native(&native), comparator().as_ref()).unwrap(),
        before
    );
    assert_eq!(
        fs::read(native.join(&directory).join(&alias)).unwrap(),
        b"alias bytes"
    );
    fs::write(target.join("a.txt"), vec![b'x'; 256 * 1024 + 1]).unwrap();
    assert_eq!(
        tree_hash(&node_path::from_native(&target), comparator().as_ref())
            .unwrap_err()
            .code,
        "target_conflict"
    );
    fs::write(target.join("a.txt"), "first").unwrap();
    let outside = root.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("retained"), "unchanged").unwrap();
    let link = target.join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        tree_hash(&node_path::from_native(&target), comparator().as_ref())
            .unwrap_err()
            .code,
        "target_conflict"
    );
    assert_eq!(fs::read(outside.join("retained")).unwrap(), b"unchanged");
}

struct Held {
    started: Semaphore,
    release: Semaphore,
    calls: Mutex<Vec<String>>,
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
async fn skills_lifetime_coalesces_and_drains_the_actual_application() {
    let root = Root::new();
    let owner = manager(&prepared(&root.0));
    let held = Arc::new(Held {
        calls: Mutex::new(vec![]),
        started: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    let first = owner.synchronize(held.clone());
    held.started.acquire().await.unwrap().forget();
    let unused = Arc::new(Unused(AtomicUsize::new(0)));
    let second = owner.synchronize(unused.clone());
    let mut close = Box::pin(owner.close().wait());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), close.as_mut())
            .await
            .is_err()
    );
    held.release.add_permits(1);
    first.wait().await;
    second.wait().await;
    close.await;
    assert_eq!(unused.0.load(Ordering::SeqCst), 0);
    assert_eq!(owner.status().state, "applied");
    assert_eq!(
        fs::read(root.0.join("skills/alpha/SKILL.md")).unwrap(),
        "default\né".as_bytes()
    );
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
        let directory = opaque_name("directory-");
        let file = opaque_name("file-");
        let source_tree = base.join("skills").join(if version == 1 {
            "ivy-managed-alpha"
        } else {
            "alpha"
        });
        fs::create_dir(source_tree.join(&directory)).unwrap();
        fs::write(
            source_tree.join(&directory).join(&file),
            b"native import bytes",
        )
        .unwrap();
        let mut state = parse(&fs::read_to_string(&source).unwrap(), usize::MAX).unwrap();
        let mut applied = field(&state, "applied").clone();
        applied.insert(
            if version == 1 {
                "ivy-managed-alpha"
            } else {
                "alpha"
            },
            tree_hash(&node_path::from_native(&source_tree), comparator().as_ref())
                .unwrap()
                .unwrap()
                .into(),
        );
        state.insert("applied", applied);
        let source_bytes = stringify(&state, usize::MAX).unwrap();
        fs::write(&source, &source_bytes).unwrap();
        let mut prepared = prepared;
        prepared.insert("state", state);
        prepared.insert("sourceDigest", digest(&source_bytes).into());
        let original = fs::read(&source).unwrap();
        let state = field(&prepared, "state");
        let authority = field(state, "authority").as_str().unwrap();
        let input = IvyJson::object([
            ("kind", "skills".into()),
            ("sourceState", field(&prepared, "source").clone()),
            ("root", field(&prepared, "target").clone()),
            ("hostId", "fixture".into()),
            ("hiveUrl", "https://fixture.example".into()),
        ]);
        let result = OfflineImport::from_snapshot(&input)
            .unwrap()
            .run(comparator())
            .await
            .unwrap();
        assert_eq!(
            result.get("unchanged"),
            Some(&IvyJson::Bool(version == 2 && in_place))
        );
        assert_eq!(
            result.get("copiedSkills"),
            Some(&IvyJson::Number(if version == 1 { 2.0 } else { 0.0 }))
        );
        if let Some(backup) = result.get("sourceBackup").and_then(IvyJson::as_js_string) {
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
            &fs::read_to_string(node_path::filesystem_path(&text(&result, "statePath"))).unwrap(),
            4 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(current.get("schemaVersion"), Some(&IvyJson::Number(2.0)));
        compare(
            field(&current, "extra"),
            field(state, "extra"),
            "retained metadata values",
        );
        assert_eq!(current.get("appliedVersion"), state.get("appliedVersion"));
        assert_eq!(
            fs::read(base.join("skills/alpha").join(&directory).join(&file)).unwrap(),
            b"native import bytes"
        );
        assert_eq!(
            fs::read(source_tree.join(&directory).join(&file)).unwrap(),
            b"native import bytes"
        );
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
