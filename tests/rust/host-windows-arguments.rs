#[path = "../../packages/host-runtime/rust/src/platform/windows_arguments.rs"]
mod windows_arguments;
use windows_arguments::{quote_argument, valid_job_name};

#[test]
fn crt_arguments_preserve_quotes_trailing_slashes_and_utf16() {
    for (raw, expected) in [("", "\"\""), ("a b", "\"a b\""),
        ("a\"b", "\"a\\\"b\""), ("C:\\tail\\", "\"C:\\tail\\\\\""),
        ("slash\\\"quote", "\"slash\\\\\\\"quote\"")] {
        assert_eq!(String::from_utf16(&quote_argument(&raw.encode_utf16().collect::<Vec<_>>())).unwrap(), expected);
    }
    assert_eq!(quote_argument(&[0xd800, 0x61, 0xdc00]), [34, 0xd800, 0x61, 0xdc00, 34]);
}

#[test]
fn named_job_has_one_exact_bounded_namespace() {
    assert!(valid_job_name("Global\\Ivy.worker-01_a.b"));
    assert!(valid_job_name(&format!("Global\\Ivy.{}", "a".repeat(128))));
    for name in ["Global\\Ivy.", "Local\\Ivy.a", "Global\\Ivy.a/b", "Global\\Ivy.ä", "Global\\Ivy.a\n"] {
        assert!(!valid_job_name(name));
    }
    assert!(!valid_job_name(&format!("Global\\Ivy.{}", "a".repeat(129))));
}
