//! Native client storage over the existing account database.
use super::auth::{AuthAccount, set};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyJson as Json,
    json::{parse, stringify},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::sync::{Mutex, OnceLock};
use wacore::store::error::{Result, StoreError};

/// Owns the account lock for the whole native client's lifetime. SQLite calls
/// finish under one mutex; no lock is held across an async suspension.
pub struct NativeAuthStore(
    pub(super) Mutex<Option<AuthAccount>>,
    pub(super) OnceLock<super::journal_owner::Journal>,
);

impl NativeAuthStore {
    pub fn new(account: AuthAccount) -> Self {
        Self(Mutex::new(Some(account)), OnceLock::new())
    }

    pub(super) fn bind_journal(
        &self,
        journal: super::journal_owner::Journal,
    ) -> ivy_contracts::IvyResult<()> {
        self.1
            .set(journal)
            .map_err(|_| ivy_contracts::IvyError::internal())
    }

    pub fn close(&self) -> ivy_contracts::IvyResult<()> {
        let account = self
            .0
            .lock()
            .map_err(|_| ivy_contracts::IvyError::internal())?
            .take();
        account.map_or(Ok(()), AuthAccount::close)
    }

    pub(super) fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut owner = self.0.lock().map_err(|_| invalid("account lock"))?;
        let account = owner.as_mut().ok_or_else(|| invalid("closed account"))?;
        f(&account.store().db)
    }

    pub(super) fn write<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut owner = self.0.lock().map_err(|_| invalid("account lock"))?;
        let account = owner.as_mut().ok_or_else(|| invalid("closed account"))?;
        let tx = account
            .store()
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database)?;
        let result = f(&tx)?;
        tx.commit().map_err(database)?;
        Ok(result)
    }
}

pub(super) fn database(error: rusqlite::Error) -> StoreError {
    StoreError::Database(Box::new(error))
}
pub(super) fn invalid(field: &str) -> StoreError {
    StoreError::Validation(format!("Invalid authentication {field}."))
}
pub(super) fn protocol(error: impl std::error::Error + Send + Sync + 'static) -> StoreError {
    StoreError::Serialization(Box::new(error))
}
pub(super) fn get_raw(db: &Connection, category: &str, id: &str) -> Result<Option<String>> {
    db.query_row(
        "SELECT value FROM auth WHERE category=? AND id=?",
        (category, id),
        |r| r.get(0),
    )
    .optional()
    .map_err(database)
}
pub(super) fn get(db: &Connection, category: &str, id: &str) -> Result<Option<Json>> {
    get_raw(db, category, id)?
        .map(|raw| parse(&raw, usize::MAX).map_err(|_| invalid("JSON")))
        .transpose()
}
pub(super) fn put(db: &Connection, category: &str, id: &str, value: Option<&Json>) -> Result<()> {
    if let Some(value) = value {
        // Preserve the exact encoded row on a semantic no-op, including its
        // whitespace and extension fields.
        if get(db, category, id)?.as_ref() == Some(value) {
            return Ok(());
        }
        let raw = stringify(value, usize::MAX).map_err(|_| invalid("JSON"))?;
        set(db, category, id, Some(&raw)).map_err(database)
    } else {
        set(db, category, id, None).map_err(database)
    }
}
pub(super) fn entries(db: &Connection, category: &str) -> Result<Vec<(String, Json)>> {
    let mut q = db
        .prepare("SELECT id,value FROM auth WHERE category=? ORDER BY id")
        .map_err(database)?;
    let rows = q
        .query_map([category], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(database)?;
    rows.map(|row| {
        let (id, raw) = row.map_err(database)?;
        Ok((id, parse(&raw, usize::MAX).map_err(|_| invalid("JSON"))?))
    })
    .collect()
}
pub(super) fn field<'a>(v: &'a Json, key: &str) -> Result<&'a Json> {
    v.get(key).ok_or_else(|| invalid(key))
}
pub(super) fn text(v: &Json) -> Result<String> {
    v.as_str().map_err(|_| invalid("text"))
}
pub(super) fn integer(v: &Json) -> Result<i64> {
    v.as_f64()
        .filter(|v| v.is_finite() && v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0)
        .map(|v| v as i64)
        .ok_or_else(|| invalid("integer"))
}
pub(super) fn uint(v: &Json) -> Result<u32> {
    u32::try_from(integer(v)?).map_err(|_| invalid("key ID"))
}
pub(super) fn number(n: i64) -> Result<Json> {
    if n.unsigned_abs() > 9_007_199_254_740_991 {
        return Err(invalid("integer"));
    }
    Ok(Json::Number(n as f64))
}
pub(super) fn bytes(v: &Json) -> Result<Vec<u8>> {
    let v = if v.as_object().is_some() {
        if text(field(v, "type")?)? != "Buffer" {
            return Err(invalid("bytes"));
        }
        field(v, "data")?
    } else {
        v
    };
    if let Some(values) = v.as_array() {
        return values
            .iter()
            .map(|v| u8::try_from(integer(v)?).map_err(|_| invalid("byte")))
            .collect();
    }
    STANDARD.decode(text(v)?).map_err(|_| invalid("base64"))
}
pub(super) fn buffer(value: &[u8]) -> Json {
    Json::object([
        ("type", "Buffer".into()),
        ("data", STANDARD.encode(value).into()),
    ])
}
pub(super) fn object(v: Option<Json>) -> Result<Json> {
    match v {
        Some(v) if v.as_object().is_some() => Ok(v),
        None => Ok(Json::object(std::iter::empty::<(&str, Json)>())),
        _ => Err(invalid("object")),
    }
}
pub(super) fn now_ms() -> Result<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_millis()).ok())
        .ok_or_else(|| invalid("clock"))
}
pub(super) fn key(parts: &[&str]) -> String {
    // Length-delimited components keep protocol IDs containing separators
    // distinct, without constraining valid user or message text.
    parts.iter().map(|s| format!("{}:{s}", s.len())).collect()
}

/// Native Signal addresses keep their namespace; the established database
/// spells the domain number before the device ID.
pub(super) fn signal_id(address: &str) -> Result<String> {
    let address = address.strip_suffix(".0").unwrap_or(address);
    let (user, server) = address
        .rsplit_once('@')
        .ok_or_else(|| invalid("Signal address"))?;
    let (user, device) = user.rsplit_once(':').unwrap_or((user, "0"));
    let device = device.parse::<u32>().map_err(|_| invalid("device ID"))?;
    if user.is_empty() {
        return Err(invalid("Signal address"));
    }
    let domain = match server {
        "s.whatsapp.net" | "c.us" => "",
        "lid" => "_1",
        "hosted" => "_128",
        "hosted.lid" => "_129",
        _ => return Err(invalid("Signal namespace")),
    };
    Ok(format!("{user}{domain}.{device}"))
}
pub(super) fn sender_id(address: &str) -> Result<String> {
    let (group, sender) = address
        .split_once(':')
        .ok_or_else(|| invalid("sender address"))?;
    let id = signal_id(sender)?;
    let (user, device) = id
        .rsplit_once('.')
        .ok_or_else(|| invalid("sender address"))?;
    Ok(format!("{group}::{user}::{device}"))
}
