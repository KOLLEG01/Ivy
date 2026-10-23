use ivy_contracts::{IvyJson, JsString};
use ivy_host_runtime::{
    filesystem,
    native_path::{self, Context, Style},
    paths, preparation_files, preparation_transfer,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-native-path-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn text(path: &Path) -> JsString {
    native_path::from_native(path)
}

#[test]
fn current_context_and_public_strings_retain_native_paths() {
    let cwd = std::env::current_dir().unwrap();
    let context = Context::current().unwrap();
    assert_eq!(context.style, Style::native());
    assert_eq!(context.resolve(&[]), text(&cwd));
    assert_eq!(native_path::to_native(&text(&cwd)), cwd);
    assert!(
        context
            .filesystem_path(&JsString::default())
            .as_os_str()
            .is_empty()
    );
    let fixture = Fixture::new();
    let context = Context {
        style: Style::native(),
        cwd: text(&fixture.0),
    };
    let child = fixture.0.join("child");
    assert_eq!(context.resolve(&["a/../child".into()]), text(&child));
    assert_eq!(
        context.relative(&text(&fixture.0), &text(&child)),
        JsString::from("child")
    );
    assert_eq!(
        context.relative(&text(&child), &text(&fixture.0)),
        JsString::from("..")
    );
    assert_eq!(
        context.relative(&text(&child), &text(&child)),
        JsString::default()
    );
    assert_eq!(
        native_path::join(Style::native(), &[text(&child), text(&fixture.0)]),
        text(&fixture.0)
    );
    assert_eq!(
        native_path::dirname(Style::native(), &text(&child)),
        text(&fixture.0)
    );
    assert_eq!(
        native_path::basename(Style::native(), &text(&child)),
        JsString::from("child")
    );
    assert!(!native_path::is_absolute(Style::native(), &"child".into()));
    assert_eq!(
        native_path::normalize(Style::native(), &"a/../b".into()),
        JsString::from("b")
    );
}

#[test]
fn containment_normalizes_parents_and_rejects_sibling_prefixes() {
    let root = std::env::temp_dir().join("ivy-path-test");
    assert!(paths::inside(&root, &root.join("a/../b")).unwrap());
    assert!(paths::inside(&root, &root).unwrap());
    assert!(!paths::inside(&root, &root.join("../other")).unwrap());
    assert!(!paths::inside(&root, &PathBuf::from(format!("{}-sibling", root.display()))).unwrap());
}

#[cfg(windows)]
#[test]
fn windows_containment_compares_native_components_and_verbatim_prefixes() {
    for (root, child) in [
        (r"C:\Local\Owned", r"c:\local\owned\child"),
        (r"C:\Local\Ä", r"\\?\c:\local\ä\child"),
        (r"\\?\C:\Local\Owned", r"C:\Local\Owned\child"),
        (r"\\server\share\owned", r"\\?\UNC\SERVER\SHARE\owned\child"),
    ] {
        assert!(
            paths::inside(Path::new(root), Path::new(child)).unwrap(),
            "{root} / {child}"
        );
    }
    for (root, child) in [
        (r"C:\Local\Owned", r"D:\Local\Owned\child"),
        (r"C:\Local\Owned", r"C:\Local\Owned-other\child"),
        (r"\\server\share\owned", r"\\server\other\owned\child"),
        (r"C:\Local\Owned", r"\\?\C:\Local\Owned\..\other"),
    ] {
        assert!(
            !paths::inside(Path::new(root), Path::new(child)).unwrap(),
            "{root} / {child}"
        );
    }
    assert!(!native_path::is_absolute(
        Style::native(),
        &r"\rooted-without-drive".into()
    ));
}

#[test]
fn canonical_paths_and_future_descendants_use_the_existing_ancestor() {
    let fixture = Fixture::new();
    let actual = fs::canonicalize(&fixture.0).unwrap();
    assert_eq!(paths::realpath(&fixture.0).unwrap(), actual);
    let future = fixture.0.join("missing/leaf");
    assert_eq!(
        paths::resolved_future_path(&future).unwrap(),
        actual.join("missing/leaf")
    );
    assert_eq!(
        paths::future_path(&text(&future)).unwrap(),
        text(&actual.join("missing/leaf"))
    );
    assert!(paths::inside(&fixture.0, &paths::resolved_future_path(&future).unwrap()).unwrap());
    assert!(!fixture.0.join("missing").exists());
    let file = fixture.0.join("file");
    fs::write(&file, b"text").unwrap();
    assert_eq!(
        paths::resolved_future_path(&file.join("child"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotADirectory
    );
    assert_eq!(
        paths::realpath(&fixture.0.join("missing"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn directory_links_resolve_to_the_target_without_authorizing_an_escape() {
    let fixture = Fixture::new();
    let root = fixture.0.join("root");
    let outside = fixture.0.join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    let link = root.join("link");
    filesystem::directory_link(&outside, &link).unwrap();
    assert!(paths::inside(&root, &link).unwrap());
    assert_eq!(
        paths::realpath(&link).unwrap(),
        paths::realpath(&outside).unwrap()
    );
    let future = paths::resolved_future_path(&link.join("missing/leaf")).unwrap();
    assert_eq!(
        future,
        paths::realpath(&outside).unwrap().join("missing/leaf")
    );
    assert!(!paths::inside(&root, &future).unwrap());
    assert_eq!(
        filesystem::stat_identity(&link).unwrap(),
        filesystem::stat_identity(&outside).unwrap()
    );
    assert!(filesystem::directory_link(&root, &link).is_err());
}

#[tokio::test]
async fn native_reads_keep_file_bounds_and_directory_containment() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("text ü 空 😀");
    fs::create_dir(&directory).unwrap();
    let file = directory.join("file");
    fs::write(&file, b"text").unwrap();
    assert_eq!(
        filesystem::realpath(text(&file)).await.unwrap(),
        Some(text(&fs::canonicalize(&file).unwrap()))
    );
    assert_eq!(
        filesystem::read_regular(text(&directory), text(&file), 4)
            .await
            .unwrap(),
        Some("text".into())
    );
    assert!(
        filesystem::read_regular(text(&directory), text(&file), 3)
            .await
            .is_err()
    );
    let other = fixture.0.join("outside");
    fs::write(&other, b"outside").unwrap();
    assert!(
        filesystem::read_regular(text(&directory), text(&other), 100)
            .await
            .is_err()
    );
    assert_eq!(
        filesystem::read_regular(text(&directory), text(&directory.join("missing")), 100)
            .await
            .unwrap(),
        None
    );
    assert!(filesystem::realpath("invalid\0path".into()).await.is_err());
    assert!(!matches!(
        filesystem::realpath(JsString::default()).await,
        Ok(Some(_))
    ));
}

#[tokio::test]
async fn native_transfer_replaces_files_and_preserves_identity() {
    let fixture = Fixture::new();
    let source = fixture.0.join("source ü 空 😀");
    let dest = fixture.0.join("destination");
    fs::write(&source, b"new").unwrap();
    fs::write(&dest, b"old").unwrap();
    let identity = filesystem::stat_identity(&source).unwrap();
    preparation_transfer::transfer_prepared_payload(source.clone(), dest.clone(), false)
        .await
        .unwrap();
    assert!(!source.exists());
    assert_eq!(fs::read(&dest).unwrap(), b"new");
    assert_eq!(filesystem::stat_identity(&dest).unwrap(), identity);
    let directory = fixture.0.join("directory");
    let occupied = fixture.0.join("occupied");
    fs::create_dir(&directory).unwrap();
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("retained"), b"retained").unwrap();
    assert!(
        preparation_transfer::transfer_prepared_payload(directory.clone(), occupied.clone(), true)
            .await
            .is_err()
    );
    assert!(directory.is_dir());
    assert_eq!(fs::read(occupied.join("retained")).unwrap(), b"retained");
}

#[cfg(windows)]
#[tokio::test]
async fn native_windows_filenames_keep_utf16_long_paths_and_exact_verbatim_names() {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let fixture = Fixture::new();
    let source = fs::canonicalize(&fixture.0).unwrap().join("source");
    fs::create_dir(&source).unwrap();
    let raw_name = OsString::from_wide(&[b'n' as u16, 0xd800, b'-' as u16, 0xdc00]);
    for name in [
        raw_name.clone(),
        "unicode ü 空 😀".into(),
        "trailing.".into(),
        "trailing ".into(),
    ] {
        let path = source.join(&name);
        fs::write(&path, b"exact").unwrap();
        assert_eq!(native_path::filesystem_name(&name), text(Path::new(&name)));
        assert_eq!(native_path::to_native(&text(&path)), path);
        assert_eq!(
            filesystem::read_regular(text(&source), text(&path), 5)
                .await
                .unwrap(),
            Some("exact".into())
        );
    }
    let names: Vec<_> = fs::read_dir(&source)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(names.contains(&raw_name));
    let long = fixture
        .0
        .join("segment-".repeat(12))
        .join("segment-".repeat(12))
        .join("segment-".repeat(12));
    fs::create_dir_all(&long).unwrap();
    let file = long.join("file");
    fs::write(&file, b"long").unwrap();
    assert_eq!(
        filesystem::read_regular(text(&long), text(&file), 4)
            .await
            .unwrap(),
        Some("long".into())
    );
    let copy = fs::canonicalize(&fixture.0).unwrap().join("copy");
    preparation_files::copy_source_tree(source, copy.clone())
        .await
        .unwrap();
    assert_eq!(fs::read(copy.join(raw_name)).unwrap(), b"exact");
    assert_eq!(fs::read(copy.join("trailing.")).unwrap(), b"exact");
    assert_eq!(fs::read(copy.join("trailing ")).unwrap(), b"exact");
}

#[cfg(unix)]
#[test]
fn posix_paths_preserve_case_and_follow_parent_components_after_links() {
    let fixture = Fixture::new();
    assert!(!paths::inside(&fixture.0.join("Owned"), &fixture.0.join("owned/child")).unwrap());
    let nested = fixture.0.join("target/nested");
    fs::create_dir_all(&nested).unwrap();
    std::os::unix::fs::symlink(&nested, fixture.0.join("alias")).unwrap();
    fs::write(fixture.0.join("target/file"), b"native").unwrap();
    let original = fixture.0.join("alias/../file");
    assert_eq!(
        fs::read(
            Context::current()
                .unwrap()
                .filesystem_path(&text(&original))
        )
        .unwrap(),
        b"native"
    );
    assert_eq!(
        paths::resolved_future_path(&fixture.0.join("alias/../new/leaf")).unwrap(),
        fixture.0.join("target/new/leaf")
    );
}

#[allow(dead_code)]
#[path = "host-service-fixture.rs"]
mod service_fixture;
#[path = "host-target-storage-cases.rs"]
mod target_storage_cases;

#[test]
fn accepted_configuration_files_remain_private_immutable_and_direct() {
    use ivy_host_runtime::accepted_configuration::*;
    use service_fixture::{ServiceFixture, host_configuration};
    fn assert_json_eq(actual: &IvyJson, expected: &IvyJson) {
        assert_eq!(
            ivy_contracts::json::canonical(actual, usize::MAX).unwrap(),
            ivy_contracts::json::canonical(expected, usize::MAX).unwrap()
        );
    }
    let fixture = ServiceFixture::new();
    let anchor = host_configuration(&fixture);
    let saved = save_accepted_configuration(&anchor).unwrap();
    let expected = format!(
        "{}\n",
        ivy_contracts::json::canonical(&anchor, usize::MAX).unwrap()
    );
    assert_eq!(fs::read_to_string(&saved).unwrap(), expected);
    assert_json_eq(
        &read_accepted_configuration(&anchor, &saved).unwrap(),
        &anchor,
    );
    let source = fixture.0.join("state/launch.json");
    fs::write(
        &source,
        ivy_contracts::json::stringify(&fixture.value(), usize::MAX).unwrap(),
    )
    .unwrap();
    let target = save_accepted_instance_configuration(&anchor, &source, "launch-1").unwrap();
    let retained = fs::read(&target).unwrap();
    assert!(save_accepted_instance_configuration(&anchor, &source, "launch-1").is_err());
    assert_eq!(fs::read(&target).unwrap(), retained);
    fs::write(&source, "null").unwrap();
    assert_json_eq(
        &read_accepted_instance_configuration(&anchor, &target).unwrap(),
        &fixture.value(),
    );
    for (mode, code) in [
        ("missing", "internal_error"),
        ("directory", "configuration_changed"),
        ("oversize", "configuration_changed"),
        ("invalid", "configuration_changed"),
        ("outside", "target_conflict"),
        ("nested", "configuration_changed"),
        ("changed", "target_conflict"),
    ] {
        let file = match mode {
            "outside" => fixture.0.join("outside.json"),
            "nested" => saved.parent().unwrap().join("nested/test.json"),
            _ => saved.parent().unwrap().join(format!("{mode}.json")),
        };
        if mode == "directory" {
            fs::create_dir(&file).unwrap();
        } else if mode != "missing" {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            let content = if mode == "oversize" {
                " ".repeat(1024 * 1024 + 1)
            } else if mode == "invalid" {
                "{".into()
            } else if mode == "changed" {
                let mut value = anchor.clone();
                value.insert("hostId", "other".into());
                ivy_contracts::json::canonical(&value, usize::MAX).unwrap()
            } else {
                expected.clone()
            };
            fs::write(&file, content).unwrap();
        }
        assert_eq!(
            read_accepted_configuration(&anchor, &file)
                .unwrap_err()
                .code,
            code,
            "{mode}"
        );
    }
    let exact = saved.parent().unwrap().join("exact-limit.json");
    let mut bytes = expected.as_bytes().to_vec();
    bytes.resize(1024 * 1024, b' ');
    fs::write(&exact, &bytes).unwrap();
    assert_json_eq(
        &read_accepted_configuration(&anchor, &exact).unwrap(),
        &anchor,
    );
    bytes.push(b' ');
    fs::write(&exact, &bytes).unwrap();
    assert_eq!(
        read_accepted_configuration(&anchor, &exact)
            .unwrap_err()
            .code,
        "configuration_changed"
    );

    // Native callers retain their OS filename representation through all I/O.
    let mut names = vec![std::ffi::OsString::from(
        "native ü 空 😀; ' configuration.json",
    )];
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        names.push(std::ffi::OsString::from_wide(&[
            110, 97, 116, 105, 118, 101, 45, 0xd800, 46, 106, 115, 111, 110,
        ]));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        names.push(std::ffi::OsString::from_vec(b"native-\xff.json".to_vec()));
    }
    let original = ivy_contracts::json::canonical(&fixture.value(), usize::MAX).unwrap();
    for (at, name) in names.into_iter().enumerate() {
        let native = source.parent().unwrap().join(name);
        fs::write(&native, &original).unwrap();
        let snapshot =
            save_accepted_instance_configuration(&anchor, &native, &format!("native-{at}"))
                .unwrap();
        assert_json_eq(
            &read_accepted_instance_configuration(&anchor, &snapshot).unwrap(),
            &fixture.value(),
        );
        assert_eq!(fs::read(&native).unwrap(), original.as_bytes());
    }

    // This reader retains replacement decoding; the diagnostic-secret policy
    // does not change configuration text or escaped public UTF-16 values.
    let mut value = fixture.value();
    value.insert(
        "settings",
        IvyJson::object([("text", "before-sentinel-after".into())]),
    );
    let mut bytes = ivy_contracts::json::canonical(&value, usize::MAX)
        .unwrap()
        .into_bytes();
    let at = bytes
        .windows(b"sentinel".len())
        .position(|part| part == b"sentinel")
        .unwrap();
    bytes.splice(at..at + b"sentinel".len(), [0xff]);
    fs::write(&source, bytes).unwrap();
    let snapshot = save_accepted_instance_configuration(&anchor, &source, "replacement").unwrap();
    value.insert(
        "settings",
        IvyJson::object([("text", "before-�-after".into())]),
    );
    assert_json_eq(
        &read_accepted_instance_configuration(&anchor, &snapshot).unwrap(),
        &value,
    );

    for (bad, code) in [
        (fixture.0.join("outside-launch.json"), "target_conflict"),
        (
            fixture.0.join("state-sibling/launch.json"),
            "target_conflict",
        ),
        (fixture.0.join("state"), "configuration_changed"),
        (PathBuf::new(), "target_conflict"),
        (fixture.0.join("state/bad\0name"), "internal_error"),
    ] {
        assert_eq!(
            save_accepted_instance_configuration(&anchor, &bad, "refused")
                .unwrap_err()
                .code,
            code
        );
        assert!(
            !accepted_instance_configuration_path(&anchor, "refused")
                .unwrap()
                .exists()
        );
    }
    let state = fixture.0.join("state-order");
    fs::create_dir(&state).unwrap();
    let mut ordered = anchor.clone();
    ordered.insert(
        "runtimeRoot",
        IvyJson::String(ivy_host_runtime::native_path::from_native(&state)),
    );
    let source = state.join("launch.json");
    fs::write(&source, original).unwrap();
    assert_eq!(
        save_accepted_instance_configuration(&ordered, &source, "../refused")
            .unwrap_err()
            .code,
        "target_conflict"
    );
    assert!(state.join("accepted-configurations").is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        assert_eq!(
            fs::metadata(&saved).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(saved.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let link = saved.parent().unwrap().join("link.json");
        symlink(&saved, &link).unwrap();
        assert_eq!(
            read_accepted_configuration(&anchor, &link)
                .unwrap_err()
                .code,
            "configuration_changed"
        );
    }
}
