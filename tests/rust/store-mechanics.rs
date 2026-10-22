use ivy_store::{atomic, rusqlite, sqlite};
use std::{
    fs,
    io::{BufRead, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

pub(super) struct Temporary {
    base: PathBuf,
    pub(super) path: PathBuf,
}
impl Temporary {
    pub(super) fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = base.join(format!("ivy-store-mechanics-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self { base, path }
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        if fs::canonicalize(&self.path).is_ok_and(|path| {
            path.parent() == Some(self.base.as_path()) && path.file_name() == self.path.file_name()
        }) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub(super) struct OwnedChild(pub(super) Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn sqlite_options_and_owner_locks_survive_process_death() {
    const CHILD_PATH: &str = "IVY_STORE_MECHANICS_LOCK_CHILD";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let _lock = sqlite::OwnerLock::acquire(std::path::Path::new(&path), true).unwrap();
        println!("IVY_STORE_LOCK_READY");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read_exact(&mut [0]);
        return;
    }
    let temp = Temporary::new();
    let path = temp.path.join("owner.sqlite");
    let db = sqlite::open(
        &path,
        sqlite::OpenOptions {
            busy_timeout: Duration::from_millis(37),
            read_only: false,
        },
    )
    .unwrap();
    assert_eq!(
        db.query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        37
    );
    assert!(db.is_autocommit());
    db.execute_batch("CREATE TABLE entries(value INTEGER) STRICT; INSERT INTO entries VALUES(7);")
        .unwrap();
    assert!(
        db.execute("INSERT INTO entries VALUES('text')", [])
            .is_err()
    );
    assert_eq!(
        db.query_row(
            "SELECT json_extract('{\"value\":7}','$.value')",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        7
    );
    db.execute_batch("CREATE VIRTUAL TABLE words USING fts5(value,tokenize='unicode61'); INSERT INTO words VALUES('café');").unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM words WHERE words MATCH 'cafe'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    drop(db);
    let read_only = sqlite::open(
        &path,
        sqlite::OpenOptions {
            busy_timeout: Duration::from_millis(37),
            read_only: true,
        },
    )
    .unwrap();
    assert_eq!(
        read_only
            .query_row("SELECT value FROM entries", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        read_only
            .execute("INSERT INTO entries VALUES(8)", [])
            .unwrap_err()
            .sqlite_error_code(),
        Some(rusqlite::ErrorCode::ReadOnly)
    );
    drop(read_only);
    let first = sqlite::OwnerLock::acquire(&path, false).unwrap();
    let second = sqlite::OwnerLock::acquire(&path, false).unwrap();
    let busy = || match sqlite::OwnerLock::acquire(&path, true) {
        Ok(_) => panic!("a live shared owner allowed exclusive ownership"),
        Err(error) => assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy)
        ),
    };
    busy();
    drop(first);
    busy();
    drop(second);
    let exclusive = sqlite::OwnerLock::acquire(&path, true).unwrap();
    assert!(sqlite::OwnerLock::acquire(&path, false).is_err());
    drop(exclusive);

    let mut command = Command::new(std::env::current_exe().unwrap());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = OwnedChild(
        command
            .args([
                "--exact",
                "storage_mechanics::sqlite_options_and_owner_locks_survive_process_death",
                "--nocapture",
            ])
            .env(CHILD_PATH, &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let output = child.0.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let ready = std::io::BufReader::new(output)
            .lines()
            .any(|line| line.is_ok_and(|line| line.ends_with("IVY_STORE_LOCK_READY")));
        let _ = send.send(ready);
    });
    let ready = receive.recv_timeout(Duration::from_secs(10));
    if !matches!(ready, Ok(true)) {
        let _ = child.0.kill();
        let _ = child.0.wait();
    }
    reader.join().unwrap();
    assert!(
        matches!(ready, Ok(true)),
        "lock child did not reach ownership: {ready:?}"
    );
    assert!(sqlite::OwnerLock::acquire(&path, false).is_err());
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    let _reclaimed = sqlite::OwnerLock::acquire(&path, true).unwrap();
}

#[test]
fn atomic_replacement_preserves_original_bytes_on_failure() {
    let temp = Temporary::new();
    let path = temp.path.join("state.json");
    let value = ivy_contracts::json::parse(r#"{"b":1,"a":"\ud800"}"#, 1024).unwrap();
    atomic::atomic_json(&path, &value).unwrap();
    let expected = b"{\"a\":\"\\ud800\",\"b\":1}\n";
    assert_eq!(fs::read(&path).unwrap(), expected);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let reader = fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(&path)
            .unwrap();
        assert!(atomic::atomic_write(&path, b"replacement").is_err());
        assert_eq!(fs::read(&path).unwrap(), expected);
        drop(reader);
    }
    atomic::atomic_write(&path, b"replacement").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"replacement");
    let directory = temp.path.join("directory-destination");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("retained"), b"original").unwrap();
    assert!(atomic::atomic_write(&directory, b"must-not-replace-directory").is_err());
    assert_eq!(fs::read(directory.join("retained")).unwrap(), b"original");
    assert!(fs::read_dir(&temp.path).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}
