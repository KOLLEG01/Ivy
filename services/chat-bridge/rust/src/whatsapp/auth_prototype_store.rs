//! NAB-3P synthetic owner. Included only by the integration-test harness.
//! Ordinary AuthStore::open/startup never initializes this receipt contract.
#[path = "auth.rs"]
mod legacy;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, Outcome,
    hash::digest,
    json::{parse, stringify},
};
use ivy_store::sqlite::OwnerLock;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::{collections::BTreeMap, path::Path};

pub fn unknown() -> IvyError {
    IvyError::new(
        "outcome_unknown",
        "The original synthetic authentication owner must be reconciled.",
    )
    .with_outcome(Outcome::Unknown)
}
pub fn refused() -> IvyError {
    IvyError::new(
        "auth_retiring",
        "The synthetic owner did not admit this work.",
    )
    .with_outcome(Outcome::NotExecuted)
}
pub fn encode(value: &Json) -> String {
    stringify(value, usize::MAX).expect("private JSON")
}
pub fn text(value: &Json, key: &str) -> IvyResult<String> {
    value
        .required(key)
        .and_then(Json::as_str)
        .map_err(|_| unknown())
}
fn put(db: &Connection, key: &str, value: &Json) -> IvyResult<()> {
    db.execute("INSERT INTO chat_auth_receipt(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", (key, encode(value))).map_err(|_| unknown())?;
    Ok(())
}
fn get(db: &Connection, key: &str) -> IvyResult<Json> {
    let value: String = db
        .query_row(
            "SELECT value FROM chat_auth_receipt WHERE key=?",
            [key],
            |r| r.get(0),
        )
        .map_err(|_| unknown())?;
    parse(&value, usize::MAX).map_err(|_| unknown())
}
pub struct Grant {
    pub id: String,
    pub token: String,
}
struct Live {
    root: String,
    token: String,
}
pub struct Store {
    auth: Option<legacy::AuthStore>,
    lock: Option<OwnerLock>,
    pub epoch: String,
    next: u64,
    live: BTreeMap<String, Live>,
    pub closing: bool,
    pub fenced: bool,
    quiesced: bool,
    released: bool,
}
impl Store {
    /// This explicit initializer is compiled only in the synthetic test harness.
    /// A table already present, even empty or malformed, is never reinitialized.
    pub fn initialize_synthetic(path: &Path, lock: &Path) -> IvyResult<Self> {
        Self::open(path, lock, true)
    }
    pub fn reopen_synthetic(path: &Path, lock: &Path) -> IvyResult<Self> {
        Self::open(path, lock, false)
    }
    fn open(path: &Path, lock: &Path, initialize: bool) -> IvyResult<Self> {
        let lock = OwnerLock::acquire(lock, true).map_err(|_| refused())?;
        if !initialize {
            // Validate read-only first: missing/malformed proof must cause no DDL.
            let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| unknown())?;
            if text(&get(&db, "format")?, "contract")? != "chat-auth-owner-v1"
                || get(&db, "format")?.get("version").and_then(Json::as_f64) != Some(1.0)
            {
                return Err(unknown());
            }
            let owner = get(&db, "owner")?;
            let epoch = text(&owner, "epoch")?;
            let next = text(&owner, "nextOperation")?;
            let ordinal = next.parse::<u64>().map_err(|_| unknown())?;
            if owner.get("version").and_then(Json::as_f64) != Some(1.0)
                || ordinal == 0
                || ordinal.to_string() != next
                || !text(&owner, "initOperation")?.starts_with(&(epoch.clone() + ":"))
                || !text(&owner, "closeOperation")?.starts_with(&(epoch + ":"))
            {
                return Err(unknown());
            }
            if text(&owner, "state")? != "quiesced" {
                return Err(unknown());
            }
            Self::validate_proof(
                owner.required("quiescence").map_err(|_| unknown())?,
                &text(&owner, "epoch")?,
                &text(&owner, "closeOperation")?,
            )?;
            let closed = get(&db, &format!("op/{}", text(&owner, "closeOperation")?))?;
            let initialized = get(&db, &format!("op/{}", text(&owner, "initOperation")?))?;
            if text(&initialized, "method")? != "initialize"
                || text(&initialized, "state")? != "completed"
                || text(&initialized, "outcome")? != "value"
            {
                return Err(unknown());
            }
            if text(&closed, "epoch")? != text(&owner, "epoch")?
                || text(&closed, "method")? != "quiesce"
                || text(&closed, "state")? != "completed"
                || text(&closed, "outcome")? != "value"
                || text(&closed, "resultDigest")?
                    != digest(
                        encode(owner.required("quiescence").map_err(|_| unknown())?).as_bytes(),
                    )
            {
                return Err(unknown());
            }
            let invalid: i64 = db.query_row("SELECT count(*) FROM chat_auth_receipt WHERE key LIKE 'op/%' AND (json_extract(value,'$.state') IS NULL OR json_extract(value,'$.state') NOT IN ('completed','not_executed'))", [], |r| r.get(0)).map_err(|_| unknown())?;
            if invalid != 0 {
                return Err(unknown());
            }
            db.close().map_err(|_| unknown())?;
        }
        let mut auth = legacy::AuthStore::open(path).map_err(|_| unknown())?;
        let epoch = uuid::Uuid::new_v4().to_string();
        let tx = auth
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| unknown())?;
        if initialize {
            let exists: i64 = tx
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='chat_auth_receipt'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|_| unknown())?;
            if exists != 0 {
                return Err(unknown());
            }
            tx.execute_batch("CREATE TABLE chat_auth_receipt(key TEXT NOT NULL PRIMARY KEY,value TEXT NOT NULL);").map_err(|_| unknown())?;
            put(
                &tx,
                "format",
                &Json::object([
                    ("version", Json::Number(1.0)),
                    ("contract", "chat-auth-owner-v1".into()),
                ]),
            )?;
        } else {
            // Keep the old positive receipt, not just its current owner pointer.
            let old = get(&tx, "owner")?;
            put(&tx, &format!("epoch/{}", text(&old, "epoch")?), &old)?;
        }
        let owner = Json::object([
            ("version", Json::Number(1.0)),
            ("epoch", epoch.as_str().into()),
            ("nextOperation", "0".into()),
            ("state", "prepared".into()),
            ("initOperation", Json::Null),
            ("closeOperation", Json::Null),
            ("quiescence", Json::Null),
        ]);
        put(&tx, "owner", &owner)?;
        tx.commit().map_err(|_| unknown())?;
        Ok(Self {
            auth: Some(auth),
            lock: Some(lock),
            epoch,
            next: 0,
            live: BTreeMap::new(),
            closing: false,
            fenced: false,
            quiesced: false,
            released: false,
        })
    }
    fn db(&self) -> IvyResult<&Connection> {
        Ok(&self.auth.as_ref().ok_or_else(unknown)?.db)
    }
    pub fn saved_creds(&self) -> IvyResult<Json> {
        Ok(self
            .auth
            .as_ref()
            .ok_or_else(unknown)?
            .get("creds", "default")
            .map_err(|_| unknown())?
            .map(Json::from)
            .unwrap_or(Json::Null))
    }
    pub fn prepare(
        &mut self,
        kind: &str,
        method: &str,
        parent: Option<(&str, &str)>,
    ) -> IvyResult<Grant> {
        if self.fenced || self.quiesced {
            return Err(unknown());
        }
        if ![
            "init",
            "socket-open",
            "outbound",
            "inbound-crypto",
            "repository",
            "raw-store",
            "credential-save",
            "background-prekey",
            "app-state",
            "close",
        ]
        .contains(&kind)
            || method.is_empty()
        {
            return Err(unknown());
        }
        let root = if let Some((id, token)) = parent {
            let live = self
                .live
                .get(id)
                .filter(|v| v.token == token)
                .ok_or_else(unknown)?;
            Some(live.root.clone())
        } else {
            if self.closing && kind != "close" {
                return Err(refused());
            }
            None
        };
        self.next = self.next.checked_add(1).ok_or_else(unknown)?;
        let id = format!("{}:{}", self.epoch, self.next);
        let token = uuid::Uuid::new_v4().to_string();
        let op = Json::object([
            ("version", Json::Number(1.0)),
            ("epoch", self.epoch.as_str().into()),
            ("operationId", id.as_str().into()),
            (
                "parentOperationId",
                parent.map(|(id, _)| Json::from(id)).unwrap_or(Json::Null),
            ),
            ("kind", kind.into()),
            ("method", method.into()),
            ("state", "prepared".into()),
            ("callbackOrdinal", "0".into()),
            ("outcome", Json::Null),
            ("resultDigest", Json::Null),
            ("libraryErrorName", Json::Null),
        ]);
        let db = &mut self.auth.as_mut().ok_or_else(unknown)?.db;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| unknown())?;
        let mut owner = get(&tx, "owner")?;
        owner.insert("nextOperation", self.next.to_string().into());
        if kind == "init" {
            owner.insert("initOperation", id.as_str().into());
        }
        if kind == "close" {
            owner.insert("closeOperation", id.as_str().into());
        }
        put(&tx, "owner", &owner)?;
        put(&tx, &format!("op/{id}"), &op)?;
        tx.commit().map_err(|_| unknown())?;
        self.live.insert(
            id.clone(),
            Live {
                root: root.unwrap_or_else(|| id.clone()),
                token: token.clone(),
            },
        );
        Ok(Grant { id, token })
    }
    pub fn enter(&mut self, id: &str) -> IvyResult<()> {
        if self.fenced {
            return Err(unknown());
        }
        let db = &mut self.auth.as_mut().ok_or_else(unknown)?.db;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| unknown())?;
        let mut op = get(&tx, &format!("op/{id}"))?;
        if text(&op, "state")? != "prepared" {
            return Err(unknown());
        }
        op.insert("state", "entered".into());
        let mut owner = get(&tx, "owner")?;
        owner.insert(
            "state",
            if self.closing { "retiring" } else { "entered" }.into(),
        );
        put(&tx, &format!("op/{id}"), &op)?;
        put(&tx, "owner", &owner)?;
        tx.commit().map_err(|_| unknown())
    }
    pub fn check(&self, id: &str, token: &str) -> IvyResult<()> {
        if self.fenced || !self.live.get(id).is_some_and(|v| v.token == token) {
            return Err(unknown());
        }
        let op = self.receipt(id)?;
        if text(&op, "epoch")? != self.epoch || text(&op, "state")? != "entered" {
            return Err(unknown());
        }
        Ok(())
    }
    pub fn receipt(&self, id: &str) -> IvyResult<Json> {
        get(self.db()?, &format!("op/{id}"))
    }
    pub fn owner(&self) -> IvyResult<Json> {
        get(self.db()?, "owner")
    }
    pub fn active(&self) -> usize {
        self.live.len()
    }
    pub fn finish(
        &mut self,
        id: &str,
        token: &str,
        outcome: &str,
        result_digest: &str,
        name: Option<&str>,
    ) -> IvyResult<()> {
        self.check(id, token)?;
        if !["value", "library_error", "not_executed"].contains(&outcome)
            || result_digest.len() != 71
            || !result_digest.starts_with("sha256:")
            || !result_digest[7..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(unknown());
        }
        if outcome == "library_error"
            && !name.is_some_and(|n| {
                [
                    "TypeError",
                    "SessionError",
                    "MessageCounterError",
                    "PreKeyError",
                    "Error",
                ]
                .contains(&n)
            })
        {
            return Err(unknown());
        }
        if self.live.iter().any(|(child, live)| {
            child != id
                && self.parent_of(child).as_deref() == Some(id)
                && live.root == self.live[id].root
        }) {
            return Err(unknown());
        }
        let db = &mut self.auth.as_mut().ok_or_else(unknown)?.db;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| unknown())?;
        let mut op = get(&tx, &format!("op/{id}"))?;
        op.insert(
            "state",
            if outcome == "not_executed" {
                "not_executed"
            } else {
                "completed"
            }
            .into(),
        );
        op.insert("outcome", outcome.into());
        op.insert("resultDigest", result_digest.into());
        op.insert(
            "libraryErrorName",
            name.map(Json::from).unwrap_or(Json::Null),
        );
        put(&tx, &format!("op/{id}"), &op)?;
        tx.commit().map_err(|_| unknown())?;
        self.live.remove(id);
        Ok(())
    }
    fn parent_of(&self, id: &str) -> Option<String> {
        self.receipt(id)
            .ok()?
            .get("parentOperationId")?
            .as_str()
            .ok()
    }
    /// Digest the exact Node-supplied private encoding, preserving mutation order.
    pub fn callback(
        &mut self,
        id: &str,
        token: &str,
        ordinal: u64,
        kind: &str,
        encoded: &str,
        expected_digest: &str,
    ) -> IvyResult<Json> {
        self.check(id, token)?;
        if digest(encoded.as_bytes()) != expected_digest || !matches!(kind, "get" | "set") {
            return Err(unknown());
        }
        let entries = parse(encoded, usize::MAX).map_err(|_| unknown())?;
        let entries = entries.as_array().ok_or_else(unknown)?;
        let op = self.receipt(id)?;
        let last = text(&op, "callbackOrdinal")?
            .parse::<u64>()
            .map_err(|_| unknown())?;
        if last.checked_add(1) != Some(ordinal) {
            return Err(unknown());
        }
        let mut values = Vec::new();
        for entry in entries {
            let entry = entry
                .as_array()
                .filter(|a| a.len() == if kind == "get" { 2 } else { 3 })
                .ok_or_else(unknown)?;
            let sql = |v: &Json| {
                v.as_js_string()
                    .map(|s| String::from_utf16_lossy(&s.0))
                    .ok_or_else(unknown)
            };
            values.push((
                sql(&entry[0])?,
                sql(&entry[1])?,
                if kind == "get" || entry[2].is_null() {
                    None
                } else {
                    Some(sql(&entry[2])?)
                },
            ));
        }
        let db = &mut self.auth.as_mut().ok_or_else(unknown)?.db;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| unknown())?;
        let mut rows = Vec::new();
        for (category, key, value) in &values {
            if kind == "set" {
                legacy::set(&tx, category, key, value.as_deref()).map_err(|_| unknown())?;
            } else {
                let value: Option<String> = tx
                    .query_row(
                        "SELECT value FROM auth WHERE category=? AND id=?",
                        (category, key),
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|_| unknown())?;
                rows.push(value.map(Json::from).unwrap_or(Json::Null));
            }
        }
        let receipt = Json::object([
            ("version", Json::Number(1.0)),
            ("operationId", id.into()),
            ("ordinal", ordinal.to_string().into()),
            ("kind", kind.into()),
            ("mutationsDigest", expected_digest.into()),
            ("state", "committed".into()),
        ]);
        put(&tx, &format!("cb/{id}/{ordinal}"), &receipt)?;
        let mut op = get(&tx, &format!("op/{id}"))?;
        op.insert("callbackOrdinal", ordinal.to_string().into());
        put(&tx, &format!("op/{id}"), &op)?;
        tx.commit().map_err(|_| unknown())?;
        Ok(if kind == "get" {
            Json::Array(rows)
        } else {
            Json::Null
        })
    }
    pub fn fence(&mut self) {
        self.fenced = true;
        if let Ok(db) = self.db() {
            if let Ok(mut owner) = get(db, "owner") {
                owner.insert("state", "unknown".into());
                let _ = put(db, "owner", &owner);
            }
        }
    }
    pub fn start_close(&mut self) -> IvyResult<Grant> {
        self.closing = true;
        let grant = self.prepare("close", "quiesce", None)?;
        self.enter(&grant.id)?;
        Ok(grant)
    }
    pub fn quiesce(&mut self, close: &Grant, proof: Json) -> IvyResult<()> {
        self.check(&close.id, &close.token)?;
        if self.live.len() != 1 || !self.closing {
            return Err(unknown());
        }
        Self::validate_proof(&proof, &self.epoch, &close.id)?;
        let db = &mut self.auth.as_mut().ok_or_else(unknown)?.db;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| unknown())?;
        let pending:i64=tx.query_row("SELECT count(*) FROM chat_auth_receipt WHERE key LIKE 'op/%' AND key<>? AND (json_extract(value,'$.state') IS NULL OR json_extract(value,'$.state') NOT IN ('completed','not_executed'))",[format!("op/{}",close.id)],|r|r.get(0)).map_err(|_|unknown())?;
        if pending != 0 {
            return Err(unknown());
        }
        let mut op = get(&tx, &format!("op/{}", close.id))?;
        op.insert("state", "completed".into());
        op.insert("outcome", "value".into());
        op.insert("resultDigest", digest(encode(&proof).as_bytes()).into());
        let mut owner = get(&tx, "owner")?;
        owner.insert("state", "quiesced".into());
        owner.insert("quiescence", proof);
        put(&tx, &format!("op/{}", close.id), &op)?;
        put(&tx, "owner", &owner)?;
        tx.commit().map_err(|_| unknown())?;
        self.live.clear();
        self.quiesced = true;
        Ok(())
    }
    fn validate_proof(proof: &Json, epoch: &str, close: &str) -> IvyResult<()> {
        if text(proof, "epoch")? != epoch || text(proof, "closeOperation")? != close {
            return Err(unknown());
        }
        for key in [
            "stopAck",
            "treeStopped",
            "protocolDrained",
            "diagnosticsDrained",
            "rootsSettled",
            "callbacksSettled",
            "eventsSettled",
        ] {
            if proof.get(key).and_then(Json::as_bool) != Some(true) {
                return Err(unknown());
            }
        }
        let process = proof.required("process").map_err(|_| unknown())?;
        if process.get("exitCode").and_then(Json::as_f64) != Some(0.0)
            || !process.get("errorCode").is_some_and(Json::is_null)
        {
            return Err(unknown());
        }
        Ok(())
    }
    pub fn retire(&mut self) -> IvyResult<()> {
        if self.released {
            return Ok(());
        }
        if !self.quiesced || self.fenced {
            return Err(unknown());
        }
        self.auth
            .take()
            .ok_or_else(unknown)?
            .close()
            .map_err(|_| unknown())?;
        self.lock.take();
        self.released = true;
        Ok(())
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        if !self.released {
            if let Some(auth) = self.auth.take() {
                std::mem::forget(auth);
            }
            if let Some(lock) = self.lock.take() {
                std::mem::forget(lock);
            }
        }
    }
}
