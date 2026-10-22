use super::storage_mechanics::{OwnedChild, Temporary};
use ivy_contracts::{IvyJson as J, hash::digest, json};
use ivy_store::{
    rusqlite::{
        Connection,
        backup::{Backup, StepResult},
        params,
    },
    sqlite,
};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

const RAW: &str = " \t{\"s\":\"\\ud800\",\"n\":1.00,\"e\":1e+01,\"emoji\":\"😀\"} \n";
fn open(path: &Path, read_only: bool) -> Connection {
    sqlite::open(
        path,
        sqlite::OpenOptions {
            read_only,
            busy_timeout: Duration::from_millis(100),
        },
    )
    .unwrap()
}
fn command() -> Command {
    let mut command =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()));
    command.arg(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/store-sqlite-compat.mjs"),
    );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
fn snapshot(db: &Connection, rows: i64) {
    assert_eq!(db.query_row("SELECT count(*),sum(id),sum(length(payload)),sum(length(CAST(raw AS BLOB))) FROM entries", [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))).unwrap(), (rows, rows * (rows + 1) / 2, 128 * 2048 + if rows == 129 { 6 } else { 0 }, rows * RAW.len() as i64));
    assert_eq!(
        db.query_row("SELECT raw FROM entries WHERE id=1", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        RAW
    );
    let payload: Vec<u8> = db
        .query_row("SELECT payload FROM entries WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(payload, vec![1; 2048]);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM entries WHERE id IN (777,999)",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        73
    );
}
fn engine(db: &Connection) -> J {
    let version: String = db
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .unwrap();
    let source: String = db
        .query_row("SELECT sqlite_source_id()", [], |r| r.get(0))
        .unwrap();
    let mut options: Vec<String> = db
        .prepare("PRAGMA compile_options")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    options.sort();
    J::object([
        ("version", version.into()),
        ("sourceId", source.into()),
        (
            "options",
            J::Array(options.into_iter().map(J::from).collect()),
        ),
    ])
}
fn evidence(name: &str, value: &J) {
    if let Some(directory) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            Path::new(&directory).join(name),
            json::stringify(value, usize::MAX).unwrap(),
        )
        .unwrap();
    }
}
fn complete_backup(source: &Connection, destination: &mut Connection) -> J {
    let backup = Backup::new(source, destination).unwrap();
    let mut progress = Vec::new();
    let mut remaining = i32::MAX;
    for _ in 0..10_000 {
        let result = backup.step(1).unwrap();
        let current = backup.progress();
        assert!(current.pagecount > 1);
        assert!((0..=current.pagecount).contains(&current.remaining));
        assert!(current.remaining <= remaining);
        remaining = current.remaining;
        progress.push(J::object([
            ("remaining", J::Number(current.remaining as f64)),
            ("pages", J::Number(current.pagecount as f64)),
        ]));
        match result {
            StepResult::More => assert!(current.remaining > 0),
            StepResult::Done => {
                assert_eq!(current.remaining, 0);
                assert!(progress.len() > 1);
                return J::Array(progress);
            }
            _ => panic!("unexpected backup contention: {result:?}"),
        }
    }
    panic!("bounded backup did not finish");
}

#[test]
fn populated_node_wal_backup_crash_and_rust_checkpoint_cross_open() {
    let temporary = Temporary::new();
    let live = temporary.path.join("live.sqlite");
    let node_copy = temporary.path.join("node-backup.sqlite");
    let rust_copy = temporary.path.join("rust-backup.sqlite");
    let mut child = OwnedChild(
        command()
            .arg("wal-owner")
            .arg(&live)
            .arg(&node_copy)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = send.send(result);
    });
    let ready = receive.recv_timeout(Duration::from_secs(15));
    if !matches!(&ready, Ok(Ok(_))) {
        let _ = child.0.kill();
        let _ = child.0.wait();
    }
    reader.join().unwrap();
    let line = ready
        .expect("owned Node fixture readiness")
        .expect("Node output");
    if line.trim().is_empty() {
        let status = child.0.wait().unwrap();
        let mut error = String::new();
        child
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        panic!("Node fixture exited without readiness ({status}): {error}");
    }
    let node_before = json::parse(&line, usize::MAX).expect("Node readiness JSON");
    assert_eq!(
        node_before.get("snapshot").unwrap().get("rawDigest"),
        Some(&digest(RAW.as_bytes()).into())
    );
    let wal_bytes = fs::metadata(live.with_file_name("live.sqlite-wal"))
        .unwrap()
        .len();
    assert!(wal_bytes > 0, "live committed data must remain in WAL");
    let db = open(&live, true);
    assert_eq!(
        db.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    snapshot(&db, 128);
    snapshot(&open(&node_copy, true), 128);
    let mut destination = open(&rust_copy, false);
    let progress = complete_backup(&db, &mut destination);
    snapshot(&destination, 128);
    drop(destination);
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    snapshot(&db, 128);
    drop(db);
    let mut db = open(&live, false);
    snapshot(&db, 128);
    {
        let transaction = db.transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO entries VALUES(777,?,?)",
                params![RAW, b"rollback".as_slice()],
            )
            .unwrap();
        transaction.rollback().unwrap();
    }
    db.execute(
        "INSERT INTO entries VALUES(129,?,?)",
        params![RAW, b"native".as_slice()],
    )
    .unwrap();
    snapshot(&db, 129);
    let checkpoint: (i64, i64, i64) = db
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(checkpoint, (0, 0, 0));
    let rust_engine = engine(&db);
    drop(db);
    let output = command()
        .arg("verify")
        .arg(&live)
        .arg(&rust_copy)
        .arg(&node_copy)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let node_after = json::parse(&String::from_utf8(output.stdout).unwrap(), usize::MAX).unwrap();
    assert_eq!(node_after.as_array().unwrap().len(), 3);
    evidence(
        "node-wal-cross-open.json",
        &J::object([
            ("nodeBefore", node_before),
            ("rustEngine", rust_engine),
            ("nodeAfter", node_after),
            ("walBytes", J::Number(wal_bytes as f64)),
            ("rustBackupProgress", progress),
            ("checkpoint", J::Array(vec![J::Number(0.0); 3])),
        ]),
    );
}

#[test]
fn unfinished_backup_rolls_back_destination_and_can_restart() {
    let temporary = Temporary::new();
    let mut source = open(&temporary.path.join("source.sqlite"), false);
    source.execute_batch("PRAGMA user_version=73; CREATE TABLE entries(id INTEGER PRIMARY KEY,raw TEXT NOT NULL,payload BLOB NOT NULL) STRICT;").unwrap();
    let transaction = source.transaction().unwrap();
    for id in 1..=128 {
        transaction
            .execute(
                "INSERT INTO entries VALUES(?,?,?)",
                params![id, RAW, vec![id as u8; 2048]],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    let mut destination = open(&temporary.path.join("destination.sqlite"), false);
    destination.execute_batch("CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('original destination');").unwrap();
    let partial = {
        let backup = Backup::new(&source, &mut destination).unwrap();
        assert_eq!(backup.step(1).unwrap(), StepResult::More);
        let current = backup.progress();
        assert!(current.remaining > 0);
        J::object([
            ("remaining", J::Number(current.remaining as f64)),
            ("pages", J::Number(current.pagecount as f64)),
        ])
    };
    assert_eq!(
        destination
            .query_row("SELECT value FROM sentinel", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "original destination"
    );
    assert_eq!(
        destination
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let progress = complete_backup(&source, &mut destination);
    snapshot(&source, 128);
    snapshot(&destination, 128);
    assert_eq!(
        destination
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='sentinel'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    evidence(
        "backup-abandon-restart.json",
        &J::object([
            ("partial", partial),
            ("completed", progress),
            ("engine", engine(&source)),
        ]),
    );
}
