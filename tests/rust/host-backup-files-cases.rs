use super::{ServiceFixture, assert_json_eq};
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, JsString, hash::digest, json};
use ivy_host_runtime::{
    backup_files::{self, BackupExclusion, BackupFile},
    filesystem, node_path,
};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

static CORPUS: Mutex<()> = Mutex::new(());
fn p(path: &Path) -> J {
    J::String(node_path::from_native(path))
}
fn wrap(result: IvyResult<J>) -> J {
    match result {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn oracle(input: &J) -> J {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
        ])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../tests/fixtures/host-backup-files-oracle.mjs"),
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
        .write_all(json::canonical(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(&String::from_utf8(output.stdout).unwrap(), usize::MAX).unwrap()
}
fn retain(value: &J) {
    if let Some(path) = std::env::var_os("IVY_HOST_BACKUP_FILES_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{}", json::canonical(value, usize::MAX).unwrap()).unwrap();
    }
}
fn compare(input: J, actual: J) {
    let expected = oracle(&input);
    retain(&J::object([
        ("input", input),
        ("actual", actual.clone()),
        ("expected", expected.clone()),
    ]));
    assert_json_eq(&actual, &expected);
}
fn observation(path: &Path) -> J {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return J::Null;
        }
        Err(error) => panic!("Destination observation failed: {error}"),
    };
    let mut pairs = vec![(
        "kind",
        if metadata.file_type().is_symlink() {
            "link"
        } else if metadata.is_dir() {
            "directory"
        } else {
            "file"
        }
        .into(),
    )];
    if metadata.is_file() {
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::MetadataExt;
            metadata.mode() & 0o777
        };
        #[cfg(windows)]
        let mode = if metadata.permissions().readonly() {
            0o444
        } else {
            0o666
        };
        pairs.extend([
            ("hash", digest(&fs::read(path).unwrap()).into()),
            ("bytes", J::Number(metadata.len() as f64)),
            ("mode", J::Number(mode as f64)),
        ]);
    }
    J::object(pairs)
}
fn report(result: IvyResult<J>, exclusions: Vec<JsString>, destination: Option<&Path>) -> J {
    J::object([
        ("result", wrap(result)),
        (
            "exclusions",
            J::Array(exclusions.into_iter().map(J::String).collect()),
        ),
        (
            "destination",
            destination.map(observation).unwrap_or(J::Null),
        ),
        (
            "parentExists",
            destination
                .map(|path| J::Bool(path.parent().unwrap().exists()))
                .unwrap_or(J::Null),
        ),
    ])
}
async fn inventory(
    _: &ServiceFixture,
    root: &Path,
    excluded: &[&str],
    throw_at: Option<&str>,
) -> J {
    let visited = Arc::new(Mutex::new(Vec::new()));
    let trace = visited.clone();
    let excluded: Vec<JsString> = excluded.iter().map(|s| (*s).into()).collect();
    let throw = throw_at.map(JsString::from);
    let exclude: BackupExclusion = Arc::new(move |local| {
        trace.lock().unwrap().push(local.clone());
        if throw.as_ref() == Some(local) {
            return Err(IvyError::new("fixture_exclusion", "Exclusion failed."));
        }
        Ok(excluded.contains(local))
    });
    let result = backup_files::inventory_files(root.to_owned(), Some(exclude))
        .await
        .map(|files| J::Array(files.iter().map(BackupFile::json).collect()));
    report(result, visited.lock().unwrap().clone(), None)
}

#[test]
fn safe_relative_members_match_original_on_both_path_styles() {
    let mut members: Vec<JsString> = [
        "",
        ".",
        "..",
        "/",
        "a",
        "a/b",
        "a//b",
        "a/",
        "./a",
        "a/../b",
        "a/./b",
        "../x",
        "/x",
        "//host/share",
        "\\x",
        "C:x",
        "C:/x",
        "x\\y",
        "x:y",
        "x\0y",
        "x\ny",
        "x\u{1f}y",
        "x\u{7f}y",
        "a b",
        "trailing.",
        "CON",
        "aux.txt",
        "a/é",
        "中/😀",
        "a/...",
        "a/..x",
    ]
    .into_iter()
    .map(JsString::from)
    .collect();
    members.extend([JsString(vec![0xd800]), JsString(vec![97, 47, 0xdc00])]);
    for (style_name, style, cwd) in [
        ("win32", node_path::Style::Windows, "C:\\work\\current"),
        ("posix", node_path::Style::Posix, "/work/current"),
    ] {
        let context = node_path::Context {
            style,
            cwd: cwd.into(),
        };
        let roots = if style_name == "win32" {
            vec![
                "C:\\root",
                "root/child",
                "C:/root/../else",
                "//server/share/root",
            ]
        } else {
            vec!["/root", "root/child", "/root/../else", "//root"]
        };
        let mut cases = Vec::new();
        let mut actual = Vec::new();
        for root in roots {
            for member in &members {
                cases.push(J::object([
                    ("root", root.into()),
                    ("member", J::String(member.clone())),
                ]));
                actual.push(wrap(
                    backup_files::relative_file_with_context(&root.into(), member, &context)
                        .map(J::String),
                ));
            }
        }
        compare(
            J::object([
                ("operation", "relative".into()),
                ("style", style_name.into()),
                ("cwd", cwd.into()),
                ("cases", J::Array(cases)),
            ]),
            J::Array(actual),
        );
    }
}
fn inventory_input(
    fixture: &ServiceFixture,
    root: &Path,
    excluded: &[&str],
    throw: Option<&str>,
) -> J {
    J::object([
        ("operation", "inventory".into()),
        ("fixtureRoot", p(&fixture.0)),
        ("root", p(root)),
        (
            "exclude",
            J::Array(excluded.iter().map(|s| (*s).into()).collect()),
        ),
        ("throwAt", throw.map(J::from).unwrap_or(J::Null)),
    ])
}

#[tokio::test]
async fn inventory_sorts_utf16_hashes_bytes_and_excludes_before_observation() {
    let fixture = ServiceFixture::new();
    let root = fixture.0.join("tree");
    fs::create_dir(&root).unwrap();
    for name in ["z", "A", "é", "中", "😀", "\u{e000}", "!", "empty"] {
        fs::write(
            root.join(name),
            if name == "empty" {
                vec![]
            } else {
                name.as_bytes().to_vec()
            },
        )
        .unwrap();
    }
    fs::create_dir_all(root.join("nested/deep")).unwrap();
    fs::write(root.join("nested/deep/binary"), [0, 255, 1, 0]).unwrap();
    fs::create_dir(root.join("skip")).unwrap();
    fs::write(root.join("skip/hidden"), b"skip").unwrap();
    for (excluded, throw) in [
        (vec![], None),
        (vec!["skip", "nested/deep/binary"], None),
        (vec!["skip"], Some("nested")),
    ] {
        let actual = inventory(&fixture, &root, &excluded, throw).await;
        compare(inventory_input(&fixture, &root, &excluded, throw), actual);
    }
    compare(
        inventory_input(&fixture, &fixture.0.join("missing"), &[], None),
        inventory(&fixture, &fixture.0.join("missing"), &[], None).await,
    );
    let default = backup_files::inventory_files(root.clone(), None)
        .await
        .unwrap();
    assert_eq!(default.len(), 10);
}

#[tokio::test]
async fn inventory_rejects_links_unless_excluded_and_preserves_callback_errors() {
    let fixture = ServiceFixture::new();
    let root = fixture.0.join("tree");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a"), b"ordinary").unwrap();
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"private").unwrap();
    filesystem::directory_link(&outside, &root.join("link")).unwrap();
    for excluded in [vec![], vec!["link"]] {
        compare(
            inventory_input(&fixture, &root, &excluded, None),
            inventory(&fixture, &root, &excluded, None).await,
        );
    }
    let linked_root = fixture.0.join("linked-root");
    filesystem::directory_link(&root, &linked_root).unwrap();
    compare(
        inventory_input(&fixture, &linked_root, &[], None),
        inventory(&fixture, &linked_root, &[], None).await,
    );
}

#[tokio::test]
async fn inventory_depth_boundary_matches_original() {
    for depth in [64, 65] {
        let fixture = ServiceFixture::new();
        let root = fixture.0.join("tree");
        fs::create_dir(&root).unwrap();
        let mut leaf = root.clone();
        for _ in 0..depth {
            leaf.push("d");
            fs::create_dir(&leaf).unwrap();
        }
        compare(
            inventory_input(&fixture, &root, &[], None),
            inventory(&fixture, &root, &[], None).await,
        );
    }
}

fn sparse(path: &Path, bytes: u64) {
    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .unwrap();
    #[cfg(windows)]
    {
        use std::{os::windows::io::AsRawHandle, ptr};
        use windows_sys::Win32::System::IO::DeviceIoControl;
        // winioctl.h: CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 49, METHOD_BUFFERED,
        // FILE_SPECIAL_ACCESS). The IOCTL is test-only; no new crate feature.
        const FSCTL_SET_SPARSE: u32 = 0x0009_00c4;
        let mut returned = 0;
        assert_ne!(
            unsafe {
                DeviceIoControl(
                    file.as_raw_handle(),
                    FSCTL_SET_SPARSE,
                    ptr::null(),
                    0,
                    ptr::null_mut(),
                    0,
                    &mut returned,
                    ptr::null_mut(),
                )
            },
            0,
            "Cannot make disposable sparse file"
        );
    }
    file.set_len(bytes).unwrap();
}
#[tokio::test]
async fn oversized_sparse_files_are_rejected_before_hashing_or_creating_copy_parents() {
    let fixture = ServiceFixture::new();
    let root = fixture.0.join("tree");
    fs::create_dir(&root).unwrap();
    let source = root.join("large");
    sparse(&source, 64 * 1024 * 1024 * 1024 + 1);
    compare(
        inventory_input(&fixture, &root, &[], None),
        inventory(&fixture, &root, &[], None).await,
    );
    compare_copy(&fixture, &source, None, false, "fresh", "absent").await;
}
fn expected(source: &Path) -> BackupFile {
    let metadata = fs::metadata(source).unwrap();
    BackupFile {
        path: "ignored/expected/member".into(),
        hash: digest(&fs::read(source).unwrap()),
        bytes: metadata.len(),
        mode: 0o751,
    }
}
async fn compare_copy(
    fixture: &ServiceFixture,
    source: &Path,
    expectation: Option<BackupFile>,
    preserve: bool,
    label: &str,
    initial: &str,
) {
    let node = fixture.0.join(format!("original-{label}"));
    let native = fixture.0.join(format!("native-{label}"));
    for root in [&node, &native] {
        if initial != "absent" {
            fs::create_dir(root).unwrap();
        }
        match initial {
            "file" => fs::write(root.join("result"), b"preserved").unwrap(),
            "directory" => fs::create_dir(root.join("result")).unwrap(),
            "ancestor-file" => {
                fs::write(root.join("parent"), b"preserved ancestor").unwrap();
            }
            "link" => {
                filesystem::directory_link(&fixture.0.join("artifact"), &root.join("result"))
                    .unwrap();
            }
            "absent" => {}
            _ => panic!("Unknown initial copy state"),
        }
    }
    let destination = |root: &Path| {
        if initial == "ancestor-file" {
            root.join("parent/result")
        } else {
            root.join("result")
        }
    };
    let native_path = destination(&native);
    let result = backup_files::copy_verified(
        source.to_owned(),
        native_path.clone(),
        expectation.clone(),
        preserve,
    )
    .await
    .map(|file| file.json());
    let actual = report(result, Vec::new(), Some(&native_path));
    let input = J::object([
        ("operation", "copy".into()),
        ("fixtureRoot", p(&fixture.0)),
        ("source", p(source)),
        ("destination", p(&destination(&node))),
        (
            "expected",
            expectation
                .as_ref()
                .map(BackupFile::json)
                .unwrap_or(J::Null),
        ),
        ("preserveMode", preserve.into()),
    ]);
    compare(input, actual);
}
#[tokio::test]
async fn verified_copy_matches_original_bytes_modes_and_expected_identity() {
    let fixture = ServiceFixture::new();
    for (label, bytes) in [
        ("empty", Vec::new()),
        ("binary", vec![0, 255, 1, 0, 128]),
        ("large", vec![171; 2 * 1024 * 1024 + 3]),
        ("unicode", "é中😀".as_bytes().to_vec()),
    ] {
        let source = fixture.0.join(label);
        fs::write(&source, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&source, fs::Permissions::from_mode(0o740)).unwrap();
        }
        for preserve in [false, true] {
            compare_copy(
                &fixture,
                &source,
                None,
                preserve,
                &format!("{label}-{preserve}-none"),
                "absent",
            )
            .await;
            compare_copy(
                &fixture,
                &source,
                Some(expected(&source)),
                preserve,
                &format!("{label}-{preserve}-expected"),
                "absent",
            )
            .await;
        }
    }
}
#[tokio::test]
async fn verified_copy_preserves_failures_and_never_overwrites_destinations() {
    let fixture = ServiceFixture::new();
    let source = fixture.0.join("source");
    fs::write(&source, b"source content").unwrap();
    let mut wrong_size = expected(&source);
    wrong_size.bytes += 1;
    compare_copy(&fixture, &source, Some(wrong_size), false, "size", "absent").await;
    let mut wrong_hash = expected(&source);
    wrong_hash.hash = digest(b"other");
    compare_copy(&fixture, &source, Some(wrong_hash), false, "hash", "absent").await;
    for initial in ["file", "directory", "ancestor-file", "link"] {
        compare_copy(&fixture, &source, None, false, initial, initial).await;
    }
    for (label, source) in [
        ("missing", fixture.0.join("missing")),
        ("directory-source", fixture.0.join("artifact")),
    ] {
        compare_copy(&fixture, &source, None, false, label, "absent").await;
    }
    let link = fixture.0.join("linked-source");
    filesystem::directory_link(&fixture.0.join("artifact"), &link).unwrap();
    compare_copy(&fixture, &link, None, false, "linked-source", "absent").await;
}

#[tokio::test]
async fn concurrent_copy_claims_one_destination_exclusively() {
    let fixture = ServiceFixture::new();
    let source = fixture.0.join("source");
    fs::write(&source, vec![41; 1024 * 1024]).unwrap();
    let destination = fixture.0.join("result");
    let (one, two) = tokio::join!(
        backup_files::copy_verified(source.clone(), destination.clone(), None, false),
        backup_files::copy_verified(source.clone(), destination.clone(), None, false)
    );
    assert_ne!(one.is_ok(), two.is_ok());
    assert_eq!(fs::read(&source).unwrap(), fs::read(&destination).unwrap());
    retain(&J::object([
        ("nativeOnly", "exclusive-destination".into()),
        ("one", wrap(one.map(|file| file.json()))),
        ("two", wrap(two.map(|file| file.json()))),
        ("destination", observation(&destination)),
    ]));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verified_copy_detects_replaced_source_identity_even_with_equal_bytes_and_timestamp() {
    let fixture = ServiceFixture::new();
    let source = fixture.0.join("source");
    let replacement = fixture.0.join("replacement");
    let destination = fixture.0.join("result");
    let content = vec![193; 64 * 1024 * 1024];
    fs::write(&source, &content).unwrap();
    fs::write(&replacement, &content).unwrap();
    drop(content);
    let at = 1_700_000_000_000;
    filesystem::set_times_ms(&source, at, at).unwrap();
    filesystem::set_times_ms(&replacement, at, at).unwrap();
    let before = filesystem::stat_identity(&source).unwrap();
    let after = filesystem::stat_identity(&replacement).unwrap();
    assert_ne!(before, after);
    let copy_source = source.clone();
    let copy_destination = destination.clone();
    let observer = tokio::spawn(async move {
        backup_files::copy_verified(copy_source, copy_destination, None, false).await
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if fs::metadata(&destination)
            .is_ok_and(|meta| meta.len() > 0 && meta.len() < 64 * 1024 * 1024)
        {
            break;
        }
        assert!(
            !observer.is_finished(),
            "Fixture did not observe admitted partial copy"
        );
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    fs::rename(&source, fixture.0.join("old-source")).unwrap();
    fs::rename(&replacement, &source).unwrap();
    let result = observer.await.unwrap();
    assert_eq!(result.as_ref().unwrap_err().code, "backup_changed");
    assert_eq!(
        digest(fs::read(&source).unwrap()),
        digest(fs::read(&destination).unwrap())
    );
    retain(&J::object([
        ("nativeOnly", "replaced-source-equal-bytes-size-time".into()),
        ("beforeInode", before.ino.into()),
        ("afterInode", after.ino.into()),
        ("result", wrap(result.map(|file| file.json()))),
        ("destination", observation(&destination)),
    ]));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_copy_finishes_flushing_after_its_observer_is_dropped() {
    let fixture = ServiceFixture::new();
    let source = fixture.0.join("source");
    let destination = fixture.0.join("result");
    fs::write(&source, vec![173; 64 * 1024 * 1024]).unwrap();
    let bytes = fs::metadata(&source).unwrap().len();
    let copy_source = source.clone();
    let copy_destination = destination.clone();
    let observer = tokio::spawn(async move {
        backup_files::copy_verified(copy_source, copy_destination, None, false).await
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if fs::metadata(&destination).is_ok_and(|meta| meta.len() > 0 && meta.len() < bytes) {
            break;
        }
        assert!(
            !observer.is_finished(),
            "Fixture did not observe admitted partial copy"
        );
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    loop {
        let complete = fs::metadata(&destination).is_ok_and(|meta| meta.len() == bytes);
        #[cfg(windows)]
        let closed = {
            use std::os::windows::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&destination)
                .is_ok()
        };
        #[cfg(unix)]
        let closed = true;
        if complete && closed {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        digest(&fs::read(&source).unwrap()),
        digest(&fs::read(&destination).unwrap())
    );
    retain(&J::object([
        ("nativeOnly", "copy-survives-observer-cancellation".into()),
        ("bytes", J::Number(bytes as f64)),
        ("destination", observation(&destination)),
    ]));
}

#[tokio::test]
async fn directory_flush_matches_original_platform_policy() {
    let fixture = ServiceFixture::new();
    let root = fixture.0.join("tree");
    fs::create_dir_all(root.join("child/deep")).unwrap();
    fs::write(root.join("child/file"), b"data").unwrap();
    let link = fixture.0.join("link");
    filesystem::directory_link(&root, &link).unwrap();
    for root in [root.clone(), fixture.0.join("missing"), link] {
        let actual = report(
            backup_files::sync_directories(root.clone())
                .await
                .map(|_| J::Null),
            vec![],
            None,
        );
        compare(
            J::object([
                ("operation", "sync".into()),
                ("fixtureRoot", p(&fixture.0)),
                ("root", p(&root)),
            ]),
            actual,
        );
    }
    filesystem::directory_link(&fixture.0.join("artifact"), &root.join("child/link")).unwrap();
    compare(
        J::object([
            ("operation", "sync".into()),
            ("fixtureRoot", p(&fixture.0)),
            ("root", p(&root)),
        ]),
        report(
            backup_files::sync_directories(root).await.map(|_| J::Null),
            vec![],
            None,
        ),
    );
}

#[cfg(unix)]
#[tokio::test]
async fn inventory_rejects_unsafe_native_names_and_special_entries() {
    for name in ["colon:name", "back\\slash", "control\u{1f}"] {
        let fixture = ServiceFixture::new();
        let root = fixture.0.join("tree");
        fs::create_dir(&root).unwrap();
        fs::write(root.join(name), b"data").unwrap();
        compare(
            inventory_input(&fixture, &root, &[], None),
            inventory(&fixture, &root, &[], None).await,
        );
        compare(
            inventory_input(&fixture, &root, &[name], None),
            inventory(&fixture, &root, &[name], None).await,
        );
    }
    let fixture = ServiceFixture::new();
    let root = fixture.0.join("tree");
    fs::create_dir(&root).unwrap();
    let socket = std::os::unix::net::UnixListener::bind(root.join("socket")).unwrap();
    compare(
        inventory_input(&fixture, &root, &[], None),
        inventory(&fixture, &root, &[], None).await,
    );
    drop(socket);
}
