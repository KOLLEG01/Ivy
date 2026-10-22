use ivy_sdk::{JsString, collation::{LocaleComparator, LocaleRuntime}};
use std::{cmp::Ordering, path::Path, process::Command, sync::Arc};

fn runtime() -> Arc<LocaleRuntime> {
    let directory = std::env::var_os("IVY_TEST_LOCALE_RUNTIME")
        .expect("Provide the qualified packaged locale runtime with IVY_TEST_LOCALE_RUNTIME");
    LocaleRuntime::load(Path::new(&directory)).unwrap()
}

#[test]
fn missing_runtime_has_no_ambient_fallback() {
    assert!(LocaleRuntime::load(Path::new("relative-runtime")).is_err());
    let missing = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    assert!(LocaleRuntime::load(&missing).is_err());
}

#[test]
fn production_comparators_match_node_without_replacing_utf16() {
    let runtime = runtime();
    let default = runtime.executing_default().unwrap();
    let english = runtime.english().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node").arg("tests/fixtures/collation-oracle.mjs")
        .current_dir(root).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let mut count = 0;
    for line in output.stdout.split(|b| *b == b'\n').filter(|s| !s.is_empty()) {
        let (locale,left,right,expected): (String,Vec<u16>,Vec<u16>,i32) = serde_json::from_slice(line).unwrap();
        let comparator = if locale == "default" { &default } else { &english };
        let observed = comparator.compare(&JsString(left.clone()), &JsString(right.clone()));
        assert_eq!(observed, expected.cmp(&0), "locale={locale} {left:?} {right:?}");
        count += 1;
    }
    assert!(count > 320_000, "Missing differential rows: {count}");
}

#[test]
fn comparator_is_shared_retains_libraries_and_preserves_equal_keys() {
    fn shareable<T: Send + Sync>() {}
    shareable::<LocaleComparator>();
    let runtime = runtime();
    let comparator = Arc::new(runtime.executing_default().unwrap());
    drop(runtime);
    let mut values = ["a\u{200d}", "a", "a\u{034f}"].map(JsString::from);
    let original = values.clone();
    values.sort_by(|a,b| comparator.compare(a,b));
    assert_eq!(values, original);
    let children: Vec<_> = (0..8).map(|_| {
        let comparator = comparator.clone();
        std::thread::spawn(move || {
            for _ in 0..4096 {
                assert_eq!(comparator.compare(&JsString::from("é"), &JsString::from("e\u{301}")), Ordering::Equal);
                assert_eq!(comparator.compare(&JsString::from("a"), &JsString::from("z")), Ordering::Less);
            }
        })
    }).collect();
    drop(comparator);
    for child in children { child.join().unwrap(); }
}

#[cfg(windows)]
#[test]
fn long_native_runtime_path_child() {
    if std::env::var_os("IVY_TEST_LOCALE_LONG_PATH_CHILD").is_none() { return; }
    let comparator = runtime().executing_default().unwrap();
    assert_eq!(comparator.compare(&JsString::from("é"), &JsString::from("e\u{301}")), Ordering::Equal);
}

#[cfg(windows)]
#[test]
fn long_native_runtime_paths_load_in_a_fresh_process() {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let root = std::env::temp_dir().join(format!("ivy-locale-{}",uuid::Uuid::new_v4()));
    let mut units = "ä-界-😀-".encode_utf16().collect::<Vec<_>>();
    units.push(0xd800);
    units.extend("-runtime".encode_utf16());
    let directory = root.join("long-directory-".repeat(8)).join("long-directory-".repeat(8)).join(OsString::from_wide(&units));
    std::fs::create_dir_all(&directory).unwrap();
    let source = std::env::var_os("IVY_TEST_LOCALE_RUNTIME").unwrap();
    for name in ["icudt78.dll","icuuc78.dll","icuin78.dll"] {
        std::fs::copy(Path::new(&source).join(name),directory.join(name)).unwrap();
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact","long_native_runtime_path_child"])
        .env("IVY_TEST_LOCALE_LONG_PATH_CHILD","1")
        .env("IVY_TEST_LOCALE_RUNTIME",&directory).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(output.status.success(), "{} {}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
}
