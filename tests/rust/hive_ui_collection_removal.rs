use super::*;
use ivy_host_runtime::{filesystem, node_path};
use std::os::windows::fs::OpenOptionsExt;

fn snapshot(root: &Path) -> Value {
    fn walk(path: &Path, name: String, entries: &mut Vec<Value>) {
        let physical = node_path::filesystem_path(&node_path::from_native(path));
        let metadata = fs::symlink_metadata(&physical).unwrap();
        let kind = metadata.file_type();
        entries.push(json!({
            "path":name,
            "kind":if kind.is_symlink(){"link"}else if kind.is_dir(){"directory"}else{"file"},
            "readonly":metadata.permissions().readonly()
        }));
        if kind.is_dir() && !kind.is_symlink() {
            for entry in fs::read_dir(physical).unwrap() {
                let entry = entry.unwrap();
                let child = entry.file_name().into_string().unwrap();
                walk(&entry.path(), format!("{name}/{child}"), entries);
            }
        }
    }
    let mut entries = Vec::new();
    walk(root, String::new(), &mut entries);
    entries.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    json!(entries)
}
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
        wire = serde_json::from_str(&stringify(&error.to_wire(), usize::MAX).unwrap()).unwrap();
        assert!(!wire.to_string().contains(&root.display().to_string()));
        if let Err(error) = retention::record_failure(
            &store,
            &Clock,
            &cause.clone().unwrap_or_else(|| error.message_js()),
            172800000.0,
        ) {
            journal_error =
                serde_json::from_str(&stringify(&error.to_wire(), usize::MAX).unwrap()).unwrap();
        }
    } else {
        assert!(
            cause.is_none(),
            "Successful cleanup must discard handled SQL"
        );
    }
    json!({
        "message":cause.map(|v|v.to_utf8().unwrap()),
        "error":wire,
        "journalError":journal_error,
        "journal":store.metadata("retention_last_failure").unwrap()
    })
}
fn compare_removal(location: &str, mode: &str, form: &str, seed: &str) {
    let root = Root::new();
    let private = match form {
        "unicode" => root.0.join("q'uöté-🙂"),
        "long" => root.0.join("x".repeat(150)).join("y".repeat(150)),
        _ => root.0.join("private"),
    };
    let context = node_path::Context::current().unwrap();
    let input = match form {
        "namespace" => context.filesystem_path(&node_path::from_native(&private)),
        "relative" => {
            node_path::to_native(&context.relative(&context.cwd, &node_path::from_native(&private)))
        }
        _ => private.clone(),
    };
    let target = private.join(match location {
        "release" => "sample-ui/old",
        "incoming" => ".incoming/old",
        "staging" => ".staging/sample-ui/old",
        _ => panic!("Unknown cleanup location"),
    });
    let mut outcomes = Vec::new();
    for original_run in [false, true] {
        // Reset only this disposable fixture after its guards have been dropped.
        assert!(private.is_absolute() && private.starts_with(&root.0));
        if private.exists() {
            fixture(
                &root.0,
                &json!([{"kind":"remove","path":private.strip_prefix(&root.0).unwrap()}]),
            );
        }
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
        fixture(
            &root.0,
            &json!([{"kind":"mtime","path":target.strip_prefix(&root.0).unwrap(),"ms":0}]),
        );
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
        let value = if original_run {
            let result = original_output(&input, seed);
            if result.status.success() {
                serde_json::from_slice(&result.stdout).unwrap()
            } else {
                json!({"processFailure":{
                    "exitCode":result.status.code(),
                    "stdoutBytes":result.stdout,
                    "stderrBytes":result.stderr
                }})
            }
        } else {
            native(&input, seed)
        };
        let effects = snapshot(&root.0);
        drop(held);
        outcomes.push((value, effects));
    }
    let ((actual, effects), (expected, original_effects)) = (&outcomes[0], &outcomes[1]);
    let case = format!("recursive-{location}-{mode}-{form}");
    record(
        &case,
        &input,
        &json!({"value":expected,"effects":original_effects}),
        &json!({"value":actual,"effects":effects,"causeMapped":form!="unicode"}),
    );
    assert_eq!(effects, original_effects, "{case} partial effects");
    if form == "unicode" {
        assert!(actual["message"].is_null(), "Do not reuse handled SQL");
        assert_eq!(actual["error"]["code"], -32000);
        assert_eq!(actual["error"]["data"]["code"], "internal_error");
        if let Some(failure) = expected.get("processFailure") {
            // Preserve the unavailable original cause as evidence; do not make
            // the native owner emulate the original subprocess termination.
            assert_eq!(failure["exitCode"], -1073740791i64);
            assert_eq!(failure["stdoutBytes"], json!([]));
            assert_eq!(failure["stderrBytes"], json!([]));
        } else {
            assert_eq!(actual["error"], expected["error"]);
        }
    } else {
        assert_eq!(actual, expected, "{case} cause, wire and failure journal");
    }
    if mode == "success" {
        assert!(!target.exists());
        assert_eq!(
            fs::read(root.0.join("external/sentinel")).unwrap(),
            b"outside"
        );
        assert!(actual["error"].is_null());
    } else {
        assert!(target.join("m-held").exists());
        assert!(target.join("z-retained").exists());
        assert_eq!(target.join("a-removed").exists(), mode == "directory-held");
        if form != "unicode" {
            assert!(
                actual["message"]
                    .as_str()
                    .unwrap()
                    .starts_with("EPERM, Permission denied:")
            );
        }
        if form == "long" {
            assert!(actual["message"].as_str().unwrap().len() > 512);
            let journal: Value = serde_json::from_str(actual["journal"].as_str().unwrap()).unwrap();
            assert_eq!(
                journal["message"].as_str().unwrap().encode_utf16().count(),
                512
            );
        }
    }
}

#[test]
fn ui_recursive_remove_causes_journal_and_partial_effects_match_original() {
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
fn ui_recursive_remove_unicode_failure_is_explicitly_unmapped_without_stale_sql() {
    for location in ["release", "incoming"] {
        compare_removal(location, "partial", "unicode", "");
    }
}
