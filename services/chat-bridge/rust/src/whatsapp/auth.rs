//! Legacy auth.sqlite storage. Values are opaque BufferJSON, including fields
//! that the native transport does not understand. Opening never re-encodes them.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::path::Path;

const SCHEMA: &str = "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
CREATE TABLE IF NOT EXISTS auth(category TEXT NOT NULL,id TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(category,id));";

pub struct AuthStore {
    pub(super) db: Connection,
}

impl AuthStore {
    /// The owner creates the protected auth directory before opening this store.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(SCHEMA)?;
        Ok(Self { db })
    }

    pub fn get(&self, category: &str, id: &str) -> rusqlite::Result<Option<String>> {
        self.db
            .query_row(
                "SELECT value FROM auth WHERE category=? AND id=?",
                (category, id),
                |row| row.get(0),
            )
            .optional()
    }

    /// Read every category from one committed state for compatibility preflight.
    /// Keep unknown rows and encoded values intact; inspection never advances keys.
    pub fn snapshot(&mut self) -> rusqlite::Result<Vec<AuthEntry>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let entries = {
            let mut query =
                tx.prepare("SELECT category,id,value FROM auth ORDER BY category,id")?;
            query
                .query_map([], |row| {
                    Ok(AuthEntry {
                        category: row.get(0)?,
                        id: row.get(1)?,
                        value: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.commit()?;
        Ok(entries)
    }

    pub fn set(&self, category: &str, id: &str, value: Option<&str>) -> rusqlite::Result<()> {
        set(&self.db, category, id, value)
    }

    /// One batch corresponds to Baileys keys.set; no partial key advancement.
    pub fn set_batch(&mut self, entries: &[AuthMutation<'_>]) -> rusqlite::Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for entry in entries {
            set(&tx, entry.category, entry.id, entry.value)?;
        }
        tx.commit()
    }

    /// Explicit close preserves errors for the owning shutdown path.
    pub fn close(self) -> rusqlite::Result<()> {
        self.db.close().map_err(|(_, error)| error)
    }
}

/// Encoded authentication material. Deliberately has no Debug implementation.
pub struct AuthEntry {
    pub category: String,
    pub id: String,
    pub value: String,
}

pub struct AuthMutation<'a> {
    pub category: &'a str,
    pub id: &'a str,
    pub value: Option<&'a str>,
}

pub(super) fn set(
    db: &Connection,
    category: &str,
    id: &str,
    value: Option<&str>,
) -> rusqlite::Result<()> {
    // The original store skips identical values, including inside keys.set.
    // Check each ordered mutation in the transaction so duplicates and deletes
    // observe earlier entries, without firing triggers for an unchanged row.
    let previous: Option<String> = db
        .query_row(
            "SELECT value FROM auth WHERE category=? AND id=?",
            (category, id),
            |row| row.get(0),
        )
        .optional()?;
    if previous.as_deref() == value {
        return Ok(());
    }
    match value {
        Some(value) => db.execute("INSERT INTO auth VALUES(?,?,?) ON CONFLICT(category,id) DO UPDATE SET value=excluded.value", (category, id, value))?,
        None => db.execute("DELETE FROM auth WHERE category=? AND id=?", (category, id))?,
    };
    Ok(())
}
