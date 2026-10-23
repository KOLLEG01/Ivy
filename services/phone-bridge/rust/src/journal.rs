//! Phone's format-7 connection and serialized durable work. Domain SQL remains
//! here; the shared store supplies SQLite and the unchanged receipt archive.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, wire_schema::validate_shared,
};
use ivy_host_runtime::paths::realpath;
use ivy_store::{
    receipts::{ReceiptArchive, ReceiptLimits},
    rusqlite::{Connection, ErrorCode, OptionalExtension, params},
    sqlite::{self, error},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};

mod calls;
mod contracts;
mod dispatch;
mod maintenance;
mod operations;
mod retirement;
mod voice;
mod writes;
pub(crate) use dispatch::NativeAdmission;

use crate::contracts::OUTCOME_BYTES;
const QUEUE_CAPACITY: usize = 128;

#[derive(Clone)]
pub struct PhoneJournalOwner {
    pub host_id: JsString,
    pub service_node_id: JsString,
}
#[derive(Clone, Copy)]
pub struct PhoneJournalLimits {
    pub max_operations: usize,
    pub max_bytes: usize,
    pub max_epochs: usize,
}
impl Default for PhoneJournalLimits {
    fn default() -> Self {
        Self {
            max_operations: 4096,
            max_bytes: 512 * 1024 * 1024,
            max_epochs: 4096,
        }
    }
}
pub struct PhoneJournalOptions {
    pub data_root: PathBuf,
    pub owner: PhoneJournalOwner,
    pub limits: PhoneJournalLimits,
    pub max_concurrent_calls: usize,
}
impl PhoneJournalOptions {
    fn validate(&self) -> IvyResult<()> {
        require(
            (1..=32).contains(&self.max_concurrent_calls),
            "invalid_arguments",
            "Call concurrency must be between 1 and 32.",
        )?;
        require(
            self.data_root.is_absolute(),
            "invalid_arguments",
            "Phone journal requires an absolute owned data directory.",
        )?;
        validate_shared("Identifier", &IvyJson::String(self.owner.host_id.clone()))?;
        validate_shared(
            "Identifier",
            &IvyJson::String(self.owner.service_node_id.clone()),
        )?;
        require(
            (1..=100_000).contains(&self.limits.max_operations)
                && (OUTCOME_BYTES..=8 * 1024usize.pow(3)).contains(&self.limits.max_bytes)
                && (1..=100_000).contains(&self.limits.max_epochs),
            "invalid_arguments",
            "Phone journal limits are outside their bounded range.",
        )
    }
}

type Job = Box<dyn FnOnce(&JournalDb<'_>) + Send>;
struct Admission {
    sender: Option<mpsc::Sender<Job>>,
}
struct Inner {
    admission: Mutex<Admission>,
    closed: watch::Receiver<Option<IvyResult<()>>>,
    data_root: PathBuf,
    limits: PhoneJournalLimits,
}

/// One worker creates, uses and closes the original connection. After a command
/// enters its bounded queue, dropping its observer cannot cancel its transaction.
/// The remaining Phone command/admission operations extend this same owner.
#[derive(Clone)]
pub struct PhoneJournal(Arc<Inner>);
#[derive(Clone)]
pub struct PhoneJournalClose(watch::Receiver<Option<IvyResult<()>>>);
impl PhoneJournalClose {
    pub async fn wait(&self) -> IvyResult<()> {
        let mut result = self.0.clone();
        loop {
            if let Some(value) = result.borrow_and_update().clone() {
                return value;
            }
            result.changed().await.map_err(|_| unavailable())?;
        }
    }
}
impl PhoneJournal {
    pub async fn open(options: PhoneJournalOptions) -> IvyResult<Self> {
        options.validate()?;
        let limits = options.limits;
        let (sender, mut receiver) = mpsc::channel::<Job>(QUEUE_CAPACITY);
        let (opened, ready) = oneshot::channel();
        let (finished, closed) = watch::channel(None);
        std::thread::Builder::new()
            .name("phone-journal".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let (connection, root) = match open_database(&options) {
                        Ok(value) => value,
                        Err(cause) => {
                            let _ = opened.send(Err(cause));
                            return Ok(());
                        }
                    };
                    let db = match JournalDb::new(&connection, &options) {
                        Ok(value) => value,
                        Err(cause) => {
                            let _ = opened.send(Err(cause));
                            return Ok(());
                        }
                    };
                    // If opening lost its observer, sender is dropped and no
                    // operation can reach the connection. Close on this worker.
                    let _ = opened.send(Ok(root));
                    while let Some(job) = receiver.blocking_recv() {
                        job(&db);
                    }
                    drop(db);
                    connection.flush_prepared_statement_cache();
                    connection.close().map_err(|(_, cause)| error(cause))
                }))
                .unwrap_or_else(|_| Err(unavailable()));
                finished.send_replace(Some(outcome));
            })
            .map_err(|cause| IvyError::new("storage_unavailable", cause.to_string()))?;
        let data_root = ready.await.map_err(|_| unavailable())??;
        Ok(Self(Arc::new(Inner {
            admission: Mutex::new(Admission {
                sender: Some(sender),
            }),
            closed,
            data_root,
            limits,
        })))
    }
    pub fn data_root(&self) -> &Path {
        &self.0.data_root
    }
    pub fn limits(&self) -> PhoneJournalLimits {
        self.0.limits
    }
    #[cfg(test)]
    pub(crate) async fn fixture_db<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Connection) -> IvyResult<T> + Send + 'static,
    ) -> IvyResult<T> {
        self.request(move |owner| work(owner.db)).await
    }
    async fn request<T: Send + 'static>(
        &self,
        work: impl FnOnce(&JournalDb<'_>) -> IvyResult<T> + Send + 'static,
    ) -> IvyResult<T> {
        let sender = self
            .0
            .admission
            .lock()
            .unwrap()
            .sender
            .clone()
            .ok_or_else(closed_error)?;
        let permit = sender.reserve_owned().await.map_err(|_| unavailable())?;
        let (reply, result) = oneshot::channel();
        {
            let admission = self.0.admission.lock().unwrap();
            require(
                admission.sender.is_some(),
                "internal_error",
                "Phone journal is closed.",
            )?;
            permit.send(Box::new(move |db| {
                let _ = reply.send(work(db));
            }));
        }
        result.await.map_err(|_| unavailable())?
    }
    /// Revoke admission synchronously, then drain every admitted original and
    /// await connection close. Every clone observes the same completion result.
    pub fn close(&self) -> PhoneJournalClose {
        self.0.admission.lock().unwrap().sender.take();
        PhoneJournalClose(self.0.closed.clone())
    }
}

fn closed_error() -> IvyError {
    IvyError::new("internal_error", "Phone journal is closed.")
}
fn unavailable() -> IvyError {
    IvyError::new("storage_unavailable", "The Phone journal worker was lost.")
        .with_outcome(Outcome::Unknown)
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn io_error(cause: std::io::Error) -> IvyError {
    IvyError::new("storage_unavailable", cause.to_string())
}

struct JournalDb<'a> {
    db: &'a Connection,
    archive: ReceiptArchive<'a>,
    limits: PhoneJournalLimits,
    max_concurrent_calls: usize,
}
fn open_database(options: &PhoneJournalOptions) -> IvyResult<(Connection, PathBuf)> {
    let data_root = &options.data_root;
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(&data_root).map_err(io_error)?;
    let root = realpath(&data_root).map_err(io_error)?;
    let filename = data_root.join("phone-commands.sqlite");
    let mut file = fs::OpenOptions::new();
    file.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file.mode(0o600);
    }
    match file.open(&filename) {
        Ok(created) => drop(created),
        Err(cause) if cause.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(cause) => return Err(io_error(cause)),
    }
    let db = sqlite::open(
        &filename,
        sqlite::OpenOptions {
            busy_timeout: Duration::from_millis(100),
            read_only: false,
        },
    )
    .map_err(error)?;
    db.execute_batch("PRAGMA locking_mode=EXCLUSIVE; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; BEGIN EXCLUSIVE; PRAGMA user_version; COMMIT;")
            .map_err(|cause| match &cause {
                ivy_store::rusqlite::Error::SqliteFailure(code, _)
                    if matches!(code.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) =>
                {
                    IvyError::new("phone_owner_already_running", "Another Phone owner holds the command journal.")
                }
                _ => error(cause),
            })?;
    let format: i64 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(error)?;
    require(
        format == 0 || format == 7,
        "unsupported_storage",
        "Phone journal requires format 7.",
    )?;
    transaction(&db, || {
        db.execute_batch(
                "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
          CREATE TABLE IF NOT EXISTS epochs(epoch TEXT PRIMARY KEY) STRICT;
          CREATE TABLE IF NOT EXISTS commands(operation_id TEXT PRIMARY KEY,epoch TEXT NOT NULL,call_id TEXT,method TEXT NOT NULL,phase TEXT NOT NULL,value TEXT NOT NULL) STRICT;
          CREATE INDEX IF NOT EXISTS commands_by_epoch ON commands(epoch,phase);
          CREATE INDEX IF NOT EXISTS commands_by_call ON commands(call_id,method);
          CREATE TABLE IF NOT EXISTS calls(call_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL UNIQUE,value TEXT NOT NULL) STRICT;
          CREATE TABLE IF NOT EXISTS targets(call_id TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
          CREATE TABLE IF NOT EXISTS active_calls(call_id TEXT PRIMARY KEY) STRICT;
          PRAGMA user_version=7;",
            ).map_err(error)?;
        let identity = ivy_contracts::json::canonical(
            &IvyJson::object([
                ("hostId", IvyJson::String(options.owner.host_id.clone())),
                (
                    "serviceNodeId",
                    IvyJson::String(options.owner.service_node_id.clone()),
                ),
            ]),
            ivy_contracts::limits::MANAGEMENT_FRAME_BYTES,
        )?;
        require(
            meta(&db, "owner")?.is_none_or(|prior| prior == identity),
            "target_conflict",
            "Phone data belongs to another host or Service Node.",
        )?;
        set_meta(&db, "owner", &identity)?;
        for (key, sql) in [
            ("count", "SELECT count(*) FROM commands"),
            ("epochs", "SELECT count(*) FROM epochs"),
            ("calls", "SELECT count(*) FROM calls"),
            ("targets", "SELECT count(*) FROM targets"),
        ] {
            let count: i64 = db.query_row(sql, [], |row| row.get(0)).map_err(error)?;
            set_meta(&db, key, &count.to_string())?;
        }
        Ok(())
    })?;
    Ok((db, root))
}
impl<'a> JournalDb<'a> {
    fn new(db: &'a Connection, options: &PhoneJournalOptions) -> IvyResult<Self> {
        let archive = ReceiptArchive::new(db, ReceiptLimits::default())?;
        db.execute_batch("CREATE INDEX IF NOT EXISTS receipt_archive_commands_by_call
        ON receipt_archive(json_extract(value,'$.intent.callId'),json_extract(value,'$.intent.method')) WHERE kind='command';
        CREATE TRIGGER IF NOT EXISTS retired_command_insert BEFORE INSERT ON commands
        WHEN EXISTS(SELECT 1 FROM receipt_archive_retired WHERE kind='command' AND key=NEW.operation_id)
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;").map_err(error)?;
        Ok(Self {
            db,
            archive,
            limits: options.limits,
            max_concurrent_calls: options.max_concurrent_calls,
        })
    }
    fn transaction<T>(&self, work: impl FnOnce(&Self) -> IvyResult<T>) -> IvyResult<T> {
        transaction(self.db, || work(self))
    }
    fn meta(&self, key: &str) -> IvyResult<Option<String>> {
        meta(self.db, key)
    }
    fn set_meta(&self, key: &str, value: &str) -> IvyResult<()> {
        set_meta(self.db, key, value)
    }
}
fn transaction<T>(db: &Connection, work: impl FnOnce() -> IvyResult<T>) -> IvyResult<T> {
    db.execute_batch("BEGIN IMMEDIATE").map_err(error)?;
    match work().and_then(|value| {
        db.execute_batch("COMMIT").map_err(error)?;
        Ok(value)
    }) {
        Ok(value) => Ok(value),
        Err(cause) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(cause)
        }
    }
}
fn meta(db: &Connection, key: &str) -> IvyResult<Option<String>> {
    db.prepare_cached("SELECT value FROM meta WHERE key=?")
        .map_err(error)?
        .query_row([key], |row| row.get(0))
        .optional()
        .map_err(error)
}
fn set_meta(db: &Connection, key: &str, value: &str) -> IvyResult<()> {
    db.prepare_cached(
        "INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .map_err(error)?
    .execute(params![key, value])
    .map(|_| ())
    .map_err(error)
}

#[cfg(test)]
#[path = "../../../../tests/rust/phone_voice_journal.rs"]
mod tests;
