use ivy_contracts::JsString;
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
