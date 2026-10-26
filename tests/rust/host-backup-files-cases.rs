use super::{ServiceFixture, assert_json_eq};
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, JsString, hash::digest, json};
use ivy_host_runtime::{
    backup_files::{self, BackupExclusion, BackupFile},
    filesystem, native_path, node_path,
};
use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn native_name(prefix: &str, unit: u16) -> OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&prefix.encode_utf16().chain([unit]).collect::<Vec<_>>())
    }
    #[cfg(not(windows))]
    {
        format!("{prefix}-日本語-{unit:x}").into()
    }
}
fn file_mode(path: &Path) -> u32 {
    let metadata = fs::metadata(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mode() & 0o777
    }
    #[cfg(windows)]
    {
        if metadata.permissions().readonly() {
            0o444
        } else {
            0o666
        }
    }
}
fn p(path: &Path) -> J {
    J::String(node_path::from_native(path))
}
fn wrap(result: IvyResult<J>) -> J {
    match result {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
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
async fn native_inventory_sorts_utf16_hashes_bytes_and_excludes_before_observation() {
    let fixture = ServiceFixture::new();
    let root = fixture.0.join(native_name("tree", 0xd800));
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
    let files: Vec<_> = [
        "!",
        "A",
        "empty",
        "nested/deep/binary",
        "skip/hidden",
        "z",
        "é",
        "中",
        "😀",
        "\u{e000}",
    ]
    .into_iter()
    .map(|name| {
        let path = root.join(name);
        BackupFile {
            path: name.into(),
            hash: digest(fs::read(&path).unwrap()),
            bytes: fs::metadata(&path).unwrap().len(),
            mode: file_mode(&path),
        }
    })
    .collect();
    let visits: Vec<JsString> = [
        "!",
        "A",
        "empty",
        "nested",
        "nested/deep",
        "nested/deep/binary",
        "skip",
        "skip/hidden",
        "z",
        "é",
        "中",
        "😀",
        "\u{e000}",
    ]
    .into_iter()
    .map(JsString::from)
    .collect();
    for excluded in [vec![], vec!["skip", "nested/deep/binary"]] {
        let selected = files.iter().filter(|file| {
            excluded.is_empty()
                || (file.path != JsString::from("skip/hidden")
                    && file.path != JsString::from("nested/deep/binary"))
        });
        let trace = visits
            .iter()
            .filter(|name| excluded.is_empty() || **name != JsString::from("skip/hidden"))
            .cloned()
            .collect();
        let expected = report(
            Ok(J::Array(selected.map(BackupFile::json).collect())),
            trace,
            None,
        );
        assert_json_eq(
            &inventory(&fixture, &root, &excluded, None).await,
            &expected,
        );
    }
    assert_json_eq(
        &inventory(&fixture, &root, &["skip"], Some("nested")).await,
        &report(
            Err(IvyError::new("fixture_exclusion", "Exclusion failed.")),
            visits[..4].to_vec(),
            None,
        ),
    );
    assert_json_eq(
        &inventory(&fixture, &fixture.0.join("missing"), &[], None).await,
        &report(Err(IvyError::internal()), vec![], None),
    );
    let default = backup_files::inventory_files(root.clone(), None)
        .await
        .unwrap();
    assert_json_eq(
        &J::Array(default.iter().map(BackupFile::json).collect()),
        &J::Array(files.iter().map(BackupFile::json).collect()),
    );
    assert_native_inventory_root_and_entry_paths().await;
}

async fn assert_native_inventory_root_and_entry_paths() {
    let fixture = ServiceFixture::new();
    let actual_parent = fixture.0.join("actual-parent");
    let actual_root = actual_parent.join("tree Ω");
    fs::create_dir_all(actual_parent.join("deep")).unwrap();
    fs::create_dir(&actual_root).unwrap();
    fs::write(actual_root.join("selected"), b"native selected bytes").unwrap();
    let lexical_alias = fixture.0.join("tree Ω");
    fs::create_dir(&lexical_alias).unwrap();
    fs::write(lexical_alias.join("selected"), b"distinct lexical alias").unwrap();
    let link = fixture.0.join("linked-parent");
    filesystem::directory_link(&actual_parent.join("deep"), &link).unwrap();
    let supplied_root = link.join("..").join("tree Ω");
    for root in [supplied_root, fs::canonicalize(&actual_root).unwrap()] {
        let path = root.join("selected");
        let bytes = fs::read(&path).unwrap();
        let file = BackupFile {
            path: "selected".into(),
            hash: digest(&bytes),
            bytes: bytes.len() as u64,
            mode: file_mode(&path),
        };
        assert_json_eq(
            &inventory(&fixture, &root, &[], None).await,
            &report(
                Ok(J::Array(vec![file.json()])),
                vec!["selected".into()],
                None,
            ),
        );
        assert_eq!(
            fs::read(lexical_alias.join("selected")).unwrap(),
            b"distinct lexical alias"
        );
        assert_eq!(
            fs::read(actual_root.join("selected")).unwrap(),
            b"native selected bytes"
        );
    }
    #[cfg(windows)]
    {
        let root = fixture.0.join("native-entries");
        fs::create_dir(&root).unwrap();
        let actual = fs::canonicalize(&root).unwrap();
        let names = [
            OsString::from("name"),
            OsString::from("name."),
            OsString::from("name "),
            native_name("opaque", 0xd800),
            native_name("opaque", 0xdfff),
            OsString::from("opaque\u{fffd}"),
        ];
        let mut expected = Vec::new();
        for (index, name) in names.into_iter().enumerate() {
            let bytes = vec![index as u8, 0, 255];
            let path = actual.join(&name);
            fs::write(&path, &bytes).unwrap();
            expected.push(BackupFile {
                path: native_path::filesystem_name(&name),
                hash: digest(&bytes),
                bytes: bytes.len() as u64,
                mode: file_mode(&path),
            });
        }
        expected.sort_by(|a, b| a.path.cmp(&b.path));
        let trace = expected
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        for root in [root, actual] {
            assert_json_eq(
                &inventory(&fixture, &root, &[], None).await,
                &report(
                    Ok(J::Array(expected.iter().map(BackupFile::json).collect())),
                    trace.clone(),
                    None,
                ),
            );
        }
    }
}

#[tokio::test]
async fn native_inventory_rejects_links_unless_excluded_and_preserves_callback_errors() {
    let fixture = ServiceFixture::new();
    let root = fixture.0.join("tree");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a"), b"ordinary").unwrap();
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"private").unwrap();
    filesystem::directory_link(&outside, &root.join("link")).unwrap();
    let visits = vec!["a".into(), "link".into()];
    let refused = report(
        Err(IvyError::new(
            "target_conflict",
            "Backup cannot follow filesystem links.",
        )),
        visits.clone(),
        None,
    );
    assert_json_eq(&inventory(&fixture, &root, &[], None).await, &refused);
    let file = BackupFile {
        path: "a".into(),
        hash: digest(b"ordinary"),
        bytes: 8,
        mode: file_mode(&root.join("a")),
    };
    let admitted = report(Ok(J::Array(vec![file.json()])), visits, None);
    assert_json_eq(
        &inventory(&fixture, &root, &["link"], None).await,
        &admitted,
    );
    assert_json_eq(
        &inventory(&fixture, &root, &[], Some("link")).await,
        &report(
            Err(IvyError::new("fixture_exclusion", "Exclusion failed.")),
            vec!["a".into(), "link".into()],
            None,
        ),
    );
    let linked_root = fixture.0.join("linked-root");
    filesystem::directory_link(&root, &linked_root).unwrap();
    assert_json_eq(
        &inventory(&fixture, &linked_root, &[], None).await,
        &report(
            Err(IvyError::new(
                "target_conflict",
                "Backup roots cannot be filesystem links.",
            )),
            vec![],
            None,
        ),
    );
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
fn expected(source: &Path) -> BackupFile {
    let metadata = fs::metadata(source).unwrap();
    BackupFile {
        path: "ignored/expected/member".into(),
        hash: digest(&fs::read(source).unwrap()),
        bytes: metadata.len(),
        mode: 0o751,
    }
}
async fn native_copy(
    fixture: &ServiceFixture,
    source: &Path,
    expectation: Option<BackupFile>,
    preserve: bool,
    label: &str,
) {
    let destination = fixture
        .0
        .join(format!("copy-{label}"))
        .join(native_name("result", 0xdfff));
    let bytes = fs::read(source).unwrap();
    let mode = expectation
        .as_ref()
        .map(|file| file.mode)
        .unwrap_or_else(|| file_mode(source));
    let result = backup_files::copy_verified(
        source.to_owned(),
        destination.clone(),
        expectation,
        preserve,
    )
    .await
    .unwrap();
    assert_json_eq(
        &result.json(),
        &BackupFile {
            path: native_path::filesystem_name(destination.file_name().unwrap()),
            hash: digest(&bytes),
            bytes: bytes.len() as u64,
            mode,
        }
        .json(),
    );
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert_eq!(fs::read(source).unwrap(), bytes);
    #[cfg(unix)]
    assert_eq!(file_mode(&destination), if preserve { mode } else { 0o600 });
    #[cfg(windows)]
    assert_eq!(file_mode(&destination), 0o666);
}
#[tokio::test]
async fn native_verified_copy_preserves_bytes_modes_and_expected_identity() {
    let fixture = ServiceFixture::new();
    for (label, bytes) in [
        ("empty", Vec::new()),
        ("binary", vec![0, 255, 1, 0, 128]),
        ("large", vec![171; 2 * 1024 * 1024 + 3]),
        ("unicode", "é中😀".as_bytes().to_vec()),
    ] {
        let source = fixture.0.join(native_name(label, 0xd800));
        fs::write(&source, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&source, fs::Permissions::from_mode(0o740)).unwrap();
        }
        for preserve in [false, true] {
            native_copy(
                &fixture,
                &source,
                None,
                preserve,
                &format!("{label}-{preserve}-none"),
            )
            .await;
            native_copy(
                &fixture,
                &source,
                Some(expected(&source)),
                preserve,
                &format!("{label}-{preserve}-expected"),
            )
            .await;
        }
    }
}
#[tokio::test]
async fn native_verified_copy_preserves_failures_and_never_overwrites_destinations() {
    let fixture = ServiceFixture::new();
    let source = fixture.0.join("source");
    fs::write(&source, b"source content").unwrap();
    let mut wrong_size = expected(&source);
    wrong_size.bytes += 1;
    let destination = fixture.0.join("size/result");
    assert_eq!(
        backup_files::copy_verified(source.clone(), destination.clone(), Some(wrong_size), false)
            .await
            .unwrap_err(),
        IvyError::new("backup_changed", "Backup source size changed."),
    );
    assert!(!destination.parent().unwrap().exists());
    let mut wrong_hash = expected(&source);
    wrong_hash.hash = digest(b"other");
    let destination = fixture.0.join("hash/result");
    assert_eq!(
        backup_files::copy_verified(source.clone(), destination.clone(), Some(wrong_hash), false)
            .await
            .unwrap_err(),
        IvyError::new(
            "backup_changed",
            "Backup source changed during its verified copy."
        ),
    );
    assert_eq!(fs::read(&destination).unwrap(), b"source content");
    for initial in ["file", "directory", "ancestor-file", "link"] {
        let parent = fixture.0.join(initial);
        fs::create_dir(&parent).unwrap();
        let destination = parent.join("result");
        match initial {
            "file" => fs::write(&destination, b"preserved").unwrap(),
            "directory" => fs::create_dir(&destination).unwrap(),
            "ancestor-file" => (),
            "link" => {
                filesystem::directory_link(&fixture.0.join("artifact"), &destination).unwrap();
            }
            _ => unreachable!(),
        };
        let destination = if initial == "ancestor-file" {
            let ancestor = parent.join("ancestor");
            fs::write(&ancestor, b"preserved ancestor").unwrap();
            ancestor.join("result")
        } else {
            destination
        };
        let before = observation(&destination);
        assert_eq!(
            backup_files::copy_verified(source.clone(), destination.clone(), None, false)
                .await
                .unwrap_err(),
            IvyError::internal(),
        );
        assert_json_eq(&observation(&destination), &before);
        if initial == "ancestor-file" {
            assert_eq!(
                fs::read(destination.parent().unwrap()).unwrap(),
                b"preserved ancestor"
            );
        }
    }
    for (label, source, expected_error) in [
        ("missing", fixture.0.join("missing"), IvyError::internal()),
        (
            "directory-source",
            fixture.0.join("artifact"),
            IvyError::new("target_conflict", "Backup copies only regular files."),
        ),
    ] {
        let destination = fixture.0.join(format!("copy-{label}/result"));
        let error = backup_files::copy_verified(source.clone(), destination.clone(), None, false)
            .await
            .unwrap_err();
        assert_eq!(error, expected_error);
        assert!(!destination.parent().unwrap().exists());
    }
    let link = fixture.0.join("linked-source");
    filesystem::directory_link(&fixture.0.join("artifact"), &link).unwrap();
    let destination = fixture.0.join("linked-copy/result");
    assert_eq!(
        backup_files::copy_verified(link, destination.clone(), None, false)
            .await
            .unwrap_err(),
        IvyError::new("target_conflict", "Backup copies only regular files."),
    );
    assert!(!destination.parent().unwrap().exists());
    assert_eq!(fs::read(&source).unwrap(), b"source content");
}

#[tokio::test]
async fn native_concurrent_copy_claims_one_destination_exclusively() {
    let fixture = ServiceFixture::new();
    let source = fixture.0.join(native_name("source", 0xd800));
    fs::write(&source, vec![41; 1024 * 1024]).unwrap();
    let destination = fixture.0.join(native_name("result", 0xdfff));
    let alias = fixture
        .0
        .join(destination.file_name().unwrap().to_string_lossy().as_ref());
    if alias != destination {
        fs::write(&alias, b"preserved alias").unwrap();
    }
    let (one, two) = tokio::join!(
        backup_files::copy_verified(source.clone(), destination.clone(), None, false),
        backup_files::copy_verified(source.clone(), destination.clone(), None, false)
    );
    assert_ne!(one.is_ok(), two.is_ok());
    assert_eq!(fs::read(&source).unwrap(), fs::read(&destination).unwrap());
    assert_eq!(
        one.as_ref().or(two.as_ref()).unwrap().path,
        native_path::filesystem_name(destination.file_name().unwrap())
    );
    if alias != destination {
        assert_eq!(fs::read(&alias).unwrap(), b"preserved alias");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_verified_copy_detects_replaced_source_identity_even_with_equal_bytes_and_timestamp()
{
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
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_admitted_copy_finishes_flushing_after_its_observer_is_dropped() {
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
}
