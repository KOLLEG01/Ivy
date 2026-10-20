// Compile the same Host domain source to isolate its SQL transaction from the
// generic file mapper. File observations here come from the original mapper.
#[path = "../../packages/host-runtime/rust/src/native_restore.rs"]
mod native_restore;
use ivy_contracts::{
    IvyError, IvyJson, JsString, Outcome,
    hash::digest,
    json::{canonical, parse, stringify},
};
use ivy_host_runtime::{node_path, paths};
use ivy_store::{
    rusqlite::Connection,
    sqlite::{self, OpenOptions},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
        ("root", node_path::from_native(root).into()),
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
fn replay_error(error: &IvyJson) -> IvyError {
    let data = value(error, "data");
    let outcome = match string(data, "outcome").as_str() {
        "unknown" => Outcome::Unknown,
        "completed" => Outcome::Completed,
        _ => Outcome::NotExecuted,
    };
    IvyError::new(string(data, "code"), string(error, "message")).with_outcome(outcome)
}
fn wire(error: IvyError) -> IvyJson {
    IvyJson::object([("ok", false.into()), ("error", error.to_wire())])
}

#[test]
fn copied_sqlite_transactions_match_original_layout_digest_and_failure_boundaries() {
    let root = Root::new();
    let cases = node(&root.0, "prepare");
    let expected = node(&root.0, "original");
    node(&root.0, "reset");
    let mappings = node(&root.0, "mappings");
    let mut replies = Vec::new();
    for case in cases.as_array().unwrap() {
        let name = string(case, "name");
        let home = PathBuf::from(string(case, "newHome"));
        let database = home.join("state_5.sqlite");
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
        let replay = mappings
            .as_array()
            .unwrap()
            .iter()
            .find(|row| string(row, "name") == name)
            .unwrap();
        let result = native_restore::relocate_database(
            db,
            value(case, "layout"),
            |raw: &JsString, scope| {
                let row = value(replay, "mappings")
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| {
                        value(row, "path").as_js_string() == Some(raw)
                            && string(row, "scope") == scope
                    })
                    .expect("Original file observation missing");
                let reply = value(row, "reply");
                if value(reply, "ok").as_bool() == Some(false) {
                    return Err(replay_error(value(reply, "error")));
                }
                Ok(value(reply, "value").as_js_string().cloned())
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
}
