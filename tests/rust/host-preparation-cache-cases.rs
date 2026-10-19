use ivy_contracts::{JsString, json};
use ivy_host_runtime::{
    IvyJson, IvyResult, filesystem, node_path,
    preparation_cache::{self, PreparationCache, WorkspaceCompare},
};
use std::{
    cmp::Ordering,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
static CORPUS: Mutex<()> = Mutex::new(());
const KEY: &str = "fixture-cache-key";
type Links = Vec<(JsString, JsString)>;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let temporary = std::env::temp_dir();
        let root = temporary.join(format!(
            "ivy-preparation-cache-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(root.parent(), Some(temporary.as_path()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("actual")).unwrap();
        fs::create_dir(root.join("original")).unwrap();
        Self(root)
    }
    fn root(&self, label: &str) -> PathBuf {
        self.0.join(label)
    }
    fn write(&self, local: &str, bytes: &[u8]) {
        for label in ["actual", "original"] {
            let path = self.root(label).join(local);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
    }
    fn mkdir(&self, local: &str) {
        for label in ["actual", "original"] {
            fs::create_dir_all(self.root(label).join(local)).unwrap();
        }
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
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn root() -> PathBuf {
    std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        PathBuf::from,
    )
}
fn path(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn links(values: &[(&str, &str)]) -> Links {
    values
        .iter()
        .map(|(location, target)| ((*location).into(), (*target).into()))
        .collect()
}
fn links_json(links: &Links) -> IvyJson {
    IvyJson::Array(
        links
            .iter()
            .map(|(location, target)| {
                IvyJson::Array(vec![
                    IvyJson::String(location.clone()),
                    IvyJson::String(target.clone()),
                ])
            })
            .collect(),
    )
}
fn append(value: IvyJson) {
    if let Some(file) = std::env::var_os("IVY_HOST_PREPARATION_CACHE_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(&value, usize::MAX).unwrap()
        )
        .unwrap();
    }
}
fn wire(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn oracle(input: &IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-preparation-cache-oracle.mjs",
        ])
        .current_dir(root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
// This finite fixture matrix is read from the actual original Node locale.
// It is not a production comparator, a guessed locale or a fallback ordering.
fn identity(links: &Links) -> PreparationCache {
    let input = IvyJson::object([("operation", "order".into()), ("links", links_json(links))]);
    let expected = oracle(&input);
    let value = expected
        .required("operation")
        .unwrap()
        .required("value")
        .unwrap();
    let keys = value
        .required("keys")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|key| match key {
            IvyJson::String(value) => value.clone(),
            _ => panic!("Node key must be a string"),
        })
        .collect::<Vec<_>>();
    let matrix = value
        .required("matrix")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap()
                .iter()
                .map(|cell| match cell {
                    IvyJson::Number(value) if *value < 0.0 => Ordering::Less,
                    IvyJson::Number(value) if *value > 0.0 => Ordering::Greater,
                    IvyJson::Number(0.0) => Ordering::Equal,
                    _ => panic!("Node comparator result must be signed"),
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    append(IvyJson::object([("comparisonProfile", expected)]));
    let compare: WorkspaceCompare = Arc::new(move |left, right| {
        let a = keys
            .iter()
            .position(|key| key == left)
            .expect("unqualified comparator operand");
        let b = keys
            .iter()
            .position(|key| key == right)
            .expect("unqualified comparator operand");
        matrix[a][b]
    });
    PreparationCache::new(KEY.into(), links.clone(), compare)
}
fn metadata(entries: IvyJson) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(3.0)),
        ("key", KEY.into()),
        ("externalOnly", true.into()),
        ("workspaceLinks", entries),
    ])
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
async fn compare(
    operation: &str,
    fixture: &Fixture,
    links: &Links,
    source_local: &str,
    cache_local: &str,
) -> IvyJson {
    let identity = identity(links);
    let actual = fixture.root("actual");
    let original = fixture.root("original");
    let source = actual.join(source_local);
    let cache = actual.join(cache_local);
    let actual_result = wire(match operation {
        "restore" => identity.restore(source, cache).await.map(IvyJson::Bool),
        "retain" => identity.retain(source, cache).await.map(|_| IvyJson::Null),
        "copy" => preparation_cache::copy_external_dependencies(source, cache, links.clone())
            .await
            .map(|_| IvyJson::Null),
        _ => unreachable!(),
    });
    let input = IvyJson::object([
        ("operation", operation.into()),
        ("actualRoot", path(&actual)),
        ("originalRoot", path(&original)),
        ("sourceLocal", source_local.into()),
        ("cacheLocal", cache_local.into()),
        ("key", KEY.into()),
        ("links", links_json(links)),
    ]);
    let expected = oracle(&input);
    append(IvyJson::object([
        ("input", input.clone()),
        ("actual", actual_result.clone()),
        ("expected", expected.clone()),
    ]));
    assert_eq!(
        &actual_result,
        expected.required("operation").unwrap(),
        "{}",
        json::stringify(
            &IvyJson::object([("input", input), ("expected", expected.clone())]),
            usize::MAX
        )
        .unwrap()
    );
    assert_eq!(expected.get("actualTree"), expected.get("originalTree"));
    actual_result
}

#[test]
fn schema_three_key_keeps_process_version_and_all_runtime_coordinates() {
    for (os, arch, node) in [
        ("win32", "x64", "v24.18.0"),
        ("win32", "arm64", "v24.18.1"),
        ("linux", "x64", "v24.18.0"),
        ("linux", "arm64", "v24.19.0"),
        ("win32", "x64", "24.18.0"),
    ] {
        let input = IvyJson::object([
            ("operation", "key".into()),
            ("lockHash", "sha256:lock ü 空 😀".into()),
            ("packageManager", "npm@11.11.0".into()),
            (
                "profile",
                IvyJson::object([
                    ("platform", os.into()),
                    ("arch", arch.into()),
                    ("version", node.into()),
                ]),
            ),
        ]);
        let actual = wire(
            preparation_cache::dependency_cache_key(
                "sha256:lock ü 空 😀".into(),
                "npm@11.11.0".into(),
                os.into(),
                arch.into(),
                node.into(),
            )
            .map(IvyJson::from),
        );
        let expected = oracle(&input);
        assert_eq!(&actual, expected.required("operation").unwrap());
        append(IvyJson::object([
            ("input", input),
            ("actual", actual),
            ("expected", expected),
        ]));
    }
}

#[test]
fn cache_metadata_uses_original_map_updates_stable_locale_ties_and_full_entry_hash() {
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
    let identity = identity(&links);
    let actual = wire(Ok(identity.workspace_entries()));
    let input = IvyJson::object([
        ("operation", "entries".into()),
        ("links", links_json(&links)),
    ]);
    let expected = oracle(&input);
    assert_eq!(&actual, expected.required("operation").unwrap());
    append(IvyJson::object([
        ("input", input),
        ("actual", actual),
        ("expected", expected),
    ]));
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
        let input = IvyJson::object([
            ("operation", "valid".into()),
            ("links", links_json(&links)),
            ("key", KEY.into()),
            ("metadata", metadata),
        ]);
        let expected = oracle(&input);
        assert_eq!(&actual, expected.required("operation").unwrap());
        append(IvyJson::object([
            ("input", input),
            ("actual", actual),
            ("expected", expected),
        ]));
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
    ] {
        let fixture = Fixture::new();
        let links = vec![];
        fixture.mkdir("source");
        if scenario != "absent" {
            fixture.metadata(&metadata(IvyJson::Array(vec![])));
        }
        match scenario {
            "wrong-key" => {
                let mut value = metadata(IvyJson::Array(vec![]));
                value.insert("key", "different".into());
                fixture.metadata(&value);
            }
            "syntax" => fixture.write("cache/cache.json", b"{"),
            "oversize" => fixture.write("cache/cache.json", &vec![b' '; 4097]),
            "directory" => {
                for label in ["actual", "original"] {
                    fs::remove_file(fixture.root(label).join("cache/cache.json")).unwrap();
                }
                fixture.mkdir("cache/cache.json");
            }
            "modules-file" => fixture.write("cache/node_modules", b"not a directory"),
            _ => (),
        }
        let result = compare("restore", &fixture, &links, "source", "cache").await;
        match scenario {
            "syntax" | "modules-file" => refuses(&result, "internal_error"),
            "oversize" | "directory" => refuses(&result, "invalid_arguments"),
            _ => succeeds(&result, false.into()),
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
        &compare("retain", &fixture, &links, "source", "cache").await,
        IvyJson::Null,
    );
    for label in ["actual", "original"] {
        let source = fixture.root(label).join("source");
        let cache = fixture.root(label).join("cache");
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
        &compare("retain", &fixture, &links, "source", "cache").await,
        IvyJson::Null,
    );
    for label in ["actual", "original"] {
        fs::remove_dir_all(fixture.root(label).join("source/node_modules")).unwrap();
    }
    fixture.write("source/packages/current/value", b"changed workspace source");
    succeeds(
        &compare("restore", &fixture, &links, "source", "cache").await,
        true.into(),
    );
    for label in ["actual", "original"] {
        assert_eq!(
            fs::read(
                fixture
                    .root(label)
                    .join("source/node_modules/a-workspace/value")
            )
            .unwrap(),
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
        let result = compare("restore", &fixture, &links, "source", "cache").await;
        if missing {
            succeeds(&result, false.into())
        } else {
            refuses(&result, "target_conflict")
        }
        for label in ["actual", "original"] {
            let root = fixture.root(label);
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
            for label in ["actual", "original"] {
                fs::remove_dir_all(fixture.root(label).join("source/node_modules")).unwrap();
            }
            fixture.write("source/node_modules", b"not a directory");
        }
        let result = compare("retain", &fixture, &links, "source", "cache").await;
        if scenario == "syntax" || scenario == "modules-file" {
            refuses(&result, "internal_error")
        } else if scenario == "source-absent" || scenario == "valid-without-cached-modules" {
            succeeds(&result, IvyJson::Null)
        }
        if scenario == "syntax" || scenario == "valid-without-cached-modules" {
            assert_eq!(
                fs::read(
                    fixture
                        .root("actual")
                        .join("source/node_modules/external/value")
                )
                .unwrap(),
                b"source bytes"
            );
        }
        assert!(
            !fs::read_dir(fixture.root("actual"))
                .unwrap()
                .any(|entry| entry
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
    let fixture = Fixture::new();
    let links = links(&[("node_modules/@scope/workspace", "packages/current")]);
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
    for label in ["actual", "original"] {
        let file = fixture
            .root(label)
            .join("source/node_modules/pkg/deep/ü 空 😀");
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(file, permissions).unwrap();
    }
    succeeds(
        &compare("copy", &fixture, &links, "source", "cache").await,
        IvyJson::Null,
    );
    for label in ["actual", "original"] {
        let root = fixture.root(label);
        assert!(!root.join("cache/@scope/workspace").exists());
        assert_eq!(
            fs::read(root.join("cache/@scope/workspace-old/value")).unwrap(),
            b"prefix sibling retained"
        );
        assert_ne!(
            filesystem::stat_identity(&root.join("source/node_modules/ordinary")).unwrap(),
            filesystem::stat_identity(&root.join("cache/ordinary")).unwrap()
        );
    }
    let fixture = Fixture::new();
    fixture.write(
        "source/node_modules/pkg/value",
        b"unchanged self-copy source",
    );
    refuses(
        &compare("copy", &fixture, &vec![], "source", "source/node_modules").await,
        "internal_error",
    );
    assert_eq!(
        fs::read(fixture.root("actual").join("source/node_modules/pkg/value")).unwrap(),
        b"unchanged self-copy source"
    );
    append(IvyJson::object([("unavailable","real EXDEV cache rename path".into()),("scope","the actual external-copy phase was compared directly; no second volume is configured and no cross-device event was injected".into())]));
}

#[tokio::test]
async fn external_copy_keeps_observed_link_targets_or_the_original_privilege_refusal() {
    let fixture = Fixture::new();
    fixture.mkdir("source/node_modules");
    fixture.write("target/value", b"outside link target bytes");
    for label in ["actual", "original"] {
        let root = fixture.root(label);
        filesystem::directory_link(
            &root.join("target"),
            &root.join("source/node_modules/observed"),
        )
        .unwrap();
    }
    let result = compare("copy", &fixture, &vec![], "source", "cache").await;
    if cfg!(unix) || std::env::var_os("IVY_REQUIRE_FILE_SYMLINKS").is_some() {
        succeeds(&result, IvyJson::Null)
    } else if result.get("ok") != Some(&true.into()) {
        #[cfg(windows)]
        {
            let error = std::os::windows::fs::symlink_file(
                fixture.root("actual").join("target/value"),
                fixture.root("actual").join("privilege-probe"),
            )
            .unwrap_err();
            assert!(
                error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(1314)
            );
        }
        append(IvyJson::object([
            (
                "unavailable",
                "successful cache copy of an observed link requires native file-symlink privilege"
                    .into(),
            ),
            (
                "scope",
                "the actual Rust/original refusal and unchanged target were compared".into(),
            ),
        ]));
    }
    for label in ["actual", "original"] {
        assert_eq!(
            fs::read(fixture.root(label).join("target/value")).unwrap(),
            b"outside link target bytes"
        );
    }
}

#[tokio::test]
async fn cache_publication_retains_native_long_path_behavior() {
    let fixture = Fixture::new();
    fixture.write("source/node_modules/pkg/value", b"long cache path bytes");
    let cache = format!("{}cache", "long-directory-component/".repeat(12));
    succeeds(
        &compare("retain", &fixture, &vec![], "source", &cache).await,
        IvyJson::Null,
    );
    assert_eq!(
        fs::read(fixture.root("actual").join("source/node_modules/pkg/value")).unwrap(),
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
    let root = fixture.root("actual");
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
    append(IvyJson::object([("observation","cancelled caller waiter after the real source move; admitted retention completed cache publication and both ordered workspace links".into())]));
}

#[cfg(windows)]
#[tokio::test]
async fn retained_cache_reader_matches_original_account_sharing_behavior() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    fixture.write("source/node_modules/pkg/value", b"unchanged source");
    fixture.metadata(&metadata(IvyJson::Array(vec![])));
    let holds = ["actual", "original"].map(|label| {
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(fixture.root(label).join("cache/cache.json"))
            .unwrap()
    });
    // Keep the same exclusive handle for Rust and the original child. The
    // executing account's backup semantics may still permit both reads.
    // Snapshot after release so it cannot introduce another sharing outcome.
    let identity = identity(&vec![]);
    let result = wire(
        identity
            .retain(
                fixture.root("actual").join("source"),
                fixture.root("actual").join("cache"),
            )
            .await
            .map(|_| IvyJson::Null),
    );
    let input = IvyJson::object([
        ("operation", "retain".into()),
        ("originalRoot", path(&fixture.root("original"))),
        ("key", KEY.into()),
        ("links", IvyJson::Array(vec![])),
    ]);
    let expected = oracle(&input);
    assert_eq!(&result, expected.required("operation").unwrap());
    if result.get("ok") == Some(&true.into()) {
        succeeds(&result, IvyJson::Null);
    } else {
        refuses(&result, "internal_error");
    }
    drop(holds);
    let snapshot = oracle(&IvyJson::object([
        ("operation", "snapshot".into()),
        ("actualRoot", path(&fixture.root("actual"))),
        ("originalRoot", path(&fixture.root("original"))),
    ]));
    assert_eq!(snapshot.get("actualTree"), snapshot.get("originalTree"));
    append(IvyJson::object([
        ("input", input),
        ("actual", result),
        ("expected", expected),
        ("snapshot", snapshot),
    ]));
}

#[tokio::test]
async fn cache_validation_ivy_errors_are_ignored_only_by_initial_retention() {
    let fixture = Fixture::new();
    fixture.write("source/node_modules/pkg/value", b"installed source bytes");
    fixture.write("cache/cache.json",br#"{"schemaVersion":3,"key":"fixture-cache-key","externalOnly":true,"workspaceLinks":[{"location":"node_modules/pkg","target":"packages/pkg","extra":1e400}]}"#);
    refuses(
        &compare("restore", &fixture, &vec![], "source", "cache").await,
        "invalid_arguments",
    );
    // The original catch includes validCache/hashJson as well as jsonFile.
    // A later Windows EPERM on replacing the existing directory still applies.
    compare("retain", &fixture, &vec![], "source", "cache").await;
}
