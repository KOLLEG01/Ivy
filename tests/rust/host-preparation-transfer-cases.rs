use ivy_contracts::json;
use ivy_host_runtime::{
    IvyJson, IvyResult, filesystem, node_path, preparation_transfer as payload,
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
static CORPUS: Mutex<()> = Mutex::new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let temporary = std::env::temp_dir();
        let root = temporary.join(format!(
            "ivy-preparation-transfer-{}-{}",
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
fn path(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn root() -> PathBuf {
    std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        PathBuf::from,
    )
}
fn append(row: IvyJson) {
    if let Some(file) = std::env::var_os("IVY_HOST_PREPARATION_TRANSFER_CORPUS") {
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
fn wire(result: IvyResult<()>) -> IvyJson {
    match result {
        Ok(()) => IvyJson::object([("ok", true.into()), ("value", IvyJson::Null)]),
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
fn succeeds(result: &IvyJson) {
    assert_eq!(result.get("ok"), Some(&true.into()));
}
fn refuses(result: &IvyJson) {
    assert_eq!(result.get("ok"), Some(&false.into()));
}
async fn operation(
    operation: &str,
    fixture: &Fixture,
    source: &str,
    destination: &str,
    directory: bool,
    trees: bool,
) -> (IvyJson, IvyJson) {
    let actual = fixture.root("actual");
    let original = fixture.root("original");
    let result = wire(match operation {
        "transfer" => {
            payload::transfer_prepared_payload(
                actual.join(source),
                actual.join(destination),
                directory,
            )
            .await
        }
        "copyPayload" => {
            payload::copy_prepared_payload(actual.join(source), actual.join(destination), directory)
                .await
        }
        _ => unreachable!(),
    });
    let mut input = IvyJson::object([
        ("operation", operation.into()),
        ("originalRoot", path(&original)),
        ("sourceLocal", source.into()),
        ("cacheLocal", destination.into()),
        ("directory", directory.into()),
    ]);
    if trees {
        input.insert("actualRoot", path(&actual));
    }
    let expected = oracle(&input);
    append(IvyJson::object([
        ("input", input.clone()),
        ("actual", result.clone()),
        ("expected", expected.clone()),
    ]));
    assert_eq!(
        &result,
        expected.required("operation").unwrap(),
        "{}",
        json::stringify(&input, usize::MAX).unwrap()
    );
    if trees {
        assert_eq!(expected.get("actualTree"), expected.get("originalTree"));
    }
    (result, input)
}

#[tokio::test]
async fn same_volume_rename_preserves_payload_identity_and_observed_directory_links() {
    for directory in [false, true] {
        let fixture = Fixture::new();
        let item = if directory {
            "payload/deep/ü 空 😀"
        } else {
            "payload"
        };
        fixture.write(item, b"prepared bytes\x00\xff");
        fixture.mkdir("published");
        let original = filesystem::stat_identity(&fixture.root("actual").join(item)).unwrap();
        if directory {
            for label in ["actual", "original"] {
                let root = fixture.root(label);
                filesystem::directory_link(
                    &root.join("payload/deep"),
                    &root.join("payload/observed"),
                )
                .unwrap();
            }
        }
        succeeds(
            &operation(
                "transfer",
                &fixture,
                "payload",
                "published/payload",
                directory,
                true,
            )
            .await
            .0,
        );
        assert!(!fixture.root("actual").join("payload").exists());
        let moved = if directory {
            "published/payload/deep/ü 空 😀"
        } else {
            "published/payload"
        };
        assert_eq!(
            filesystem::stat_identity(&fixture.root("actual").join(moved)).unwrap(),
            original
        );
    }
}

#[tokio::test]
async fn ordinary_rename_errors_never_select_a_copy_or_remove_existing_bytes() {
    for scenario in [
        "occupied-directory",
        "missing-parent",
        "missing-source",
        "file-to-directory",
        "invalid-name",
    ] {
        let fixture = Fixture::new();
        fixture.write("payload/value", b"source stays");
        fixture.write("existing/retained", b"destination stays");
        let (source, destination, directory) = match scenario {
            "occupied-directory" => ("payload", "existing", true),
            "missing-parent" => ("payload/value", "absent/payload", false),
            "missing-source" => ("absent", "existing/retained", false),
            "file-to-directory" => ("payload/value", "existing", false),
            _ => ("payload/value", "invalid\0name", false),
        };
        refuses(
            &operation("transfer", &fixture, source, destination, directory, true)
                .await
                .0,
        );
        assert_eq!(
            fs::read(fixture.root("actual").join("payload/value")).unwrap(),
            b"source stays"
        );
        assert_eq!(
            fs::read(fixture.root("actual").join("existing/retained")).unwrap(),
            b"destination stays"
        );
    }
    let fixture = Fixture::new();
    fixture.write("payload", b"self identity");
    operation("transfer", &fixture, "payload", "payload", false, true).await;
    assert_eq!(
        fs::read(fixture.root("actual").join("payload")).unwrap(),
        b"self identity"
    );
}

#[tokio::test]
async fn directory_copy_phase_merges_independent_files_and_creates_missing_parents() {
    for existing in [false, true] {
        let fixture = Fixture::new();
        fixture.write("payload/pkg/deep/ü 空 😀", b"independent prepared bytes");
        fixture.write("payload/ordinary", b"replacement");
        let destination = if existing {
            fixture.write("published/ordinary", b"old bytes");
            fixture.write("published/retained", b"retained sibling");
            "published"
        } else {
            "published/deep/payload"
        };
        for label in ["actual", "original"] {
            let file = fixture.root(label).join("payload/pkg/deep/ü 空 😀");
            let mut permissions = fs::metadata(&file).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(file, permissions).unwrap();
        }
        succeeds(
            &operation("copyPayload", &fixture, "payload", destination, true, true)
                .await
                .0,
        );
        for label in ["actual", "original"] {
            let root = fixture.root(label);
            assert_eq!(
                fs::read(root.join("payload/ordinary")).unwrap(),
                b"replacement"
            );
            assert_eq!(
                fs::read(root.join(destination).join("ordinary")).unwrap(),
                b"replacement"
            );
            assert_ne!(
                filesystem::stat_identity(&root.join("payload/ordinary")).unwrap(),
                filesystem::stat_identity(&root.join(destination).join("ordinary")).unwrap()
            );
        }
    }
}

#[tokio::test]
async fn file_copy_phase_retains_source_and_does_not_create_destination_parents() {
    for scenario in [
        "new",
        "replace",
        "readonly-destination",
        "missing-parent",
        "self",
        "hardlink",
    ] {
        let fixture = Fixture::new();
        fixture.write("payload", b"file phase bytes");
        let destination = match scenario {
            "replace" => {
                fixture.write("published", b"old bytes");
                "published"
            }
            "readonly-destination" => {
                fixture.write("published", b"old readonly bytes");
                for label in ["actual", "original"] {
                    let file = fixture.root(label).join("published");
                    let mut permissions = fs::metadata(&file).unwrap().permissions();
                    permissions.set_readonly(true);
                    fs::set_permissions(file, permissions).unwrap();
                }
                "published"
            }
            "missing-parent" => "absent/published",
            "self" => "payload",
            "hardlink" => {
                for label in ["actual", "original"] {
                    let root = fixture.root(label);
                    fs::hard_link(root.join("payload"), root.join("published")).unwrap();
                }
                "published"
            }
            _ => "published",
        };
        let result = operation("copyPayload", &fixture, "payload", destination, false, true)
            .await
            .0;
        if scenario == "new" || scenario == "replace" {
            succeeds(&result)
        }
        if scenario == "missing-parent" {
            refuses(&result)
        }
        assert_eq!(
            fs::read(fixture.root("actual").join("payload")).unwrap(),
            b"file phase bytes"
        );
    }
}

fn relative_link(target: &str, destination: &Path, directory: bool) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        if directory {
            std::os::windows::fs::symlink_dir(target, destination)
        } else {
            std::os::windows::fs::symlink_file(target, destination)
        }
    }
    #[cfg(unix)]
    {
        let _ = directory;
        std::os::unix::fs::symlink(target, destination)
    }
}
#[tokio::test]
async fn directory_copy_uses_absolute_link_targets_and_keeps_existing_target_guards() {
    let fixture = Fixture::new();
    fixture.mkdir("payload");
    fixture.write("external/value", b"outside link target remains");
    let first = relative_link(
        "../external/./value",
        &fixture.root("actual").join("payload/file-link"),
        false,
    );
    if let Err(error) = first {
        assert!(
            cfg!(windows)
                && (error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(1314))
        );
        assert!(
            std::env::var_os("IVY_REQUIRE_FILE_SYMLINKS").is_none(),
            "successful link qualification is required on this account"
        );
        for label in ["actual", "original"] {
            let root = fixture.root(label);
            filesystem::directory_link(&root.join("external"), &root.join("payload/dir-link"))
                .unwrap();
        }
        refuses(
            &operation("copyPayload", &fixture, "payload", "published", true, true)
                .await
                .0,
        );
        append(IvyJson::object([("unavailable","successful relative-link creation/default copy requires native Windows symlink privilege".into()),("scope","actual junction-copy refusal was compared against the original".into())]));
        return;
    }
    relative_link(
        "../external/./value",
        &fixture.root("original").join("payload/file-link"),
        false,
    )
    .unwrap();
    for label in ["actual", "original"] {
        relative_link(
            "../external",
            &fixture.root(label).join("payload/dir-link"),
            true,
        )
        .unwrap();
    }
    succeeds(
        &operation("copyPayload", &fixture, "payload", "published", true, true)
            .await
            .0,
    );
    for label in ["actual", "original"] {
        let root = fixture.root(label);
        assert_eq!(
            fs::read(root.join("published/file-link")).unwrap(),
            b"outside link target remains"
        );
        assert_eq!(
            fs::read_link(root.join("payload/file-link")).unwrap(),
            PathBuf::from("../external/./value")
        );
        let target = fs::read_link(root.join("published/file-link")).unwrap();
        assert!(target.is_absolute());
    }
    succeeds(
        &operation(
            "copyPayload",
            &fixture,
            "payload/file-link",
            "copied-file",
            false,
            true,
        )
        .await
        .0,
    );
    fixture.write("sink/value", b"old destination target");
    for label in ["actual", "original"] {
        relative_link(
            "../sink/value",
            &fixture.root(label).join("published/file-sink"),
            false,
        )
        .unwrap();
    }
    succeeds(
        &operation(
            "copyPayload",
            &fixture,
            "external/value",
            "published/file-sink",
            false,
            true,
        )
        .await
        .0,
    );
    for label in ["actual", "original"] {
        let root = fixture.root(label);
        assert!(
            !fs::symlink_metadata(root.join("copied-file"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read(root.join("sink/value")).unwrap(),
            b"outside link target remains"
        );
        assert!(
            fs::symlink_metadata(root.join("published/file-sink"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    let fixture = Fixture::new();
    fixture.mkdir("payload");
    fixture.mkdir("published");
    fixture.write("external/parent/child/value", b"existing target stays");
    for label in ["actual", "original"] {
        let root = fixture.root(label);
        relative_link("../external/parent", &root.join("payload/link"), true).unwrap();
        relative_link(
            "../external/parent/child",
            &root.join("published/link"),
            true,
        )
        .unwrap();
    }
    refuses(
        &operation("copyPayload", &fixture, "payload", "published", true, true)
            .await
            .0,
    );
    assert_eq!(
        fs::read(fixture.root("actual").join("external/parent/child/value")).unwrap(),
        b"existing target stays"
    );
}

#[tokio::test]
async fn directory_copy_refuses_self_subdirectories_type_changes_and_shared_file_identity() {
    for scenario in [
        "self",
        "subdirectory",
        "directory-to-file",
        "file-to-directory",
        "same-inode",
    ] {
        let fixture = Fixture::new();
        fixture.write("payload/value", b"source unchanged");
        fixture.write("ordinary", b"ordinary unchanged");
        let (source, destination) = match scenario {
            "self" => ("payload", "payload"),
            "subdirectory" => ("payload", "payload/child"),
            "directory-to-file" => ("payload", "ordinary"),
            "file-to-directory" => ("ordinary", "payload"),
            _ => {
                for label in ["actual", "original"] {
                    let root = fixture.root(label);
                    fs::hard_link(root.join("ordinary"), root.join("hardlink")).unwrap();
                }
                ("ordinary", "hardlink")
            }
        };
        refuses(
            &operation("copyPayload", &fixture, source, destination, true, true)
                .await
                .0,
        );
        assert_eq!(
            fs::read(fixture.root("actual").join("payload/value")).unwrap(),
            b"source unchanged"
        );
        assert_eq!(
            fs::read(fixture.root("actual").join("ordinary")).unwrap(),
            b"ordinary unchanged"
        );
    }
}

#[test]
fn admitted_transfer_and_copy_complete_after_the_waiter_is_dropped() {
    use std::{
        future::Future,
        sync::mpsc,
        task::{Context, Poll, Waker},
        time::{Duration, Instant},
    };
    for copying in [false, true] {
        let fixture = Fixture::new();
        fixture.write("payload", b"admitted bytes");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let occupied = tokio::task::spawn_blocking(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            });
            entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let source = fixture.root("actual").join("payload");
            let destination = fixture.root("actual").join("published");
            let mut future: std::pin::Pin<Box<dyn Future<Output = IvyResult<()>>>> = if copying {
                Box::pin(payload::copy_prepared_payload(
                    source.clone(),
                    destination.clone(),
                    false,
                ))
            } else {
                Box::pin(payload::transfer_prepared_payload(
                    source.clone(),
                    destination.clone(),
                    false,
                ))
            };
            assert!(matches!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
            drop(future);
            release_tx.send(()).unwrap();
            occupied.await.unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if fs::read(&destination).is_ok_and(|bytes| bytes == b"admitted bytes") {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "admitted filesystem phase did not complete"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(source.exists(), copying);
        });
    }
    append(IvyJson::object([("observation","real rename and copy jobs were queued behind the test's blocking worker, their Pending waiters dropped, then both admitted jobs completed without a product hook".into())]));
}

#[cfg(windows)]
#[tokio::test]
async fn file_copy_phase_preserves_the_original_account_sharing_outcome() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    fixture.write("payload", b"held prepared source");
    let holds = ["actual", "original"].map(|label| {
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(fixture.root(label).join("payload"))
            .unwrap()
    });
    operation(
        "copyPayload",
        &fixture,
        "payload",
        "published",
        false,
        false,
    )
    .await;
    drop(holds);
    let input = IvyJson::object([
        ("operation", "snapshot".into()),
        ("actualRoot", path(&fixture.root("actual"))),
        ("originalRoot", path(&fixture.root("original"))),
    ]);
    let snapshot = oracle(&input);
    assert_eq!(snapshot.get("actualTree"), snapshot.get("originalTree"));
    assert_eq!(
        fs::read(fixture.root("actual").join("payload")).unwrap(),
        b"held prepared source"
    );
    append(IvyJson::object([("observation","sharing handles were retained for both native copy phases; exact filesystem projection was compared after release".into()),("snapshot",snapshot)]));
}

#[tokio::test]
async fn cross_volume_transfer_uses_an_explicit_existing_fixture_volume() {
    let Some(base) = std::env::var_os("IVY_HOST_CROSS_DEVICE_TMP") else {
        append(IvyJson::object([("unavailable","native EXDEV payload transfer".into()),("reason","IVY_HOST_CROSS_DEVICE_TMP is not configured; actual copy phases are compared directly without injecting an EXDEV event or provisioning a volume".into())]));
        assert!(std::env::var_os("IVY_REQUIRE_CROSS_DEVICE").is_none());
        return;
    };
    let base = PathBuf::from(base);
    assert!(base.is_absolute() && base.is_dir());
    let base = fs::canonicalize(base).unwrap();
    let fixture = Fixture::new();
    let cross_path = base.join(fixture.0.file_name().unwrap());
    assert_eq!(cross_path.parent(), Some(base.as_path()));
    fs::create_dir(&cross_path).unwrap();
    struct Cross {
        path: PathBuf,
        parent: PathBuf,
    }
    impl Drop for Cross {
        fn drop(&mut self) {
            if let Ok(target) = fs::canonicalize(&self.path) {
                if target.parent() == Some(self.parent.as_path()) {
                    let _ = fs::remove_dir_all(target);
                }
            }
        }
    }
    let cross = Cross {
        path: cross_path,
        parent: base,
    };
    fixture.write("volume-probe", b"cross-volume probe");
    let error = fs::hard_link(
        fixture.root("actual").join("volume-probe"),
        cross.path.join("must-not-link"),
    )
    .unwrap_err();
    #[cfg(windows)]
    assert_eq!(error.raw_os_error(), Some(17));
    #[cfg(unix)]
    assert_eq!(error.raw_os_error(), Some(libc::EXDEV));
    for directory in [false, true] {
        let name = if directory { "directory" } else { "file" };
        let source_local = if directory {
            "payload-directory"
        } else {
            "payload-file"
        };
        let source_file = if directory {
            "payload-directory/value"
        } else {
            "payload-file"
        };
        fixture.write(source_file, b"actual cross-volume bytes");
        for label in ["actual", "original"] {
            fs::create_dir_all(cross.path.join(label)).unwrap();
        }
        let source_actual = fixture.root("actual").join(source_local);
        let destination_actual = cross.path.join("actual").join(name);
        let destination_original = cross.path.join("original").join(name);
        let result = wire(
            payload::transfer_prepared_payload(
                source_actual.clone(),
                destination_actual.clone(),
                directory,
            )
            .await,
        );
        let input = IvyJson::object([
            ("operation", "transfer".into()),
            ("directory", directory.into()),
            ("actualRoot", path(&fixture.root("actual"))),
            ("originalRoot", path(&fixture.root("original"))),
            (
                "sourceOriginal",
                path(&fixture.root("original").join(source_local)),
            ),
            ("destinationActual", path(&destination_actual)),
            ("destinationOriginal", path(&destination_original)),
        ]);
        let expected = oracle(&input);
        append(IvyJson::object([
            ("input", input),
            ("actual", result.clone()),
            ("expected", expected.clone()),
        ]));
        assert_eq!(&result, expected.required("operation").unwrap());
        assert_eq!(expected.get("actualTree"), expected.get("originalTree"));
        assert_eq!(
            expected.get("actualDestinationTree"),
            expected.get("originalDestinationTree")
        );
        succeeds(&result);
        assert!(source_actual.exists());
    }
}
