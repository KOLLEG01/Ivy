//! Adapter over the existing TaskBoard journal's blobs table; owns no schema or transaction.
use crate::evidence_bytes::{BlobStore, ChunkKey};
use ivy_contracts::{IvyError, JsString};
use ivy_store::rusqlite::{self, Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobPin {
    pub object_id: JsString,
    pub revision: u64,
}

#[derive(Debug)]
pub enum BlobError {
    Domain(IvyError),
    Sqlite(rusqlite::Error),
}
impl From<IvyError> for BlobError {
    fn from(error: IvyError) -> Self {
        Self::Domain(error)
    }
}
impl From<rusqlite::Error> for BlobError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
impl std::fmt::Display for BlobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(error) => write!(f, "{}: {}", error.code, error.message),
            Self::Sqlite(error) => std::fmt::Display::fmt(error, f),
        }
    }
}
impl std::error::Error for BlobError {}

pub struct LocalBlobs<'a> {
    connection: &'a Connection,
}
impl<'a> LocalBlobs<'a> {
    pub fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    pub fn save_blob(
        &self,
        key: &JsString,
        name: &JsString,
        parent: &JsString,
        bytes: &[u8],
    ) -> Result<BlobPin, BlobError> {
        // node:sqlite binds JavaScript strings as UTF-8 with lone surrogates replaced.
        let (key, name, parent) = (sqlite_text(key), sqlite_text(name), sqlite_text(parent));
        let existing: Option<(String, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT id,bytes FROM blobs WHERE key=? AND name=? AND parent_id=?",
                params![key, name, parent],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, saved)) = existing {
            if saved != bytes {
                return Err(IvyError::new(
                    "revision_conflict",
                    "The local evidence identity differs.",
                )
                .into());
            }
            return Ok(BlobPin {
                object_id: id.into(),
                revision: 1,
            });
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.connection.execute(
            "INSERT INTO blobs VALUES(?,?,?,?,?)",
            params![id, key, name, parent, bytes],
        )?;
        Ok(BlobPin {
            object_id: id.into(),
            revision: 1,
        })
    }

    pub fn read_blob(
        &self,
        key: &JsString,
        pin: &BlobPin,
        parent: &JsString,
    ) -> Result<Vec<u8>, BlobError> {
        if pin.revision != 1 {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "Local evidence blobs have one revision.",
            )
            .into());
        }
        let row = self
            .connection
            .query_row(
                "SELECT bytes FROM blobs WHERE id=? AND key=? AND parent_id=?",
                params![
                    sqlite_text(&pin.object_id),
                    sqlite_text(key),
                    sqlite_text(parent)
                ],
                |row| row.get(0),
            )
            .optional()?;
        row.ok_or_else(|| {
            IvyError::new(
                "task_board_evidence_mismatch",
                "The local evidence blob is unavailable.",
            )
            .into()
        })
    }
}
fn sqlite_text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}

impl BlobStore for LocalBlobs<'_> {
    type Pin = BlobPin;
    type Error = BlobError;
    fn save(
        &self,
        key: ChunkKey,
        name: &str,
        parent: &JsString,
        bytes: &[u8],
    ) -> Result<BlobPin, BlobError> {
        self.save_blob(&key.as_str().into(), &name.into(), parent, bytes)
    }
    fn read(&self, key: ChunkKey, pin: &BlobPin, parent: &JsString) -> Result<Vec<u8>, BlobError> {
        self.read_blob(&key.as_str().into(), pin, parent)
    }
}
