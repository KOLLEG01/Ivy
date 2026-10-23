#[path = "../../packages/host-runtime/rust/src/platform/windows_arguments.rs"]
mod windows_arguments;
use windows_arguments::{quote_argument, valid_job_name};

#[test]
fn crt_arguments_preserve_quotes_trailing_slashes_and_utf16() {
    for (raw, expected) in [
        ("", "\"\""),
        ("plain", "plain"),
        ("/d", "/d"),
        ("/c", "/c"),
        ("a b", "\"a b\""),
        ("a\tb", "\"a\tb\""),
        ("a\"b", "\"a\\\"b\""),
        ("C:\\tail\\", "C:\\tail\\"),
        ("C:\\space dir\\", "\"C:\\space dir\\\\\""),
        ("slash\\\"quote", "\"slash\\\\\\\"quote\""),
    ] {
        assert_eq!(
            String::from_utf16(&quote_argument(&raw.encode_utf16().collect::<Vec<_>>())).unwrap(),
            expected
        );
    }
    assert_eq!(
        quote_argument(&[0xd800, 0x61, 0xdc00]),
        [0xd800, 0x61, 0xdc00]
    );
    assert_eq!(
        quote_argument(&[0xd800, 32, 0xdc00]),
        [34, 0xd800, 32, 0xdc00, 34]
    );
}

#[test]
fn named_job_has_one_exact_bounded_namespace() {
    assert!(valid_job_name("Global\\Ivy.worker-01_a.b"));
    assert!(valid_job_name(&format!("Global\\Ivy.{}", "a".repeat(128))));
    for name in [
        "Global\\Ivy.",
        "Local\\Ivy.a",
        "Global\\Ivy.a/b",
        "Global\\Ivy.ä",
        "Global\\Ivy.a\n",
    ] {
        assert!(!valid_job_name(name));
    }
    assert!(!valid_job_name(&format!("Global\\Ivy.{}", "a".repeat(129))));
}

#[cfg(windows)]
#[test]
fn application_filename_preserves_extended_only_names_and_utf16() {
    use std::{
        ffi::OsString,
        fs,
        os::windows::ffi::{OsStrExt, OsStringExt},
    };
    use windows_arguments::application_path;
    let root = std::env::temp_dir().join(format!("ivy-application-path-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(&root).unwrap();
    let file = root.join("ordinary.exe");
    fs::write(&file, b"ordinary").unwrap();
    let selected = fs::canonicalize(&file).unwrap();
    let ordinary = application_path(selected.as_os_str());
    assert!(
        !ordinary
            .encode_wide()
            .collect::<Vec<_>>()
            .starts_with(&[92, 92, 63, 92])
    );
    assert_eq!(fs::canonicalize(&ordinary).unwrap(), selected);
    let trailing = root.join("ordinary.exe ");
    fs::write(&trailing, b"distinct extended filename").unwrap();
    assert_eq!(application_path(trailing.as_os_str()), trailing.as_os_str());
    let unicode = root.join(OsString::from_wide(&[0x61, 0xd800, 0x62]));
    fs::write(&unicode, b"utf16").unwrap();
    let ordinary = application_path(unicode.as_os_str());
    assert!(
        ordinary
            .encode_wide()
            .collect::<Vec<_>>()
            .ends_with(&[0x61, 0xd800, 0x62])
    );
    assert_eq!(
        fs::canonicalize(&ordinary).unwrap(),
        fs::canonicalize(&unicode).unwrap()
    );
    let long_root = root.join("long".repeat(30)).join("nested".repeat(20));
    fs::create_dir_all(&long_root).unwrap();
    let long = long_root.join("program.exe");
    fs::write(&long, b"extended path").unwrap();
    assert!(long.as_os_str().encode_wide().count() > 260);
    assert_eq!(application_path(long.as_os_str()), long.as_os_str());
    fs::remove_dir_all(&root).unwrap();
}
