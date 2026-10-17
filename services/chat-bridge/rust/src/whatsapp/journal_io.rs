//! Ordered SQLite transport writer and synchronous memory view. A flush is a
//! committed-write fence, never just acknowledgement of queue admission.
use rusqlite::{Connection, params_from_iter, types::Value};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS inbox (id TEXT PRIMARY KEY,sender TEXT NOT NULL,jid TEXT NOT NULL,message TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending',request TEXT,received INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS outgoing (sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,jid TEXT NOT NULL,text TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending');
CREATE TABLE IF NOT EXISTS retired_inbox(id TEXT PRIMARY KEY,fingerprint TEXT NOT NULL,retired INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS retired_outgoing(id TEXT PRIMARY KEY,fingerprint TEXT NOT NULL);
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

struct Statement {
    sql: &'static str,
    args: Vec<Value>,
}
enum Write {
    Batch(Vec<Statement>),
    Fence(mpsc::SyncSender<StorageResult<()>>),
}

pub struct JournalIo {
    db: Connection,
    writer: Option<mpsc::SyncSender<Write>>,
    worker: Option<thread::JoinHandle<StorageResult<()>>>,
    failure: Arc<AtomicBool>,
    batch: Option<Vec<Statement>>,
}

impl JournalIo {
    /// The owner establishes the protected data directory before opening.
    /// Call on the database owner task, outside the asynchronous reactor.
    pub fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref();
        let disk = Connection::open(path)?;
        disk.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;",
        )?;
        disk.execute_batch(SCHEMA)?;
        let db = Connection::open_in_memory()?;
        db.execute_batch(SCHEMA)?;
        db.execute_batch("BEGIN")?;
        for table in [
            "kv",
            "inbox",
            "outgoing",
            "retired_inbox",
            "retired_outgoing",
        ] {
            let mut query = disk.prepare(&format!("SELECT * FROM {table}"))?;
            let columns = query.column_count();
            let sql = format!(
                "INSERT INTO {table} VALUES({})",
                vec!["?"; columns].join(",")
            );
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? {
                let values = (0..columns)
                    .map(|i| row.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                db.execute(&sql, params_from_iter(values))?;
            }
        }
        // sqlite_sequence is durable even if the newest output row was removed.
        let mut sequences = disk.prepare("SELECT name,seq FROM sqlite_sequence")?;
        let rows = sequences.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (name, sequence) = row?;
            db.execute("DELETE FROM sqlite_sequence WHERE name=?", [&name])?;
            db.execute("INSERT INTO sqlite_sequence VALUES(?,?)", (&name, sequence))?;
        }
        drop(sequences);
        db.execute_batch("COMMIT")?;
        let failure = Arc::new(AtomicBool::new(false));
        let (writer, worker) = if path == Path::new(":memory:") {
            disk.close().map_err(|_| StorageUnavailable)?;
            (None, None)
        } else {
            let (send, receive) = mpsc::sync_channel(1);
            let failed = Arc::clone(&failure);
            let worker = thread::Builder::new()
                .name("chat-whatsapp-writer".into())
                .spawn(move || {
                    while let Ok(message) = receive.recv() {
                        match message {
                            Write::Batch(batch) if !failed.load(Ordering::Acquire) => {
                                let result = commit(&disk, batch);
                                if result.is_err() {
                                    failed.store(true, Ordering::Release);
                                }
                            }
                            Write::Batch(_) => {}
                            Write::Fence(reply) => {
                                let _ = reply.send(if failed.load(Ordering::Acquire) {
                                    Err(StorageUnavailable)
                                } else {
                                    Ok(())
                                });
                            }
                        }
                    }
                    disk.close().map_err(|_| StorageUnavailable)
                })
                .map_err(|_| StorageUnavailable)?;
            (Some(send), Some(worker))
        };
        Ok(Self {
            db,
            writer,
            worker,
            failure,
            batch: None,
        })
    }

    /// Read access for the domain journal. Mutations must go through write so
    /// memory and durable order cannot diverge during ordinary operation.
    pub fn connection(&self) -> &Connection {
        &self.db
    }

    pub fn write(&mut self, sql: &'static str, args: Vec<Value>) -> StorageResult<()> {
        self.healthy()?;
        self.db.execute(sql, params_from_iter(&args))?;
        let statement = Statement { sql, args };
        if let Some(batch) = self.batch.as_mut() {
            batch.push(statement);
        } else {
            self.submit(vec![statement])?;
        }
        Ok(())
    }

    pub fn transaction<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> StorageResult<T>,
    ) -> StorageResult<T> {
        self.healthy()?;
        if self.batch.is_some() {
            return body(self);
        }
        self.db.execute_batch("BEGIN")?;
        self.batch = Some(Vec::new());
        let result = body(self);
        let batch = self.batch.take().expect("transaction owns batch");
        match result {
            Ok(value) => {
                self.db.execute_batch("COMMIT")?;
                self.submit(batch)?;
                Ok(value)
            }
            Err(error) => {
                self.db.execute_batch("ROLLBACK")?;
                Err(error)
            }
        }
    }

    pub fn flush(&self) -> StorageResult<()> {
        self.healthy()?;
        // No fence may claim a not-yet-enqueued transaction has committed.
        if self.batch.is_some() {
            return Err(StorageUnavailable);
        }
        if let Some(writer) = &self.writer {
            let (send, receive) = mpsc::sync_channel(1);
            writer
                .send(Write::Fence(send))
                .map_err(|_| StorageUnavailable)?;
            receive.recv().map_err(|_| StorageUnavailable)??;
        }
        self.healthy()
    }

    pub fn close(mut self) -> StorageResult<()> {
        let flushed = self.flush();
        self.writer.take();
        let joined = self.join();
        // Drop closes the memory view after the durable writer has exited.
        flushed.and(joined)
    }

    fn healthy(&self) -> StorageResult<()> {
        if self.failure.load(Ordering::Acquire) {
            Err(StorageUnavailable)
        } else {
            Ok(())
        }
    }
    fn submit(&self, batch: Vec<Statement>) -> StorageResult<()> {
        if !batch.is_empty()
            && let Some(writer) = &self.writer
        {
            if writer.send(Write::Batch(batch)).is_err() {
                self.failure.store(true, Ordering::Release);
                return Err(StorageUnavailable);
            }
        }
        Ok(())
    }
    fn join(&mut self) -> StorageResult<()> {
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| StorageUnavailable)??;
        }
        Ok(())
    }
}

impl Drop for JournalIo {
    fn drop(&mut self) {
        // Explicit close reports failures. Drop still drains accepted batches and
        // joins the owner, so a normal unwinding path cannot orphan the writer.
        self.writer.take();
        let _ = self.join();
    }
}

fn commit(db: &Connection, batch: Vec<Statement>) -> StorageResult<()> {
    db.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        for statement in batch {
            db.execute(statement.sql, params_from_iter(statement.args))?;
        }
        db.execute_batch("COMMIT")
    })();
    if result.is_err() {
        let _ = db.execute_batch("ROLLBACK");
    }
    result.map_err(Into::into)
}
