use super::LocaleRuntime;
use crate::JsString;
use std::{
    ffi::{CStr, CString, c_char},
    path::Path,
    process::Command,
};

fn configured_runtime() -> std::sync::Arc<LocaleRuntime> {
    let path = std::env::var_os("IVY_TEST_LOCALE_RUNTIME").unwrap();
    LocaleRuntime::load(Path::new(&path)).unwrap()
}

fn set_default(runtime: &LocaleRuntime, locale: &CStr) {
    type SetDefault = unsafe extern "C" fn(*const c_char, *mut i32);
    let symbol = runtime._libraries[1].symbol(c"uloc_setDefault_78").unwrap();
    let set: SetDefault = unsafe { std::mem::transmute(symbol) };
    let mut status = 0;
    unsafe {
        set(locale.as_ptr(), &mut status);
    }
    assert!(status <= 0, "ICU default locale setup failed: {status}");
}

#[test]
fn default_locale_lowercase_preserves_context_utf16_and_captured_locale() {
    for locale in ["runtime", "en-US", "tr", "az", "lt", "el"] {
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "collation::casing_tests::default_locale_lowercase_child",
                "--nocapture",
            ])
            .env("IVY_TEST_LOCALE_CASE_CHILD", locale)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "locale={locale}\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn default_locale_lowercase_child() {
    let Ok(locale) = std::env::var("IVY_TEST_LOCALE_CASE_CHILD") else {
        return;
    };
    let runtime = configured_runtime();
    if locale != "runtime" {
        set_default(&runtime, &CString::new(locale.as_str()).unwrap());
    }
    let selected = runtime.executing_default().unwrap();
    let english = runtime.english().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut command = Command::new("node");
    command.args(["tests/fixtures/collation-oracle.mjs", "--lowercase"]);
    if locale != "runtime" {
        command.arg(&locale);
    }
    let output = command.current_dir(root).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Changing the process default later must not change an existing owner.
    set_default(&runtime, if locale == "tr" { c"en" } else { c"tr" });
    drop(runtime);
    let mut count = 0;
    for line in output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let (input, expected): (Vec<u16>, Vec<u16>) = serde_json::from_slice(line).unwrap();
        let value = JsString(input);
        let before = value.clone();
        assert_eq!(
            selected.lowercase(&value).unwrap(),
            JsString(expected),
            "locale={locale} input={value:?}"
        );
        assert_eq!(value, before);
        count += 1;
    }
    assert!(count > 180);
    assert_eq!(
        english.lowercase(&JsString::from("Iİ")).unwrap(),
        JsString::from("ii\u{307}")
    );
}
