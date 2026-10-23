use ivy_contracts::{
    JsString,
    checked_native_contract::{native_catalog_path, native_versions},
};
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "ivy-catalog-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let root = std::fs::canonicalize(&self.0).unwrap();
        let temporary = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        assert!(root.starts_with(&temporary) && root != temporary);
        std::fs::remove_dir_all(root).unwrap();
    }
}
fn text(path: &Path) -> JsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        JsString(path.as_os_str().encode_wide().collect())
    }
    #[cfg(not(windows))]
    {
        path.to_str().unwrap().into()
    }
}
fn directory(root: &Path, version: &JsString) -> PathBuf {
    root.join("specs")
        .join("native")
        .join(format!("codex-{}", version.to_utf8().unwrap()))
}
fn current_target() -> (JsString, JsString) {
    (
        match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            name => name,
        }
        .into(),
        match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "x86" => "ia32",
            name => name,
        }
        .into(),
    )
}

#[test]
fn native_catalog_selection_preserves_versions_names_and_read_only_fallbacks() {
    let fixture = Fixture::new();
    let root = fixture.0.join("space; quote' 日本語 e\u{301} 😀");
    let (platform, architecture) = current_target();
    for version in native_versions().unwrap() {
        let directory = directory(&root, version);
        let fallback = directory.join("catalog.json");
        assert_eq!(
            native_catalog_path(&text(&root), version, None, None).unwrap(),
            text(&fallback)
        );
        assert!(!directory.exists(), "Selection must not create files.");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&fallback, b"fallback bytes").unwrap();
        let selected = directory.join(format!(
            "catalog.{}-{}.json",
            platform.to_utf8().unwrap(),
            architecture.to_utf8().unwrap()
        ));
        std::fs::write(&selected, b"platform bytes").unwrap();
        for target in [(None, None), (Some(&platform), Some(&architecture))] {
            assert_eq!(
                native_catalog_path(&text(&root), version, target.0, target.1).unwrap(),
                text(&selected)
            );
        }
        // Catalog target names are independent of the owning OS's path syntax.
        let alternate = directory.join("catalog.other-cpu.json");
        assert_eq!(
            native_catalog_path(
                &text(&root),
                version,
                Some(&"other".into()),
                Some(&"cpu".into())
            )
            .unwrap(),
            text(&fallback)
        );
        std::fs::create_dir(&alternate).unwrap();
        assert_eq!(
            native_catalog_path(
                &text(&root),
                version,
                Some(&"other".into()),
                Some(&"cpu".into())
            )
            .unwrap(),
            text(&alternate)
        );
        assert_eq!(std::fs::read(selected).unwrap(), b"platform bytes");
        assert_eq!(std::fs::read(fallback).unwrap(), b"fallback bytes");
    }
}

#[test]
fn native_catalog_paths_keep_native_relative_parent_and_verbatim_roots() {
    let fixture = Fixture::new();
    let version = native_versions().unwrap().last().unwrap();
    let base = fixture.0.join("native");
    let child = base.join("child");
    let selected = directory(&base, version).join("catalog.other-cpu.json");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::create_dir_all(selected.parent().unwrap()).unwrap();
    std::fs::write(&selected, b"native parent").unwrap();
    for root in [
        base.clone(),
        child.join(".."),
        std::fs::canonicalize(&base).unwrap(),
    ] {
        let actual = native_catalog_path(
            &text(&root),
            version,
            Some(&"other".into()),
            Some(&"cpu".into()),
        )
        .unwrap();
        assert_eq!(
            actual,
            text(&directory(&root, version).join("catalog.other-cpu.json"))
        );
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let actual = PathBuf::from(std::ffi::OsString::from_wide(&actual.0));
            assert_eq!(std::fs::read(actual).unwrap(), b"native parent");
        }
    }
    for root in ["", ".", "relative space; quote'"] {
        assert_eq!(
            native_catalog_path(
                &root.into(),
                version,
                Some(&"other".into()),
                Some(&"cpu".into())
            )
            .unwrap(),
            text(&directory(Path::new(root), version).join("catalog.json"))
        );
    }
}

#[test]
fn invalid_catalog_selectors_keep_the_public_error_without_filesystem_effects() {
    let fixture = Fixture::new();
    let root = fixture.0.join("not-created");
    let version = native_versions().unwrap().last().unwrap();
    for value in [
        "",
        "UPPER",
        "x-y",
        "x_y",
        "x.y",
        "x/y",
        "x\\y",
        "x\n",
        "x\r",
        "x\u{2028}",
        " x",
        "x ",
        "é",
        "../..",
    ] {
        let value: JsString = value.into();
        for (version, platform, arch) in [
            (version, &value, &JsString::from("x64")),
            (version, &JsString::from("win32"), &value),
            (&value, &JsString::from("win32"), &JsString::from("x64")),
        ] {
            let error =
                native_catalog_path(&text(&root), version, Some(platform), Some(arch)).unwrap_err();
            assert_eq!(error.code, "native_version_unsupported");
            assert_eq!(
                error.message,
                "Supported native platform and version required."
            );
        }
    }
    assert!(!root.exists());
}

#[cfg(windows)]
#[test]
fn windows_catalog_selection_keeps_opaque_filenames_distinct_from_replacements() {
    use std::os::windows::ffi::OsStringExt;
    let fixture = Fixture::new();
    let version = native_versions().unwrap().last().unwrap();
    for unit in [0xd800, 0xdfff] {
        let original = fixture.0.join(std::ffi::OsString::from_wide(&[110, unit]));
        let replacement = fixture.0.join("n�");
        let alternate = directory(&replacement, version).join("catalog.other-cpu.json");
        std::fs::create_dir_all(alternate.parent().unwrap()).unwrap();
        std::fs::write(&alternate, b"replacement").unwrap();
        assert_eq!(
            native_catalog_path(
                &text(&original),
                version,
                Some(&"other".into()),
                Some(&"cpu".into())
            )
            .unwrap(),
            text(&directory(&original, version).join("catalog.json"))
        );
        assert!(
            !original.exists(),
            "A lossy alias must not select the replacement file."
        );
        let selected = directory(&original, version).join("catalog.other-cpu.json");
        std::fs::create_dir_all(selected.parent().unwrap()).unwrap();
        std::fs::write(&selected, b"opaque native").unwrap();
        let actual = native_catalog_path(
            &text(&original),
            version,
            Some(&"other".into()),
            Some(&"cpu".into()),
        )
        .unwrap();
        assert_eq!(actual, text(&selected));
        assert_eq!(
            std::fs::read(PathBuf::from(std::ffi::OsString::from_wide(&actual.0))).unwrap(),
            b"opaque native"
        );
        assert_eq!(std::fs::read(&alternate).unwrap(), b"replacement");
    }
}
