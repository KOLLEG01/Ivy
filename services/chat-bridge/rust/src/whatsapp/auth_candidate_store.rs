//! Unbound receipt candidate. Only an explicit stopped synthetic fixture opens it.
//! Ordinary startup and the separate NAB-3P prototype do not use this contract.
#[path = "auth.rs"]
mod legacy;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, Outcome,
    hash::digest,
    json::{canonical, parse, stringify},
};
use ivy_store::sqlite::OwnerLock;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const CONTRACT: &str = "chat-auth-production-owner-v1";
pub const TERMINAL_LIMIT: usize = 4096;

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
#[derive(Clone)]
pub struct Grant {
    pub id: String,
    pub token: String,
}
struct Live {
    root: String,
    token: String,
    parent: Option<String>,
    returned: bool,
    reserved: bool,
    lifecycle: bool,
    result_pending: bool,
}
pub struct Store {
    auth: Option<legacy::AuthStore>,
    lock: Option<OwnerLock>,
    pub epoch: String,
    next: u64,
    live: BTreeMap<String, Live>,
    frozen: BTreeSet<String>,
    pub closing: bool,
    pub fenced: bool,
    quiesced: bool,
    released: bool,
}
impl Store {
    /// This explicit initializer is compiled only in the synthetic test harness.
    /// A table already present, even empty or malformed, is never reinitialized.
    pub fn adopt_stopped_fixture(path: &Path, lock: &Path) -> IvyResult<Self> {
        Self::open(path, lock, true)
    }
    pub fn reopen_stopped_fixture(path: &Path, lock: &Path) -> IvyResult<Self> {
        Self::open(path, lock, false)
    }
    fn open(path: &Path, lock: &Path, initialize: bool) -> IvyResult<Self> {
        let lock = OwnerLock::acquire(lock, true).map_err(|_| refused())?;
        if initialize && path.exists() {
            let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| unknown())?;
            let present: i64 = db
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='chat_auth_receipt'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|_| unknown())?;
            if present != 0 {
                return Err(unknown());
            }
        }
        if !initialize {
            // Validate read-only first: missing/malformed proof must cause no DDL.
            let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| unknown())?;
            Self::validate_format(&db)?;
            let owner = get(&db, "owner")?;
            let epoch = text(&owner, "epoch")?;
            let next = text(&owner, "nextOperation")?;
            let ordinal = next.parse::<u64>().map_err(|_| unknown())?;
            if owner.get("version").and_then(Json::as_f64) != Some(1.0)
                || ordinal == 0
                || ordinal.to_string() != next
                || !text(&owner, "initOperation")?.starts_with(&(epoch.clone() + ":"))
                || !text(&owner, "closeOperation")?.starts_with(&(epoch.clone() + ":"))
                || !text(&owner, "runtimeOperation")?.starts_with(&(epoch + ":"))
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
            let invalid: i64 = db.query_row("SELECT count(*) FROM chat_auth_receipt WHERE key LIKE 'op/%' AND (json_extract(value,'$.state') IS NULL OR json_extract(value,'$.state') NOT IN ('completed','not_executed') OR json_extract(value,'$.settled') IS NOT 1)", [], |r| r.get(0)).map_err(|_| unknown())?;
            if invalid != 0 {
                return Err(unknown());
            }
            Self::validate_epoch(&db, &owner)?;
            {
                let mut query = db
                    .prepare("SELECT key,value FROM chat_auth_receipt WHERE key LIKE 'epoch/%'")
                    .map_err(|_| unknown())?;
                for row in query
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                    .map_err(|_| unknown())?
                {
                    let (key, value) = row.map_err(|_| unknown())?;
                    let prior = parse(&value, usize::MAX).map_err(|_| unknown())?;
                    if key != format!("epoch/{}", text(&prior, "epoch")?) {
                        return Err(unknown());
                    }
                    Self::validate_epoch(&db, &prior)?;
                }
            }
            Self::validate_indexes(&db)?;
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
                    ("contract", CONTRACT.into()),
                    ("storageFormat", Json::Number(2.0)),
                    ("terminalReceiptLimit", Json::Number(TERMINAL_LIMIT as f64)),
                    ("callbackDigest", "sha256-canonical-chain-v1".into()),
                ]),
            )?;
        } else {
            // Copy exact positive proof dependencies before collecting older epochs.
            let old = get(&tx, "owner")?;
            let dependencies = ["initOperation", "runtimeOperation", "closeOperation"]
                .into_iter()
                .map(|key| text(&old, key))
                .collect::<IvyResult<Vec<_>>>()?;
            let keep: BTreeSet<String> = dependencies.iter().map(|id| format!("op/{id}")).collect();
            let keys = {
                let mut query = tx
                    .prepare(
                        "SELECT key FROM chat_auth_receipt WHERE key NOT IN ('format','owner')",
                    )
                    .map_err(|_| unknown())?;
                query
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|_| unknown())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|_| unknown())?
            };
            for key in keys {
                if !keep.contains(&key) {
                    tx.execute("DELETE FROM chat_auth_receipt WHERE key=?", [key])
                        .map_err(|_| unknown())?;
                }
            }
            put(&tx, &format!("epoch/{}", text(&old, "epoch")?), &old)?;
        }
        let owner = Json::object([
            ("version", Json::Number(1.0)),
            ("epoch", epoch.as_str().into()),
            ("nextOperation", "0".into()),
            ("state", "prepared".into()),
            ("initOperation", Json::Null),
            ("runtimeOperation", Json::Null),
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
            frozen: BTreeSet::new(),
            closing: false,
            fenced: false,
            quiesced: false,
            released: false,
        })
    }
    fn validate_format(db: &Connection) -> IvyResult<()> {
        let f = get(db, "format")?;
        if text(&f, "contract")? != CONTRACT
            || f.get("version").and_then(Json::as_f64) != Some(1.0)
            || f.get("storageFormat").and_then(Json::as_f64) != Some(2.0)
            || f.get("terminalReceiptLimit").and_then(Json::as_f64) != Some(TERMINAL_LIMIT as f64)
            || text(&f, "callbackDigest")? != "sha256-canonical-chain-v1"
        {
            return Err(unknown());
        }
        Ok(())
    }
    fn validate_epoch(db: &Connection, owner: &Json) -> IvyResult<()> {
        let epoch = text(owner, "epoch")?;
        if uuid::Uuid::parse_str(&epoch)
            .map_err(|_| unknown())?
            .to_string()
            != epoch
            || text(owner, "state")? != "quiesced"
        {
            return Err(unknown());
        }
        let close = text(owner, "closeOperation")?;
        Self::validate_proof(
            owner.required("quiescence").map_err(|_| unknown())?,
            &epoch,
            &close,
        )?;
        let mut seen = BTreeSet::new();
        for (field, kind, method) in [
            ("initOperation", "init", "initialize"),
            ("runtimeOperation", "runtime", "protocol.runtime"),
            ("closeOperation", "close", "quiesce"),
        ] {
            let id = text(owner, field)?;
            if !seen.insert(id.clone()) {
                return Err(unknown());
            }
            let row = get(db, &format!("op/{id}"))?;
            let counter = text(&row, "allocationCounter")?
                .parse::<u64>()
                .map_err(|_| unknown())?;
            if counter == 0
                || text(&row, "allocationCounter")? != counter.to_string()
                || id != format!("{epoch}:{counter}")
                || text(&row, "operationId")? != id
                || text(&row, "epoch")? != epoch
                || text(&row, "kind")? != kind
                || text(&row, "method")? != method
                || text(&row, "state")? != "completed"
                || text(&row, "outcome")? != "value"
                || row.get("settled").and_then(Json::as_bool) != Some(true)
            {
                return Err(unknown());
            }
            if kind != "init"
                && text(&row, "resultDigest")?
                    != digest(
                        encode(owner.required("quiescence").map_err(|_| unknown())?).as_bytes(),
                    )
            {
                return Err(unknown());
            }
        }
        Ok(())
    }
    fn validate_index(db: &Connection, key: &str, value: &str) -> IvyResult<String> {
        let id = parse(value, usize::MAX)
            .map_err(|_| unknown())?
            .as_str()
            .map_err(|_| unknown())?;
        let op = get(db, &format!("op/{id}"))?;
        let counter = text(&op, "allocationCounter")?
            .parse::<u64>()
            .map_err(|_| unknown())?;
        let epoch = text(&op, "epoch")?;
        if counter == 0
            || text(&op, "allocationCounter")? != counter.to_string()
            || id != format!("{epoch}:{counter}")
            || key != format!("terminal/{epoch}/{counter:020}")
            || text(&op, "operationId")? != id
            || !matches!(text(&op, "state")?.as_str(), "completed" | "not_executed")
            || op.get("settled").and_then(Json::as_bool) != Some(true)
            || matches!(
                text(&op, "kind")?.as_str(),
                "init" | "runtime" | "close" | "close-cleanup"
            )
        {
            return Err(unknown());
        }
        Ok(id)
    }
    fn validate_indexes(db: &Connection) -> IvyResult<()> {
        let mut query = db
            .prepare("SELECT key,value FROM chat_auth_receipt WHERE key LIKE 'terminal/%'")
            .map_err(|_| unknown())?;
        for row in query
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|_| unknown())?
        {
            let (key, value) = row.map_err(|_| unknown())?;
            Self::validate_index(db, &key, &value)?;
        }
        Ok(())
    }
    fn compact(db: &Connection, epoch: &str) -> IvyResult<()> {
        let prefix = format!("terminal/{epoch}/%");
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM chat_auth_receipt WHERE key LIKE ?",
                [&prefix],
                |r| r.get(0),
            )
            .map_err(|_| unknown())?;
        let extra = count.saturating_sub(TERMINAL_LIMIT as i64).max(0);
        if extra == 0 {
            return Ok(());
        }
        let rows = {
            let mut q = db
                .prepare(
                    "SELECT key,value FROM chat_auth_receipt WHERE key LIKE ? ORDER BY key LIMIT ?",
                )
                .map_err(|_| unknown())?;
            q.query_map((&prefix, extra), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|_| unknown())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| unknown())?
        };
        for (key, value) in rows {
            let id = Self::validate_index(db, &key, &value)?;
            db.execute(
                "DELETE FROM chat_auth_receipt WHERE key=?",
                [format!("op/{id}")],
            )
            .map_err(|_| unknown())?;
            db.execute("DELETE FROM chat_auth_receipt WHERE key=?", [key])
                .map_err(|_| unknown())?;
        }
        Ok(())
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
        self.allocate(kind, method, parent, false)
    }
    pub fn attach_caller(&mut self, id: &str, caller: &str) -> IvyResult<()> {
        let mut receipt = self.receipt(id)?;
        if caller.is_empty()
            || text(&receipt, "state")? != "prepared"
            || receipt.get("callerOperationId").is_some()
        {
            return Err(unknown());
        }
        receipt.insert("callerOperationId", caller.into());
        put(self.db()?, &format!("op/{id}"), &receipt)
    }
    fn allocate(
        &mut self,
        kind: &str,
        method: &str,
        parent: Option<(&str, &str)>,
        close_owner: bool,
    ) -> IvyResult<Grant> {
        if self.fenced || self.quiesced {
            return Err(unknown());
        }
        if matches!(kind, "init" | "runtime") {
            let field = if kind == "init" {
                "initOperation"
            } else {
                "runtimeOperation"
            };
            if !self.owner()?.get(field).is_some_and(Json::is_null) {
                return Err(refused());
            }
        }
        if ![
            "init",
            "runtime",
            "socket-open",
            "outbound",
            "inbound-crypto",
            "repository",
            "raw-store",
            "credential-save",
            "background-prekey",
            "app-state",
            "close",
            "close-cleanup",
        ]
        .contains(&kind)
            || method.is_empty()
        {
            return Err(unknown());
        }
        if matches!(kind, "close" | "close-cleanup") && !close_owner {
            return Err(refused());
        }
        if self.closing && !close_owner {
            return Err(refused());
        }
        let root = if let Some((id, token)) = parent {
            let live = self
                .live
                .get(id)
                .filter(|v| v.token == token && !v.returned)
                .ok_or_else(unknown)?;
            if text(&self.receipt(id)?, "state")? != "entered" {
                return Err(unknown());
            }
            (!live.lifecycle).then(|| live.root.clone())
        } else {
            if self.closing && !close_owner {
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
            ("allocationCounter", self.next.to_string().into()),
            (
                "finiteRoot",
                root.clone().unwrap_or_else(|| id.clone()).into(),
            ),
            ("settled", false.into()),
            ("callbacksDigest", digest(b"").into()),
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
        if kind == "runtime" {
            owner.insert("runtimeOperation", id.as_str().into());
        }
        put(&tx, "owner", &owner)?;
        put(&tx, &format!("op/{id}"), &op)?;
        tx.commit().map_err(|_| unknown())?;
        self.live.insert(
            id.clone(),
            Live {
                root: root.unwrap_or_else(|| id.clone()),
                token: token.clone(),
                parent: parent.map(|(id, _)| id.to_owned()),
                returned: false,
                reserved: false,
                lifecycle: kind == "runtime",
                result_pending: false,
            },
        );
        Ok(Grant { id, token })
    }
    pub fn enter(&mut self, id: &str) -> IvyResult<()> {
        if self.fenced {
            return Err(unknown());
        }
        let live = self.live.get(id).ok_or_else(unknown)?;
        if self.closing && !self.frozen.contains(id) && text(&self.receipt(id)?, "kind")? != "close"
        {
            return Err(refused());
        }
        if self.closing && !live.reserved && text(&self.receipt(id)?, "kind")? != "close" {
            return Err(refused());
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
        if self.fenced
            || !self
                .live
                .get(id)
                .is_some_and(|v| v.token == token && !v.returned)
        {
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
        self.live
            .values()
            .filter(|v| !v.returned || v.result_pending)
            .count()
    }
    pub fn finite_active(&self) -> usize {
        self.live
            .values()
            .filter(|v| (!v.returned || v.result_pending) && !v.lifecycle)
            .count()
    }
    pub fn has_live(&self, id: &str) -> bool {
        self.live.contains_key(id)
    }
    pub fn hold_result(&mut self, grant: &Grant) -> IvyResult<()> {
        self.check(&grant.id, &grant.token)?;
        let live = self.live.get(&grant.id).ok_or_else(unknown)?;
        if live.lifecycle || live.result_pending {
            return Err(unknown());
        }
        let db = self.db()?;
        let mut op = self.receipt(&grant.id)?;
        op.insert("bodyPending", true.into());
        put(db, &format!("op/{}", grant.id), &op)?;
        self.live
            .get_mut(&grant.id)
            .ok_or_else(unknown)?
            .result_pending = true;
        Ok(())
    }
    /// The original body and its recorded digest were matched by the private
    /// protocol owner. Caller observer cancellation is a separate fact.
    pub fn release_result(&mut self, grant: &Grant) -> IvyResult<()> {
        let live = self.live.get(&grant.id).ok_or_else(unknown)?;
        if live.token != grant.token || !live.returned || !live.result_pending {
            return Err(unknown());
        }
        if live.root != grant.id {
            let db = self.db()?;
            let mut op = self.receipt(&grant.id)?;
            op.insert("bodyPending", false.into());
            put(db, &format!("op/{}", grant.id), &op)?;
            self.live
                .get_mut(&grant.id)
                .ok_or_else(unknown)?
                .result_pending = false;
            return Ok(());
        }
        if self.live.iter().any(|(id, v)| {
            id != &grant.id && v.root == grant.id && (!v.returned || v.result_pending)
        }) {
            return Err(unknown());
        }
        let ids = self
            .live
            .iter()
            .filter(|(_, v)| v.root == grant.id)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let tx = self
            .auth
            .as_mut()
            .ok_or_else(unknown)?
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| unknown())?;
        Self::settle_records(&tx, &self.epoch, &ids)?;
        Self::compact(&tx, &self.epoch)?;
        tx.commit().map_err(|_| unknown())?;
        for id in ids {
            self.live.remove(&id);
            self.frozen.remove(&id);
        }
        Ok(())
    }
    pub fn runtime_grant(&mut self) -> IvyResult<Grant> {
        let grant = self.prepare("runtime", "protocol.runtime", None)?;
        self.enter(&grant.id)?;
        Ok(grant)
    }
    /// A concrete, single-use continuation is allocated before close, not inferred
    /// later from an epoch ID or inherited asynchronous context.
    pub fn reserve(&mut self, parent: &Grant, method: &str) -> IvyResult<Grant> {
        self.reserve_kind(parent, "raw-store", method)
    }
    pub fn reserve_kind(&mut self, parent: &Grant, kind: &str, method: &str) -> IvyResult<Grant> {
        self.check(&parent.id, &parent.token)?;
        if matches!(kind, "init" | "runtime" | "close" | "close-cleanup") {
            return Err(refused());
        }
        let grant = self.prepare(kind, method, Some((&parent.id, &parent.token)))?;
        self.live.get_mut(&grant.id).ok_or_else(unknown)?.reserved = true;
        Ok(grant)
    }
    pub fn activate_reserved(&mut self, grant: &Grant, method: &str) -> IvyResult<()> {
        if !self
            .live
            .get(&grant.id)
            .is_some_and(|v| v.token == grant.token && v.reserved && !v.returned)
            || text(&self.receipt(&grant.id)?, "method")? != method
        {
            return Err(refused());
        }
        self.enter(&grant.id)
    }
    pub fn cancel_reserved(&mut self, grant: &Grant) -> IvyResult<()> {
        let op = self.receipt(&grant.id)?;
        if !self
            .live
            .get(&grant.id)
            .is_some_and(|v| v.token == grant.token && v.reserved && !v.returned)
            || text(&op, "state")? != "prepared"
        {
            return Err(unknown());
        }
        // The original owner still holds this unconsumed reservation. It alone
        // can prove no entered/dispatch gate was crossed.
        self.finish_record(grant, "not_executed", &digest(b"null"), None)
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
        if matches!(
            text(&self.receipt(id)?, "kind")?.as_str(),
            "runtime" | "close"
        ) {
            return Err(refused());
        }
        if !["value", "library_error"].contains(&outcome)
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
                && live.parent.as_deref() == Some(id)
                && (!live.returned || live.result_pending)
        }) {
            return Err(unknown());
        }
        self.finish_record(
            &Grant {
                id: id.into(),
                token: token.into(),
            },
            outcome,
            result_digest,
            name,
        )
    }
    fn finish_record(
        &mut self,
        grant: &Grant,
        outcome: &str,
        result_digest: &str,
        name: Option<&str>,
    ) -> IvyResult<()> {
        let id = grant.id.as_str();
        let root = self.live.get(id).ok_or_else(unknown)?.root.clone();
        let at_root = root == id;
        if at_root
            && self.live.iter().any(|(child, live)| {
                child != id && live.root == root && (!live.returned || live.result_pending)
            })
        {
            return Err(unknown());
        }
        let settled = if at_root && !self.live[id].result_pending {
            self.live
                .iter()
                .filter(|(_, live)| live.root == root)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
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
        Self::settle_records(&tx, &self.epoch, &settled)?;
        Self::compact(&tx, &self.epoch)?;
        tx.commit().map_err(|_| unknown())?;
        if !settled.is_empty() {
            for done in settled {
                self.live.remove(&done);
                self.frozen.remove(&done);
            }
        } else {
            self.live.get_mut(id).ok_or_else(unknown)?.returned = true;
        }
        Ok(())
    }
    fn settle_records(db: &Connection, epoch: &str, ids: &[String]) -> IvyResult<()> {
        for finished in ids {
            let mut row = get(db, &format!("op/{finished}"))?;
            if !matches!(text(&row, "state")?.as_str(), "completed" | "not_executed") {
                return Err(unknown());
            }
            row.insert("settled", true.into());
            row.insert("bodyPending", false.into());
            put(db, &format!("op/{finished}"), &row)?;
            db.execute(
                "DELETE FROM chat_auth_receipt WHERE key LIKE ?",
                [format!("cb/{finished}/%")],
            )
            .map_err(|_| unknown())?;
            if !matches!(
                text(&row, "kind")?.as_str(),
                "init" | "runtime" | "close" | "close-cleanup"
            ) {
                let n = text(&row, "allocationCounter")?
                    .parse::<u64>()
                    .map_err(|_| unknown())?;
                put(
                    db,
                    &format!("terminal/{epoch}/{n:020}"),
                    &Json::from(finished.as_str()),
                )?;
            }
        }
        Ok(())
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
        if digest(encoded.as_bytes()) != expected_digest
            || !matches!(kind, "get" | "set")
            || !matches!(
                text(&self.receipt(id)?, "kind")?.as_str(),
                "raw-store" | "close-cleanup"
            )
        {
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
        let chain = Json::object([
            ("previous", text(&op, "callbacksDigest")?.into()),
            ("operationId", id.into()),
            ("ordinal", ordinal.to_string().into()),
            ("kind", kind.into()),
            ("requestDigest", expected_digest.into()),
        ]);
        op.insert(
            "callbacksDigest",
            digest(
                canonical(&chain, usize::MAX)
                    .map_err(|_| unknown())?
                    .as_bytes(),
            )
            .into(),
        );
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
        if self.closing {
            return Err(refused());
        }
        self.closing = true;
        self.frozen = self
            .live
            .iter()
            .filter(|(_, v)| !v.returned && !v.lifecycle)
            .map(|(id, _)| id.clone())
            .collect();
        let grant = self.allocate("close", "quiesce", None, true)?;
        self.enter(&grant.id)?;
        Ok(grant)
    }
    /// Close owns the exact allowlist; a child cannot nominate arbitrary cleanup.
    pub fn token_index_cleanup(&mut self, close: &Grant) -> IvyResult<Grant> {
        self.check(&close.id, &close.token)?;
        if !self.closing || text(&self.receipt(&close.id)?, "kind")? != "close" {
            return Err(refused());
        }
        if self.live.keys().any(|id| {
            self.receipt(id)
                .ok()
                .and_then(|r| text(&r, "kind").ok())
                .as_deref()
                == Some("close-cleanup")
        }) {
            return Err(refused());
        }
        let grant = self.allocate(
            "close-cleanup",
            "token-index.flush",
            Some((&close.id, &close.token)),
            true,
        )?;
        let live = self.live.get_mut(&grant.id).ok_or_else(unknown)?;
        live.reserved = true;
        self.frozen.insert(grant.id.clone());
        Ok(grant)
    }
    pub fn quiesce(&mut self, close: &Grant, proof: Json) -> IvyResult<()> {
        self.check(&close.id, &close.token)?;
        let runtime = text(&self.owner()?, "runtimeOperation")?;
        if !self.closing
            || self.live.iter().any(|(id, v)| {
                (!v.returned || v.result_pending) && id != &close.id && id != &runtime
            })
        {
            return Err(unknown());
        }
        Self::validate_proof(&proof, &self.epoch, &close.id)?;
        let db = &mut self.auth.as_mut().ok_or_else(unknown)?.db;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| unknown())?;
        for (id, live) in &self.live {
            if id != &close.id && live.root == close.id {
                if !live.returned {
                    return Err(unknown());
                }
                let mut op = get(&tx, &format!("op/{id}"))?;
                if !matches!(text(&op, "state")?.as_str(), "completed" | "not_executed") {
                    return Err(unknown());
                }
                op.insert("settled", true.into());
                put(&tx, &format!("op/{id}"), &op)?;
                tx.execute(
                    "DELETE FROM chat_auth_receipt WHERE key LIKE ?",
                    [format!("cb/{id}/%")],
                )
                .map_err(|_| unknown())?;
                if text(&op, "kind")? != "close-cleanup" {
                    let n = text(&op, "allocationCounter")?
                        .parse::<u64>()
                        .map_err(|_| unknown())?;
                    put(
                        &tx,
                        &format!("terminal/{}/{n:020}", self.epoch),
                        &Json::from(id.as_str()),
                    )?;
                }
            }
        }
        let pending:i64=tx.query_row("SELECT count(*) FROM chat_auth_receipt WHERE key LIKE 'op/%' AND key NOT IN (?,?) AND (json_extract(value,'$.state') IS NULL OR json_extract(value,'$.state') NOT IN ('completed','not_executed') OR json_extract(value,'$.settled') IS NOT 1)",(format!("op/{}",close.id),format!("op/{runtime}")),|r|r.get(0)).map_err(|_|unknown())?;
        if pending != 0 {
            return Err(unknown());
        }
        let mut op = get(&tx, &format!("op/{}", close.id))?;
        op.insert("state", "completed".into());
        op.insert("outcome", "value".into());
        op.insert("resultDigest", digest(encode(&proof).as_bytes()).into());
        op.insert("settled", true.into());
        let mut runtime_op = get(&tx, &format!("op/{runtime}"))?;
        if text(&runtime_op, "state")? != "entered" || text(&runtime_op, "kind")? != "runtime" {
            return Err(unknown());
        }
        runtime_op.insert("state", "completed".into());
        runtime_op.insert("outcome", "value".into());
        runtime_op.insert("resultDigest", digest(encode(&proof).as_bytes()).into());
        runtime_op.insert("settled", true.into());
        let mut owner = get(&tx, "owner")?;
        owner.insert("state", "quiesced".into());
        owner.insert("quiescence", proof);
        put(&tx, &format!("op/{}", close.id), &op)?;
        put(&tx, &format!("op/{runtime}"), &runtime_op)?;
        put(&tx, "owner", &owner)?;
        Self::compact(&tx, &self.epoch)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhausted_counter_never_recycles_an_id_or_dispatch_intent() {
        let root = std::env::temp_dir().join(format!("candidate-counter-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("whatsapp-owner")).unwrap();
        let mut store = Store::adopt_stopped_fixture(
            &root.join("auth.sqlite"),
            &root.join("whatsapp-owner/executor-lock.sqlite"),
        )
        .unwrap();
        store.next = u64::MAX;
        let mut owner = store.owner().unwrap();
        owner.insert("nextOperation", u64::MAX.to_string().into());
        put(store.db().unwrap(), "owner", &owner).unwrap();
        assert!(store.prepare("init", "initialize", None).is_err());
        assert_eq!(store.next, u64::MAX);
        let count: i64 = store
            .db()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM chat_auth_receipt WHERE key LIKE 'op/%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(store.owner().unwrap(), owner);
        store.fence();
        if let Some(capture) = std::env::var_os("IVY_CHAT_CANDIDATE_CAPTURE") {
            std::fs::write(
                format!("{}-counter.json", Path::new(&capture).display()),
                encode(&Json::object([
                    ("owner", store.owner().unwrap()),
                    ("root", root.to_string_lossy().as_ref().into()),
                ])),
            )
            .unwrap();
        }
    }
}
