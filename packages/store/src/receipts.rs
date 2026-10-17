use crate::sqlite::error;
use ivy_contracts::{IvyError, IvyJson, IvyResult, hash::digest, json::parse};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

fn count_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<usize> {
    let value = row.get::<_, i64>(index)?;
    usize::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

pub fn has_unresolved_outcome(value: &IvyJson) -> bool {
    match value {
        IvyJson::Array(values) => values.iter().any(has_unresolved_outcome),
        IvyJson::Object(pairs) => pairs.iter().any(|(key, part)| {
            let key = key.to_utf8().unwrap_or_default();
            let text = match part {
                IvyJson::String(s) => s.to_utf8().unwrap_or_default(),
                _ => String::new(),
            };
            (["phase", "state", "ownerPhase", "outcome", "execution"].contains(&key.as_str())
                && ["unknown", "outcome_unknown", "needs_attention"].contains(&text.as_str()))
                || (["error", "errorCode", "code"].contains(&key.as_str())
                    && (text.contains("outcome_unknown")
                        || ["native_connection_lost", "native_epoch_lost"]
                            .contains(&text.as_str())))
                || has_unresolved_outcome(part)
        }),
        _ => false,
    }
}
#[derive(Clone, Copy)]
pub struct ReceiptLimits {
    pub count: usize,
    pub bytes: usize,
}
impl Default for ReceiptLimits {
    fn default() -> Self {
        Self {
            count: 4096,
            bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct ReceiptUsage {
    pub records: usize,
    pub bytes: usize,
}

/// Borrows the owner connection, including its active transaction. Values remain original text.
pub struct ReceiptArchive<'a> {
    db: &'a Connection,
    limits: ReceiptLimits,
}
impl<'a> ReceiptArchive<'a> {
    pub fn new(db: &'a Connection, limits: ReceiptLimits) -> IvyResult<Self> {
        db.execute_batch("CREATE TABLE IF NOT EXISTS receipt_archive(kind TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(kind,key)) STRICT;
        CREATE TABLE IF NOT EXISTS receipt_archive_usage(id INTEGER PRIMARY KEY CHECK(id=1),records INTEGER NOT NULL,bytes INTEGER NOT NULL) STRICT;
        INSERT OR IGNORE INTO receipt_archive_usage VALUES(1,0,0);
        CREATE TRIGGER IF NOT EXISTS receipt_archive_insert AFTER INSERT ON receipt_archive BEGIN UPDATE receipt_archive_usage SET records=records+1,bytes=bytes+length(CAST(NEW.value AS BLOB)) WHERE id=1; END;
        CREATE TRIGGER IF NOT EXISTS receipt_archive_delete AFTER DELETE ON receipt_archive BEGIN UPDATE receipt_archive_usage SET records=records-1,bytes=bytes-length(CAST(OLD.value AS BLOB)) WHERE id=1; END;
        CREATE TABLE IF NOT EXISTS receipt_archive_history(sequence INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,key TEXT NOT NULL,UNIQUE(kind,key));
        CREATE TABLE IF NOT EXISTS receipt_archive_retired(kind TEXT NOT NULL,key TEXT NOT NULL,value_hash TEXT NOT NULL,PRIMARY KEY(kind,key)) STRICT;").map_err(error)?;
        Ok(Self { db, limits })
    }
    fn lookup(&self, kind: &str, key: &str) -> IvyResult<Option<String>> {
        self.db
            .query_row(
                "SELECT value FROM receipt_archive WHERE kind=? AND key=?",
                params![kind, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)
    }
    pub fn read(&self, kind: &str, key: &str) -> IvyResult<Option<String>> {
        let value = self.lookup(kind, key)?;
        if value.is_none()
            && self
                .db
                .query_row(
                    "SELECT 1 FROM receipt_archive_retired WHERE kind=? AND key=?",
                    params![kind, key],
                    |_| Ok(()),
                )
                .optional()
                .map_err(error)?
                .is_some()
        {
            return Err(IvyError::new(
                "operation_expired",
                "The released receipt was evicted; its identity cannot be reused.",
            ));
        }
        Ok(value)
    }
    pub fn retain(&self, kind: &str, key: &str, value: &str) -> IvyResult<()> {
        let retired: Option<String> = self
            .db
            .query_row(
                "SELECT value_hash FROM receipt_archive_retired WHERE kind=? AND key=?",
                params![kind, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        let conflict = || {
            IvyError::new(
                "receipt_archive_conflict",
                "An original receipt cannot be replaced.",
            )
        };
        if retired.is_some_and(|h| h != digest(value)) {
            return Err(conflict());
        }
        match self.read(kind, key)? {
            Some(prior) if prior != value => Err(conflict()),
            Some(_) => Ok(()),
            None => self
                .db
                .execute(
                    "INSERT INTO receipt_archive VALUES(?,?,?)",
                    params![kind, key, value],
                )
                .map(|_| ())
                .map_err(error),
        }
    }
    pub fn release(&self, kind: &str, key: &str) -> IvyResult<()> {
        if self.db.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(self.db, TransactionBehavior::Immediate)
                .map_err(error)?;
            self.release_inner(kind, key)?;
            tx.commit().map_err(error)
        } else {
            self.release_inner(kind, key)
        }
    }
    fn release_inner(&self, kind: &str, key: &str) -> IvyResult<()> {
        let Some(value) = self.lookup(kind, key)? else {
            return Ok(());
        };
        if parse(&value, usize::MAX).is_ok_and(|v| has_unresolved_outcome(&v)) {
            return Err(IvyError::new(
                "receipt_archive_unresolved",
                "An unresolved outcome cannot enter the history ring.",
            ));
        }
        self.db
            .execute(
                "INSERT OR IGNORE INTO receipt_archive_history(kind,key) VALUES(?,?)",
                params![kind, key],
            )
            .map_err(error)?;
        let(mut count,mut bytes):(usize,usize)=self.db.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(a.value AS BLOB))),0) FROM receipt_archive_history h JOIN receipt_archive a ON a.kind=h.kind AND a.key=h.key",[],|r|Ok((count_column(r,0)?,count_column(r,1)?))).map_err(error)?;
        if count <= self.limits.count && bytes <= self.limits.bytes {
            return Ok(());
        }
        let mut statement=self.db.prepare("SELECT a.kind,a.key,a.value FROM receipt_archive_history h JOIN receipt_archive a ON a.kind=h.kind AND a.key=h.key ORDER BY h.sequence").map_err(error)?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        for (kind, key, value) in rows {
            self.db
                .execute(
                    "INSERT INTO receipt_archive_retired VALUES(?,?,?)",
                    params![kind, key, digest(&value)],
                )
                .map_err(error)?;
            self.db
                .execute(
                    "DELETE FROM receipt_archive WHERE kind=? AND key=?",
                    params![kind, key],
                )
                .map_err(error)?;
            self.db
                .execute(
                    "DELETE FROM receipt_archive_history WHERE kind=? AND key=?",
                    params![kind, key],
                )
                .map_err(error)?;
            count -= 1;
            bytes -= value.len();
            if count <= self.limits.count && bytes <= self.limits.bytes {
                break;
            }
        }
        Ok(())
    }
    pub fn status(&self) -> IvyResult<ReceiptUsage> {
        self.db
            .query_row(
                "SELECT records,bytes FROM receipt_archive_usage WHERE id=1",
                [],
                |r| {
                    Ok(ReceiptUsage {
                        records: count_column(r, 0)?,
                        bytes: count_column(r, 1)?,
                    })
                },
            )
            .map_err(error)
    }
}
