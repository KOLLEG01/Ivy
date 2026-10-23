use super::*;
use ivy_host_runtime::{filesystem, native_path};
use std::os::windows::fs::OpenOptionsExt;

fn readonly(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions).unwrap();
}
fn native(root: &Path, seed: &str) -> Value {
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(seed).unwrap();
    let registry = Registry::default();
    let files = UiFiles::new(root);
    let owner = Uis::new(&registry, &files);
    let previous_journal = store.metadata("retention_last_failure").unwrap();
    let (result, cause) = store.capture_failure(|store| {
        let error = store
            .connection()
            .execute_batch("SELECT * FROM handled_before_recursive_remove")
            .unwrap_err();
        let _ = store.query_error(error);
        owner.collect_files(store, &|| 172800000.0)
    });
    let mut wire = Value::Null;
    let mut journal_error = Value::Null;
    if let Err(error) = result {
        assert_eq!(error.to_wire(), IvyError::internal().to_wire());
        wire = serde_json::from_str(&stringify(&error.to_wire(), usize::MAX).unwrap()).unwrap();
        assert!(!wire.to_string().contains(&root.display().to_string()));
        if let Err(error) =
            retention::record_failure(&store, &Clock, cause.as_ref().unwrap(), 172800000.0)
        {
            journal_error =
                serde_json::from_str(&stringify(&error.to_wire(), usize::MAX).unwrap()).unwrap();
        }
    } else {
        assert!(cause.is_none(), "Successful cleanup discards handled SQL");
    }
    json!({"message":cause.map(|v|v.to_utf8().unwrap()),"error":wire,
        "journalError":journal_error,"journal":store.metadata("retention_last_failure").unwrap(),"previousJournal":previous_journal})
}
fn compare_removal(location: &str, mode: &str, form: &str, seed: &str) {
    let root = Root::new();
    let private = match form {
        "unicode" => root.0.join("q'uöté-🙂"),
        "long" => root.0.join("x".repeat(150)).join("y".repeat(150)),
        _ => root.0.join("private"),
    };
    let context = native_path::Context::current().unwrap();
    let input = if form == "relative" {
        native_path::to_native(&context.relative(&context.cwd, &native_path::from_native(&private)))
    } else {
        private.clone()
    };
    let target = private.join(match location {
        "release" => "sample-ui/old",
        "incoming" => ".incoming/old",
        "staging" => ".staging/sample-ui/old",
        _ => unreachable!(),
    });
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("a-removed"), b"before").unwrap();
    fs::write(target.join("m-held"), b"held").unwrap();
    fs::write(target.join("z-retained"), b"after").unwrap();
    let external = root.0.join("external");
    if mode == "success" {
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("sentinel"), b"outside").unwrap();
        filesystem::directory_link(&external, &target.join("embedded-link")).unwrap();
        fs::create_dir(target.join("nested")).unwrap();
        fs::write(target.join("nested/value"), b"readonly").unwrap();
        readonly(&target.join("nested/value"));
        readonly(&target.join("nested"));
    }
    if mode == "readonly-held" {
        readonly(&target.join("m-held"));
    }
    filesystem::set_times_ms(&target, 0, 0).unwrap();
    let held = match mode {
        "success" => None,
        "directory-held" => Some(
            fs::OpenOptions::new()
                .read(true)
                .share_mode(3)
                .custom_flags(0x02000000)
                .open(&target)
                .unwrap(),
        ),
        _ => Some(
            fs::OpenOptions::new()
                .read(true)
                .share_mode(3)
                .open(target.join("m-held"))
                .unwrap(),
        ),
    };
    let expected_cause = held
        .as_ref()
        .map(|_| std::io::Error::from_raw_os_error(32).to_string());
    let actual = native(&input, seed);
    drop(held);
    if mode == "readonly-held" {
        assert!(
            fs::metadata(target.join("m-held"))
                .unwrap()
                .permissions()
                .readonly()
        );
    }
    if mode == "success" {
        assert!(!target.exists());
        assert_eq!(fs::read(external.join("sentinel")).unwrap(), b"outside");
        assert!(actual["error"].is_null());
    } else {
        assert_eq!(actual["error"]["data"]["code"], "internal_error");
        if mode == "directory-held" {
            assert!(target.is_dir());
        } else {
            assert_eq!(fs::read(target.join("m-held")).unwrap(), b"held");
        }
        // Native recursive removal may leave partial effects; sibling traversal order is not prescribed.
        assert_eq!(
            actual["message"].as_str().unwrap(),
            expected_cause.unwrap(),
            "{location}/{mode}/{form}"
        );
        if seed.is_empty() {
            assert!(actual["journalError"].is_null());
            let journal: Value = serde_json::from_str(actual["journal"].as_str().unwrap()).unwrap();
            assert_eq!(journal["message"], actual["message"]);
        } else {
            assert_eq!(actual["journalError"]["data"]["code"], "internal_error");
            assert_eq!(actual["journal"], actual["previousJournal"]);
        }
    }
}

#[test]
fn ui_recursive_remove_native_causes_journal_and_partial_effects() {
    for location in ["release", "incoming", "staging"] {
        compare_removal(location, "partial", "absolute", "");
    }
    compare_removal("release", "directory-held", "absolute", "");
    compare_removal("incoming", "readonly-held", "absolute", "");
    compare_removal("staging", "partial", "namespace", "");
    compare_removal("release", "partial", "relative", "");
    compare_removal("release", "partial", "long", "");
    compare_removal(
        "release",
        "partial",
        "absolute",
        "CREATE TRIGGER reject_journal BEFORE INSERT ON metadata WHEN NEW.key='retention_last_failure' BEGIN SELECT RAISE(ABORT,'private journal failure'); END;",
    );
}
#[test]
fn ui_recursive_remove_readonly_and_embedded_links_preserve_external_targets() {
    for location in ["release", "incoming", "staging"] {
        compare_removal(location, "success", "absolute", "");
    }
}
#[test]
fn ui_recursive_remove_unicode_failures_replace_stale_sql_with_native_causes() {
    for location in ["release", "incoming"] {
        compare_removal(location, "partial", "unicode", "");
    }
}
