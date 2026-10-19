//! Storage-owner feasibility checks. These qualify the native SQLite boundary,
//! not the unfinished server. See worker-backup-online.test.ts for the wire cases.

#[path = "hive_storage_read.rs"]
mod storage_read;
#[path = "hive_store.rs"]
mod store;

use rusqlite::{
    Connection, ErrorCode, OpenFlags,
    backup::{Backup, StepResult},
    functions::FunctionFlags,
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct TemporaryDirectory(PathBuf);
fn temporary_directory(prefix: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("{prefix}-{}-{nonce}-{serial}", std::process::id()));
    fs::create_dir(&path).unwrap();
    path
}
impl TemporaryDirectory {
    fn new() -> Self {
        Self(temporary_directory("ivy-hive-worker"))
    }
}
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn backup_interleaves_reads_and_committed_writes_on_its_single_source_connection() {
    let root = TemporaryDirectory::new();
    let source = Connection::open(root.0.join("source.sqlite")).unwrap();
    source
        .execute_batch(
            "PRAGMA locking_mode=EXCLUSIVE; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE payload(id INTEGER PRIMARY KEY, bytes BLOB NOT NULL) STRICT;
        CREATE TABLE credential(id INTEGER PRIMARY KEY, revoked INTEGER NOT NULL) STRICT;
        INSERT INTO credential VALUES(1,0);
        WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<256)
        INSERT INTO payload SELECT x,zeroblob(4096) FROM n;",
        )
        .unwrap();
    let mut destination = Connection::open(root.0.join("snapshot.sqlite")).unwrap();
    let backup = Backup::new(&source, &mut destination).unwrap();
    let mut steps = 0;
    loop {
        let state = backup.step(1).unwrap();
        steps += 1;
        if steps == 2 {
            // The storage actor must be able to process credential revocation and
            // ordinary queries between backup steps, without a second writer.
            source
                .execute_batch("BEGIN IMMEDIATE; UPDATE credential SET revoked=1; COMMIT;")
                .unwrap();
        }
        let revoked: i64 = source
            .query_row("SELECT revoked FROM credential", [], |row| row.get(0))
            .unwrap();
        assert_eq!(revoked, i64::from(steps >= 2));
        match state {
            StepResult::Done => break,
            StepResult::More => assert!(steps < 4096, "backup stopped progressing"),
            other => panic!("unexpected backup state with one owner: {other:?}"),
        }
    }
    assert!(steps > 2);
    let progress = backup.progress();
    assert_eq!(progress.remaining, 0);
    drop(backup);
    assert_eq!(
        destination
            .query_row("SELECT revoked FROM credential", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        destination
            .query_row("SELECT count(*) FROM payload", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        256
    );
    assert_eq!(
        destination
            .query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn interrupt_retires_running_sql_and_rolls_back_its_uncommitted_effects() {
    let root = TemporaryDirectory::new();
    let path = root.0.join("source.sqlite");
    let worker_path = path.clone();
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let source = Connection::open(worker_path).unwrap();
        source.execute_batch("CREATE TABLE effects(value INTEGER) STRICT; BEGIN IMMEDIATE; INSERT INTO effects VALUES(1);").unwrap();
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_entered = entered.clone();
        source
            .progress_handler(
                1000,
                Some(move || {
                    hook_entered.store(true, std::sync::atomic::Ordering::Release);
                    false
                }),
            )
            .unwrap();
        started_tx
            .send((source.get_interrupt_handle(), entered))
            .unwrap();
        let result = source.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n", [], |row| row.get::<_, i64>(0));
        assert_eq!(
            result.unwrap_err().sqlite_error_code(),
            Some(ErrorCode::OperationInterrupted)
        );
        if !source.is_autocommit() {
            source.execute_batch("ROLLBACK").unwrap();
        }
        drop(source);
        done_tx.send(()).unwrap();
    });
    let (interrupt, entered) = started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let limit = std::time::Instant::now() + Duration::from_secs(5);
    while !entered.load(std::sync::atomic::Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < limit,
            "query did not enter SQLite"
        );
        thread::yield_now();
    }
    interrupt.interrupt();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("SQL owner did not close within the test deadline");
    worker.join().unwrap();
    let reopened = Connection::open(path).unwrap();
    assert_eq!(
        reopened
            .query_row("SELECT count(*) FROM effects", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn sqlite_interrupt_cannot_retire_a_thread_blocked_inside_a_host_callback() {
    // Model a stalled storage callback (for example synchronous filesystem I/O).
    // Interrupt/progress hooks run around SQLite instructions, not inside Rust.
    // This is a negative feasibility result; it must not be called proof that a
    // cooperative actor can satisfy the server's forced-retirement contract.
    let root = TemporaryDirectory::new();
    let path = root.0.join("source.sqlite");
    let worker_path = path.clone();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_gate = gate.clone();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (interrupt_tx, interrupt_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let source = Connection::open(worker_path).unwrap();
        source.execute_batch("PRAGMA locking_mode=EXCLUSIVE; PRAGMA journal_mode=WAL;
            CREATE TABLE effects(value INTEGER) STRICT; BEGIN IMMEDIATE; INSERT INTO effects VALUES(1);").unwrap();
        source
            .create_scalar_function(
                "blocked_host_io",
                0,
                FunctionFlags::SQLITE_UTF8,
                move |_| {
                    entered_tx.send(()).unwrap();
                    let (mutex, wake) = &*worker_gate;
                    let guard = mutex.lock().unwrap();
                    let (released, _) = wake
                        .wait_timeout_while(guard, Duration::from_secs(5), |released| !*released)
                        .unwrap();
                    assert!(*released, "test driver failed to release callback");
                    Ok(1)
                },
            )
            .unwrap();
        interrupt_tx.send(source.get_interrupt_handle()).unwrap();
        let result = source.query_row("SELECT blocked_host_io()", [], |row| row.get::<_, i64>(0));
        if !source.is_autocommit() {
            source.execute_batch("ROLLBACK").unwrap();
        }
        drop(source);
        done_tx.send(result).unwrap();
    });
    let interrupt = interrupt_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    interrupt.interrupt();
    let retirement = done_rx.recv_timeout(Duration::from_millis(100));
    // Always release the fixture before asserting, including on regressions.
    let still_running = matches!(retirement, Err(mpsc::RecvTimeoutError::Timeout));
    let competing = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    competing.busy_timeout(Duration::from_millis(25)).unwrap();
    let ownership = competing.execute_batch("BEGIN IMMEDIATE");
    drop(competing);
    let (mutex, wake) = &*gate;
    *mutex.lock().unwrap() = true;
    wake.notify_one();
    if still_running {
        let _ = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    worker.join().unwrap();
    assert!(
        still_running,
        "the fixture no longer demonstrates blocked callback retirement"
    );
    assert_eq!(
        ownership.unwrap_err().sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy)
    );
    let reopened = Connection::open(path).unwrap();
    assert_eq!(
        reopened
            .query_row("SELECT count(*) FROM effects", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
