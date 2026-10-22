//! Secretary's service-owned local journal. Domain schemas are validated by the
//! caller; persisted JSON stays in the shared lossless wire representation.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::{canonical, parse},
    operation::parse_operation_id,
};
use ivy_store::{
    receipts::has_unresolved_outcome,
    rusqlite::{self, Connection, OptionalExtension, Transaction, TransactionBehavior, params},
    sqlite,
};
use std::{collections::HashSet, path::Path, time::Duration};

#[path = "journal_import.rs"]
mod import;
pub use import::ImportedCaptureAcknowledgements;

const FRAME: usize = 33_554_432;
const SOURCE: &str = "secretary/source";
const OPERATION: &str = "secretary/operation";
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub object_id: String,
    pub revision: i64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub pin: Pin,
    pub name: String,
    pub value: IvyJson,
}
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub count: i64,
    pub bytes: i64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            count: 10_000,
            bytes: 256 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub count: i64,
    pub bytes: i64,
}
pub struct Journal {
    pub db: Connection,
    pub limits: Limits,
}
fn storage(error: rusqlite::Error) -> IvyError {
    if error.to_string().contains("operation_expired") {
        IvyError::new("operation_expired", "The original operation has expired.")
    } else {
        sqlite::error(error)
    }
}
fn text(value: &IvyJson, key: &str) -> String {
    value
        .get(key)
        .and_then(|x| x.as_str().ok())
        .unwrap_or_default()
}
fn operation_identity(value: &IvyJson) -> Option<String> {
    match value {
        IvyJson::String(_) => value.as_str().ok().filter(|s| {
            parse_operation_id(s).is_ok_and(|v| v.issued_at_unix_ms >= 1_000_000_000_000)
        }),
        IvyJson::Array(a) => a.iter().find_map(operation_identity),
        IvyJson::Object(a) => a.iter().find_map(|(_, v)| operation_identity(v)),
        _ => None,
    }
}
fn references(value: &IvyJson, available: &HashSet<String>, found: &mut HashSet<String>) {
    match value {
        IvyJson::String(_) => {
            if let Ok(s) = value.as_str() {
                if available.contains(&s) {
                    found.insert(s);
                }
            }
        }
        IvyJson::Array(a) => {
            for v in a {
                references(v, available, found);
            }
        }
        IvyJson::Object(a) => {
            for (_, v) in a {
                references(v, available, found);
            }
        }
        _ => (),
    }
}
impl Journal {
    pub fn open(path: &Path, limits: Limits) -> IvyResult<Self> {
        let db = sqlite::open(
            path,
            sqlite::OpenOptions {
                busy_timeout: Duration::from_millis(5000),
                read_only: false,
            },
        )
        .map_err(storage)?;
        let journal = Self { db, limits };
        journal.initialize_schema()?;
        Ok(journal)
    }
    pub(crate) fn initialize_schema(&self) -> IvyResult<()> {
        // Core intentionally neither reads nor changes user_version. Spool is separate.
        self.db.execute_batch(include_str!("journal.sql")).map_err(storage)
    }
    /// The production owner and importer claim the core itself, so an older
    /// process that does not know an auxiliary lock file cannot keep writing.
    /// Do not initialize/convert a retained schema before its import backup.
    pub(crate) fn claim_exclusive(path: &Path, limits: Limits) -> IvyResult<Self> {
        let db = sqlite::open(path, sqlite::OpenOptions {
            busy_timeout: Duration::from_millis(5000), read_only: false,
        }).map_err(storage)?;
        db.execute_batch("PRAGMA locking_mode=EXCLUSIVE;BEGIN EXCLUSIVE;COMMIT;").map_err(storage)?;
        Ok(Self { db, limits })
    }
    fn transaction<T>(&self, work: impl FnOnce() -> IvyResult<T>) -> IvyResult<T> {
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)
            .map_err(storage)?;
        let result = work()?;
        tx.commit().map_err(storage)?;
        Ok(result)
    }
    pub fn usage(&self) -> IvyResult<Usage> {
        self.db.query_row("SELECT (SELECT COUNT(*) FROM revisions)+(SELECT COUNT(*) FROM technical_revisions)+(SELECT COUNT(*) FROM technical_blobs)+(SELECT COUNT(*) FROM documents WHERE key='secretary/operation' AND completed_at IS NULL), COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))) FROM revisions),0)+COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))) FROM technical_revisions),0)+COALESCE((SELECT SUM(length(bytes)) FROM technical_blobs),0)+COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))+512) FROM documents WHERE key='secretary/operation' AND completed_at IS NULL),0)", [], |r| Ok(Usage { count: r.get(0)?, bytes: r.get(1)? })).map_err(storage)
    }
    fn capacity(
        &self,
        extra: i64,
        protected: Option<&str>,
        incoming: Option<&IvyJson>,
        count: i64,
    ) -> IvyResult<()> {
        let mut usage = self.usage()?;
        while usage.count + count > self.limits.count || usage.bytes + extra > self.limits.bytes {
            let before = usage;
            self.cleanup(extra, count, protected, incoming)?;
            usage = self.usage()?;
            if usage.count >= before.count && usage.bytes >= before.bytes {
                break;
            }
        }
        if usage.count + count > self.limits.count || usage.bytes + extra > self.limits.bytes {
            return Err(IvyError::new(
                "capacity_exceeded",
                "Secretary journal capacity is occupied by live or unresolved work.",
            ));
        }
        Ok(())
    }
    pub fn expired_before(&self) -> IvyResult<u64> {
        let value: Option<String> = self
            .db
            .query_row(
                "SELECT value FROM journal_state WHERE key='expiredBefore'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        Ok(value.and_then(|v| v.parse().ok()).unwrap_or(0))
    }
    fn fence(&self, value: &IvyJson) -> IvyResult<()> {
        let Some(id) = operation_identity(value) else {
            return Ok(());
        };
        let issued = parse_operation_id(&id)?.issued_at_unix_ms;
        let boundary = issued.max(self.expired_before()?);
        self.db
            .execute(
                "INSERT OR IGNORE INTO retired_operations VALUES(?,?)",
                params![id, issued as i64],
            )
            .map_err(storage)?;
        self.db.execute("INSERT INTO journal_state(key,value) VALUES('expiredBefore',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [boundary.to_string()]).map_err(storage)?;
        let boundary = i64::try_from(boundary).map_err(|_| {
            IvyError::new(
                "storage_unavailable",
                "The journal expiry boundary is invalid.",
            )
        })?;
        self.db
            .execute(
                "DELETE FROM retired_operations WHERE issued_at<?",
                [boundary],
            )
            .map_err(storage)?;
        Ok(())
    }
    pub fn complete_technical_workflow(&self, root: &str, now: i64) -> IvyResult<()> {
        self.db
            .execute(
                "INSERT INTO technical_cleanup VALUES(?,?) ON CONFLICT(root_id) DO NOTHING",
                params![root, now],
            )
            .map_err(storage)?;
        Ok(())
    }
    pub fn cleanup(
        &self,
        extra_bytes: i64,
        extra_count: i64,
        protected: Option<&str>,
        incoming: Option<&IvyJson>,
    ) -> IvyResult<()> {
        let work = || self.cleanup_inner(extra_bytes, extra_count, protected, incoming);
        if self.db.is_autocommit() {
            self.transaction(work)
        } else {
            work()
        }
    }
    fn cleanup_inner(
        &self,
        extra_bytes: i64,
        extra_count: i64,
        protected: Option<&str>,
        incoming: Option<&IvyJson>,
    ) -> IvyResult<()> {
        let needs_room = || -> IvyResult<bool> {
            let u = self.usage()?;
            Ok(u.count + extra_count > self.limits.count
                || u.bytes + extra_bytes > self.limits.bytes)
        };
        if !needs_room()? {
            return Ok(());
        }
        self.db.execute("INSERT OR IGNORE INTO technical_cleanup SELECT id,0 FROM technical_documents WHERE key IN ('secretary/public-operation','secretary/follow-up') AND json_extract(value_json,'$.phase')='succeeded'", []).map_err(storage)?;
        let operations: Vec<(String, String, i64)> = self.db.prepare("SELECT id,value_json,completed_at FROM documents WHERE key='secretary/operation' AND completed_at IS NOT NULL ORDER BY completed_at,rowid").map_err(storage)?.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(storage)?.collect::<rusqlite::Result<_>>().map_err(storage)?;
        let raw: Vec<(String, String, String)> = self
            .db
            .prepare("SELECT id,key,value_json FROM technical_documents")
            .map_err(storage)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(storage)?
            .collect::<rusqlite::Result<_>>()
            .map_err(storage)?;
        let documents = raw
            .into_iter()
            .map(|(id, key, v)| Ok((id, key, parse(&v, FRAME)?)))
            .collect::<IvyResult<Vec<_>>>()?;
        let mut available: HashSet<String> = operations
            .iter()
            .map(|r| r.0.clone())
            .chain(documents.iter().map(|r| r.0.clone()))
            .collect();
        available.extend(
            self.db
                .prepare("SELECT id FROM technical_blobs")
                .map_err(storage)?
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(storage)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage)?,
        );
        let refs = |value: &IvyJson| {
            let mut set = HashSet::new();
            references(value, &available, &mut set);
            set
        };
        let incoming_refs = incoming.map(&refs).unwrap_or_default();
        let mut protected_operations = incoming_refs.clone();
        if let Some(id) = protected {
            protected_operations.insert(id.into());
        }
        for (_, _, value) in &documents {
            protected_operations.extend(refs(value));
        }
        let pending: Vec<String> = self.db.prepare("SELECT value_json FROM documents WHERE key='secretary/operation' AND completed_at IS NULL").map_err(storage)?.query_map([], |r| r.get(0)).map_err(storage)?.collect::<rusqlite::Result<_>>().map_err(storage)?;
        for value in pending {
            protected_operations.extend(refs(&parse(&value, FRAME)?));
        }
        let roots: Vec<(String, i64)> = self
            .db
            .prepare(
                "SELECT root_id,delete_after FROM technical_cleanup ORDER BY delete_after,rowid",
            )
            .map_err(storage)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(storage)?
            .collect::<rusqlite::Result<_>>()
            .map_err(storage)?;
        // Stable sort keeps original operation-before-technical ordering at equal times.
        let mut candidates: Vec<(&str, i64, Option<&str>)> = operations
            .iter()
            .map(|(id, v, at)| (id.as_str(), *at, Some(v.as_str())))
            .chain(roots.iter().map(|(id, at)| (id.as_str(), *at, None)))
            .collect();
        candidates.sort_by_key(|c| c.1);
        for (id, _, operation) in candidates {
            if protected == Some(id) {
                continue;
            }
            if let Some(raw) = operation {
                let value = parse(raw, FRAME)?;
                if !matches!(text(&value, "phase").as_str(), "succeeded" | "failed")
                    || protected_operations.contains(id)
                    || has_unresolved_outcome(&value)?
                {
                    continue;
                }
                if text(&value,"action") == "capture" && text(&value,"phase") == "succeeded" && !self.db.query_row("SELECT EXISTS(SELECT 1 FROM capture_acknowledgements WHERE capture_id=?)",[id],|r|r.get::<_,bool>(0)).map_err(storage)? { continue; }
                let Some(operation_id) = value.get("operationId") else {
                    continue;
                };
                if operation_identity(operation_id).is_none() {
                    continue;
                }
                self.fence(operation_id)?;
                for sql in [
                    "DELETE FROM revisions WHERE id=?",
                    "DELETE FROM documents WHERE id=?",
                    "DELETE FROM capture_acknowledgements WHERE capture_id=?",
                ] {
                    self.db.execute(sql, [id]).map_err(storage)?;
                }
                if !needs_room()? {
                    break;
                }
                continue;
            }
            let mut selected = HashSet::from([id.to_owned()]);
            loop {
                let count = selected.len();
                for (doc_id, key, value) in &documents {
                    if key == "secretary/triage-thread" {
                        continue;
                    }
                    let found = refs(value);
                    if selected.contains(doc_id) || found.iter().any(|r| selected.contains(r)) {
                        selected.insert(doc_id.clone());
                        selected.extend(found);
                    }
                }
                if count == selected.len() {
                    break;
                }
            }
            let members: Vec<_> = documents
                .iter()
                .filter(|(id, _, _)| selected.contains(id))
                .collect();
            if members.iter().any(|(_, key, value)| {
                key == "secretary/follow-up-context"
                    && (value.get("threadId").is_some_and(IvyJson::is_null)
                        || documents.iter().any(|(_, key, other)| {
                            key == "secretary/follow-up"
                                && matches!(
                                    text(other, "phase").as_str(),
                                    "prepared" | "running" | "outcome_unknown"
                                )
                                && other.get("request").and_then(|r| r.get("itemId"))
                                    == value.get("itemId")
                        }))
            }) {
                continue;
            }
            if incoming_refs.iter().any(|r| selected.contains(r))
                || protected.is_some_and(|r| selected.contains(r))
            {
                continue;
            }
            let mut blocked = false;
            for (_, _, value) in &members {
                if has_unresolved_outcome(value)?
                    || matches!(
                        text(value, "phase").as_str(),
                        "accepted" | "dispatched" | "prepared" | "running" | "pending" | "archiving"
                    )
                {
                    blocked = true;
                    break;
                }
            }
            if blocked {
                continue;
            }
            if documents.iter().any(|(id, _, v)| {
                !selected.contains(id) && refs(v).iter().any(|r| selected.contains(r))
            }) {
                continue;
            }
            let all: Vec<(String, String)> = self
                .db
                .prepare("SELECT id,value_json FROM documents")
                .map_err(storage)?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(storage)?
                .collect::<rusqlite::Result<_>>()
                .map_err(storage)?;
            let mut referenced = false;
            for (id, raw) in all {
                if !selected.contains(&id)
                    && refs(&parse(&raw, FRAME)?)
                        .iter()
                        .any(|r| selected.contains(r))
                {
                    referenced = true;
                    break;
                }
            }
            if referenced {
                continue;
            }
            for (_, _, value) in members {
                self.fence(
                    value
                        .get("operationId")
                        .filter(|v| !v.is_null())
                        .or_else(|| value.get("request").and_then(|r| r.get("operationId")))
                        .unwrap_or(&IvyJson::Null),
                )?;
            }
            for id in selected {
                for sql in [
                    "DELETE FROM technical_blobs WHERE id=?",
                    "DELETE FROM technical_revisions WHERE id=?",
                    "DELETE FROM technical_documents WHERE id=? AND key<>'secretary/triage-thread'",
                    "DELETE FROM technical_cleanup WHERE root_id=?",
                ] {
                    self.db.execute(sql, [&id]).map_err(storage)?;
                }
            }
            if !needs_room()? {
                break;
            }
        }
        Ok(())
    }
    pub fn read(
        &self,
        key: &str,
        id: &str,
        revision: Option<i64>,
        technical: bool,
    ) -> IvyResult<Document> {
        let (head_sql, revision_sql) = if technical {
            (
                "SELECT name,current_revision FROM technical_documents WHERE id=? AND key=?",
                "SELECT value_json FROM technical_revisions WHERE id=? AND revision=?",
            )
        } else {
            (
                "SELECT name,current_revision FROM documents WHERE id=? AND key=?",
                "SELECT value_json FROM revisions WHERE id=? AND revision=?",
            )
        };
        let head: Option<(String, i64)> = self
            .db
            .query_row(head_sql, params![id, key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(storage)?;
        let (name, current) = head.ok_or_else(|| {
            IvyError::new("not_found", "The local Secretary record does not exist.")
        })?;
        let revision = revision.unwrap_or(current);
        let raw: Option<String> = self
            .db
            .query_row(revision_sql, params![id, revision], |r| r.get(0))
            .optional()
            .map_err(storage)?;
        let value = parse(
            &raw.ok_or_else(|| {
                IvyError::new("not_found", "The local Secretary revision does not exist.")
            })?,
            FRAME,
        )?;
        Ok(Document {
            pin: Pin {
                object_id: id.into(),
                revision,
            },
            name,
            value,
        })
    }
    pub fn named(&self, key: &str, name: &str, technical: bool) -> IvyResult<Option<Document>> {
        let sql = if technical {
            "SELECT id FROM technical_documents WHERE key=? AND name=?"
        } else {
            "SELECT id FROM documents WHERE key=? AND name=?"
        };
        let id: Option<String> = self
            .db
            .query_row(sql, params![key, name], |r| r.get(0))
            .optional()
            .map_err(storage)?;
        id.map(|id| self.read(key, &id, None, technical))
            .transpose()
    }
    pub fn list(&self, key: &str, technical: bool) -> IvyResult<Vec<Document>> {
        let sql = if technical {
            "SELECT id FROM technical_documents WHERE key=? ORDER BY id"
        } else {
            "SELECT id FROM documents WHERE key=? ORDER BY created_at,id"
        };
        let ids: Vec<String> = self
            .db
            .prepare(sql)
            .map_err(storage)?
            .query_map([key], |r| r.get(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<_>>()
            .map_err(storage)?;
        ids.into_iter()
            .map(|id| self.read(key, &id, None, technical))
            .collect()
    }
    pub fn accepted_ids(&self, limit: usize, after: &str) -> IvyResult<Vec<String>> {
        self.db.prepare("SELECT id FROM documents WHERE key='secretary/operation' AND completed_at IS NULL AND id>? ORDER BY id LIMIT ?").map_err(storage)?.query_map(params![after,limit as i64],|r|r.get(0)).map_err(storage)?.collect::<rusqlite::Result<_>>().map_err(storage)
    }
    pub fn create_local(&self, key: &str, name: &str, value: &IvyJson, now: i64) -> IvyResult<Pin> {
        if !matches!(key, SOURCE | OPERATION) {
            return Err(IvyError::new(
                "invalid_arguments",
                "Only source and operation records belong in the local domain journal.",
            ));
        }
        let encoded = canonical(value, FRAME)?;
        self.transaction(|| {
            let pending = key == OPERATION && text(value, "phase") == "accepted";
            let capture = pending && text(value, "action") == "capture";
            self.capacity(
                encoded.len() as i64 * if pending { 2 } else { 1 }
                    + if pending { 512 } else { 0 }
                    + if capture { 65_536 } else { 0 },
                None,
                Some(value),
                if pending { 2 } else { 1 } + if capture { 2 } else { 0 },
            )?;
            let id = uuid::Uuid::new_v4().to_string();
            let completed = (key == OPERATION && !pending).then_some(now);
            self.db
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![id, key, name, 1, now, completed, encoded],
                )
                .map_err(storage)?;
            self.db
                .execute(
                    "INSERT INTO revisions VALUES(?,?,?)",
                    params![id, 1, encoded],
                )
                .map_err(storage)?;
            Ok(Pin {
                object_id: id,
                revision: 1,
            })
        })
    }
    pub fn amend_local(&self, key: &str, pin: &Pin, value: &IvyJson, now: i64) -> IvyResult<Pin> {
        let encoded = canonical(value, FRAME)?;
        self.transaction(|| {
            let prior: Option<(String,Option<i64>)> = self.db.query_row("SELECT value_json,completed_at FROM documents WHERE id=?",[&pin.object_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(storage)?;
            let released = key == OPERATION && prior.as_ref().is_some_and(|(_,at)|at.is_none()) && text(value,"phase") != "accepted";
            if key == SOURCE {
                let (count,bytes): (i64,i64) = self.db.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(value_json AS BLOB))),0) FROM revisions WHERE id=?",[&pin.object_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(storage)?;
                self.capacity(encoded.len() as i64 - bytes,Some(&pin.object_id),Some(value),1-count)?;
            } else { self.capacity(encoded.len() as i64 - if released { prior.as_ref().unwrap().0.len() as i64 + 512 } else {0},Some(&pin.object_id),Some(value),if released {0} else {1})?; }
            let next = pin.revision+1;
            let completed = (key == OPERATION && text(value,"phase") != "accepted").then_some(now);
            let changed = self.db.execute("UPDATE documents SET current_revision=?,value_json=?,completed_at=? WHERE id=? AND key=? AND current_revision=?",params![next,encoded,completed,pin.object_id,key,pin.revision]).map_err(storage)?;
            if changed != 1 { return Err(IvyError::new("revision_conflict","The local Secretary row changed.")); }
            self.db.execute("INSERT INTO revisions VALUES(?,?,?)",params![pin.object_id,next,encoded]).map_err(storage)?;
            if key == OPERATION && text(value,"action") == "checkpoint" && text(value,"phase") == "succeeded" {
                if let Some(captures) = value.get("checkpointCaptures").and_then(IvyJson::as_array) { for capture in captures { self.db.execute("INSERT OR IGNORE INTO capture_acknowledgements VALUES(?)",[text(capture,"objectId")]).map_err(storage)?; } }
            }
            if key == SOURCE { self.db.execute("DELETE FROM revisions WHERE id=? AND revision<>?",params![pin.object_id,next]).map_err(storage)?; }
            Ok(Pin { object_id:pin.object_id.clone(),revision:next })
        })
    }
    pub fn create_technical(&self, key: &str, name: &str, value: &IvyJson) -> IvyResult<Document> {
        if let Some(prior) = self.named(key, name, true)? {
            return Ok(prior);
        }
        let encoded = canonical(value, FRAME)?;
        let result = self.transaction(|| {
            self.capacity(encoded.len() as i64, None, Some(value), 1)?;
            let id = uuid::Uuid::new_v4().to_string();
            self.db
                .execute(
                    "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                    params![id, key, name, 1, encoded],
                )
                .map_err(storage)?;
            self.db
                .execute(
                    "INSERT INTO technical_revisions VALUES(?,?,?)",
                    params![id, 1, encoded],
                )
                .map_err(storage)?;
            Ok(Document {
                pin: Pin {
                    object_id: id,
                    revision: 1,
                },
                name: name.into(),
                value: value.clone(),
            })
        });
        match result {
            Ok(v) => Ok(v),
            Err(e) => self.named(key, name, true)?.ok_or(e),
        }
    }
    pub fn amend_technical(&self, key: &str, pin: &Pin, value: &IvyJson) -> IvyResult<Document> {
        let encoded = canonical(value, FRAME)?;
        self.transaction(|| {
            let (count,bytes):(i64,i64) = self.db.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(value_json AS BLOB))),0) FROM technical_revisions WHERE id=?",[&pin.object_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(storage)?;
            self.capacity(encoded.len() as i64-bytes,Some(&pin.object_id),Some(value),1-count)?;
            let next = pin.revision+1;
            let changed = self.db.execute("UPDATE technical_documents SET current_revision=?,value_json=? WHERE id=? AND key=? AND current_revision=?",params![next,encoded,pin.object_id,key,pin.revision]).map_err(storage)?;
            if changed != 1 { return Err(IvyError::new("revision_conflict","The local workflow row changed.")); }
            self.db.execute("INSERT INTO technical_revisions VALUES(?,?,?)",params![pin.object_id,next,encoded]).map_err(storage)?;
            self.db.execute("DELETE FROM technical_revisions WHERE id=? AND revision<>?",params![pin.object_id,next]).map_err(storage)?;
            self.read(key,&pin.object_id,Some(next),true)
        })
    }
    pub fn delete_technical(&self, key: &str, id: &str) -> IvyResult<()> {
        self.transaction(|| {
            self.db
                .execute("DELETE FROM technical_revisions WHERE id=?", [id])
                .map_err(storage)?;
            self.db
                .execute(
                    "DELETE FROM technical_documents WHERE id=? AND key=?",
                    params![id, key],
                )
                .map_err(storage)?;
            Ok(())
        })
    }
    pub fn blob(
        &self,
        key: &str,
        name: &str,
        pin: Option<&Pin>,
    ) -> IvyResult<Option<(Pin, Vec<u8>)>> {
        let row: Option<(String, Vec<u8>)> = if let Some(pin) = pin {
            self.db.query_row(
                "SELECT id,bytes FROM technical_blobs WHERE id=? AND key=?",
                params![pin.object_id, key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        } else {
            self.db.query_row(
                "SELECT id,bytes FROM technical_blobs WHERE key=? AND name=?",
                params![key, name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        }
        .optional()
        .map_err(storage)?;
        let Some((id, bytes)) = row else {
            return Ok(None);
        };
        if pin.is_some_and(|p| p.revision != 1) {
            return Err(IvyError::new(
                "secretary_evidence_conflict",
                "The local blob revision is invalid.",
            ));
        }
        Ok(Some((
            Pin {
                object_id: id,
                revision: 1,
            },
            bytes,
        )))
    }
    pub fn save_blob(&self, key: &str, name: &str, bytes: &[u8]) -> IvyResult<Pin> {
        if let Some((pin, old)) = self.blob(key, name, None)? {
            if old != bytes {
                return Err(IvyError::new(
                    "secretary_identity_conflict",
                    "The local blob identity has different bytes.",
                ));
            }
            return Ok(pin);
        }
        let result = self.transaction(|| {
            self.capacity(bytes.len() as i64, None, None, 1)?;
            let id = uuid::Uuid::new_v4().to_string();
            self.db
                .execute(
                    "INSERT INTO technical_blobs VALUES(?,?,?,?)",
                    params![id, key, name, bytes],
                )
                .map_err(storage)?;
            Ok(Pin {
                object_id: id,
                revision: 1,
            })
        });
        match result {
            Ok(v) => Ok(v),
            Err(e) => match self.blob(key, name, None)? {
                Some((pin, old)) if old == bytes => Ok(pin),
                _ => Err(e),
            },
        }
    }
}
