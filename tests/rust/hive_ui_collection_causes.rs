use super::*;
use ivy_hive::{
    registry::Registry,
    retention::{self, Runtime},
    uis::Uis,
};
use std::{
    cmp::Ordering,
    io::Write,
    process::{Command, Stdio},
    sync::Mutex,
};

static CORPUS: Mutex<()> = Mutex::new(());
#[cfg(windows)]
#[path = "hive_ui_collection_removal.rs"]
mod removal;
struct Clock;
impl Runtime for Clock {
    fn parse_date(&self, _: &JsString) -> f64 {
        panic!("No date parsing in failure journal")
    }
    fn iso_date(&self, value: f64) -> IvyResult<JsString> {
        assert_eq!(value, 172800000.0);
        Ok("1970-01-03T00:00:00.000Z".into())
    }
    fn compare_contract_keys(&self, _: &JsString, _: &JsString) -> Ordering {
        panic!("No locale sorting in failure journal")
    }
}
fn original_output(root: &Path, seed: &str) -> std::process::Output {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tests/fixtures/hive-ui-collection-cause-oracle.mjs"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json!({"root":root,"seedSql":seed}).to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
fn original(root: &Path, seed: &str) -> Value {
    let result = original_output(root, seed);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}
fn record(case: &str, root: &Path, original: &Value, native: &Value) {
    if let Some(path) = std::env::var_os("IVY_HIVE_UI_COLLECTION_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            json!({"case":case,"root":root,"original":original,"native":native})
        )
        .unwrap();
    }
}
fn compare(case: &str, root: &Path, seed: &str, mapped: bool) {
    let expected = original(root, seed);
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(seed).unwrap();
    let registry = Registry::default();
    let files = UiFiles::new(root);
    let owner = Uis::new(&registry, &files);
    let (result, cause) = store.capture_failure(|store| {
        // A later filesystem cause must replace an earlier handled SQL failure.
        let error = store
            .connection()
            .execute_batch("SELECT * FROM handled_before_files")
            .unwrap_err();
        let _ = store.query_error(error);
        owner.collect_files(store, &|| 172800000.0)
    });
    let error = result.unwrap_err();
    let wire =
        serde_json::from_str::<Value>(&stringify(&error.to_wire(), usize::MAX).unwrap()).unwrap();
    assert_eq!(wire, expected["error"], "{case} wire");
    assert!(!wire.to_string().contains(&root.display().to_string()));
    if mapped {
        assert_eq!(
            cause.as_ref().unwrap().to_utf8().unwrap(),
            expected["message"].as_str().unwrap(),
            "{case} cause"
        );
    } else {
        assert!(
            cause.is_none(),
            "Unqualified recursive rm cannot reuse SQL or libuv text"
        );
    }
    let journal_result = retention::record_failure(
        &store,
        &Clock,
        &cause.clone().unwrap_or_else(|| error.message_js()),
        172800000.0,
    );
    let journal_error = journal_result.err().map(|e| {
        serde_json::from_str::<Value>(&stringify(&e.to_wire(), usize::MAX).unwrap()).unwrap()
    });
    if mapped {
        assert_eq!(
            journal_error,
            expected["journalError"]
                .as_object()
                .map(|_| expected["journalError"].clone())
        );
        assert_eq!(
            store
                .metadata("retention_last_failure")
                .unwrap()
                .map(Value::String)
                .unwrap_or(Value::Null),
            expected["journal"],
            "{case} journal"
        );
    }
    let actual = json!({"cause":cause.map(|c|c.to_utf8().unwrap()),"error":wire,"journalError":journal_error,"journal":store.metadata("retention_last_failure").unwrap(),"mapped":mapped});
    record(case, root, &expected, &actual);
}

#[test]
fn ui_collection_private_scandir_causes_and_journal_match_original_without_wire_disclosure() {
    let root = Root::new();
    let private = root.0.join("é-'quote-🙂");
    fs::write(&private, b"root is a file").unwrap();
    compare("root-file-scandir", &private, "", true);
    let seed = "CREATE TRIGGER reject_journal BEFORE INSERT ON metadata WHEN NEW.key='retention_last_failure' BEGIN SELECT RAISE(ABORT,'private journal failure'); END;";
    compare("failure-journal-can-supersede-io", &private, seed, true);
    fs::remove_file(&private).unwrap();
    fs::create_dir(&private).unwrap();
    let incoming = private.join(".incoming");
    fs::write(&incoming, b"not a directory").unwrap();
    compare("incoming-scandir-after-release-cleanup", &private, "", true);
    fs::remove_file(&incoming).unwrap();
    fs::write(private.join(".staging"), b"not a directory").unwrap();
    compare("staging-scandir-after-incoming", &private, "", true);
    fs::remove_file(private.join(".staging")).unwrap();
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    let registry = Registry::default();
    let files = UiFiles::new(&private);
    let owner = Uis::new(&registry, &files);
    let (result, cause) =
        store.capture_failure(|store| owner.collect_files(store, &|| 172800000.0));
    result.unwrap();
    assert!(cause.is_none());
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    let (result, cause) = store.capture_failure(|store| {
        store.connection().execute_batch("DROP TABLE apps").unwrap();
        owner.collect_files(store, &|| 172800000.0)
    });
    assert!(result.is_err());
    assert_eq!(cause.unwrap().to_utf8().unwrap(), "no such table: apps");
}

#[cfg(windows)]
#[test]
fn ui_collection_actual_windows_acl_rmdir_and_recursive_causes_preserve_boundaries() {
    use std::os::windows::fs::OpenOptionsExt;
    struct Denied(PathBuf);
    impl Denied {
        fn command(&self, args: &[&str]) {
            let output = Command::new(
                PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/icacls.exe"),
            )
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
            assert!(
                output.status.success(),
                "Owned disposable ACL operation failed"
            );
        }
    }
    impl Drop for Denied {
        fn drop(&mut self) {
            self.command(&["/remove:d", "*S-1-1-0"]);
        }
    }
    let root = Root::new();
    let private = root.0.join("private");
    fs::create_dir(&private).unwrap();
    {
        let denied = Denied(private.clone());
        denied.command(&["/deny", "*S-1-1-0:(RD)"]);
        compare("root-acl-scandir", &private, "", true);
    }
    for (case, directory) in [
        ("ui-rmdir", private.join("sample-ui")),
        ("staging-rmdir", private.join(".staging/sample-ui")),
    ] {
        fs::create_dir_all(&directory).unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .custom_flags(0x02000000)
            .open(&directory)
            .unwrap();
        compare(case, &private, "", true);
        drop(held);
        fs::remove_dir(&directory).unwrap();
    }
    let old = private.join("sample-ui/old");
    fs::create_dir_all(&old).unwrap();
    let child = old.join("locked");
    fs::write(&child, b"held child").unwrap();
    fixture(
        &root.0,
        &json!([{"kind":"mtime","path":"private/sample-ui/old","ms":0}]),
    );
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&child)
        .unwrap();
    compare("recursive-rm-private-cause", &private, "", true);
    drop(held);
    assert!(child.exists());
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    let registry = Registry::default();
    let files = UiFiles::new(&private);
    let owner = Uis::new(&registry, &files);
    let (result, cause) =
        store.capture_failure(|store| owner.collect_files(store, &|| 172800000.0));
    result.unwrap();
    assert!(cause.is_none());
    assert!(!old.exists());
}
