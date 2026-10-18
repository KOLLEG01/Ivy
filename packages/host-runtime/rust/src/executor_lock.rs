//! Reusable kernel-backed SQLite ownership, independent of HostJournal.
use ivy_contracts::{IvyError, IvyResult};
use ivy_store::{rusqlite::Connection, sqlite};
use std::{fs, path::PathBuf, sync::{Arc, Mutex}, time::Duration};

fn internal<T>(result: Result<T, impl std::fmt::Debug>) -> IvyResult<T> { result.map_err(|_| IvyError::internal()) }
/// Exclusive ownership stays with the live connection; shared readers hold a
/// transaction. Drop releases the connection; close waits for explicit release.
pub struct ExecutorLock { connection: Arc<Mutex<Option<Connection>>> }
impl ExecutorLock {
    pub async fn acquire(root: PathBuf, shared: bool) -> IvyResult<Self> {
        let connection = internal(tokio::task::spawn_blocking(move || -> IvyResult<Connection> {
            internal(fs::create_dir_all(&root))?;
            let db = internal(sqlite::open(&root.join("executor-lock.sqlite"), sqlite::OpenOptions { busy_timeout: Duration::from_millis(100), read_only: false }))?;
            let claim = || -> ivy_store::rusqlite::Result<()> {
                if shared { db.execute_batch("CREATE TABLE IF NOT EXISTS owner(pid INTEGER); BEGIN; SELECT * FROM owner;")?; }
                else {
                    db.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; CREATE TABLE IF NOT EXISTS owner(pid INTEGER); DELETE FROM owner;")?;
                    db.execute("INSERT INTO owner VALUES (?)", [std::process::id()])?;
                    db.execute_batch("COMMIT")?;
                }
                Ok(())
            };
            claim().map_err(|_| IvyError::new("executor_already_running", "Another executor holds the host lock."))?;
            Ok(db)
        }).await)??;
        Ok(Self { connection: Arc::new(Mutex::new(Some(connection))) })
    }
    pub async fn close(&self) -> IvyResult<()> {
        let connection = self.connection.clone();
        internal(tokio::task::spawn_blocking(move || { internal(connection.lock())?.take(); Ok(()) }).await)?
    }
}
