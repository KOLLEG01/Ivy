use ivy_contracts::{IvyError, IvyResult, JsString, json::stringify};
use ivy_hive::{
    registry::Registry,
    retention::{self, Runtime},
    store::Store,
    ui_files::UiFiles,
    uis::Uis,
};
use serde_json::{Value, json};
use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
};

#[cfg(windows)]
#[path = "hive_ui_collection_removal.rs"]
mod removal;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-ui-collection-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = ivy_host_runtime::filesystem::remove_force_sync(&self.0);
    }
}
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
fn compare(case: &str, root: &Path, seed: &str, expected_cause: &str) {
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(seed).unwrap();
    let registry = Registry::default();
    let files = UiFiles::new(root);
    let owner = Uis::new(&registry, &files);
    let (result, cause) = store.capture_failure(|store| {
        let error = store
            .connection()
            .execute_batch("SELECT * FROM handled_before_files")
            .unwrap_err();
        let _ = store.query_error(error);
        owner.collect_files(store, &|| 172800000.0)
    });
    let error = result.unwrap_err();
    assert_eq!(
        error.to_wire(),
        IvyError::internal().to_wire(),
        "{case} public error"
    );
    let wire = stringify(&error.to_wire(), usize::MAX).unwrap();
    assert!(
        !wire.contains(&root.display().to_string()),
        "{case} disclosure"
    );
    assert_eq!(
        cause.as_ref().unwrap().to_utf8().unwrap(),
        expected_cause,
        "{case} native cause replaces SQL"
    );
    let previous_journal = store.metadata("retention_last_failure").unwrap();
    let journal = retention::record_failure(&store, &Clock, cause.as_ref().unwrap(), 172800000.0);
    if seed.is_empty() {
        journal.unwrap();
        let saved: Value =
            serde_json::from_str(&store.metadata("retention_last_failure").unwrap().unwrap())
                .unwrap();
        assert_eq!(saved["message"], expected_cause, "{case} journal");
    } else {
        assert!(journal.is_err(), "{case} refused journal write");
        assert_eq!(
            store.metadata("retention_last_failure").unwrap(),
            previous_journal
        );
    }
}

#[test]
fn ui_collection_native_scandir_causes_and_journal_keep_public_errors_private() {
    let root = Root::new();
    let private = root.0.join("é-'quote-🙂");
    fs::write(&private, b"root is a file").unwrap();
    let expected = fs::read_dir(&private).unwrap_err().to_string();
    compare("root-file-scandir", &private, "", &expected);
    compare(
        "failure-journal-can-fail",
        &private,
        "CREATE TRIGGER reject_journal BEFORE INSERT ON metadata WHEN NEW.key='retention_last_failure' BEGIN SELECT RAISE(ABORT,'private journal failure'); END;",
        &expected,
    );
    fs::remove_file(&private).unwrap();
    fs::create_dir(&private).unwrap();
    for name in [".incoming", ".staging"] {
        let path = private.join(name);
        fs::write(&path, b"not a directory").unwrap();
        compare(
            name,
            &private,
            "",
            &fs::read_dir(&path).unwrap_err().to_string(),
        );
        fs::remove_file(path).unwrap();
    }
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

    let ordered = Root::new();
    let files = UiFiles::new(ordered.0.join("private"));
    for ui in ["z", "a"] {
        let release = files.root.join(ui).join("r1");
        fs::create_dir_all(&release).unwrap();
        fs::write(release.join("payload"), ui).unwrap();
        ivy_host_runtime::filesystem::set_times_ms(&release, 86400000, 86400000).unwrap();
    }
    let incoming = files.root.join(".incoming");
    let boundary = incoming.join("opaque\u{fffd}");
    fs::create_dir_all(&boundary).unwrap();
    fs::write(boundary.join("payload"), b"retained replacement").unwrap();
    ivy_host_runtime::filesystem::set_times_ms(&boundary, 86400000, 86400000).unwrap();
    #[cfg(windows)]
    let opaque = {
        use std::os::windows::ffi::OsStringExt;
        [0xd800, 0xdfff].map(|unit| {
            let name = std::ffi::OsString::from_wide(&[111, 112, 97, 113, 117, 101, unit]);
            let path = incoming.join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("payload"), b"expired native name").unwrap();
            ivy_host_runtime::filesystem::set_times_ms(&path, 0, 0).unwrap();
            path
        })
    };
    let calls = std::cell::Cell::new(0);
    files
        .collect_all(&Default::default(), &|| {
            let index = calls.get();
            calls.set(index + 1);
            [172799999.0, 172800001.0, 172800000.0][index]
        })
        .unwrap();
    assert_eq!(calls.get(), 3);
    assert_eq!(fs::read(files.root.join("a/r1/payload")).unwrap(), b"a");
    assert!(!files.root.join("z").exists());
    assert_eq!(
        fs::read(boundary.join("payload")).unwrap(),
        b"retained replacement"
    );
    #[cfg(windows)]
    for path in opaque {
        assert!(!path.exists());
    }
}

#[cfg(windows)]
#[test]
fn ui_collection_native_acl_and_sharing_errors_preserve_journal_boundaries() {
    use std::os::windows::fs::OpenOptionsExt;
    struct Denied(PathBuf);
    impl Denied {
        fn command(&self, args: &[&str]) {
            let output = std::process::Command::new(
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
        match fs::read_dir(&private) {
            Err(error) => compare("root-acl-scandir", &private, "", &error.to_string()),
            Ok(entries) => {
                // Access depends on the effective Windows token. When the OS
                // grants enumeration, collection must succeed without inventing
                // an access error or retaining an earlier handled SQL failure.
                drop(entries);
                let mut store = Store::open(Path::new(":memory:")).unwrap();
                let previous_journal = store.metadata("retention_last_failure").unwrap();
                let registry = Registry::default();
                let files = UiFiles::new(&private);
                let owner = Uis::new(&registry, &files);
                let (result, cause) = store.capture_failure(|store| {
                    let error = store
                        .connection()
                        .execute_batch("SELECT * FROM handled_before_acl")
                        .unwrap_err();
                    let _ = store.query_error(error);
                    owner.collect_files(store, &|| 172800000.0)
                });
                result.unwrap();
                assert!(cause.is_none());
                assert_eq!(
                    store.metadata("retention_last_failure").unwrap(),
                    previous_journal
                );
            }
        }
    }
    for directory in [
        private.join("sample-ui"),
        private.join(".staging/sample-ui"),
    ] {
        fs::create_dir_all(&directory).unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .custom_flags(0x02000000)
            .open(&directory)
            .unwrap();
        compare(
            "directory-sharing",
            &private,
            "",
            &fs::remove_dir(&directory).unwrap_err().to_string(),
        );
        drop(held);
        fs::remove_dir(&directory).unwrap();
    }
    let old = private.join("sample-ui/old");
    fs::create_dir_all(&old).unwrap();
    let child = old.join("locked");
    fs::write(&child, b"held child").unwrap();
    ivy_host_runtime::filesystem::set_times_ms(&old, 0, 0).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&child)
        .unwrap();
    compare(
        "recursive-sharing",
        &private,
        "",
        &fs::remove_file(&child).unwrap_err().to_string(),
    );
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
