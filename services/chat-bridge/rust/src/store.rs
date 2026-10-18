//! ChatBridge persistence. Admission and domain transitions belong to the caller.
pub mod hive;
pub mod local;

use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, hash::hash_json};

#[derive(Debug)]
pub enum StoreError {
    Domain(IvyError),
    Sqlite(rusqlite::Error),
}
impl From<IvyError> for StoreError {
    fn from(value: IvyError) -> Self {
        Self::Domain(value)
    }
}
impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(e) => e.fmt(f),
            Self::Sqlite(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for StoreError {}
impl StoreError {
    /// Match IvyError.from at the original RPC boundary; keep the SQLite cause
    /// available internally instead of turning raw database failures into a new code.
    pub fn into_ivy(self) -> IvyError {
        match self {
            Self::Domain(e) => e,
            Self::Sqlite(_) => IvyError::internal(),
        }
    }
}
pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub object_id: String,
    pub revision: i64,
}
impl Pin {
    pub fn json(&self) -> Json {
        Json::object([
            ("objectId", self.object_id.clone().into()),
            ("revision", Json::Number(self.revision as f64)),
        ])
    }
}

#[derive(Debug)]
pub struct Document {
    pub key: String,
    pub pin: Pin,
    pub value: Json,
    pub metadata: Json,
    pub revision: Json,
}

pub enum Destination<'a> {
    Create {
        parent_id: Option<&'a str>,
        name: &'a str,
    },
    Update {
        object_id: &'a str,
        expected_revision: i64,
    },
}

#[derive(Clone, Copy)]
pub enum Reference<'a> {
    Current(&'a str),
    Pinned(&'a Pin),
}
impl Reference<'_> {
    pub fn json(self) -> Json {
        match self {
            Self::Current(id) => Json::object([("objectId", id.into())]),
            Self::Pinned(pin) => pin.json(),
        }
    }
}

pub fn mutation(scope: &str, step: &str) -> IvyResult<String> {
    Ok(format!(
        "chat:{}",
        &hash_json(&Json::object([
            ("scope", scope.into()),
            ("step", step.into())
        ]))?[7..]
    ))
}
