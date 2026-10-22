use crate::{IvyError, IvyResult, backup_files, node_path};
use ivy_store::{
    rusqlite::{
        Connection,
        backup::{Backup, StepResult},
    },
    sqlite,
};
use std::{fs::OpenOptions, path::PathBuf, time::Duration};

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
/// Copy a stable read transaction through SQLite's backup API, then flush and
/// explicitly close its destination. This is not a byte copy of a live WAL DB.
pub async fn snapshot_journal(source: PathBuf, destination: PathBuf) -> IvyResult<()> {
    internal(
        tokio::task::spawn_blocking(move || {
            let source = node_path::node_filesystem_path(&node_path::from_native(&source))
                .map_err(|_| IvyError::internal())?;
            let destination =
                node_path::node_filesystem_path(&node_path::from_native(&destination))
                    .map_err(|_| IvyError::internal())?;
            let db = internal(sqlite::open(
                &source,
                sqlite::OpenOptions {
                    read_only: true,
                    busy_timeout: Duration::from_millis(1000),
                },
            ))?;
            let result = snapshot(&db, &destination);
            // Finally precedence: rollback/close errors replace the copy error.
            if !db.is_autocommit() {
                internal(db.execute_batch("ROLLBACK"))?;
            }
            internal(db.close().map_err(|(_, e)| e))?;
            result
        })
        .await,
    )?
}
fn snapshot(db: &Connection, destination: &std::path::Path) -> IvyResult<()> {
    internal(db.execute_batch("PRAGMA query_only=ON; BEGIN"))?;
    {
        let mut check = internal(db.prepare("PRAGMA quick_check"))?;
        let rows = internal(check.query_map([], |row| row.get::<_, String>(0)))?;
        for row in rows {
            if internal(row)? != "ok" {
                return Err(IvyError::new(
                    "storage_invalid",
                    "Host journal failed its integrity check.",
                ));
            }
        }
    }
    {
        let mut destination = internal(sqlite::open(
            destination,
            sqlite::OpenOptions {
                read_only: false,
                busy_timeout: Duration::ZERO,
            },
        ))?;
        let backup = internal(Backup::new(db, &mut destination))?;
        loop {
            let step = internal(backup.step(100))?;
            if step == StepResult::Done {
                break;
            }
            // Node's original loop retries only while pages remain. In
            // particular an initially locked destination fails, not spins.
            if backup.progress().remaining == 0 {
                return Err(IvyError::internal());
            }
            std::thread::yield_now();
        }
    }
    backup_files::sync_file(internal(
        OpenOptions::new().read(true).write(true).open(destination),
    )?)
}
