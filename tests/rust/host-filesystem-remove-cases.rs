use ivy_host_runtime::{filesystem, native_path};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

struct Fixture {
    root: PathBuf,
    base: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = base.join(format!("ivy-remove-sync-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self { root, base }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.root.parent(), Some(self.base.as_path()));
        assert!(
            self.root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-remove-sync-")
        );
        let _ = filesystem::remove_force_sync(&self.root);
    }
}
fn readonly(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn filesystem_remove_accepts_missing_and_removes_native_files_trees_and_links() {
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
        let fixture = Fixture::new();
        let root = fixture.path("root");
        let outside = fixture.path("outside");
        fs::create_dir(&outside).unwrap();
        let sentinel = outside.join("sentinel");
        fs::write(&sentinel, b"preserved").unwrap();
        match case {
            "file" | "readonly-file" => {
                fs::write(&root, b"owned").unwrap();
                if case == "readonly-file" {
                    readonly(&root);
                }
            }
            "root-link" | "dangling-link" => {
                filesystem::directory_link(&outside, &root).unwrap();
                if case == "dangling-link" {
                    fs::remove_dir_all(&outside).unwrap();
                }
            }
            "tree" | "embedded-link" => {
                fs::create_dir_all(root.join("nested")).unwrap();
                fs::write(root.join("nested/value"), b"owned").unwrap();
                if case == "embedded-link" {
                    filesystem::directory_link(&outside, &root.join("link")).unwrap();
                }
            }
            "missing" | "empty-path" => (),
            _ => unreachable!(),
        }
        let selected = if case == "empty-path" {
            Path::new("")
        } else {
            &root
        };
        filesystem::remove_force_sync(selected).unwrap();
        assert!(fs::symlink_metadata(&root).is_err(), "{case}");
        if case != "dangling-link" {
            assert_eq!(fs::read(&sentinel).unwrap(), b"preserved", "{case}");
        }
    }
}

#[test]
fn filesystem_remove_preserves_native_path_names() {
    let fixture = Fixture::new();
    let context = native_path::Context::current().unwrap();
    for name in ["relative", "q'uöté-😀"] {
        let root = fixture.path(name);
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/value"), b"owned").unwrap();
        let selected = if name == "relative" {
            native_path::to_native(
                &context.relative(&context.cwd, &native_path::from_native(&root)),
            )
        } else {
            root.clone()
        };
        filesystem::remove_force_sync(&selected).unwrap();
        assert!(!root.exists());
    }
    #[cfg(windows)]
    {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        for name in [
            OsString::from_wide(&[b'n' as u16, 0xd800]),
            "trailing. ".into(),
        ] {
            let root = fixture.root.join(name);
            fs::write(&root, b"owned").unwrap();
            filesystem::remove_force_sync(&root).unwrap();
            assert!(fs::symlink_metadata(&root).is_err());
        }
        let root = fixture
            .path("segment-".repeat(12).as_str())
            .join("segment-".repeat(12))
            .join("segment-".repeat(12));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("value"), b"owned").unwrap();
        filesystem::remove_force_sync(&root).unwrap();
        assert!(!root.exists());
    }
}

#[test]
fn filesystem_remove_returns_native_invalid_path_errors_without_effects() {
    let fixture = Fixture::new();
    let retained = fixture.path("retained");
    fs::write(&retained, b"kept").unwrap();
    #[cfg(windows)]
    let invalid = {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        OsString::from_wide(&[b'n' as u16, 0, b'x' as u16])
    };
    #[cfg(unix)]
    let invalid = {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        OsString::from_vec(b"n\0x".to_vec())
    };
    let error = filesystem::remove_force_sync(&fixture.root.join(invalid)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let expected = fs::symlink_metadata(retained.join("child")).unwrap_err();
    let actual = filesystem::remove_force_sync(&retained.join("child"));
    if expected.kind() == io::ErrorKind::NotFound {
        actual.unwrap();
    } else {
        let actual = actual.unwrap_err();
        assert_eq!(actual.raw_os_error(), expected.raw_os_error());
        assert_eq!(actual.kind(), expected.kind());
    }
    assert_eq!(fs::read(&retained).unwrap(), b"kept");
}

#[cfg(windows)]
struct DeniedDirectory(PathBuf);
#[cfg(windows)]
impl DeniedDirectory {
    fn command(&self, args: &[&str]) {
        let tool =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/icacls.exe");
        let output = std::process::Command::new(tool)
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Owned fixture ACL operation failed"
        );
    }
    fn new(path: PathBuf) -> Self {
        let owned = Self(path);
        owned.command(&["/deny", "*S-1-1-0:(RD)"]);
        owned
    }
}
#[cfg(windows)]
impl Drop for DeniedDirectory {
    fn drop(&mut self) {
        self.command(&["/remove:d", "*S-1-1-0"]);
    }
}

#[cfg(windows)]
#[test]
fn filesystem_remove_keeps_real_sharing_and_acl_failures_native() {
    use std::os::windows::fs::OpenOptionsExt;
    for share in [0, 3] {
        for nested in [false, true] {
            let fixture = Fixture::new();
            let root = fixture.path("root");
            let held = if nested {
                fs::create_dir(&root).unwrap();
                fs::write(root.join("before"), b"before").unwrap();
                root.join("held")
            } else {
                root.clone()
            };
            fs::write(&held, b"retained").unwrap();
            readonly(&held);
            let guard = fs::OpenOptions::new()
                .read(true)
                .share_mode(share)
                .open(&held)
                .unwrap();
            let error = filesystem::remove_force_sync(&root).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(32));
            assert!(guard.metadata().unwrap().permissions().readonly());
            drop(guard);
            assert_eq!(fs::read(&held).unwrap(), b"retained");
            filesystem::remove_force_sync(&root).unwrap();
            assert!(!root.exists());
        }
    }
    for nested in [false, true] {
        let fixture = Fixture::new();
        let root = fixture.path("root");
        let denied = if nested {
            root.join("denied")
        } else {
            root.clone()
        };
        fs::create_dir_all(&denied).unwrap();
        fs::write(denied.join("retained"), b"kept").unwrap();
        let guard = DeniedDirectory::new(denied.clone());
        let error = filesystem::remove_force_sync(&root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.raw_os_error().is_some());
        drop(guard);
        assert_eq!(fs::read(denied.join("retained")).unwrap(), b"kept");
        filesystem::remove_force_sync(&root).unwrap();
        assert!(!root.exists());
    }
}

#[cfg(windows)]
#[test]
fn filesystem_remove_removes_readonly_entries_without_changing_link_targets() {
    let fixture = Fixture::new();
    let root = fixture.path("root");
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join("nested/owned"), b"owned").unwrap();
    readonly(&root.join("nested/owned"));
    readonly(&root.join("nested"));
    let outside = fixture.path("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"preserved").unwrap();
    readonly(&outside.join("sentinel"));
    filesystem::directory_link(&outside, &root.join("link")).unwrap();
    filesystem::remove_force_sync(&root).unwrap();
    assert!(!root.exists());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"preserved");
    assert!(
        fs::metadata(outside.join("sentinel"))
            .unwrap()
            .permissions()
            .readonly()
    );
}

#[tokio::test]
async fn filesystem_remove_async_owner_uses_the_same_native_policy() {
    let fixture = Fixture::new();
    let root = fixture.path("root");
    let outside = fixture.path("outside");
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(root.join("nested/owned"), b"owned").unwrap();
    fs::write(outside.join("sentinel"), b"preserved").unwrap();
    filesystem::directory_link(&outside, &root.join("link")).unwrap();
    filesystem::remove_force(&root).await.unwrap();
    filesystem::remove_force(&root).await.unwrap();
    assert!(!root.exists());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"preserved");
}
