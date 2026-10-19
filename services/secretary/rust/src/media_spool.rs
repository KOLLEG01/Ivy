//! Durable media acquisition state. The runtime owns the checked directory and
//! exclusive owner connection; this module owns the unchanged media.sqlite format.
use crate::media_schema::{MediaSchema, validate_media};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::hash_json,
    json::{canonical, parse},
};
use ivy_store::{
    rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params},
    sqlite,
};
use std::collections::HashSet;

const FRAME: usize = 32 * 1024 * 1024;
pub const MAXIMUM_BYTES: i64 = 32 * 1024 * 1024;
pub const QUOTA_BYTES: i64 = 512 * 1024 * 1024;
const NATIVE_RESERVE: i64 = 8 * 1024 * 1024;
const MESSAGE: &str =
    "Media spool must retain its exact original owner, intent, evidence and complete bytes.";

/// Supplied by the native adapter. Media schemas are always checked locally;
/// native operation/progress validation remains required before persistence.
pub trait Validation {
    fn operation(&self, value: &IvyJson) -> IvyResult<()>;
    fn progress(&self, previous: &IvyJson, next: &IvyJson) -> IvyResult<()>;
}
fn check(condition: bool, code: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, MESSAGE))
    }
}
fn encode(value: &IvyJson) -> IvyResult<String> {
    canonical(value, FRAME)
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(encode(a)? == encode(b)?)
}
fn field<'a>(v: &'a IvyJson, name: &str) -> IvyResult<&'a IvyJson> {
    v.required(name)
}
fn id(v: &IvyJson, name: &str) -> IvyResult<JsString> {
    field(v, name)?
        .as_js_string()
        .cloned()
        .ok_or_else(|| IvyError::new("media_spool_invalid", MESSAGE))
}
fn number(v: &IvyJson, name: &str) -> IvyResult<f64> {
    field(v, name)?
        .as_f64()
        .ok_or_else(|| IvyError::new("media_spool_invalid", MESSAGE))
}
// node:sqlite binds JS text through UTF-8 replacement; JSON identities stay lossless.
fn sql_text(v: &JsString) -> String {
    String::from_utf16_lossy(&v.0)
}
fn decode(value: Option<String>) -> IvyResult<Option<IvyJson>> {
    value.map(|s| parse(&s, FRAME)).transpose()
}
#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    pub intent: IvyJson,
    pub phase: String,
    pub evidence: Option<IvyJson>,
    pub error_code: Option<String>,
}
struct Row {
    intent: String,
    materialization: String,
    phase: String,
    evidence: Option<String>,
    payload: Option<Vec<u8>>,
    error_code: Option<String>,
}
impl Row {
    fn attempt(&self) -> IvyResult<Attempt> {
        Ok(Attempt {
            intent: parse(&self.intent, FRAME)?,
            phase: self.phase.clone(),
            evidence: decode(self.evidence.clone())?,
            error_code: self.error_code.clone(),
        })
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct NativeCall {
    pub call: IvyJson,
    pub seen: Option<IvyJson>,
    pub observation: Option<IvyJson>,
}
pub struct SpoolJournal<V> {
    pub db: Connection,
    validation: V,
    active: HashSet<JsString>,
    // Fields drop in declaration order: media.sqlite closes before owner.sqlite.
    _owner: Option<Connection>,
}
impl<V: Validation> SpoolJournal<V> {
    /// Open the original protected spool and retain its exclusive owner
    /// transaction until this journal closes or its process exits.
    pub fn open(root: &std::path::Path, owner: &IvyJson, validation: V) -> IvyResult<Self> {
        let (db, lock) = crate::media_spool_owner::open(root)?;
        let mut journal = Self::from_owned_connection(db, owner, validation)?;
        journal._owner = Some(lock);
        Ok(journal)
    }
    /// Caller must hold the original exclusive owner.sqlite transaction and
    /// validate direct files/permissions before opening this media connection.
    pub fn from_owned_connection(
        db: Connection,
        owner: &IvyJson,
        validation: V,
    ) -> IvyResult<Self> {
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(sqlite::error)?;
        let has_tables: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table')",
                [],
                |r| r.get(0),
            )
            .map_err(sqlite::error)?;
        check(
            version == 1 || (version == 0 && !has_tables),
            "media_spool_format_unsupported",
        )?;
        db.execute_batch(include_str!("media_spool.sql"))
            .map_err(sqlite::error)?;
        let owner_hash = hash_json(owner)?;
        db.execute(
            "INSERT OR IGNORE INTO identity(singleton,owner_hash) VALUES(1,?)",
            [&owner_hash],
        )
        .map_err(sqlite::error)?;
        let original: String = db
            .query_row(
                "SELECT owner_hash FROM identity WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(sqlite::error)?;
        check(original == owner_hash, "media_spool_owner_changed")?;
        db.execute("UPDATE attempts SET phase='failed',error_code='media_download_interrupted' WHERE phase='downloading'", []).map_err(sqlite::error)?;
        Ok(Self {
            db,
            validation,
            active: HashSet::new(),
            _owner: None,
        })
    }
    fn row(&self, operation_id: &JsString) -> IvyResult<Option<Row>> {
        self.db.query_row("SELECT intent,materialization,phase,evidence,payload,error_code FROM attempts WHERE operation_id=?", [sql_text(operation_id)], |r| Ok(Row { intent:r.get(0)?, materialization:r.get(1)?, phase:r.get(2)?, evidence:r.get(3)?, payload:r.get(4)?, error_code:r.get(5)? })).optional().map_err(sqlite::error)
    }
    pub fn get(&self, operation_id: &JsString) -> IvyResult<Option<Attempt>> {
        self.row(operation_id)?.map(|r| r.attempt()).transpose()
    }
    pub fn used_bytes(&self) -> IvyResult<i64> {
        self.db.query_row("SELECT (SELECT COALESCE(SUM(CASE WHEN phase IN ('prepared','downloading') THEN ? ELSE COALESCE(LENGTH(payload),0) END + LENGTH(CAST(intent AS BLOB)) + LENGTH(CAST(materialization AS BLOB)) + COALESCE(LENGTH(CAST(evidence AS BLOB)),0)),0) FROM attempts) + (SELECT COALESCE(SUM(LENGTH(CAST(value AS BLOB))),0) FROM read_plans) + (SELECT COALESCE(SUM(LENGTH(CAST(value AS BLOB))+CASE WHEN observation IS NULL THEN 8388608 ELSE COALESCE(LENGTH(CAST(seen AS BLOB)),0)+LENGTH(CAST(observation AS BLOB)) END),0) FROM read_calls)", [MAXIMUM_BYTES], |r| r.get(0)).map_err(sqlite::error)
    }
    pub fn archive_status(&self) -> IvyResult<IvyJson> {
        Ok(IvyJson::object([
            ("records", IvyJson::Number(0.0)),
            ("bytes", IvyJson::Number(0.0)),
            ("activeBytes", IvyJson::Number(self.used_bytes()? as f64)),
            ("maximumBytes", IvyJson::Number(QUOTA_BYTES as f64)),
        ]))
    }
    pub fn prepare(&self, intent: &IvyJson, materialization: &IvyJson) -> IvyResult<Attempt> {
        validate_media(MediaSchema::Intent, intent)?;
        validate_media(MediaSchema::Materialization, materialization)?;
        let admission = field(intent, "admission")?;
        check(
            field(field(intent, "item")?, "objectId")? == field(admission, "itemObjectId")?
                && field(intent, "materializationHash")?.as_str()? == hash_json(materialization)?
                && field(admission, "providerMessageId")?
                    == field(field(materialization, "attachment")?, "messageId")?,
            "media_spool_intent_mismatch",
        )?;
        let operation_id = id(intent, "operationId")?;
        if let Some(old) = self.row(&operation_id)? {
            let attempt = old.attempt()?;
            check(
                same(&attempt.intent, intent)?
                    && same(&parse(&old.materialization, FRAME)?, materialization)?,
                "media_spool_intent_mismatch",
            )?;
            return Ok(attempt);
        }
        let count: i64 = self
            .db
            .query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get(0))
            .map_err(sqlite::error)?;
        let charge = encode(&IvyJson::Array(vec![
            intent.clone(),
            materialization.clone(),
        ]))?
        .len() as i64;
        check(
            count < 4096
                && self.used_bytes()? + MAXIMUM_BYTES + 2 * NATIVE_RESERVE + charge <= QUOTA_BYTES,
            "media_spool_full",
        )?;
        self.db.execute("INSERT INTO attempts(operation_id,intent,materialization,phase) VALUES(?,?,?,'prepared')",params![sql_text(&operation_id),encode(intent)?,encode(materialization)?]).map_err(sqlite::error)?;
        self.get(&operation_id)?
            .ok_or_else(|| IvyError::new("media_spool_intent_missing", MESSAGE))
    }
    /// Called after the runtime's async owner guard and abort check. False means
    /// another acquisition already claimed it; there must be no second GET.
    pub fn begin_acquire(&mut self, operation_id: &JsString) -> IvyResult<bool> {
        check(
            self.row(operation_id)?.is_some(),
            "media_spool_intent_missing",
        )?;
        let changed = self
            .db
            .execute(
                "UPDATE attempts SET phase='downloading' WHERE operation_id=? AND phase='prepared'",
                [sql_text(operation_id)],
            )
            .map_err(sqlite::error)?;
        if changed != 0 {
            self.active.insert(operation_id.clone());
        }
        Ok(changed != 0)
    }
    /// Private signed materialization; never include this value in diagnostics.
    pub fn materialization(&self, operation_id: &JsString) -> IvyResult<IvyJson> {
        let row = self
            .row(operation_id)?
            .ok_or_else(|| IvyError::new("media_spool_intent_missing", MESSAGE))?;
        parse(&row.materialization, FRAME)
    }
    /// The bounded downloader supplies complete, validated evidence and bytes.
    /// Partial bodies never pass this boundary; errors are retained by fail_acquire.
    pub fn complete_acquire(
        &mut self,
        operation_id: &JsString,
        evidence: &IvyJson,
        bytes: &[u8],
    ) -> IvyResult<()> {
        let result = (|| {
            validate_media(MediaSchema::DownloadEvidence, evidence)?;
            self.db.execute("UPDATE attempts SET phase='acquired',evidence=?,payload=? WHERE operation_id=? AND phase='downloading'",params![encode(evidence)?,bytes,sql_text(operation_id)]).map_err(sqlite::error)?;
            Ok(())
        })();
        self.active.remove(operation_id);
        result
    }
    pub fn fail_acquire(&mut self, operation_id: &JsString, code: &str) -> IvyResult<()> {
        let result = self.db.execute("UPDATE attempts SET phase='failed',error_code=? WHERE operation_id=? AND phase='downloading'",params![code,sql_text(operation_id)]).map_err(sqlite::error);
        self.active.remove(operation_id);
        result.map(|_| ())
    }
    pub fn payload(&self, operation_id: &JsString) -> IvyResult<(Attempt, Vec<u8>)> {
        let row = self
            .row(operation_id)?
            .ok_or_else(|| IvyError::new("media_spool_intent_missing", MESSAGE))?;
        check(
            row.phase == "acquired" && row.payload.is_some(),
            "media_spool_payload_unavailable",
        )?;
        Ok((row.attempt()?, row.payload.unwrap()))
    }
    pub fn read_plan(&self, item: &IvyJson) -> IvyResult<Option<IvyJson>> {
        decode(
            self.db
                .query_row(
                    "SELECT value FROM read_plans WHERE item_hash=?",
                    [hash_json(item)?],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sqlite::error)?,
        )
    }
    pub fn prepare_read(&self, plan: &IvyJson) -> IvyResult<IvyJson> {
        validate_media(MediaSchema::ReadPlan, plan)?;
        let item = field(plan, "item")?;
        if let Some(old) = self.read_plan(item)? {
            return Ok(old);
        }
        let encoded = encode(plan)?;
        let count: i64 = self
            .db
            .query_row("SELECT COUNT(*) FROM read_plans", [], |r| r.get(0))
            .map_err(sqlite::error)?;
        check(
            count < 4096
                && self.used_bytes()? + encoded.len() as i64 + NATIVE_RESERVE <= QUOTA_BYTES,
            "media_spool_full",
        )?;
        self.db
            .execute(
                "INSERT INTO read_plans(item_hash,plan_hash,value) VALUES(?,?,?)",
                params![hash_json(item)?, hash_json(plan)?, encoded],
            )
            .map_err(sqlite::error)?;
        Ok(plan.clone())
    }
    pub fn native_call(&self, plan_hash: &str, slot: i64) -> IvyResult<Option<NativeCall>> {
        let raw: Option<(String, Option<String>, Option<String>)> = self
            .db
            .query_row(
                "SELECT value,seen,observation FROM read_calls WHERE plan_hash=? AND slot=?",
                params![plan_hash, slot],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(sqlite::error)?;
        raw.map(|(call, seen, observation)| {
            Ok(NativeCall {
                call: parse(&call, FRAME)?,
                seen: decode(seen)?,
                observation: decode(observation)?,
            })
        })
        .transpose()
    }
    fn coordinates(call: &IvyJson) -> IvyResult<(String, i64)> {
        Ok((
            field(call, "planHash")?.as_str()?,
            number(call, "slot")? as i64,
        ))
    }
    pub fn prepare_native_call(&self, call: &IvyJson) -> IvyResult<NativeCall> {
        validate_media(MediaSchema::ReadCall, call)?;
        let (plan_hash, slot) = Self::coordinates(call)?;
        if let Some(old) = self.native_call(&plan_hash, slot)? {
            check(same(&old.call, call)?, "media_native_intent_mismatch")?;
            return Ok(old);
        }
        let exists: bool = self
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM read_plans WHERE plan_hash=?)",
                [&plan_hash],
                |r| r.get(0),
            )
            .map_err(sqlite::error)?;
        check(exists, "media_native_plan_missing")?;
        let encoded = encode(call)?;
        let count: i64 = self
            .db
            .query_row("SELECT COUNT(*) FROM read_calls", [], |r| r.get(0))
            .map_err(sqlite::error)?;
        check(
            count < 65536
                && self.used_bytes()?
                    + encoded.len() as i64
                    + NATIVE_RESERVE * if slot == 38 { 1 } else { 2 }
                    <= QUOTA_BYTES,
            "media_spool_full",
        )?;
        self.db
            .execute(
                "INSERT INTO read_calls(plan_hash,slot,value) VALUES(?,?,?)",
                params![plan_hash, slot, encoded],
            )
            .map_err(sqlite::error)?;
        Ok(NativeCall {
            call: call.clone(),
            seen: None,
            observation: None,
        })
    }
    pub fn observe_native_call(
        &self,
        call: &IvyJson,
        operation: &IvyJson,
    ) -> IvyResult<NativeCall> {
        let (plan_hash, slot) = Self::coordinates(call)?;
        let original = self
            .native_call(&plan_hash, slot)?
            .ok_or_else(|| IvyError::new("media_native_intent_mismatch", MESSAGE))?;
        check(same(&original.call, call)?, "media_native_intent_mismatch")?;
        self.validation.operation(operation)?;
        let terminal = matches!(
            field(operation, "phase")?.as_str()?.as_str(),
            "succeeded" | "failed"
        );
        let encoded = canonical(operation, 4 * 1024 * 1024)?;
        let request = IvyJson::object([
            ("method", field(call, "method")?.clone()),
            ("params", field(call, "params")?.clone()),
        ]);
        check(
            field(operation, "operationId")? == field(call, "operationId")?
                && field(operation, "method")? == field(call, "method")?
                && same(field(operation, "params")?, field(call, "params")?)?
                && field(operation, "requestHash")?.as_str()? == hash_json(&request)?,
            "media_native_intent_mismatch",
        )?;
        if let Some(previous) = original.observation.as_ref().or(original.seen.as_ref()) {
            self.validation.progress(previous, operation)?;
            if same(previous, operation)? {
                return Ok(original);
            }
        }
        check(
            original.observation.is_none(),
            "media_native_outcome_changed",
        )?;
        let retained_seen = original
            .seen
            .as_ref()
            .map(|v| canonical(v, 4 * 1024 * 1024).map(|s| s.len() as i64))
            .transpose()?
            .unwrap_or(0);
        let charge = if terminal {
            retained_seen + encoded.len() as i64
        } else {
            NATIVE_RESERVE
        };
        check(
            charge <= NATIVE_RESERVE && self.used_bytes()? - NATIVE_RESERVE + charge <= QUOTA_BYTES,
            "media_spool_full",
        )?;
        let sql = if terminal {
            "UPDATE read_calls SET observation=? WHERE plan_hash=? AND slot=?"
        } else {
            "UPDATE read_calls SET seen=? WHERE plan_hash=? AND slot=?"
        };
        self.db
            .execute(sql, params![encoded, plan_hash, slot])
            .map_err(sqlite::error)?;
        Ok(NativeCall {
            call: original.call,
            seen: if terminal {
                original.seen
            } else {
                Some(operation.clone())
            },
            observation: if terminal {
                Some(operation.clone())
            } else {
                None
            },
        })
    }
    pub fn archive_published(&self, plan: &IvyJson) -> IvyResult<()> {
        let item = field(plan, "item")?;
        let old = self.read_plan(item)?;
        check(
            old.as_ref()
                .map(|v| same(v, plan))
                .transpose()?
                .unwrap_or(false),
            "media_spool_invalid",
        )?;
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)
            .map_err(sqlite::error)?;
        let rows: Vec<String> = self.db.prepare("SELECT operation_id FROM attempts WHERE json_extract(intent,'$.item.objectId')=? AND json_extract(intent,'$.item.revision')=?").map_err(sqlite::error)?.query_map(params![sql_text(&id(item,"objectId")?),number(item,"revision")?],|r|r.get(0)).map_err(sqlite::error)?.collect::<Result<_,_>>().map_err(sqlite::error)?;
        for operation_id in rows {
            let original: JsString = operation_id.clone().into();
            let row = self
                .row(&original)?
                .ok_or_else(|| IvyError::new("media_spool_invalid", MESSAGE))?;
            check(
                matches!(row.phase.as_str(), "acquired" | "failed")
                    && !self.active.contains(&original),
                "media_spool_busy",
            )?;
            self.db
                .execute("DELETE FROM attempts WHERE operation_id=?", [operation_id])
                .map_err(sqlite::error)?;
        }
        self.db
            .execute(
                "DELETE FROM read_calls WHERE plan_hash=?",
                [hash_json(plan)?],
            )
            .map_err(sqlite::error)?;
        self.db
            .execute(
                "DELETE FROM read_plans WHERE item_hash=?",
                [hash_json(item)?],
            )
            .map_err(sqlite::error)?;
        tx.commit().map_err(sqlite::error)
    }
    pub fn ensure_idle(&self) -> IvyResult<()> {
        check(self.active.is_empty(), "media_spool_busy")
    }
}
