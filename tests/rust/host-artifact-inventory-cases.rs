use ivy_contracts::{JsString, json};
use ivy_host_runtime::{
    IvyJson, IvyResult,
    artifact::{ArtifactFile, FileCompare, artifact_files, artifact_source_identity},
    candidate_identity::candidate_build_id,
    filesystem, node_path,
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

#[test]
fn native_file_compare_missing_runtime_is_not_executed() {
    const CHILD: &str = "IVY_TEST_FILE_COMPARE_MISSING_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let error = match ivy_host_runtime::artifact::native_file_compare() {
            Ok(_) => panic!("An absent package locale runtime must be refused."),
            Err(error) => error,
        };
        assert_eq!(error.code, "locale_unavailable");
        assert_eq!(error.outcome, ivy_host_runtime::Outcome::NotExecuted);
        return;
    }
    let fixture = Fixture::new();
    let image = fixture.0.join(std::env::current_exe().unwrap().file_name().unwrap());
    fs::copy(std::env::current_exe().unwrap(), &image).unwrap();
    let module = module_path!().split_once("::").unwrap().1;
    let selected = format!("{module}::native_file_compare_missing_runtime_is_not_executed");
    let output = Command::new(image)
        .args(["--exact", &selected, "--nocapture"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "requires the pinned ICU payload beside the test executable"]
async fn native_file_compare_retains_packaged_locale_across_thread_and_inventory() {
    let expected = json::parse(
        include_str!("../fixtures/host-file-compare.json"),
        usize::MAX,
    ).unwrap();
    let paths: Vec<JsString> = expected.required("paths").unwrap().as_array().unwrap()
        .iter().map(|p| p.as_js_string().unwrap().clone()).collect();
    let compare = ivy_host_runtime::artifact::native_file_compare().unwrap();
    let retained = compare.clone();
    drop(compare);
    let (compare, matrix) = std::thread::spawn(move || {
        let mut matrix = Vec::new();
        for left in &paths {
            for right in &paths {
                matrix.push(match retained(left, right) {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                });
            }
        }
        (retained, matrix)
    }).join().unwrap();
    let original: Vec<i32> = expected.required("matrix").unwrap().as_array().unwrap()
        .iter().flat_map(|row| row.as_array().unwrap())
        .map(|v| v.as_f64().unwrap() as i32).collect();
    assert_eq!(matrix.len(), 169);
    assert_eq!(matrix, original);
    let fixture = Fixture::new();
    let rows = expected.required("files").unwrap().as_array().unwrap();
    for row in rows {
        fixture.write(&row.required("path").unwrap().as_str().unwrap(),
            row.required("text").unwrap().as_str().unwrap().as_bytes());
    }
    let files = artifact_files(fixture.0.clone(), compare).await.unwrap();
    let original = IvyJson::Array(rows.iter().map(|row| IvyJson::object(
        ["path", "hash", "bytes", "mode"].map(|key| (key, row.required(key).unwrap().clone()))
    )).collect());
    let actual = IvyJson::Array(files.iter().map(ArtifactFile::json).collect());
    assert_eq!(files.len(), 9);
    assert_eq!(json::canonical(&actual, usize::MAX).unwrap(),
        json::canonical(&original, usize::MAX).unwrap());
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let temporary = std::env::temp_dir();
        let root = temporary.join(format!(
            "ivy-artifact-inventory-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(root.parent(), Some(temporary.as_path()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let file = self.0.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, bytes).unwrap();
        file
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
                            writable(&entry.path());
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
fn source_root() -> PathBuf {
    std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        PathBuf::from,
    )
}
fn append(row: IvyJson) {
    if let Some(file) = std::env::var_os("IVY_HOST_ARTIFACT_INVENTORY_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(&row, usize::MAX).unwrap()
        )
        .unwrap();
    }
}
fn oracle(input: &IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-artifact-inventory-oracle.mjs",
        ])
        .current_dir(source_root())
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
fn wire(result: IvyResult<Vec<ArtifactFile>>) -> IvyJson {
    match result {
        Ok(files) => IvyJson::object([
            ("ok", true.into()),
            (
                "value",
                IvyJson::Array(files.iter().map(ArtifactFile::json).collect()),
            ),
        ]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn input(root: &Path, operation: &str) -> IvyJson {
    IvyJson::object([
        ("operation", operation.into()),
        ("root", IvyJson::String(node_path::from_native(root))),
    ])
}
fn compare_paths(paths: &[String]) -> FileCompare {
    let input = IvyJson::object([
        ("operation", "order".into()),
        (
            "paths",
            IvyJson::Array(paths.iter().cloned().map(Into::into).collect()),
        ),
    ]);
    let expected = oracle(&input);
    append(IvyJson::object([
        ("input", input),
        ("expected", expected.clone()),
    ]));
    let value = expected
        .required("operation")
        .unwrap()
        .required("value")
        .unwrap();
    let paths: Vec<_> = paths
        .iter()
        .map(|path| JsString::from(path.as_str()))
        .collect();
    let matrix: Vec<Vec<_>> = value
        .required("matrix")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap()
                .iter()
                .map(|value| match value.as_f64().unwrap() as i32 {
                    -1 => Ordering::Less,
                    0 => Ordering::Equal,
                    1 => Ordering::Greater,
                    _ => unreachable!(),
                })
                .collect()
        })
        .collect();
    // A matrix qualifies only these fixture keys; unknown keys cannot fall back.
    Arc::new(move |left, right| {
        matrix[paths
            .iter()
            .position(|path| path == left)
            .expect("unqualified comparison key")][paths
            .iter()
            .position(|path| path == right)
            .expect("unqualified comparison key")]
    })
}
async fn compare(root: &Path, compare: FileCompare) -> IvyJson {
    let input = input(root, "inventory");
    let actual = wire(artifact_files(root.to_owned(), compare).await);
    let expected = oracle(&input);
    append(IvyJson::object([
        ("input", input),
        ("actual", actual.clone()),
        ("expected", expected.clone()),
    ]));
    assert_eq!(&actual, expected.required("operation").unwrap());
    actual
}
fn succeeds(result: &IvyJson) {
    assert_eq!(result.get("ok"), Some(&true.into()));
}
fn refuses(result: &IvyJson, code: &str) {
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

#[tokio::test]
async fn inventory_preserves_all_ordinary_bytes_modes_paths_and_supplied_locale_order() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("empty/nested")).unwrap();
    let mut paths = Vec::new();
    for name in [
        "z",
        "é",
        "e\u{301}",
        "a-10",
        "a-2",
        "_hidden",
        ".cache/value",
        "tests/spec.map",
        "ü 空 😀/payload",
    ] {
        fixture.write(name, name.as_bytes());
        paths.push(name.to_owned());
    }
    for index in 0..18 {
        let name = format!("branch-{index}/file");
        fixture.write(&name, &[index]);
        paths.push(name);
    }
    let executable = fixture.write("executable", b"mode bytes");
    paths.push("executable".into());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o751)).unwrap();
    }
    #[cfg(windows)]
    {
        let mut mode = fs::metadata(&executable).unwrap().permissions();
        mode.set_readonly(true);
        fs::set_permissions(&executable, mode).unwrap();
    }
    fixture.write("large", &vec![0xa7; 4 * 1024 * 1024 + 17]);
    paths.push("large".into());
    fixture.write("empty-file", b"");
    paths.push("empty-file".into());
    fs::hard_link(fixture.0.join("large"), fixture.0.join("same-bytes")).unwrap();
    paths.push("same-bytes".into());
    succeeds(&compare(&fixture.0, compare_paths(&paths)).await);
    assert_eq!(fs::read(fixture.0.join("z")).unwrap(), b"z");
}

#[tokio::test]
async fn source_identity_excludes_modes_empty_directories_and_capture_location_but_keeps_file_bytes()
 {
    let fixture = Fixture::new();
    let root = fixture.0.join("source");
    fixture.write("source/value", b"fixed source bytes");
    let compare = compare_paths(&["value".into()]);
    let mut identities = Vec::new();
    for phase in 0..3 {
        if phase == 1 {
            fs::create_dir(root.join("empty")).unwrap();
            let path = root.join("value");
            let mut mode = fs::metadata(&path).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                mode = fs::Permissions::from_mode(0o751);
            }
            #[cfg(windows)]
            mode.set_readonly(true);
            fs::set_permissions(path, mode).unwrap();
        }
        if phase == 2 {
            #[cfg(windows)]
            {
                let path = root.join("value");
                let mut mode = fs::metadata(&path).unwrap().permissions();
                mode.set_readonly(false);
                fs::set_permissions(path, mode).unwrap();
            }
            fs::write(root.join("value"), b"changed source bytes").unwrap();
        }
        let files = artifact_files(root.clone(), compare.clone()).await.unwrap();
        let identity = artifact_source_identity(&files).unwrap();
        let actual = IvyJson::object([
            ("ok", true.into()),
            (
                "value",
                IvyJson::object([
                    ("sourceIdentity", identity.clone().into()),
                    (
                        "buildId",
                        candidate_build_id(
                            &"sample".into(),
                            &"1.2.3".into(),
                            &identity.clone().into(),
                        )
                        .unwrap()
                        .into(),
                    ),
                ]),
            ),
        ]);
        let mut input = input(&root, "identity");
        input.insert("componentId", "sample".into());
        input.insert("version", "1.2.3".into());
        let expected = oracle(&input);
        append(IvyJson::object([
            ("input", input),
            ("actual", actual.clone()),
            ("expected", expected.clone()),
        ]));
        assert_eq!(&actual, expected.required("operation").unwrap());
        identities.push(identity);
    }
    assert_eq!(identities[0], identities[1]);
    assert_ne!(identities[1], identities[2]);
    let other = fixture.0.join("other");
    fs::create_dir(&other).unwrap();
    fs::copy(root.join("value"), other.join("value")).unwrap();
    assert_eq!(
        identities[2],
        artifact_source_identity(&artifact_files(other, compare).await.unwrap()).unwrap()
    );
}

#[tokio::test]
async fn inventory_refuses_observed_links_and_missing_or_file_roots_without_touching_targets() {
    for kind in ["link", "missing", "file", "empty"] {
        let fixture = Fixture::new();
        let root = fixture.0.join("selected");
        if kind == "file" {
            fixture.write("selected", b"ordinary file");
        } else if kind != "missing" {
            fs::create_dir(&root).unwrap();
        }
        if kind == "link" {
            fixture.write("outside/retained", b"outside bytes");
            filesystem::directory_link(&fixture.0.join("outside"), &root.join("observed")).unwrap();
        }
        let value = compare(&root, compare_paths(&[])).await;
        if kind == "empty" {
            succeeds(&value);
        } else {
            refuses(
                &value,
                if kind == "link" {
                    "artifact_changed"
                } else {
                    "internal_error"
                },
            );
        }
        if kind == "link" {
            assert_eq!(
                fs::read(fixture.0.join("outside/retained")).unwrap(),
                b"outside bytes"
            );
        }
    }
    #[cfg(unix)]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let fixture = Fixture::new();
        let fifo = CString::new(fixture.0.join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        refuses(
            &compare(&fixture.0, compare_paths(&[])).await,
            "artifact_changed",
        );
    }
    #[cfg(windows)]
    append(IvyJson::object([(
        "unavailable",
        "native Unix special-entry rejection requires a Unix account".into(),
    )]));
}

#[tokio::test]
async fn inventory_checks_the_actual_file_limit_before_hashing() {
    let fixture = Fixture::new();
    for index in 0..50_001 {
        fs::File::create(fixture.0.join(format!("{index:05}"))).unwrap();
    }
    refuses(
        &compare(&fixture.0, compare_paths(&[])).await,
        "limit_exceeded",
    );
}

#[cfg(windows)]
#[tokio::test]
async fn inventory_preserves_the_original_account_file_sharing_outcome() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let file = fixture.write("held", b"shared byte identity");
    let _held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(file)
        .unwrap();
    compare(&fixture.0, compare_paths(&["held".into()])).await;
}

#[test]
fn inventory_admission_survives_a_dropped_waiter() {
    use std::{
        future::Future,
        sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
        task::Poll,
        time::Duration,
    };
    let fixture = Fixture::new();
    fixture.write("a", b"first");
    fixture.write("b", b"second");
    let finished = Arc::new(AtomicBool::new(false));
    let signal = finished.clone();
    let compare: FileCompare = Arc::new(move |left, right| {
        signal.store(true, AtomicOrdering::Release);
        left.0.cmp(&right.0)
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, hold) = std::sync::mpsc::channel();
        let gate = tokio::task::spawn_blocking(move || {
            let _ = entered.send(());
            hold.recv().unwrap();
        });
        waiting.await.unwrap();
        let mut operation = Box::pin(artifact_files(fixture.0.clone(), compare));
        std::future::poll_fn(|context| match operation.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("inventory passed a held blocking worker"),
        })
        .await;
        drop(operation);
        release.send(()).unwrap();
        gate.await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !finished.load(AtomicOrdering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    assert_eq!(fs::read(fixture.0.join("a")).unwrap(), b"first");
    append(IvyJson::object([("scope","actual admitted inventory completes through final comparison after its caller drops the waiter".into())]));
}
