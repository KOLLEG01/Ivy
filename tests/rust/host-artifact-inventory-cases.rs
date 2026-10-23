use ivy_contracts::{JsString, hash};
use ivy_host_runtime::{
    IvyJson,
    artifact::{
        ArtifactFile, FileCompare, artifact_files, artifact_source_identity, native_file_compare,
    },
    candidate_identity::candidate_build_id,
    filesystem,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-artifact-inventory-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let file = self.0.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        }
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
fn expected_file(path: &str, bytes: &[u8], mode: u32) -> ArtifactFile {
    ArtifactFile {
        path: path.into(),
        hash: hash::digest(bytes),
        bytes: bytes.len() as u64,
        mode,
    }
}
fn ordinary_mode() -> u32 {
    if cfg!(windows) { 0o666 } else { 0o640 }
}

#[test]
fn native_file_compare_orders_retained_text_without_locale_runtime() {
    let compare = native_file_compare().unwrap();
    // Explicit retained UTF-16 order, including values that are not UTF-8 text.
    let ordered = [
        JsString(vec![]),
        "A".into(),
        "a".into(),
        JsString(vec![97, 0]),
        JsString(vec![0xd800]),
        "🙂".into(),
        JsString(vec![0xdfff]),
        "\u{e000}".into(),
        "�".into(),
    ];
    for (left, a) in ordered.iter().enumerate() {
        for (right, b) in ordered.iter().enumerate() {
            assert_eq!(compare(a, b), left.cmp(&right));
        }
    }
}

#[tokio::test]
async fn native_file_compare_retains_order_across_thread_and_inventory() {
    let compare = native_file_compare().unwrap();
    let compare = std::thread::spawn(move || {
        assert!(compare(&"🙂".into(), &"\u{e000}".into()).is_lt());
        compare
    })
    .join()
    .unwrap();
    let fixture = Fixture::new();
    let mut root_name = std::ffi::OsString::from("native root ü 空 😀; '");
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        root_name.push(std::ffi::OsString::from_wide(&[0xd800]));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        root_name.push(std::ffi::OsString::from_vec(vec![0xff]));
    }
    let root = fixture.0.join(root_name);
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("empty")).unwrap();
    let mut names = vec![
        std::ffi::OsString::from("é"),
        std::ffi::OsString::from("🙂"),
    ];
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        names.push(std::ffi::OsString::from_wide(&[114, 97, 119, 45, 0xd800]));
        names.push(std::ffi::OsString::from("raw-�"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        names.push(std::ffi::OsString::from("raw-�"));
        // Equal public spellings retain native Unix byte traversal order.
        names.push(std::ffi::OsString::from_vec(b"raw-\xff".to_vec()));
    }
    let mut expected = Vec::new();
    for (at, name) in names.into_iter().enumerate() {
        let path = root.join(&name);
        fs::write(&path, [at as u8]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        expected.push(ArtifactFile {
            path: ivy_host_runtime::native_path::from_native(Path::new(&name)),
            hash: hash::digest(&[at as u8]),
            bytes: 1,
            mode: ordinary_mode(),
        });
    }
    expected.sort_by(|a, b| a.path.0.cmp(&b.path.0));
    for selected in [
        root.clone(),
        root.join("."),
        fs::canonicalize(&root).unwrap(),
    ] {
        assert_eq!(
            artifact_files(selected, compare.clone()).await.unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn inventory_preserves_all_ordinary_bytes_modes_paths_and_supplied_order() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("empty/nested")).unwrap();
    let mut expected = Vec::new();
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
        expected.push(expected_file(name, name.as_bytes(), ordinary_mode()));
    }
    for index in 0..18u8 {
        let name = format!("branch-{index}/file");
        fixture.write(&name, &[index]);
        expected.push(expected_file(&name, &[index], ordinary_mode()));
    }
    let executable = fixture.write("executable", b"mode bytes");
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
    expected.push(expected_file(
        "executable",
        b"mode bytes",
        if cfg!(windows) { 0o444 } else { 0o751 },
    ));
    let large = vec![0xa7; 4 * 1024 * 1024 + 17];
    fixture.write("large", &large);
    expected.push(expected_file("large", &large, ordinary_mode()));
    fixture.write("empty-file", b"");
    expected.push(expected_file("empty-file", b"", ordinary_mode()));
    fs::hard_link(fixture.0.join("large"), fixture.0.join("same-bytes")).unwrap();
    expected.push(expected_file("same-bytes", &large, ordinary_mode()));
    expected.reverse();
    let order: Vec<_> = expected.iter().map(|file| file.path.clone()).collect();
    let compare: FileCompare = Arc::new(move |a, b| {
        order
            .iter()
            .position(|key| key == a)
            .expect("known path")
            .cmp(&order.iter().position(|key| key == b).expect("known path"))
    });
    let actual = artifact_files(fixture.0.clone(), compare).await.unwrap();
    assert_eq!(actual, expected);
    assert_eq!(fs::read(fixture.0.join("z")).unwrap(), b"z");
}

#[tokio::test]
async fn source_identity_excludes_modes_empty_directories_and_capture_location_but_keeps_file_bytes()
 {
    let fixture = Fixture::new();
    let root = fixture.0.join("source");
    fixture.write("source/value", b"fixed source bytes");
    let compare = native_file_compare().unwrap();
    let mut identities = Vec::new();
    let mut builds = Vec::new();
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
        let bytes = if phase == 2 {
            b"changed source bytes".as_slice()
        } else {
            b"fixed source bytes".as_slice()
        };
        let files = artifact_files(root.clone(), compare.clone()).await.unwrap();
        let identity = artifact_source_identity(&files).unwrap();
        // The public identity is a canonical array of path/hash/byte records;
        // permissions are inventory data but do not identify source bytes.
        let rows = IvyJson::Array(vec![IvyJson::object([
            ("path", "value".into()),
            ("hash", hash::digest(bytes).into()),
            ("bytes", IvyJson::Number(bytes.len() as f64)),
        ])]);
        assert_eq!(identity, hash::hash_json(&rows).unwrap());
        builds.push(
            candidate_build_id(&"sample".into(), &"1.2.3".into(), &identity.clone().into())
                .unwrap(),
        );
        identities.push(identity);
    }
    assert_eq!(identities[0], identities[1]);
    assert_ne!(identities[1], identities[2]);
    assert_eq!(builds[0], builds[1]);
    assert_ne!(builds[1], builds[2]);
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
        let result = artifact_files(root, native_file_compare().unwrap()).await;
        if kind == "empty" {
            assert!(result.unwrap().is_empty());
        } else {
            assert_eq!(
                result.unwrap_err().code,
                if kind == "link" {
                    "artifact_changed"
                } else {
                    "internal_error"
                }
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
        assert_eq!(
            artifact_files(fixture.0.clone(), native_file_compare().unwrap())
                .await
                .unwrap_err()
                .code,
            "artifact_changed"
        );
    }
}

#[tokio::test]
async fn inventory_checks_the_actual_file_limit_before_hashing() {
    let fixture = Fixture::new();
    for index in 0..50_001 {
        fs::File::create(fixture.0.join(format!("{index:05}"))).unwrap();
    }
    assert_eq!(
        artifact_files(fixture.0.clone(), native_file_compare().unwrap())
            .await
            .unwrap_err()
            .code,
        "limit_exceeded"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn inventory_preserves_native_file_sharing_refusal_and_recovers_after_release() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let path = fixture.write("held", b"shared byte identity");
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .unwrap();
    assert_eq!(
        artifact_files(fixture.0.clone(), native_file_compare().unwrap())
            .await
            .unwrap_err()
            .code,
        "internal_error"
    );
    drop(held);
    let rows = artifact_files(fixture.0.clone(), native_file_compare().unwrap())
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![expected_file(
            "held",
            b"shared byte identity",
            ordinary_mode()
        )]
    );
}

#[test]
fn inventory_admission_survives_a_dropped_waiter() {
    use std::{
        future::Future,
        sync::atomic::{AtomicBool, Ordering},
        task::Poll,
        time::Duration,
    };
    let fixture = Fixture::new();
    fixture.write("a", b"first");
    fixture.write("b", b"second");
    let finished = Arc::new(AtomicBool::new(false));
    let signal = finished.clone();
    let compare: FileCompare = Arc::new(move |a, b| {
        signal.store(true, Ordering::Release);
        a.0.cmp(&b.0)
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
            while !finished.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    assert_eq!(fs::read(fixture.0.join("a")).unwrap(), b"first");
}
