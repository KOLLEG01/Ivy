use ivy_contracts::{IvyJson, JsString, json};
use ivy_host_runtime::{
    filesystem,
    filesystem_error::NodeFilesystemError,
    filesystem_remove,
    node_path::{self, Context, Style},
};
use std::{
    any::Any,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};
static CORPUS: Mutex<()> = Mutex::new(());

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-remove-sync-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn reset(&self) {
        let target = self.0.canonicalize().unwrap();
        assert!(target.starts_with(std::env::temp_dir().canonicalize().unwrap()));
        assert!(
            target
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-remove-sync-")
        );
        fs::remove_dir_all(&target).unwrap();
        fs::create_dir(&target).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn node(input: &JsString) -> IvyJson {
    let mut child = Command::new("node")
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../tests/fixtures/host-filesystem-remove-oracle.mjs"),
        )
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
            json::stringify(
                &IvyJson::object([("path", IvyJson::String(input.clone()))]),
                usize::MAX,
            )
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
    let result = child.wait_with_output().unwrap();
    if !result.status.success() {
        let bytes = |value: &[u8]| {
            IvyJson::Array(value.iter().map(|v| IvyJson::Number(*v as f64)).collect())
        };
        return IvyJson::object([
            (
                "result",
                IvyJson::object([("ok", false.into()), ("error", IvyJson::Null)]),
            ),
            (
                "processFailure",
                IvyJson::object([
                    (
                        "exitCode",
                        result
                            .status
                            .code()
                            .map(|v| IvyJson::Number(v as f64))
                            .unwrap_or(IvyJson::Null),
                    ),
                    ("stdoutBytes", bytes(&result.stdout)),
                    ("stderrBytes", bytes(&result.stderr)),
                ]),
            ),
        ]);
    }
    json::parse(std::str::from_utf8(&result.stdout).unwrap(), usize::MAX).unwrap()
}
fn cause(value: NodeFilesystemError) -> IvyJson {
    IvyJson::object([
        ("code", value.code.into()),
        ("errno", IvyJson::Number(value.errno as f64)),
        ("syscall", value.syscall.into()),
        ("path", IvyJson::String(value.path)),
        ("message", IvyJson::String(value.message)),
    ])
}
fn snapshot(root: &Path) -> IvyJson {
    fn walk(path: &Path, relative: String, entries: &mut Vec<IvyJson>) {
        let physical = node_path::filesystem_path(&node_path::from_native(path));
        let metadata = fs::symlink_metadata(&physical).unwrap();
        let kind = metadata.file_type();
        entries.push(IvyJson::object([
            ("path", relative.clone().into()),
            (
                "kind",
                if kind.is_symlink() {
                    "link"
                } else if kind.is_dir() {
                    "directory"
                } else {
                    "file"
                }
                .into(),
            ),
            ("readonly", metadata.permissions().readonly().into()),
        ]));
        if kind.is_dir() && !kind.is_symlink() {
            for entry in fs::read_dir(physical).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                walk(
                    &entry.path(),
                    if relative.is_empty() {
                        name
                    } else {
                        format!("{relative}/{name}")
                    },
                    entries,
                );
            }
        }
    }
    let mut entries = Vec::new();
    walk(root, String::new(), &mut entries);
    entries.sort_by_key(|r| r.required("path").unwrap().as_str().unwrap());
    IvyJson::Array(entries)
}
fn record(
    case: &str,
    path: &JsString,
    raw: Option<i32>,
    actual: &IvyJson,
    original: &IvyJson,
    effects: &IvyJson,
    unmapped: bool,
) {
    if let Some(file) = std::env::var_os("IVY_HOST_FILESYSTEM_REMOVE_CORPUS") {
        let _lock = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(
                &IvyJson::object([
                    ("case", case.into()),
                    ("path", IvyJson::String(path.clone())),
                    (
                        "nativeRawError",
                        raw.map(|v| IvyJson::Number(v as f64))
                            .unwrap_or(IvyJson::Null)
                    ),
                    ("actual", actual.clone()),
                    ("original", original.clone()),
                    ("effects", effects.clone()),
                    ("explicitUnmapped", unmapped.into())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
}
fn readonly(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions).unwrap();
}
fn setup(case: &str, root: &Path, container: &Path) -> Vec<Box<dyn Any>> {
    let mut guards: Vec<Box<dyn Any>> = Vec::new();
    if case == "missing" || case == "empty-path" || case == "oversized-component" {
        return guards;
    }
    if matches!(case, "file" | "readonly-file" | "held-root") {
        fs::write(root, b"owned").unwrap();
        if case == "readonly-file" {
            readonly(root);
        }
    } else if matches!(case, "root-link" | "dangling-link") {
        let external = container.join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("sentinel"), b"preserved").unwrap();
        filesystem::directory_link(&external, root).unwrap();
        if case == "dangling-link" {
            fs::remove_dir_all(external).unwrap();
        }
    } else {
        fs::create_dir(root).unwrap();
        fs::write(root.join("a-removed"), b"before").unwrap();
        if case == "tree" || case == "readonly-directory" {
            fs::create_dir(root.join("nested")).unwrap();
            fs::write(root.join("nested/value"), b"nested").unwrap();
        }
        if case == "readonly-directory" {
            readonly(&root.join("a-removed"));
            readonly(&root.join("nested"));
        }
        if case == "embedded-link" {
            let external = container.join("external");
            fs::create_dir(&external).unwrap();
            fs::write(external.join("sentinel"), b"preserved").unwrap();
            filesystem::directory_link(&external, &root.join("link")).unwrap();
        }
        #[cfg(windows)]
        if case == "denied-root" || case == "denied-child" {
            let denied = if case == "denied-root" {
                root.to_owned()
            } else {
                let p = root.join("m-denied");
                fs::create_dir(&p).unwrap();
                fs::write(p.join("held"), b"denied").unwrap();
                p
            };
            guards.push(Box::new(
                super::filesystem_error_cases::DeniedDirectory::new(denied),
            ));
        }
        if case.starts_with("held-") && case != "held-root" {
            fs::write(root.join("m-held"), b"held").unwrap();
            fs::write(root.join("z-retained"), b"after").unwrap();
            if case == "held-readonly" {
                readonly(&root.join("m-held"));
            }
        }
    }
    #[cfg(windows)]
    if case.starts_with("held-") {
        use std::os::windows::fs::OpenOptionsExt;
        let path = if case == "held-root" {
            root.to_owned()
        } else {
            root.join("m-held")
        };
        guards.push(Box::new(
            fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(path)
                .unwrap(),
        ));
    }
    guards
}
fn compare(case: &str, form: &str) {
    let fixture = Fixture::new();
    let context = Context::current().unwrap();
    let mut path = node_path::from_native(&fixture.0.join(if form == "unicode" {
        "q'uöté-😀"
    } else if form == "trailing" {
        "root. "
    } else {
        "root"
    }));
    if form == "surrogate" {
        path.0.extend([0xd800]);
    }
    if form == "relative" {
        path = context.relative(&context.cwd, &path);
    }
    if form == "namespace" {
        path = node_path::from_native(&context.filesystem_path(&path));
    }
    if case == "empty-path" {
        path = JsString::default();
    }
    if case == "oversized-component" {
        path = node_path::from_native(&fixture.0.join("x".repeat(400)));
    }
    let physical = context.filesystem_path(&path);
    let mut outcomes = Vec::new();
    let mut original = IvyJson::Null;
    let mut raw = None;
    let mut unmapped = false;
    // Both operations see independently recreated fixtures at exactly the same
    // physical path, including the same parent-owned denied handles and ACLs.
    for original_run in [false, true] {
        fixture.reset();
        let guards = setup(case, &physical, &fixture.0);
        let result = if original_run {
            let value = node(&path);
            let result = value.required("result").unwrap().clone();
            original = value;
            result
        } else {
            match filesystem_remove::remove_sync_force_at(&path, &context) {
                Ok(()) => IvyJson::object([("ok", true.into()), ("error", IvyJson::Null)]),
                Err(failure) => {
                    raw = failure.source.raw_os_error();
                    unmapped = failure.node.is_none();
                    IvyJson::object([
                        ("ok", false.into()),
                        ("error", failure.node.map(cause).unwrap_or(IvyJson::Null)),
                    ])
                }
            }
        };
        // Restore only the fixture's ACL before inspection. Handles stay open
        // until after the operation and effect snapshot, matching both runs.
        #[cfg(windows)]
        if case == "denied-root" || case == "denied-child" {
            drop(guards);
            outcomes.push((result, snapshot(&fixture.0)));
            continue;
        }
        let effects = snapshot(&fixture.0);
        drop(guards);
        outcomes.push((result, effects));
    }
    let ((actual, effects), (expected, original_effects)) = (&outcomes[0], &outcomes[1]);
    assert_eq!(
        json::canonical(effects, usize::MAX).unwrap(),
        json::canonical(original_effects, usize::MAX).unwrap(),
        "effects {case}/{form}"
    );
    record(
        &format!("{case}/{form}"),
        &path,
        raw,
        actual,
        &original,
        effects,
        unmapped,
    );
    if unmapped {
        assert!(matches!(form, "unicode" | "surrogate"));
        assert_eq!(case, "held-partial");
        assert!(raw.is_some());
        assert_eq!(expected.required("ok").unwrap(), &IvyJson::Bool(false));
        assert!(
            original.get("processFailure").is_some()
                || expected.required("error").unwrap().get("code").is_some()
        );
    } else {
        assert_eq!(
            json::canonical(actual, usize::MAX).unwrap(),
            json::canonical(expected, usize::MAX).unwrap(),
            "cause {case}/{form}; native={raw:?}"
        );
    }
    if case == "held-partial" || case == "held-readonly" {
        let names = effects
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.required("path").unwrap().as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(!names.iter().any(|p| p.ends_with("a-removed")));
        assert!(names.iter().any(|p| p.ends_with("m-held")));
        assert!(names.iter().any(|p| p.ends_with("z-retained")));
    }
}

#[test]
fn filesystem_remove_sync_matches_node_successes_paths_and_link_effects() {
    for case in [
        "missing",
        "empty-path",
        "file",
        "readonly-file",
        "tree",
        "root-link",
        "dangling-link",
        "embedded-link",
    ] {
        compare(case, "absolute");
    }
    #[cfg(windows)]
    compare("readonly-directory", "absolute");
    for form in ["relative", "unicode", "namespace", "surrogate"] {
        compare("tree", form);
    }
    #[cfg(windows)]
    compare("tree", "trailing");
}

#[cfg(windows)]
#[test]
fn filesystem_remove_sync_sharing_acl_and_partial_effects_match_node() {
    for case in [
        "held-root",
        "held-partial",
        "held-readonly",
        "denied-root",
        "denied-child",
    ] {
        compare(case, "absolute");
    }
    for form in ["relative", "unicode", "namespace", "surrogate", "trailing"] {
        compare("held-partial", form);
    }
}

#[test]
fn filesystem_remove_sync_unmapped_and_invalid_boundaries_do_not_fabricate_causes() {
    #[cfg(windows)]
    compare("oversized-component", "absolute");
    let fixture = Fixture::new();
    let context = Context::current().unwrap();
    let path = node_path::from_native(&fixture.0.join("kept"));
    fs::write(context.filesystem_path(&path), b"kept").unwrap();
    let mut bad = path.clone();
    bad.0.push(0);
    let failure = filesystem_remove::remove_sync_force(&bad).unwrap_err();
    assert!(failure.node.is_none());
    assert!(failure.source.raw_os_error().is_none());
    let mut other = context;
    other.style = if Style::native() == Style::Windows {
        Style::Posix
    } else {
        Style::Windows
    };
    let failure = filesystem_remove::remove_sync_force_at(&path, &other).unwrap_err();
    assert!(failure.node.is_none());
    assert!(failure.source.raw_os_error().is_none());
    assert_eq!(fs::read(fixture.0.join("kept")).unwrap(), b"kept");
}
