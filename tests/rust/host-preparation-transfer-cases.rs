use ivy_host_runtime::{IvyResult, filesystem, preparation_transfer as payload};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
        let mut name = std::ffi::OsString::from(format!(
            "ivy-preparation-transfer-{}-",
            uuid::Uuid::new_v4()
        ));
        name.push(opaque_name());
        let root = temporary.join(name);
        assert_eq!(root.parent(), Some(temporary.as_path()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("actual")).unwrap();
        Self(root)
    }
    fn root(&self) -> PathBuf {
        self.0.join("actual")
    }
    fn write(&self, local: &str, bytes: &[u8]) {
        let path = self.root().join(local);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn mkdir(&self, local: &str) {
        fs::create_dir_all(self.root().join(local)).unwrap();
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
        let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
        if self.0.parent() == Some(temporary.as_path()) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
fn opaque_name() -> std::ffi::OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut name = std::ffi::OsString::from("native ü 空 😀; ' ");
        name.push(std::ffi::OsString::from_wide(&[0xd800]));
        name
    }
    #[cfg(not(windows))]
    {
        "native ü 空 😀; '".into()
    }
}
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory(u32),
    File(Vec<u8>, u32),
    Link(PathBuf),
}
type Tree = BTreeMap<PathBuf, Entry>;
fn mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    }
    #[cfg(windows)]
    {
        u32::from(metadata.permissions().readonly())
    }
}
fn tree(root: &Path) -> Tree {
    fn walk(root: &Path, local: &Path, result: &mut Tree) {
        for entry in fs::read_dir(root.join(local)).unwrap() {
            let entry = entry.unwrap();
            let local = local.join(entry.file_name());
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            let value = if metadata.file_type().is_symlink() {
                Entry::Link(fs::read_link(entry.path()).unwrap())
            } else if metadata.is_dir() {
                Entry::Directory(mode(&metadata))
            } else {
                assert!(metadata.is_file());
                Entry::File(fs::read(entry.path()).unwrap(), mode(&metadata))
            };
            result.insert(local.clone(), value);
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                walk(root, &local, result);
            }
        }
    }
    let mut result = Tree::new();
    walk(root, Path::new(""), &mut result);
    result
}
fn succeeds(result: &IvyResult<()>) {
    assert!(result.is_ok(), "{result:?}");
}
fn refuses(result: &IvyResult<()>) {
    assert_eq!(result.as_ref().unwrap_err().code, "internal_error");
}
async fn operation(
    operation: &str,
    fixture: &Fixture,
    source: &str,
    destination: &str,
    directory: bool,
) -> IvyResult<()> {
    let root = fixture.root();
    match operation {
        "transfer" => {
            payload::transfer_prepared_payload(root.join(source), root.join(destination), directory)
                .await
        }
        "copyPayload" => {
            payload::copy_prepared_payload(root.join(source), root.join(destination), directory)
                .await
        }
        _ => unreachable!(),
    }
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
        let root = fixture.root();
        let identity = filesystem::stat_identity(&root.join(item)).unwrap();
        let target = root.join("payload/deep");
        if directory {
            filesystem::directory_link(&target, &root.join("payload/observed")).unwrap();
        }
        let link_target = directory.then(|| fs::read_link(root.join("payload/observed")).unwrap());
        succeeds(
            &operation(
                "transfer",
                &fixture,
                "payload",
                "published/payload",
                directory,
            )
            .await,
        );
        assert!(!root.join("payload").exists());
        let moved = if directory {
            "published/payload/deep/ü 空 😀"
        } else {
            "published/payload"
        };
        assert_eq!(
            fs::read(root.join(moved)).unwrap(),
            b"prepared bytes\x00\xff"
        );
        assert_eq!(
            filesystem::stat_identity(&root.join(moved)).unwrap(),
            identity
        );
        if let Some(target) = link_target {
            assert!(
                fs::symlink_metadata(root.join("published/payload/observed"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                fs::read_link(root.join("published/payload/observed")).unwrap(),
                target
            );
        }
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
        let before = tree(&fixture.root());
        refuses(&operation("transfer", &fixture, source, destination, directory).await);
        assert_eq!(tree(&fixture.root()), before, "{scenario}");
    }
    let fixture = Fixture::new();
    fixture.write("payload", b"self identity");
    let before = tree(&fixture.root());
    succeeds(&operation("transfer", &fixture, "payload", "payload", false).await);
    assert_eq!(tree(&fixture.root()), before);
}

#[tokio::test]
async fn directory_copy_phase_merges_independent_files_and_creates_missing_parents() {
    for existing in [false, true] {
        let fixture = Fixture::new();
        fixture.write("payload/pkg/deep/ü 空 😀", b"independent prepared bytes");
        fixture.write("payload/ordinary", b"replacement");
        fixture.mkdir("payload/empty/nested");
        let root = fixture.root();
        let opaque = PathBuf::from("pkg/deep").join(opaque_name());
        fs::write(root.join("payload").join(&opaque), b"opaque payload").unwrap();
        fixture.write(
            "payload/pkg/deep/native ü 空 😀; ' �",
            b"replacement neighbor",
        );
        let destination = if existing {
            fixture.write("published/ordinary", b"old bytes");
            fixture.write("published/retained", b"retained sibling");
            "published"
        } else {
            "published/deep/payload"
        };
        let mut permissions = fs::metadata(root.join("payload/pkg/deep/ü 空 😀"))
            .unwrap()
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(root.join("payload/pkg/deep/ü 空 😀"), permissions).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.join("payload"), fs::Permissions::from_mode(0o750)).unwrap();
            fs::set_permissions(root.join("payload/pkg"), fs::Permissions::from_mode(0o751))
                .unwrap();
            if existing {
                fs::set_permissions(root.join(destination), fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
        }
        let source_before = tree(&root.join("payload"));
        let mut expected = tree(&root.join("payload"));
        if existing {
            expected.insert(
                "retained".into(),
                Entry::File(
                    b"retained sibling".to_vec(),
                    mode(&fs::metadata(root.join("published/retained")).unwrap()),
                ),
            );
        }
        let expected_root_mode = mode(
            &fs::metadata(if existing {
                root.join(destination)
            } else {
                root.join("payload")
            })
            .unwrap(),
        );
        succeeds(&operation("copyPayload", &fixture, "payload", destination, true).await);
        assert_eq!(tree(&root.join(destination)), expected);
        assert_eq!(tree(&root.join("payload")), source_before);
        assert_eq!(
            mode(&fs::metadata(root.join(destination)).unwrap()),
            expected_root_mode
        );
        assert_ne!(
            filesystem::stat_identity(&root.join("payload/ordinary")).unwrap(),
            filesystem::stat_identity(&root.join(destination).join("ordinary")).unwrap()
        );
        fs::write(root.join("payload/ordinary"), b"changed source").unwrap();
        assert_eq!(
            fs::read(root.join(destination).join("ordinary")).unwrap(),
            b"replacement"
        );
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
        let root = fixture.root();
        let destination = match scenario {
            "replace" => {
                fixture.write("published", b"old bytes");
                "published"
            }
            "readonly-destination" => {
                fixture.write("published", b"old readonly bytes");
                let file = root.join("published");
                let mut permissions = fs::metadata(&file).unwrap().permissions();
                permissions.set_readonly(true);
                fs::set_permissions(file, permissions).unwrap();
                "published"
            }
            "missing-parent" => "absent/published",
            "self" => "payload",
            "hardlink" => {
                fs::hard_link(root.join("payload"), root.join("published")).unwrap();
                "published"
            }
            _ => "published",
        };
        let result = operation("copyPayload", &fixture, "payload", destination, false).await;
        match scenario {
            "missing-parent" => {
                refuses(&result);
                assert!(!root.join("absent").exists());
            }
            "readonly-destination" => {
                #[cfg(windows)]
                {
                    refuses(&result);
                    assert_eq!(
                        fs::read(root.join(destination)).unwrap(),
                        b"old readonly bytes"
                    );
                }
                #[cfg(unix)]
                {
                    // Effective account privileges decide whether this native copy may write.
                    if result.is_ok() {
                        assert_eq!(
                            fs::read(root.join(destination)).unwrap(),
                            b"file phase bytes"
                        );
                    } else {
                        refuses(&result);
                        assert_eq!(
                            fs::read(root.join(destination)).unwrap(),
                            b"old readonly bytes"
                        );
                    }
                }
            }
            _ => {
                succeeds(&result);
                assert_eq!(
                    fs::read(root.join(destination)).unwrap(),
                    b"file phase bytes"
                );
                let from = filesystem::stat_identity(&root.join("payload")).unwrap();
                let to = filesystem::stat_identity(&root.join(destination)).unwrap();
                if ["self", "hardlink"].contains(&scenario) {
                    assert_eq!(from, to);
                } else {
                    assert_ne!(from, to);
                }
            }
        }
        assert_eq!(fs::read(root.join("payload")).unwrap(), b"file phase bytes");
    }
}

fn relative_link(target: &str, destination: &Path, directory: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        let target = target.replace('/', "\\");
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
    let root = fixture.root();
    relative_link(
        "../external/./value",
        &root.join("payload/file-link"),
        false,
    )
    .unwrap();
    relative_link("../external", &root.join("payload/dir-link"), true).unwrap();
    let source_target = fs::read_link(root.join("payload/file-link")).unwrap();
    succeeds(&operation("copyPayload", &fixture, "payload", "published", true).await);
    for path in ["published/file-link", "published/dir-link"] {
        assert!(
            fs::symlink_metadata(root.join(path))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(fs::read_link(root.join(path)).unwrap().is_absolute());
    }
    assert_eq!(
        fs::read(root.join("published/file-link")).unwrap(),
        b"outside link target remains"
    );
    assert_eq!(
        fs::read(root.join("published/dir-link/value")).unwrap(),
        b"outside link target remains"
    );
    assert_eq!(
        fs::read_link(root.join("payload/file-link")).unwrap(),
        source_target
    );
    succeeds(
        &operation(
            "copyPayload",
            &fixture,
            "payload/file-link",
            "copied-file",
            false,
        )
        .await,
    );
    assert!(
        !fs::symlink_metadata(root.join("copied-file"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(root.join("copied-file")).unwrap(),
        b"outside link target remains"
    );
    fixture.write("sink/value", b"old destination target");
    relative_link("../sink/value", &root.join("published/file-sink"), false).unwrap();
    succeeds(
        &operation(
            "copyPayload",
            &fixture,
            "external/value",
            "published/file-sink",
            false,
        )
        .await,
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

    let fixture = Fixture::new();
    fixture.mkdir("payload");
    fixture.mkdir("published");
    fixture.write("external/parent/child/value", b"existing target stays");
    let root = fixture.root();
    relative_link("../external/parent", &root.join("payload/link"), true).unwrap();
    relative_link(
        "../external/parent/child",
        &root.join("published/link"),
        true,
    )
    .unwrap();
    let before = tree(&root);
    refuses(&operation("copyPayload", &fixture, "payload", "published", true).await);
    assert_eq!(tree(&root), before);
}

#[tokio::test]
async fn directory_copy_refuses_self_subdirectories_type_changes_and_shared_file_identity() {
    for scenario in [
        "self",
        "subdirectory",
        "directory-to-file",
        "file-to-directory",
        "same-inode",
        "aliased-subdirectory",
        "aliased-missing-parents",
    ] {
        let fixture = Fixture::new();
        fixture.write("payload/value", b"source unchanged");
        fixture.write("ordinary", b"ordinary unchanged");
        let root = fixture.root();
        let (source, destination) = match scenario {
            "self" => ("payload", "payload"),
            "subdirectory" => ("payload", "payload/child"),
            "directory-to-file" => ("payload", "ordinary"),
            "file-to-directory" => ("ordinary", "payload"),
            "aliased-subdirectory" | "aliased-missing-parents" => {
                fixture.mkdir("aliases");
                filesystem::directory_link(&root.join("payload"), &root.join("aliases/link"))
                    .unwrap();
                (
                    "payload",
                    if scenario == "aliased-subdirectory" {
                        "aliases/link/child"
                    } else {
                        "aliases/link/missing/child"
                    },
                )
            }
            _ => {
                fs::hard_link(root.join("ordinary"), root.join("hardlink")).unwrap();
                ("ordinary", "hardlink")
            }
        };
        let before = tree(&root);
        refuses(&operation("copyPayload", &fixture, source, destination, true).await);
        assert_eq!(tree(&root), before, "{scenario}");
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
            let source = fixture.root().join("payload");
            let destination = fixture.root().join("published");
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
}

#[cfg(windows)]
#[tokio::test]
async fn file_copy_phase_preserves_native_sharing_refusal_and_recovers_after_release() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    fixture.write("payload", b"held prepared source");
    let root = fixture.root();
    let before = tree(&root);
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(root.join("payload"))
        .unwrap();
    refuses(&operation("copyPayload", &fixture, "payload", "published", false).await);
    assert!(!root.join("published").exists());
    drop(held);
    assert_eq!(tree(&root), before);
    succeeds(&operation("copyPayload", &fixture, "payload", "published", false).await);
    assert_eq!(
        fs::read(root.join("payload")).unwrap(),
        b"held prepared source"
    );
    assert_eq!(
        fs::read(root.join("published")).unwrap(),
        b"held prepared source"
    );
}

#[tokio::test]
async fn cross_volume_transfer_uses_an_explicit_existing_fixture_volume() {
    let Some(base) = std::env::var_os("IVY_HOST_CROSS_DEVICE_TMP") else {
        eprintln!(
            "Unavailable: native EXDEV/error17 transfer needs IVY_HOST_CROSS_DEVICE_TMP; direct copy cases do not establish an actual cross-volume rename."
        );
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
        fixture.root().join("volume-probe"),
        cross.path.join("must-not-link"),
    )
    .unwrap_err();
    #[cfg(windows)]
    assert_eq!(error.raw_os_error(), Some(17));
    #[cfg(unix)]
    assert_eq!(error.raw_os_error(), Some(libc::EXDEV));
    for directory in [false, true] {
        let name = if directory { "directory" } else { "file" };
        let source = fixture.root().join(name);
        if directory {
            fixture.write("directory/deep/value", b"actual cross-volume bytes");
        } else {
            fixture.write("file", b"actual cross-volume bytes");
        }
        let destination = cross.path.join(name);
        succeeds(
            &payload::transfer_prepared_payload(source.clone(), destination.clone(), directory)
                .await,
        );
        let source_file = if directory {
            source.join("deep/value")
        } else {
            source.clone()
        };
        let destination_file = if directory {
            destination.join("deep/value")
        } else {
            destination.clone()
        };
        assert!(
            source.exists(),
            "native cross-volume fallback must retain its source"
        );
        assert_eq!(
            fs::read(&source_file).unwrap(),
            b"actual cross-volume bytes"
        );
        assert_eq!(
            fs::read(&destination_file).unwrap(),
            b"actual cross-volume bytes"
        );
        assert_ne!(
            filesystem::stat_identity(&source_file).unwrap(),
            filesystem::stat_identity(&destination_file).unwrap()
        );
    }
}
