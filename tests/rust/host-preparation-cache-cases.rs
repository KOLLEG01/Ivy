use ivy_contracts::{JsString, json};
use ivy_host_runtime::{
    IvyJson, IvyResult, filesystem,
    preparation_cache::{self, PreparationCache, WorkspaceCompare},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
const KEY: &str = "fixture-cache-key";
type Links = Vec<(JsString, JsString)>;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
        let mut name =
            std::ffi::OsString::from(format!("ivy-preparation-cache-{}-", uuid::Uuid::new_v4()));
        name.push(opaque_name());
        let root = temporary.join(name);
        assert_eq!(root.parent(), Some(temporary.as_path()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn root(&self) -> PathBuf {
        self.0.clone()
    }
    fn write(&self, local: impl AsRef<Path>, bytes: &[u8]) {
        let path = self.root().join(local);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn mkdir(&self, local: &str) {
        fs::create_dir_all(self.root().join(local)).unwrap();
    }
    fn metadata(&self, value: &IvyJson) {
        self.write(
            "cache/cache.json",
            json::canonical(value, usize::MAX).unwrap().as_bytes(),
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        #[cfg(windows)]
        fn writable(path: &Path) {
            if let Ok(metadata) = fs::symlink_metadata(path) {
                if metadata.file_type().is_symlink() {
                    return;
                }
                if metadata.is_dir() {
                    if let Ok(entries) = fs::read_dir(path) {
                        for entry in entries.flatten() {
                            writable(&entry.path())
                        }
                    }
                }
                if metadata.permissions().readonly() {
                    let mut permissions = metadata.permissions();
                    permissions.set_readonly(false);
                    let _ = fs::set_permissions(path, permissions);
                }
            }
        }
        #[cfg(windows)]
        writable(&self.0);
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        if self.0.parent() == Some(base.as_path()) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
fn opaque_name() -> std::ffi::OsString {
    let mut name = std::ffi::OsString::from("native ü 空 😀; ' ");
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        name.push(std::ffi::OsString::from_wide(&[0xd800]));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        name.push(std::ffi::OsString::from_vec(vec![0xff]));
    }
    name
}
fn links(values: &[(&str, &str)]) -> Links {
    values
        .iter()
        .map(|(location, target)| ((*location).into(), (*target).into()))
        .collect()
}
fn wire(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn identity(links: &Links) -> PreparationCache {
    PreparationCache::new(
        KEY.into(),
        links.clone(),
        ivy_host_runtime::artifact::native_file_compare().unwrap(),
    )
}
fn metadata(entries: IvyJson) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(3.0)),
        ("key", KEY.into()),
        ("externalOnly", true.into()),
        ("workspaceLinks", entries),
    ])
}
fn assert_json_eq(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        json::canonical(actual, usize::MAX).unwrap(),
        json::canonical(expected, usize::MAX).unwrap()
    );
}
fn succeeds(result: &IvyJson, value: IvyJson) {
    assert_eq!(
        result,
        &IvyJson::object([("ok", true.into()), ("value", value)])
    );
}
fn refuses(result: &IvyJson, code: &str) {
    assert_eq!(result.get("ok"), Some(&false.into()));
    assert_eq!(
        result
            .required("error")
            .unwrap()
            .required("data")
            .unwrap()
            .get("code"),
        Some(&code.into())
    );
}
async fn operation(
    operation: &str,
    fixture: &Fixture,
    links: &Links,
    source_local: &str,
    cache_local: &str,
) -> IvyJson {
    let identity = identity(links);
    let actual = fixture.root();
    let source = actual.join(source_local);
    let cache = actual.join(cache_local);
    wire(match operation {
        "restore" => identity.restore(source, cache).await.map(IvyJson::Bool),
        "retain" => identity.retain(source, cache).await.map(|_| IvyJson::Null),
        "copy" => preparation_cache::copy_external_dependencies(source, cache, links.clone())
            .await
            .map(|_| IvyJson::Null),
        _ => unreachable!(),
    })
}

#[test]
fn schema_three_key_keeps_process_version_and_all_runtime_coordinates() {
    for (os, arch, node, expected) in [
        (
            "win32",
            "x64",
            "v24.18.0",
            "sha256:dbd69d4da66d0c529831014f182e9d284be1d08d52de714c84eac29f50b699cc",
        ),
        (
            "win32",
            "arm64",
            "v24.18.1",
            "sha256:5362ee5e8c78c51d927b527bc0fb0bcdb6a97942faad9e734e24ccf9c9c67dbb",
        ),
        (
            "linux",
            "x64",
            "v24.18.0",
            "sha256:63abf4e39aa6c72f4241e83b6a1bac7ae164cd87c4db5f108633b5e90eecc3c6",
        ),
        (
            "linux",
            "arm64",
            "v24.19.0",
            "sha256:e5e842fb66e82a4dcd5d37c1c0b9a53e790f54a1b6ad232bc6098638140e2d05",
        ),
        (
            "win32",
            "x64",
            "24.18.0",
            "sha256:9401f14f9552ed4e50cda576f83230cf1c7c1282469b5cb3a5de474d2b744c36",
        ),
    ] {
        assert_eq!(
            preparation_cache::dependency_cache_key(
                "sha256:lock ü 空 😀".into(),
                "npm@11.11.0".into(),
                os.into(),
                arch.into(),
                node.into(),
            )
            .unwrap(),
            expected,
        );
    }
}

#[test]
fn cache_metadata_preserves_map_updates_supplied_order_stable_ties_and_full_entry_hash() {
    let links = links(&[
        ("node_modules/Z", "last"),
        ("node_modules/é", "composed"),
        ("node_modules/e\u{301}", "decomposed"),
        ("node_modules/ä", "umlaut"),
        ("node_modules/a", "lower"),
        ("node_modules/A", "upper"),
        ("node_modules/Z", "updated"),
        ("node_modules/空 😀", "unicode"),
    ]);
    let groups: Vec<Vec<JsString>> = [
        vec!["node_modules/空 😀"],
        vec!["node_modules/a"],
        vec!["node_modules/A"],
        vec!["node_modules/ä"],
        vec!["node_modules/é", "node_modules/e\u{301}"],
        vec!["node_modules/Z"],
    ]
    .into_iter()
    .map(|group| group.into_iter().map(JsString::from).collect())
    .collect();
    let compare: WorkspaceCompare = Arc::new(move |left, right| {
        let rank = |value: &JsString| {
            groups
                .iter()
                .position(|group| group.contains(value))
                .unwrap()
        };
        rank(left).cmp(&rank(right))
    });
    let identity = PreparationCache::new(KEY.into(), links.clone(), compare);
    let expected = [
        ("node_modules/空 😀", "unicode"),
        ("node_modules/a", "lower"),
        ("node_modules/A", "upper"),
        ("node_modules/ä", "umlaut"),
        ("node_modules/é", "composed"),
        ("node_modules/e\u{301}", "decomposed"),
        ("node_modules/Z", "updated"),
    ]
    .into_iter()
    .map(|(location, target)| {
        IvyJson::object([("location", location.into()), ("target", target.into())])
    })
    .collect();
    assert_eq!(identity.workspace_entries(), IvyJson::Array(expected));
    let valid = metadata(identity.workspace_entries());
    let mut extra_top = valid.clone();
    extra_top.insert("ignored", true.into());
    let mut extra_entry = valid.clone();
    let mut entries = identity.workspace_entries().as_array().unwrap().to_vec();
    entries[0].insert("extra", true.into());
    extra_entry.insert("workspaceLinks", IvyJson::Array(entries));
    let mut reversed = valid.clone();
    let mut entries = identity.workspace_entries().as_array().unwrap().to_vec();
    entries.reverse();
    reversed.insert("workspaceLinks", IvyJson::Array(entries));
    let mut wrong_key = valid.clone();
    wrong_key.insert("key", "other-key".into());
    let mut schema = valid.clone();
    schema.insert("schemaVersion", IvyJson::Number(2.0));
    let mut external = valid.clone();
    external.insert("externalOnly", false.into());
    let mut wrong_entry = valid.clone();
    wrong_entry.insert(
        "workspaceLinks",
        IvyJson::Array(vec![IvyJson::object([
            ("location", IvyJson::Number(1.0)),
            ("target", "package".into()),
        ])]),
    );
    for (metadata, accept) in [
        (valid, true),
        (extra_top, true),
        (extra_entry, false),
        (reversed, false),
        (wrong_key, false),
        (schema, false),
        (external, false),
        (wrong_entry, false),
        (IvyJson::Null, false),
        (IvyJson::Array(vec![]), false),
    ] {
        let actual = wire(identity.valid_metadata(&metadata).map(IvyJson::Bool));
        succeeds(&actual, accept.into());
    }
}

#[tokio::test]
async fn restore_miss_invalid_json_bounded_file_and_missing_modules_keep_distinct_outcomes() {
    for scenario in [
        "absent",
        "wrong-key",
        "syntax",
        "oversize",
        "directory",
        "missing-modules",
        "modules-file",
        "missing-workspace",
        "workspace-conflict",
    ] {
        let fixture = Fixture::new();
        fixture.write("source/retained", b"source retained");
        fixture.write("outside/retained", b"outside retained");
        let links = match scenario {
            "missing-workspace" => links(&[("node_modules/workspace", "packages/missing")]),
            "workspace-conflict" => links(&[("../outside", "packages/good")]),
            _ => vec![],
        };
        let identity = PreparationCache::new(
            KEY.into(),
            links,
            ivy_host_runtime::artifact::native_file_compare().unwrap(),
        );
        if scenario != "absent" {
            fixture.metadata(&metadata(identity.workspace_entries()));
        }
        match scenario {
            "wrong-key" => {
                let mut value = metadata(identity.workspace_entries());
                value.insert("key", "different".into());
                fixture.metadata(&value);
            }
            "syntax" => fixture.write("cache/cache.json", b"{"),
            "oversize" => fixture.write("cache/cache.json", &vec![b' '; 4097]),
            "directory" => {
                fs::remove_file(fixture.root().join("cache/cache.json")).unwrap();
                fs::create_dir(fixture.root().join("cache/cache.json")).unwrap();
            }
            "modules-file" => fixture.write("cache/node_modules", b"not a directory"),
            "missing-workspace" => {
                fixture.mkdir("cache/node_modules");
                fixture.write(
                    "source/node_modules/workspace/retained",
                    b"destination retained",
                );
            }
            "workspace-conflict" => {
                fixture.mkdir("cache/node_modules");
                fixture.mkdir("source/packages/good");
            }
            _ => (),
        }
        let root = fixture.root();
        let result = identity
            .restore(root.join("source"), root.join("cache"))
            .await;
        match scenario {
            "syntax" | "modules-file" => assert_eq!(result.unwrap_err().code, "internal_error"),
            "oversize" | "directory" => assert_eq!(result.unwrap_err().code, "invalid_arguments"),
            "workspace-conflict" => assert_eq!(result.unwrap_err().code, "target_conflict"),
            _ => assert!(!result.unwrap()),
        }
        assert_eq!(
            fs::read(root.join("source/retained")).unwrap(),
            b"source retained"
        );
        assert_eq!(
            fs::read(root.join("outside/retained")).unwrap(),
            b"outside retained"
        );
        if scenario == "missing-workspace" {
            assert_eq!(
                fs::read(root.join("source/node_modules/workspace/retained")).unwrap(),
                b"destination retained"
            );
        }
    }
}

#[tokio::test]
async fn retain_then_restore_keeps_external_identity_and_current_workspace_bytes_in_map_order() {
    let fixture = Fixture::new();
    let links = links(&[
        ("node_modules/z-workspace", "packages/current"),
        ("node_modules/a-workspace", "node_modules/z-workspace"),
    ]);
    fixture.write("source/node_modules/external/value", b"external package");
    fixture.write(
        "source/node_modules/@scope/external/value",
        b"scoped external",
    );
    fixture.write("source/node_modules/.package-lock.json", b"lock bytes");
    fixture.write(
        "source/node_modules/z-workspace/obsolete",
        b"obsolete install",
    );
    fixture.write(
        "source/node_modules/a-workspace/obsolete",
        b"obsolete install",
    );
    fixture.write("source/packages/current/value", b"current source");
    succeeds(
        &operation("retain", &fixture, &links, "source", "cache").await,
        IvyJson::Null,
    );
    assert_json_eq(
        &ivy_host_runtime::config::json_file(fixture.root().join("cache/cache.json"), 4096)
            .await
            .unwrap(),
        &metadata(identity(&links).workspace_entries()),
    );
    {
        let source = fixture.root().join("source");
        let cache = fixture.root().join("cache");
        assert!(!cache.join("node_modules/z-workspace").exists());
        assert!(!cache.join("node_modules/a-workspace").exists());
        assert_eq!(
            fs::read(source.join("node_modules/a-workspace/value")).unwrap(),
            b"current source"
        );
        assert_eq!(
            filesystem::stat_identity(&source.join("node_modules/.package-lock.json")).unwrap(),
            filesystem::stat_identity(&cache.join("node_modules/.package-lock.json")).unwrap()
        );
    }
    succeeds(
        &operation("retain", &fixture, &links, "source", "cache").await,
        IvyJson::Null,
    );
    {
        fs::remove_dir_all(fixture.root().join("source/node_modules")).unwrap();
    }
    fixture.write("source/packages/current/value", b"changed workspace source");
    succeeds(
        &operation("restore", &fixture, &links, "source", "cache").await,
        true.into(),
    );
    {
        assert_eq!(
            fs::read(fixture.root().join("source/node_modules/a-workspace/value")).unwrap(),
            b"changed workspace source"
        );
    }
}

#[tokio::test]
async fn missing_workspace_is_a_partial_restore_miss_but_policy_conflict_stays_an_error() {
    for missing in [true, false] {
        let fixture = Fixture::new();
        let links = links(&[
            ("node_modules/z-workspace", "packages/current"),
            (
                "node_modules/a-workspace",
                if missing {
                    "packages/missing"
                } else {
                    "../outside"
                },
            ),
        ]);
        let identity = identity(&links);
        fixture.metadata(&metadata(identity.workspace_entries()));
        fixture.write("cache/node_modules/external/value", b"cache bytes");
        fixture.write("source/packages/current/value", b"workspace bytes");
        fixture.write(
            "source/node_modules/a-workspace/retained",
            b"destination unchanged",
        );
        fixture.write("outside/retained", b"outside unchanged");
        let result = operation("restore", &fixture, &links, "source", "cache").await;
        if missing {
            succeeds(&result, false.into())
        } else {
            refuses(&result, "target_conflict")
        }
        {
            let root = fixture.root();
            assert_eq!(
                fs::read(root.join("source/node_modules/z-workspace/value")).unwrap(),
                b"workspace bytes"
            );
            assert_eq!(
                fs::read(root.join("source/node_modules/a-workspace/retained")).unwrap(),
                b"destination unchanged"
            );
            assert_eq!(
                fs::read(root.join("outside/retained")).unwrap(),
                b"outside unchanged"
            );
        }
    }
}

#[tokio::test]
async fn retain_existing_identity_skips_modules_check_but_syntax_and_type_errors_leave_source() {
    for scenario in [
        "source-absent",
        "valid-without-cached-modules",
        "syntax",
        "oversize",
        "directory",
        "modules-file",
    ] {
        let fixture = Fixture::new();
        let links = vec![];
        fixture.mkdir("source");
        if scenario != "source-absent" {
            fixture.write("source/node_modules/external/value", b"source bytes");
        }
        if scenario == "valid-without-cached-modules" {
            fixture.metadata(&metadata(IvyJson::Array(vec![])));
        }
        if scenario == "syntax" {
            fixture.write("cache/cache.json", b"{");
        }
        if scenario == "oversize" {
            fixture.write("cache/cache.json", &vec![b' '; 4097]);
        }
        if scenario == "directory" {
            fixture.mkdir("cache/cache.json");
        }
        if scenario == "modules-file" {
            {
                fs::remove_dir_all(fixture.root().join("source/node_modules")).unwrap();
            }
            fixture.write("source/node_modules", b"not a directory");
        }
        let result = operation("retain", &fixture, &links, "source", "cache").await;
        if scenario == "syntax" || scenario == "modules-file" {
            refuses(&result, "internal_error")
        } else {
            succeeds(&result, IvyJson::Null);
        }
        if scenario != "source-absent" && scenario != "modules-file" {
            assert_eq!(
                fs::read(fixture.root().join("source/node_modules/external/value")).unwrap(),
                b"source bytes"
            );
        }
        if scenario == "oversize" || scenario == "directory" {
            assert_json_eq(
                &ivy_host_runtime::config::json_file(fixture.root().join("cache/cache.json"), 4096)
                    .await
                    .unwrap(),
                &metadata(IvyJson::Array(vec![])),
            );
            assert_eq!(
                filesystem::stat_identity(
                    &fixture.root().join("source/node_modules/external/value")
                )
                .unwrap(),
                filesystem::stat_identity(
                    &fixture.root().join("cache/node_modules/external/value")
                )
                .unwrap(),
            );
        }
        assert!(
            !fs::read_dir(fixture.root()).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")),
            "failed retention must drain cleanup"
        );
    }
}

#[tokio::test]
async fn copy_phase_excludes_only_workspace_paths_preserves_bytes_and_refuses_self_copy() {
    fn files(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn walk(
            root: &Path,
            local: &Path,
            found: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
        ) {
            for entry in fs::read_dir(root.join(local)).unwrap() {
                let entry = entry.unwrap();
                let local = local.join(entry.file_name());
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    walk(root, &local, found);
                } else {
                    assert!(kind.is_file());
                    found.insert(local, fs::read(entry.path()).unwrap());
                }
            }
        }
        let mut found = std::collections::BTreeMap::new();
        walk(root, Path::new(""), &mut found);
        found
    }
    let fixture = Fixture::new();
    let opaque = PathBuf::from(opaque_name());
    let replacement = PathBuf::from(opaque.as_os_str().to_string_lossy().into_owned());
    let mut links = links(&[("node_modules/@scope/workspace", "packages/current")]);
    links.push((
        format!("node_modules/{}", replacement.to_str().unwrap()).into(),
        "packages/replacement".into(),
    ));
    fixture.write(
        Path::new("source/node_modules").join(&opaque),
        b"opaque external bytes",
    );
    fixture.write(
        Path::new("source/node_modules").join(&replacement),
        b"excluded replacement workspace",
    );
    fixture.write(
        "source/node_modules/@scope/workspace/deep/value",
        b"workspace excluded",
    );
    fixture.write(
        "source/node_modules/@scope/workspace-old/value",
        b"prefix sibling retained",
    );
    fixture.write(
        "source/node_modules/pkg/deep/ü 空 😀",
        b"external bytes\x00\xff",
    );
    fixture.write("source/node_modules/ordinary", b"ordinary bytes");
    fixture.write("cache/ordinary", b"replaced bytes");
    fixture.write("cache/retained", b"destination sibling");
    let root = fixture.root();
    let file = root.join("source/node_modules/pkg/deep/ü 空 😀");
    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&file, permissions).unwrap();
    let source_before = files(&root.join("source"));
    preparation_cache::copy_external_dependencies(root.join("source"), root.join("cache"), links)
        .await
        .unwrap();
    assert_eq!(
        files(&root.join("cache")),
        [
            (
                PathBuf::from("@scope/workspace-old/value"),
                b"prefix sibling retained".to_vec()
            ),
            (
                PathBuf::from("pkg/deep/ü 空 😀"),
                b"external bytes\x00\xff".to_vec()
            ),
            (PathBuf::from("ordinary"), b"ordinary bytes".to_vec()),
            (PathBuf::from("retained"), b"destination sibling".to_vec()),
            (opaque.clone(), b"opaque external bytes".to_vec()),
        ]
        .into_iter()
        .collect()
    );
    assert!(!root.join("cache/@scope/workspace").exists());
    assert!(!root.join("cache").join(&replacement).exists());
    assert_eq!(
        fs::metadata(&file).unwrap().permissions(),
        fs::metadata(root.join("cache/pkg/deep/ü 空 😀"))
            .unwrap()
            .permissions()
    );
    assert_eq!(files(&root.join("source")), source_before);
    assert_ne!(
        filesystem::stat_identity(&root.join("source/node_modules/ordinary")).unwrap(),
        filesystem::stat_identity(&root.join("cache/ordinary")).unwrap()
    );
    assert_ne!(
        filesystem::stat_identity(&root.join("source/node_modules").join(&opaque)).unwrap(),
        filesystem::stat_identity(&root.join("cache").join(&opaque)).unwrap()
    );
    fs::write(
        root.join("source/node_modules/ordinary"),
        b"later workspace bytes",
    )
    .unwrap();
    assert_eq!(
        fs::read(root.join("cache/ordinary")).unwrap(),
        b"ordinary bytes"
    );

    let fixture = Fixture::new();
    fixture.write(
        "source/node_modules/pkg/value",
        b"unchanged self-copy source",
    );
    let root = fixture.root();
    let before = files(&root.join("source"));
    let error = preparation_cache::copy_external_dependencies(
        root.join("source"),
        root.join("source/node_modules"),
        vec![],
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "internal_error");
    assert_eq!(files(&root.join("source")), before);
}

#[tokio::test]
async fn external_copy_keeps_observed_native_link_targets_and_target_bytes() {
    let fixture = Fixture::new();
    fixture.mkdir("source/node_modules");
    fixture.write("target/value", b"outside link target bytes");
    let root = fixture.root();
    let source_link = root.join("source/node_modules/observed");
    filesystem::directory_link(&root.join("target"), &source_link).unwrap();
    let target = fs::read_link(&source_link).unwrap();
    preparation_cache::copy_external_dependencies(root.join("source"), root.join("cache"), vec![])
        .await
        .unwrap();
    let destination = root.join("cache/observed");
    assert!(
        fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&destination).unwrap(), target);
    assert_eq!(
        fs::read(destination.join("value")).unwrap(),
        b"outside link target bytes"
    );
    assert_eq!(
        fs::read(root.join("target/value")).unwrap(),
        b"outside link target bytes"
    );
    assert_eq!(fs::read_link(&source_link).unwrap(), target);
}

#[tokio::test]
async fn cache_publication_retains_native_long_path_behavior() {
    let fixture = Fixture::new();
    fixture.write("source/node_modules/pkg/value", b"long cache path bytes");
    let cache = format!("{}cache", "long-directory-component/".repeat(12));
    succeeds(
        &operation("retain", &fixture, &vec![], "source", &cache).await,
        IvyJson::Null,
    );
    assert_eq!(
        fs::read(fixture.root().join("source/node_modules/pkg/value")).unwrap(),
        b"long cache path bytes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_retention_waiter_preserves_admitted_publication_and_workspace_restore() {
    use std::sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        mpsc,
    };
    use std::time::{Duration, Instant};
    let fixture = Fixture::new();
    let root = fixture.root();
    let links = links(&[
        ("node_modules/z-workspace", "packages/current"),
        ("node_modules/a-workspace", "node_modules/z-workspace"),
    ]);
    let qualified = identity(&links);
    fixture.write(
        "source/node_modules/external/value",
        b"retained external bytes",
    );
    fixture.write("source/packages/current/value", b"current workspace bytes");
    let expected = qualified.workspace_entries();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let first = AtomicBool::new(true);
    let compare: WorkspaceCompare = Arc::new(move |left, right| {
        if first.swap(false, AtomicOrdering::SeqCst) {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .expect("test must release the admitted writer");
        }
        let entries = expected.as_array().unwrap();
        let rank = |key: &JsString| {
            entries
                .iter()
                .position(|entry| entry.get("location") == Some(&IvyJson::String(key.clone())))
                .unwrap()
        };
        rank(left).cmp(&rank(right))
    });
    let identity = PreparationCache::new(KEY.into(), links, compare);
    let source = root.join("source");
    let cache = root.join("cache");
    let source_copy = source.clone();
    let cache_copy = cache.clone();
    let waiter = tokio::spawn(async move { identity.retain(source_copy, cache_copy).await });
    tokio::task::spawn_blocking(move || {
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("retention must reach its supplied comparator")
    })
    .await
    .unwrap();
    assert!(
        !source.join("node_modules").exists(),
        "the source move must already be admitted"
    );
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if cache.join("cache.json").is_file()
            && source.join("node_modules/a-workspace/value").is_file()
            && source.join("node_modules/external/value").is_file()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "admitted retention did not finish after waiter cancellation"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        fs::read(source.join("node_modules/a-workspace/value")).unwrap(),
        b"current workspace bytes"
    );
    assert_eq!(
        fs::read(source.join("node_modules/external/value")).unwrap(),
        b"retained external bytes"
    );
    assert_eq!(
        fs::read(cache.join("node_modules/external/value")).unwrap(),
        b"retained external bytes"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn retained_cache_reader_preserves_native_sharing_refusal_and_recovers_after_release() {
    use ivy_host_runtime::config;
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    fixture.write("source/node_modules/pkg/value", b"unchanged source");
    let expected = metadata(IvyJson::Array(vec![]));
    fixture.metadata(&expected);
    let root = fixture.root();
    let cache_file = root.join("cache/cache.json");
    let before = fs::read(&cache_file).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&cache_file)
        .unwrap();
    assert_eq!(fs::read(&cache_file).unwrap_err().raw_os_error(), Some(32));
    let result = operation("retain", &fixture, &vec![], "source", "cache").await;
    refuses(&result, "internal_error");
    drop(held);
    assert_eq!(fs::read(&cache_file).unwrap(), before);
    assert_eq!(
        fs::read(root.join("source/node_modules/pkg/value")).unwrap(),
        b"unchanged source"
    );
    succeeds(
        &operation("retain", &fixture, &vec![], "source", "cache").await,
        IvyJson::Null,
    );
    assert_json_eq(
        &config::json_file(&cache_file, 4096).await.unwrap(),
        &expected,
    );
}

#[tokio::test]
async fn cache_validation_ivy_errors_are_ignored_only_by_initial_retention() {
    let fixture = Fixture::new();
    fixture.write("source/node_modules/pkg/value", b"installed source bytes");
    fixture.write("cache/cache.json",br#"{"schemaVersion":3,"key":"fixture-cache-key","externalOnly":true,"workspaceLinks":[{"location":"node_modules/pkg","target":"packages/pkg","extra":1e400}]}"#);
    refuses(
        &operation("restore", &fixture, &vec![], "source", "cache").await,
        "invalid_arguments",
    );
    let root = fixture.root();
    let original_metadata = fs::read(root.join("cache/cache.json")).unwrap();
    let result = operation("retain", &fixture, &vec![], "source", "cache").await;
    // Validation failure is ignored initially: the source move and temporary
    // publication happen. Replacing the existing cache still obeys native IO.
    refuses(&result, "invalid_arguments");
    assert!(!root.join("source/node_modules").exists());
    assert_eq!(
        fs::read(root.join("cache/cache.json")).unwrap(),
        original_metadata
    );
    assert!(!fs::read_dir(&root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}
