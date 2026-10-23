// Exercise the actual public Host module and file mapper against the original.
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    hash::digest,
    json::{canonical, parse, stringify},
};
use ivy_host_runtime::{native_restore, node_path, paths};
use ivy_store::{
    rusqlite::Connection,
    sqlite::{self, OpenOptions},
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let parent = paths::realpath(&std::env::temp_dir()).unwrap();
        let path = parent.join(format!("ivy-agent-native-restore-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        assert!(paths::inside(&parent, &paths::realpath(&path).unwrap()).unwrap());
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("Retained copied-database fixture: {}", self.0.display());
            return;
        }
        let actual = paths::realpath(&self.0).unwrap();
        let parent = paths::realpath(&std::env::temp_dir()).unwrap();
        assert!(
            paths::inside(&parent, &actual).unwrap()
                && actual
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("ivy-agent-native-restore-")
        );
        fs::remove_dir_all(actual).unwrap();
    }
}
fn value<'a>(v: &'a IvyJson, key: &str) -> &'a IvyJson {
    v.required(key).unwrap()
}
fn string(v: &IvyJson, key: &str) -> String {
    value(v, key).as_str().unwrap()
}
fn same(a: &IvyJson, b: &IvyJson, name: &str) {
    assert_eq!(
        canonical(a, usize::MAX).unwrap(),
        canonical(b, usize::MAX).unwrap(),
        "{name}"
    );
}
fn node(root: &Path, action: &str) -> IvyJson {
    let mut child = Command::new("node")
        .current_dir(env!("CARGO_MANIFEST_DIR").to_owned() + "/../../..")
        .arg("tests/fixtures/agent-native-restore-oracle.mjs")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = IvyJson::object([
        ("root", IvyJson::String(node_path::from_native(root))),
        ("action", action.into()),
    ]);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(&request, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{action}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    parse(&String::from_utf8(result.stdout).unwrap(), usize::MAX).unwrap()
}
fn wire(error: IvyError) -> IvyJson {
    IvyJson::object([("ok", false.into()), ("error", error.to_wire())])
}

#[test]
fn copied_sqlite_transactions_match_original_layout_digest_and_failure_boundaries() {
    let root = Root::new();
    let cases = node(&root.0, "prepare");
    let expected = node(&root.0, "original");
    let positive: Vec<_> = expected
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| value(value(case, "reply"), "ok").as_bool() == Some(true))
        .map(|case| string(case, "name"))
        .collect();
    assert_eq!(
        positive,
        [
            "0.154.0",
            "0.158.0",
            "0.159.2",
            "optional-outside",
            "typed-values",
            "invalid-utf8"
        ]
    );
    node(&root.0, "reset");
    let mut replies = Vec::new();
    for case in cases.as_array().unwrap() {
        let name = string(case, "name");
        let home = PathBuf::from(string(case, "newHome"));
        let database = home.join("state_5.sqlite");
        if name.starts_with("0.") {
            assert!(
                value(value(case, "manifest"), "files")
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|file| string(file, "path").ends_with("/state_5.sqlite-wal")
                        && value(file, "bytes").as_f64().unwrap() > 0.0)
            );
        }
        assert!(paths::inside(&root.0, &paths::realpath(&database).unwrap()).unwrap());
        assert!(
            !fs::symlink_metadata(&database)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let mut reader = None;
        if name == "checkpoint-busy" {
            let db = Connection::open(&database).unwrap();
            db.execute_batch("BEGIN; SELECT count(*) FROM threads")
                .unwrap();
            reader = Some(db);
        }
        let db = sqlite::open(
            &database,
            OpenOptions {
                busy_timeout: Duration::ZERO,
                read_only: false,
            },
        )
        .unwrap();
        let files: BTreeMap<_, _> = value(value(case, "manifest"), "files")
            .as_array()
            .unwrap()
            .iter()
            .map(|file| {
                (
                    value(file, "path").as_js_string().unwrap().clone(),
                    file.clone(),
                )
            })
            .collect();
        let context = node_path::Context::current().unwrap();
        let result = native_restore::relocate_database(
            db,
            value(case, "layout"),
            |raw: &JsString, scope| {
                native_restore::mapped_file(
                    raw,
                    scope,
                    value(case, "oldHome").as_js_string().unwrap(),
                    value(case, "newHome").as_js_string().unwrap(),
                    value(case, "prefix").as_js_string().unwrap(),
                    &files,
                    &context,
                )
            },
        );
        let reply = match result {
            Ok(result) => {
                fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&database)
                    .unwrap()
                    .sync_all()
                    .unwrap();
                let hash = digest(fs::read(&database).unwrap());
                IvyJson::object([
                    ("ok", true.into()),
                    (
                        "value",
                        IvyJson::object([
                            ("logicalDataHash", result.logical_data_hash.into()),
                            ("changes", IvyJson::Array(result.changes)),
                            ("workingHash", hash.into()),
                        ]),
                    ),
                ])
            }
            Err(error) => wire(error),
        };
        let original = expected
            .as_array()
            .unwrap()
            .iter()
            .find(|row| string(row, "name") == name)
            .unwrap();
        let mut wanted = value(original, "reply").clone();
        let mut comparable = reply.clone();
        // SQLite engine versions may write different physical headers/pages.
        // Each reported workingHash is separately checked against its own bytes.
        if value(&wanted, "ok").as_bool() == Some(true) {
            let left = comparable.get("value").unwrap().clone();
            let right = wanted.get("value").unwrap().clone();
            let mut left = left;
            let mut right = right;
            left.remove("workingHash");
            right.remove("workingHash");
            comparable.insert("value", left);
            wanted.insert("value", right);
        }
        same(&comparable, &wanted, &name);
        drop(reader);
        replies.push((name, reply));
    }
    let observed = node(&root.0, "inspect");
    for original in expected.as_array().unwrap() {
        let name = string(original, "name");
        let actual = observed
            .as_array()
            .unwrap()
            .iter()
            .find(|row| string(row, "name") == name)
            .unwrap();
        same(
            value(actual, "data"),
            value(original, "data"),
            &format!("{name} stored typed rows"),
        );
        assert_eq!(value(actual, "sourcePreserved").as_bool(), Some(true));
        assert_eq!(value(actual, "backupPreserved").as_bool(), Some(true));
        let reply = &replies.iter().find(|(n, _)| n == &name).unwrap().1;
        if value(reply, "ok").as_bool() == Some(true) {
            same(
                value(value(reply, "value"), "workingHash"),
                value(actual, "workingHash"),
                &format!("{name} current hash"),
            );
        }
    }
    save_proof(
        "sql-and-real-mapper",
        &IvyJson::object([
            ("original", expected),
            ("observed", observed),
            (
                "replies",
                IvyJson::Array(
                    replies
                        .into_iter()
                        .map(|(name, reply)| {
                            IvyJson::object([("name", name.into()), ("reply", reply)])
                        })
                        .collect(),
                ),
            ),
        ]),
    );
}

fn save_proof(group: &str, value: &IvyJson) {
    if let Some(path) = std::env::var_os("IVY_NATIVE_RESTORE_PROOF") {
        let mut out = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        let record = IvyJson::object([("group", group.into()), ("value", value.clone())]);
        writeln!(out, "{}", stringify(&record, usize::MAX).unwrap()).unwrap();
    }
}

fn comparable_report(value: &IvyJson) -> IvyJson {
    let mut report = value.clone();
    let timestamp = string(&report, "completedAt");
    chrono::DateTime::parse_from_rfc3339(&timestamp).unwrap();
    assert_eq!(timestamp.len(), 24);
    report.insert("completedAt", "<executing-clock>".into());
    let instances = value
        .get("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let mut row = row.clone();
            // Physical SQLite output is engine-specific. Its returned digest is
            // checked against that execution's actual bytes below.
            row.remove("workingHash");
            row
        })
        .collect();
    report.insert("instances", IvyJson::Array(instances));
    report
}

#[tokio::test]
async fn copied_restore_admission_before_images_and_publication_match_original() {
    use ivy_host_runtime::{layout::StorageContext, native_backup_paths::NativeBackupPolicy};
    let root = Root::new();
    let cases = node(&root.0, "outer-prepare");
    let expected = node(&root.0, "outer-original");
    let positive = expected
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| value(value(row, "reply"), "ok").as_bool() == Some(true))
        .map(|row| string(row, "name"))
        .collect::<Vec<_>>();
    assert_eq!(
        positive,
        [
            "0.154.0",
            "0.158.0",
            "0.159.2",
            "external-shared",
            "implicit-shared",
            "zero-alternate",
            "absent-metadata",
            "platform-preferred"
        ]
    );
    node(&root.0, "outer-reset");
    let mut replies = Vec::new();
    for case in cases.as_array().unwrap() {
        let policy = NativeBackupPolicy::new(
            StorageContext {
                paths: node_path::Context::current().unwrap(),
                account_home: node_path::from_native(&root.0),
                compare: Arc::new(|_, _| {
                    panic!("Single-instance fixture must not use a locale fallback")
                }),
            },
            BTreeMap::new(),
            node_path::Context::current().unwrap().cwd,
        );
        let result = native_restore::relocate_native_storage(
            value(case, "original"),
            value(case, "target"),
            value(case, "manifest"),
            Path::new(&string(case, "distributionRoot")),
            &policy,
        )
        .await;
        let reply = match result {
            Ok(report) => IvyJson::object([("ok", true.into()), ("value", report)]),
            Err(error) => wire(error),
        };
        let name = string(case, "name");
        let original = expected
            .as_array()
            .unwrap()
            .iter()
            .find(|row| string(row, "name") == name)
            .unwrap();
        if value(&reply, "ok").as_bool() == Some(true) {
            assert_eq!(
                value(value(original, "reply"), "ok").as_bool(),
                Some(true),
                "{name}"
            );
            same(
                &comparable_report(value(&reply, "value")),
                &comparable_report(value(value(original, "reply"), "value")),
                &name,
            );
        } else {
            same(&reply, value(original, "reply"), &name);
        }
        replies.push((name, reply));
    }
    let observed = node(&root.0, "outer-inspect");
    for original in expected.as_array().unwrap() {
        let name = string(original, "name");
        let actual = observed
            .as_array()
            .unwrap()
            .iter()
            .find(|row| string(row, "name") == name)
            .unwrap();
        for key in ["data", "files", "sourcePreserved", "backupPreserved"] {
            same(
                value(actual, key),
                value(original, key),
                &format!("{name} {key}"),
            );
        }
        let reply = &replies.iter().find(|(n, _)| n == &name).unwrap().1;
        if value(&reply, "ok").as_bool() == Some(true) {
            // The saved report is exactly the returned value, and its final
            // database hash names this execution's bytes rather than the oracle's.
            same(
                value(actual, "report"),
                value(reply, "value"),
                &format!("{name} saved report"),
            );
            let instances = value(value(reply, "value"), "instances")
                .as_array()
                .unwrap();
            for instance in instances {
                same(
                    value(instance, "workingHash"),
                    value(actual, "workingHash"),
                    &format!("{name} working bytes"),
                );
            }
        } else {
            assert!(
                value(actual, "report").is_null(),
                "Failure published a success report: {name}"
            );
        }
    }
    save_proof(
        "outer-restore",
        &IvyJson::object([
            ("original", expected),
            ("observed", observed),
            (
                "replies",
                IvyJson::Array(
                    replies
                        .into_iter()
                        .map(|(name, reply)| {
                            IvyJson::object([("name", name.into()), ("reply", reply)])
                        })
                        .collect(),
                ),
            ),
        ]),
    );
}
