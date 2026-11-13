//! Secretary's service-owned local journal. Domain schemas are validated by the
//! caller; persisted JSON stays in the shared lossless wire representation.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
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
#[path = "journal_retention.rs"]
mod retention;
#[path = "journal_heads.rs"]
mod heads;
pub(crate) use heads::check_technical_head;
use heads::technical_head;
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
        self.db
            .execute_batch(include_str!("journal.sql"))
            .map_err(storage)?;
        self.compact_technical_heads()
    }
    /// The production owner and importer claim the core itself, so an older
    /// process that does not know an auxiliary lock file cannot keep writing.
    /// Do not initialize/convert a retained schema before its import backup.
    pub(crate) fn claim_exclusive(path: &Path, limits: Limits) -> IvyResult<Self> {
        let db = sqlite::open(
            path,
            sqlite::OpenOptions {
                busy_timeout: Duration::from_millis(5000),
                read_only: false,
            },
        )
        .map_err(storage)?;
        db.execute_batch("PRAGMA locking_mode=EXCLUSIVE;BEGIN EXCLUSIVE;COMMIT;")
            .map_err(storage)?;
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
        self.db
            .query_row(
                "SELECT count,bytes FROM journal_usage WHERE id=1",
                [],
                |r| {
                    Ok(Usage {
                        count: r.get(0)?,
                        bytes: r.get(1)?,
                    })
                },
            )
            .map_err(storage)
    }
    fn capacity(
        &self,
        extra: i64,
        protected: Option<&str>,
        incoming: Option<(&str, &IvyJson)>,
        count: i64,
    ) -> IvyResult<()> {
        let mut usage = self.usage()?;
        while usage.count + count > self.limits.count || usage.bytes + extra > self.limits.bytes {
            let before = usage;
            self.cleanup_record(extra, count, protected, incoming)?;
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
        self.cleanup_record(
            extra_bytes,
            extra_count,
            protected,
            incoming.map(|value| ("", value)),
        )
    }
    fn cleanup_record(
        &self,
        extra_bytes: i64,
        extra_count: i64,
        protected: Option<&str>,
        incoming: Option<(&str, &IvyJson)>,
    ) -> IvyResult<()> {
        let work = || self.cleanup_inner(extra_bytes, extra_count, protected, incoming);
        if self.db.is_autocommit() {
            self.transaction(work)
        } else {
            work()
        }
    }
    pub fn read(
        &self,
        key: &str,
        id: &str,
        revision: Option<i64>,
        technical: bool,
    ) -> IvyResult<Document> {
        let sql = if technical {
            "SELECT d.name,d.current_revision,r.value_json FROM technical_documents d LEFT JOIN technical_revisions r ON r.id=d.id AND r.revision=COALESCE(?1,d.current_revision) WHERE d.id=?2 AND d.key=?3"
        } else {
            "SELECT d.name,d.current_revision,r.value_json FROM documents d LEFT JOIN revisions r ON r.id=d.id AND r.revision=COALESCE(?1,d.current_revision) WHERE d.id=?2 AND d.key=?3"
        };
        let head: Option<(String, i64, Option<String>)> = self
            .db
            .query_row(sql, params![revision, id, key], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()
            .map_err(storage)?;
        let (name, current, raw) = head.ok_or_else(|| {
            IvyError::new("not_found", "The local Secretary record does not exist.")
        })?;
        let revision = revision.unwrap_or(current);
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
            "SELECT d.id,d.name,d.current_revision,r.value_json FROM technical_documents d LEFT JOIN technical_revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key=? AND d.name=?"
        } else {
            "SELECT d.id,d.name,d.current_revision,r.value_json FROM documents d LEFT JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key=? AND d.name=?"
        };
        Ok(self.query_documents(sql, params![key, name])?.pop())
    }
    pub fn list(&self, key: &str, technical: bool) -> IvyResult<Vec<Document>> {
        let sql = if technical {
            "SELECT d.id,d.name,d.current_revision,r.value_json FROM technical_documents d LEFT JOIN technical_revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key=? ORDER BY d.id"
        } else {
            "SELECT d.id,d.name,d.current_revision,r.value_json FROM documents d LEFT JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key=? ORDER BY d.created_at,d.id"
        };
        self.query_documents(sql, [key])
    }
    fn query_documents(
        &self,
        sql: &str,
        parameters: impl rusqlite::Params,
    ) -> IvyResult<Vec<Document>> {
        let mut statement = self.db.prepare(sql).map_err(storage)?;
        let mut rows = statement.query(parameters).map_err(storage)?;
        let mut documents = Vec::new();
        while let Some(row) = rows.next().map_err(storage)? {
            let raw: Option<String> = row.get(3).map_err(storage)?;
            let raw = raw.ok_or_else(|| {
                IvyError::new("not_found", "The local Secretary revision does not exist.")
            })?;
            documents.push(Document {
                pin: Pin {
                    object_id: row.get(0).map_err(storage)?,
                    revision: row.get(2).map_err(storage)?,
                },
                name: row.get(1).map_err(storage)?,
                value: parse(&raw, FRAME)?,
            });
        }
        Ok(documents)
    }
    /// Window writers persist normalized UTC ISO dates, so the indexed text
    /// comparison selects the same due windows as their native date comparison.
    pub fn due_windows(
        &self,
        key: &str,
        assignment: &IvyJson,
        now: i64,
    ) -> IvyResult<Vec<Document>> {
        let assignment = canonical(assignment, FRAME)?;
        let due = ivy_sdk::clock::date_to_iso(now as f64)?.to_utf8()?;
        self.query_documents(
            "SELECT d.id,d.name,d.current_revision,r.value_json FROM technical_documents d LEFT JOIN technical_revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key IN ('secretary/object-window','secretary/channel-window') AND d.key=? AND json_extract(d.value_json,'$.assignmentId')=json_extract(?,'$') AND json_extract(d.value_json,'$.pending.dueAt')<=? ORDER BY d.id",
            params![key, assignment, due],
        )
    }
    pub fn follow_up_busy(&self, thread: &JsString, except_name: &str) -> IvyResult<bool> {
        // Compare decoded JSON scalars inside SQLite, preserving lossless native
        // identifiers without converting an unpaired UTF-16 surrogate to UTF-8.
        let thread = canonical(&IvyJson::String(thread.clone()), FRAME)?;
        self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM technical_documents WHERE key='secretary/follow-up' AND json_extract(value_json,'$.threadId')=json_extract(?,'$') AND name<>? AND COALESCE(json_extract(value_json,'$.phase'),'') NOT IN ('succeeded','failed'))",
            params![thread, except_name], |row| row.get(0),
        ).map_err(storage)
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
                Some((key, value)),
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
                self.capacity(encoded.len() as i64 - bytes,Some(&pin.object_id),Some((key, value)),1-count)?;
            } else { self.capacity(encoded.len() as i64 - if released { prior.as_ref().unwrap().0.len() as i64 + 512 } else {0},Some(&pin.object_id),Some((key, value)),if released {0} else {1})?; }
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
        let head = technical_head(value)?;
        let result = self.transaction(|| {
            self.capacity(encoded.len() as i64, None, Some((key, value)), 1)?;
            let id = uuid::Uuid::new_v4().to_string();
            self.db
                .execute(
                    "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                    params![id, key, name, 1, head],
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
        let head = technical_head(value)?;
        self.transaction(|| {
            let (count,bytes):(i64,i64) = self.db.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(value_json AS BLOB))),0) FROM technical_revisions WHERE id=?",[&pin.object_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(storage)?;
            self.capacity(encoded.len() as i64-bytes,Some(&pin.object_id),Some((key, value)),1-count)?;
            let next = pin.revision+1;
            let changed = self.db.execute("UPDATE technical_documents SET current_revision=?,value_json=? WHERE id=? AND key=? AND current_revision=?",params![next,head,pin.object_id,key,pin.revision]).map_err(storage)?;
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
