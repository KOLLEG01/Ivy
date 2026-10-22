use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-hive-backup-cli-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let path = fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            path.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-hive-backup-cli-")
        );
        fs::remove_dir_all(path).unwrap();
    }
}
fn node() -> Command {
    Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
}
fn bounded(mut command: Command, input: Option<&[u8]>) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    } else {
        drop(child.stdin.take());
    }
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(15) {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("Owned offline helper process exceeded the 15s fixture bound");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}
fn streams(output: Output) -> Value {
    json!({"status":output.status.code(),"stdout":String::from_utf8(output.stdout).unwrap(),"stderr":String::from_utf8(output.stderr).unwrap()})
}
fn original(args: &[String]) -> Value {
    let mut command = node();
    command.arg(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/fixtures/hive-offline-backup-oracle.mjs"),
    );
    let output = bounded(
        command,
        Some(serde_json::to_string(&[args]).unwrap().as_bytes()),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout)
        .unwrap()
        .as_array()
        .unwrap()[0]
        .clone()
}
fn native(args: &[String]) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ivy-hive-offline-backup"));
    command.args(args);
    streams(bounded(command, None))
}
fn compare(name: &str, actual_args: &[String], expected_args: &[String]) -> Value {
    let expected = original(expected_args);
    let start = Instant::now();
    let actual = native(actual_args);
    let elapsed = start.elapsed().as_millis();
    if let Some(path) = std::env::var_os("IVY_HIVE_OFFLINE_BACKUP_CORPUS") {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _lock = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let row = json!({"test":name,"originalArgs":expected_args,"nativeArgs":actual_args,"expected":expected,"actual":actual,"nativeElapsedMs":elapsed});
        let line = serde_json::to_string(&row).unwrap() + "\n";
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }
    assert_eq!(actual, expected, "{name}: {actual_args:?}");
    actual
}
fn seed(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch("PRAGMA user_version=4; CREATE TABLE objects(id TEXT PRIMARY KEY, bytes BLOB, archived INTEGER); CREATE TABLE revisions(object_id TEXT,revision INTEGER, content TEXT); CREATE INDEX by_revision ON revisions(revision); INSERT INTO objects VALUES('root',X'00FFFE0100',0),('archive',X'',1); INSERT INTO revisions VALUES('root',1,'old'),('root',2,'current'),('archive',1,NULL); CREATE TRIGGER append_revision AFTER INSERT ON objects BEGIN INSERT INTO revisions VALUES(new.id,1,'created'); END;").unwrap();
    db.execute("INSERT INTO objects VALUES(?,X'CAFEBABE',0)", ["é/🙂"])
        .unwrap();
}
fn compare_database(source: &Path, destination: &Path) {
    let db = Connection::open(destination).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        db.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    db.execute("ATTACH ? AS original", [source.to_str().unwrap()])
        .unwrap();
    for kind in ["table", "index", "trigger", "view"] {
        let schema:i64=db.query_row("SELECT count(*) FROM (SELECT type,name,tbl_name,sql FROM main.sqlite_schema WHERE type=? EXCEPT SELECT type,name,tbl_name,sql FROM original.sqlite_schema WHERE type=?)",[kind,kind],|r|r.get(0)).unwrap();
        assert_eq!(schema, 0);
    }
    let tables = db
        .prepare("SELECT name FROM main.sqlite_schema WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for table in tables {
        let name = table.replace('"', "\"\"");
        for (left, right) in [("main", "original"), ("original", "main")] {
            let count:i64=db.query_row(&format!("SELECT count(*) FROM (SELECT * FROM {left}.\"{name}\" EXCEPT SELECT * FROM {right}.\"{name}\")"),[],|r|r.get(0)).unwrap();
            assert_eq!(count, 0, "{left}/{right}/{table}");
        }
    }
}
#[test]
fn real_offline_backup_cli_preserves_rows_schema_and_original_process_contract() {
    let root = Root::new();
    let source = root.0.join("source.sqlite");
    seed(&source);
    let before = fs::read(&source).unwrap();
    let path = |name: &str| root.0.join(name).to_str().unwrap().to_owned();
    let actual = compare(
        "format-four-snapshot",
        &[path("source.sqlite"), path("native.sqlite")],
        &[path("source.sqlite"), path("original.sqlite")],
    );
    assert_eq!(actual["status"], 0);
    compare_database(&source, &root.0.join("native.sqlite"));
    compare_database(&source, &root.0.join("original.sqlite"));
    let actual = compare(
        "extra-arguments",
        &[
            path("source.sqlite"),
            path("native-extra.sqlite"),
            "ignored".into(),
        ],
        &[
            path("source.sqlite"),
            path("original-extra.sqlite"),
            "ignored".into(),
        ],
    );
    assert_eq!(actual["status"], 0);
    compare_database(&source, &root.0.join("native-extra.sqlite"));
    fs::write(
        root.0.join("corrupt.sqlite"),
        b"This is not a SQLite database.",
    )
    .unwrap();
    fs::create_dir(root.0.join("directory")).unwrap();
    for (format, name) in [(0, "format-zero.sqlite"), (99, "format-future.sqlite")] {
        let db = Connection::open(root.0.join(name)).unwrap();
        db.execute_batch(&format!(
            "PRAGMA user_version={format}; CREATE TABLE future(value TEXT);"
        ))
        .unwrap();
    }
    let cases = vec![
        ("missing-both", vec![]),
        ("missing-destination", vec![path("source.sqlite")]),
        (
            "empty-source",
            vec!["".into(), path("empty-source-output.sqlite")],
        ),
        ("empty-destination", vec![path("source.sqlite"), "".into()]),
        (
            "relative-source",
            vec![
                "relative.sqlite".into(),
                path("relative-source-output.sqlite"),
            ],
        ),
        (
            "relative-destination",
            vec![path("source.sqlite"), "relative.sqlite".into()],
        ),
        (
            "absent-source",
            vec![path("absent.sqlite"), path("absent-output.sqlite")],
        ),
        (
            "same-source",
            vec![path("source.sqlite"), path("source.sqlite")],
        ),
        (
            "existing-destination",
            vec![path("source.sqlite"), path("native.sqlite")],
        ),
        (
            "directory-destination",
            vec![path("source.sqlite"), path("directory")],
        ),
        (
            "missing-parent",
            vec![path("source.sqlite"), path("absent-parent/output.sqlite")],
        ),
        (
            "directory-source",
            vec![path("directory"), path("directory-output.sqlite")],
        ),
        (
            "corrupt-source",
            vec![path("corrupt.sqlite"), path("corrupt-output.sqlite")],
        ),
        (
            "format-zero",
            vec![path("format-zero.sqlite"), path("zero-output.sqlite")],
        ),
        (
            "format-future",
            vec![path("format-future.sqlite"), path("future-output.sqlite")],
        ),
    ];
    for (name, args) in cases {
        let result = compare(name, &args, &args);
        assert_eq!(result["status"], 1);
        assert_eq!(result["stdout"], "");
        assert!(
            !result["stderr"]
                .as_str()
                .unwrap()
                .contains(root.0.to_str().unwrap())
        );
    }
    assert_eq!(fs::read(&source).unwrap(), before);
    for name in [
        "absent.sqlite",
        "empty-source-output.sqlite",
        "relative-source-output.sqlite",
        "absent-output.sqlite",
        "directory-output.sqlite",
        "corrupt-output.sqlite",
        "zero-output.sqlite",
        "future-output.sqlite",
        "source.sqlite-wal",
    ] {
        assert!(!root.0.join(name).exists(), "Unexpected {name}");
    }
}
#[test]
fn historical_snapshot_and_stopped_writer_lock_timeout_use_the_actual_helper() {
    let root = Root::new();
    let source = root.0.join("historical.sqlite");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/hive-current.sqlite.gz");
    let mut command = node();
    command.args(["--input-type=module","-e","import{readFileSync,writeFileSync}from'node:fs';import{gunzipSync}from'node:zlib';writeFileSync(process.argv[2],gunzipSync(readFileSync(process.argv[1])));"]).arg(&fixture).arg(&source);
    assert!(bounded(command, None).status.success());
    let before = fs::read(&source).unwrap();
    let destination = root.0.join("native.sqlite");
    let reference = root.0.join("original.sqlite");
    let result = compare(
        "historical-format-four",
        &[
            source.to_str().unwrap().into(),
            destination.to_str().unwrap().into(),
        ],
        &[
            source.to_str().unwrap().into(),
            reference.to_str().unwrap().into(),
        ],
    );
    assert_eq!(result["status"], 0);
    compare_database(&source, &destination);
    assert_eq!(fs::read(&source).unwrap(), before);
    let locked = root.0.join("locked.sqlite");
    seed(&locked);
    let lock = Connection::open(&locked).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let args = vec![
        locked.to_str().unwrap().into(),
        root.0.join("locked-output.sqlite").to_str().unwrap().into(),
    ];
    let start = Instant::now();
    let result = compare("stopped-writer-exclusive-lock", &args, &args);
    assert_eq!(result["status"], 1);
    assert!(start.elapsed() < Duration::from_secs(10));
    assert!(!root.0.join("locked-output.sqlite").exists());
    lock.execute_batch("ROLLBACK").unwrap();
    drop(lock);
    let result = compare(
        "lock-released",
        &[
            locked.to_str().unwrap().into(),
            root.0
                .join("native-released.sqlite")
                .to_str()
                .unwrap()
                .into(),
        ],
        &[
            locked.to_str().unwrap().into(),
            root.0
                .join("original-released.sqlite")
                .to_str()
                .unwrap()
                .into(),
        ],
    );
    assert_eq!(result["status"], 0);
    compare_database(&locked, &root.0.join("native-released.sqlite"));
}
