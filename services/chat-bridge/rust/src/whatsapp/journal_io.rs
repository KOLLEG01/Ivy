//! One durable WAL database, owned by the transport database worker.
use rusqlite::{Connection, params_from_iter, types::Value};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS inbox (id TEXT PRIMARY KEY,sender TEXT NOT NULL,jid TEXT NOT NULL,message TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending',request TEXT,received INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS outgoing (sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,jid TEXT NOT NULL,text TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending');
CREATE TABLE IF NOT EXISTS retired_inbox(id TEXT PRIMARY KEY,fingerprint TEXT NOT NULL,retired INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS retired_outgoing(id TEXT PRIMARY KEY,fingerprint TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS file_bytes(id TEXT PRIMARY KEY,byte_length INTEGER NOT NULL,content BLOB NOT NULL,CHECK(byte_length>0 AND byte_length=length(content)));
CREATE INDEX IF NOT EXISTS inbox_pending ON inbox(received,id) WHERE state='pending';
CREATE INDEX IF NOT EXISTS outgoing_pending ON outgoing(jid,sequence) WHERE state<>'sent';";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageUnavailable;
impl std::fmt::Display for StorageUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WhatsApp transport storage failed.")
    }
}
impl std::error::Error for StorageUnavailable {}
impl From<rusqlite::Error> for StorageUnavailable {
    fn from(_: rusqlite::Error) -> Self {
        Self
    }
}
pub type StorageResult<T> = Result<T, StorageUnavailable>;

pub struct JournalIo {
    db: Connection,
    failed: bool,
}
impl JournalIo {
    /// The owner establishes the protected data directory before opening.
    /// Call on the database owner task, outside the asynchronous reactor.
    pub fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;",
        )?;
        db.execute_batch(SCHEMA)?;
        let mut io = Self { db, failed: false };
        crate::files::adopt_file_bytes(&mut io).map_err(|_| StorageUnavailable)?;
        Ok(io)
    }
    pub fn connection(&self) -> &Connection {
        &self.db
    }
    pub fn write(&mut self, sql: &'static str, args: Vec<Value>) -> StorageResult<()> {
        self.healthy()?;
        if self.db.execute(sql, params_from_iter(args)).is_err() {
            self.failed = true;
            return Err(StorageUnavailable);
        }
        Ok(())
    }
    pub fn transaction<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> StorageResult<T>,
    ) -> StorageResult<T> {
        self.try_transaction(body)
    }
    pub fn try_transaction<T, E: From<StorageUnavailable>>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        self.healthy()?;
        if !self.db.is_autocommit() {
            return body(self);
        }
        self.db
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(StorageUnavailable::from)?;
        let result = body(self);
        match result {
            Ok(value) if !self.failed => {
                if self.db.execute_batch("COMMIT").is_err() {
                    self.failed = true;
                    let _ = self.db.execute_batch("ROLLBACK");
                    return Err(StorageUnavailable.into());
                }
                Ok(value)
            }
            other => {
                if self.db.execute_batch("ROLLBACK").is_err() {
                    self.failed = true;
                    return Err(StorageUnavailable.into());
                }
                match other {
                    Err(error) => Err(error),
                    Ok(_) => Err(StorageUnavailable.into()),
                }
            }
        }
    }
    /// Every successful mutation already committed. Never confirm an open
    /// transaction or let a failed write be mistaken for durable progress.
    pub fn flush(&self) -> StorageResult<()> {
        self.healthy()?;
        if !self.db.is_autocommit() {
            return Err(StorageUnavailable);
        }
        Ok(())
    }
    pub fn close(self) -> StorageResult<()> {
        let committed = self.flush();
        let closed = self.db.close().map_err(|_| StorageUnavailable);
        committed.and(closed)
    }
    fn healthy(&self) -> StorageResult<()> {
        if self.failed {
            Err(StorageUnavailable)
        } else {
            Ok(())
        }
    }
}
