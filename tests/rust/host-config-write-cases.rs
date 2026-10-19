use ivy_contracts::{IvyJson, IvyResult, hash, json};
use ivy_host_runtime::{config, node_path};
use std::{
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
        let root = std::env::temp_dir().join(format!("ivy-config-write-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn roots(&self) -> (PathBuf, PathBuf) {
        let actual = self.0.join("actual");
        let original = self.0.join("original");
        for root in [&actual, &original] {
            fs::create_dir(root).unwrap();
        }
        (actual, original)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn root() -> PathBuf {
    std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        PathBuf::from,
    )
}
fn value(kind: &str) -> IvyJson {
    match kind {
        "unsafe" => IvyJson::object([("extra", IvyJson::Number(9007199254740992.0))]),
        "infinite" => IvyJson::object([("extra", IvyJson::Number(f64::INFINITY))]),
        "nan" => IvyJson::object([("extra", IvyJson::Number(f64::NAN))]),
        "deep" => {
            let mut value = IvyJson::Null;
            for _ in 0..130 {
                value = IvyJson::Array(vec![value]);
            }
            value
        }
        _ => IvyJson::object([
            ("z", "last".into()),
            ("a", "ü 空 😀".into()),
            (
                "nested",
                IvyJson::object([("b", false.into()), ("a", IvyJson::Null)]),
            ),
        ]),
    }
}
fn tree(root: &Path) -> IvyJson {
    fn walk(root: &Path, directory: &Path, rows: &mut Vec<(String, IvyJson)>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let file = entry.path();
            let metadata = fs::symlink_metadata(&file).unwrap();
            let mut path = file
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            if path.ends_with(".tmp") && path.len() >= 41 {
                let start = path.len() - 40;
                let candidate = &path[start..path.len() - 4];
                if let Ok(id) = uuid::Uuid::parse_str(candidate) {
                    assert_eq!(id.get_version_num(), 4);
                    assert!(path[..start].ends_with('.'));
                    path.replace_range(start..path.len() - 4, "<uuid>");
                }
            }
            let row = if metadata.is_dir() {
                IvyJson::object([("path", path.clone().into()), ("kind", "directory".into())])
            } else {
                let bytes = fs::read(&file).unwrap();
                IvyJson::object([
                    ("path", path.clone().into()),
                    ("kind", "file".into()),
                    ("bytes", IvyJson::Number(bytes.len() as f64)),
                    ("hash", hash::digest(&bytes).into()),
                ])
            };
            rows.push((path, row));
            if metadata.is_dir() {
                walk(root, &file, rows);
            }
        }
    }
    let mut rows = Vec::new();
    walk(root, root, &mut rows);
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    IvyJson::Array(rows.into_iter().map(|(_, row)| row).collect())
}
fn compare(
    actual_root: &Path,
    original_root: &Path,
    path: &str,
    kind: &str,
    result: IvyResult<()>,
) -> IvyJson {
    let input = IvyJson::object([
        (
            "root",
            IvyJson::String(node_path::from_native(original_root)),
        ),
        (
            "path",
            IvyJson::String(node_path::from_native(&original_root.join(path))),
        ),
        ("kind", kind.into()),
    ]);
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-config-write-oracle.mjs",
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
        .write_all(json::stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let operation = match result {
        Ok(()) => IvyJson::object([("ok", true.into()), ("value", IvyJson::Null)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    };
    let actual = IvyJson::object([
        ("operation", operation.clone()),
        ("tree", tree(actual_root)),
    ]);
    if let Some(file) = std::env::var_os("IVY_HOST_CONFIG_WRITE_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(
                &IvyJson::object([
                    ("input", input),
                    ("actual", actual.clone()),
                    ("expected", expected.clone())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_eq!(actual, expected);
    operation
}
#[tokio::test]
async fn canonical_rejection_keeps_the_original_empty_temporary_and_existing_destination() {
    for kind in ["unsafe", "infinite", "nan", "deep"] {
        for existing in [false, true] {
            let fixture = Fixture::new();
            let (actual, original) = fixture.roots();
            let path = "nested/value.json";
            if existing {
                for root in [&actual, &original] {
                    fs::create_dir(root.join("nested")).unwrap();
                    fs::write(root.join(path), b"previous bytes").unwrap();
                }
            }
            let result = config::atomic_json(actual.join(path), value(kind)).await;
            let result = compare(&actual, &original, path, kind, result);
            assert_eq!(result.get("ok"), Some(&false.into()));
            assert_eq!(
                fs::read_dir(actual.join("nested")).unwrap().count(),
                1 + usize::from(existing)
            );
        }
    }
}
#[tokio::test]
async fn successful_replacement_and_failed_rename_keep_the_original_cleanup_boundaries() {
    for mode in [
        "new",
        "replace",
        "directory",
        "parent-file-valid",
        "parent-file-unsafe",
    ] {
        let fixture = Fixture::new();
        let (actual, original) = fixture.roots();
        let path = "nested/value.json";
        for root in [&actual, &original] {
            if mode.starts_with("parent-file") {
                fs::write(root.join("nested"), b"must remain").unwrap();
            } else if mode != "new" {
                fs::create_dir(root.join("nested")).unwrap();
                if mode == "directory" {
                    fs::create_dir(root.join(path)).unwrap();
                    fs::write(root.join(path).join("retained"), b"keep").unwrap();
                } else {
                    fs::write(root.join(path), b"previous bytes").unwrap();
                }
            }
        }
        let kind = if mode == "parent-file-unsafe" {
            "unsafe"
        } else {
            "valid"
        };
        let result = config::atomic_json(actual.join(path), value(kind)).await;
        let result = compare(&actual, &original, path, kind, result);
        assert_eq!(
            result.get("ok"),
            Some(&(mode == "new" || mode == "replace").into())
        );
    }
}
#[cfg(windows)]
#[tokio::test]
async fn replacement_denied_by_a_reader_keeps_old_bytes_and_removes_its_temporary() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let (actual, original) = fixture.roots();
    for root in [&actual, &original] {
        fs::write(root.join("value.json"), b"previous bytes").unwrap();
    }
    let held: Vec<_> = [&actual, &original]
        .map(|root| {
            fs::OpenOptions::new()
                .read(true)
                .share_mode(3)
                .open(root.join("value.json"))
                .unwrap()
        })
        .into();
    let result = config::atomic_json(actual.join("value.json"), value("valid")).await;
    let result = compare(&actual, &original, "value.json", "valid", result);
    assert_eq!(result.get("ok"), Some(&false.into()));
    drop(held);
    assert_eq!(fs::read_dir(&actual).unwrap().count(), 1);
}
