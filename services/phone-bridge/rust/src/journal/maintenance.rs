use super::contracts::{field, uuid};
use super::*;
use ivy_contracts::{
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};

pub(super) struct Usage {
    pub operations: f64,
    pub calls: f64,
    pub reserved_bytes: f64,
    pub epochs: f64,
}
impl PhoneJournal {
    pub async fn epoch(&self) -> IvyResult<Option<JsString>> {
        self.request(|db| Ok(db.epoch()?.map(Into::into))).await
    }
    pub async fn epoch_operations(&self) -> IvyResult<f64> {
        self.request(|db| db.counter("epochOperations")).await
    }
    pub async fn status(&self) -> IvyResult<IvyJson> {
        self.request(|db| {
            let usage = db.usage()?;
            Ok(IvyJson::object([
                ("operations", IvyJson::Number(usage.operations)),
                ("calls", IvyJson::Number(usage.calls)),
                ("reservedBytes", IvyJson::Number(usage.reserved_bytes)),
                ("epochs", IvyJson::Number(usage.epochs)),
            ]))
        })
        .await
    }
    pub async fn archive_status(&self) -> IvyResult<IvyJson> {
        self.request(|db| {
            let usage = db.archive.status()?;
            Ok(IvyJson::object([
                ("records", IvyJson::Number(usage.records as f64)),
                ("bytes", IvyJson::Number(usage.bytes as f64)),
            ]))
        })
        .await
    }
    /// The process owner must first resolve OS ownership and retire its old epoch.
    pub async fn begin_epoch(&self, epoch: JsString) -> IvyResult<()> {
        self.request(move |db| db.begin_epoch(epoch)).await
    }
    /// A late old close cannot retire a new process or resolve lost writes.
    pub async fn lose_epoch(&self, epoch: JsString) -> IvyResult<()> {
        self.request(move |db| db.lose_epoch(epoch)).await
    }
    /// Original startup checks, retirement and begin execute without yielding
    /// between journal operations. Keep their separate original transactions.
    pub(crate) async fn commit_process_epoch(
        &self,
        previous: Option<JsString>,
        epoch: JsString,
    ) -> IvyResult<()> {
        self.request(move |db| {
            require(
                db.epoch()?.map(JsString::from) == previous,
                "phone_epoch_mismatch",
                "Phone journal ownership changed during launch.",
            )?;
            if let Some(previous) = previous {
                db.lose_epoch(previous)?;
            }
            db.begin_epoch(epoch)
        })
        .await
    }
}
impl JournalDb<'_> {
    fn begin_epoch(&self, epoch: JsString) -> IvyResult<()> {
        let db = self;
        uuid(Some(&epoch.clone().into()))?;
        db.compact()?;
        db.transaction(|db| {
            require(
                db.epoch()?.is_none(),
                "phone_epoch_active",
                "The original Phone epoch must be retired before another process starts.",
            )?;
            let id = epoch.to_utf8()?;
            let exists = db
                .db
                .query_row("SELECT epoch FROM epochs WHERE epoch=?", [&id], |_| Ok(()))
                .optional()
                .map_err(error)?
                .is_some();
            require(
                !exists
                    && db
                        .archive
                        .read("epoch", &id)?
                        .is_none_or(|value| value.is_empty()),
                "mutation_conflict",
                "A Phone process epoch can never be reused.",
            )?;
            let count = db.usage()?.epochs;
            require(
                count < db.limits.max_epochs as f64,
                "phone_journal_capacity",
                "Phone epoch history is full.",
            )?;
            db.db
                .execute("INSERT INTO epochs VALUES (?)", [&id])
                .map_err(error)?;
            db.set_counter("epochs", count + 1.0)?;
            db.set_meta("epoch", &id)?;
            db.set_meta("epochOperations", "0")
        })
    }
    fn lose_epoch(&self, epoch: JsString) -> IvyResult<()> {
        let db = self;
        uuid(Some(&epoch.clone().into()))?;
        db.transaction(|db| {
                let id = epoch.to_utf8()?;
                if db.epoch()?.as_deref() != Some(&id) {return Ok(())}
                let now = super::writes::timestamp();
                loop {
                    let operation_id: Option<String> = db.db.query_row("SELECT operation_id FROM commands WHERE epoch=? AND phase='submitted' LIMIT 1", [&id], |row| row.get(0)).optional().map_err(error)?;
                    let Some(operation_id) = operation_id else {break};
                    let mut operation = db.operation(&operation_id.into())?.ok_or_else(IvyError::internal)?;
                    operation.insert("phase", "outcome_unknown".into());
                    operation.insert("updatedAt", now.clone().into());
                    db.save(operation)?;
                }
                db.set_meta("epoch", "")?;
                db.db.execute_batch("DELETE FROM active_calls;").map_err(error)
            })
    }
    pub(super) fn counter(&self, key: &str) -> IvyResult<f64> {
        let Some(text) = self.meta(key)? else {
            return Ok(0.0);
        };
        let count = text.parse::<u64>().map_err(|_| {
            IvyError::new(
                "phone_storage_invalid",
                "Phone journal counter is not an unsigned integer.",
            )
        })?;
        require(
            count <= 9_007_199_254_740_991,
            "phone_storage_invalid",
            "Phone journal counter exceeds its JSON bound.",
        )?;
        Ok(count as f64)
    }
    pub(super) fn set_counter(&self, key: &str, count: f64) -> IvyResult<()> {
        require(
            count.is_finite()
                && count.fract() == 0.0
                && (0.0..=9_007_199_254_740_991.0).contains(&count),
            "phone_storage_invalid",
            "Phone journal counter must be a bounded unsigned integer.",
        )?;
        self.set_meta(key, &(count as u64).to_string())
    }
    pub(super) fn epoch(&self) -> IvyResult<Option<String>> {
        Ok(self.meta("epoch")?.filter(|value| !value.is_empty()))
    }
    pub(super) fn epoch_json(&self) -> IvyResult<IvyJson> {
        Ok(self.epoch()?.map_or(IvyJson::Null, Into::into))
    }
    pub(super) fn usage(&self) -> IvyResult<Usage> {
        let operations = self.counter("count")?;
        let calls = self.counter("calls")?;
        Ok(Usage {
            operations,
            calls,
            reserved_bytes: (operations + self.counter("targets")?) * OUTCOME_BYTES as f64
                + calls * 65_536.0,
            epochs: self.counter("epochs")?,
        })
    }
    pub(super) fn compact(&self) -> IvyResult<()> {
        self.transaction(|db| {
            // Materialize each selection before mutating it, like the original .all().
            let calls = db.db.prepare_cached("SELECT call_id,value,operation_id FROM calls WHERE call_id NOT IN (SELECT call_id FROM active_calls) AND NOT EXISTS
        (SELECT 1 FROM commands WHERE commands.call_id=calls.call_id AND
          (phase='submitted' AND epoch=? OR method='call.archiveVoice' AND
            (phase!='result' OR json_extract(value,'$.receipt.result.state') IS NOT 'archived') AND
            json_extract(value,'$.archiveResolution.state') IS NOT 'observed_archived')) LIMIT 32").map_err(error)?
                .query_map([db.epoch()?.unwrap_or_default()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))).map_err(error)?
                .collect::<Result<Vec<_>,_>>().map_err(error)?;
            for (id, value, operation_id) in calls {
                db.archive.retain("call", &id, &value)?;
                db.archive.retain("call-operation", &operation_id, &id)?;
                let commands = db.db.prepare_cached("SELECT operation_id,method,value FROM commands WHERE call_id=? ORDER BY rowid").map_err(error)?
                    .query_map([&id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))).map_err(error)?
                    .collect::<Result<Vec<_>,_>>().map_err(error)?;
                for (operation_id, method, value) in commands {
                    db.archive.retain("command", &operation_id, &value)?;
                    let command = parse(&value, usize::MAX).map_err(|_| IvyError::internal())?;
                    let intent = field(&command, "intent")?;
                    let generation = intent.get("voiceGeneration").filter(|value| !value.is_null()).cloned().unwrap_or(IvyJson::Number(0.0));
                    let mut key = vec![id.clone().into(), method.clone().into()];
                    if generation != IvyJson::Number(0.0) {key.push(generation);}
                    let key = canonical(&IvyJson::Array(key), MANAGEMENT_FRAME_BYTES)?;
                    if db.archive.read("call-command", &key)?.is_none() {db.archive.retain("call-command", &key, &operation_id)?;}
                    if intent.get("commandSequence").and_then(IvyJson::as_f64).is_some_and(|sequence| sequence > 0.0) {
                        let kind = match method.as_str() {"call.command.feedback" => Some("command-feedback"), "call.selectVoice" => Some("voice-selection"), _ => None};
                        if let Some(kind) = kind {
                            db.archive.retain(kind, &canonical(&IvyJson::Array(vec![id.clone().into(), field(intent, "commandSequence")?.clone()]), MANAGEMENT_FRAME_BYTES)?, &operation_id)?;
                        }
                    }
                }
                let target: Option<String> = db.db.query_row("SELECT value FROM targets WHERE call_id=?", [&id], |row| row.get(0)).optional().map_err(error)?;
                if let Some(target) = target { db.archive.retain("target", &id, &target)?; }
                for sql in ["DELETE FROM commands WHERE call_id=?", "DELETE FROM targets WHERE call_id=?", "DELETE FROM calls WHERE call_id=?"] { db.db.execute(sql, [&id]).map_err(error)?; }
            }
            let commands = db.db.prepare_cached("SELECT operation_id,value FROM commands WHERE call_id IS NULL AND phase IN ('result','outcome_unknown') LIMIT 128").map_err(error)?
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).map_err(error)?.collect::<Result<Vec<_>,_>>().map_err(error)?;
            for (id, value) in commands {
                db.archive.retain("command", &id, &value)?;
                db.db.execute("DELETE FROM commands WHERE operation_id=?", [&id]).map_err(error)?;
            }
            let epochs = db.db.prepare_cached("SELECT epoch FROM epochs WHERE epoch!=? LIMIT 128").map_err(error)?
                .query_map([db.epoch()?.unwrap_or_default()], |row| row.get::<_, String>(0)).map_err(error)?.collect::<Result<Vec<_>,_>>().map_err(error)?;
            for epoch in epochs {
                db.archive.retain("epoch", &epoch, &epoch)?;
                db.db.execute("DELETE FROM epochs WHERE epoch=?", [&epoch]).map_err(error)?;
            }
            // Admission IDs, Voice pointers and all uncertainty remain recovery state.
            let released = db.db.prepare_cached("SELECT a.key FROM receipt_archive a
        WHERE a.kind='command' AND json_extract(a.value,'$.phase')='result'
          AND json_extract(a.value,'$.intent.method') NOT IN
            ('call.bindVoice','call.createVoice','call.selectSpeakerVoice','call.archiveVoice','call.release')
          AND NOT EXISTS(SELECT 1 FROM receipt_archive_history h WHERE h.kind=a.kind AND h.key=a.key)
          AND NOT EXISTS(SELECT 1 FROM json_tree(a.value) j WHERE
            j.key IN ('phase','state','ownerPhase','outcome','execution') AND j.value IN ('unknown','outcome_unknown','needs_attention') OR
            j.key IN ('error','errorCode','code') AND (j.value GLOB '*outcome_unknown*' OR j.value IN ('native_connection_lost','native_epoch_lost')))
          AND (json_extract(a.value,'$.intent.callId') IS NULL OR (
            EXISTS(SELECT 1 FROM receipt_archive r WHERE r.kind='command'
              AND json_extract(r.value,'$.intent.callId')=json_extract(a.value,'$.intent.callId')
              AND json_extract(r.value,'$.intent.method')='call.release' AND json_extract(r.value,'$.receipt.ok')=1)
            AND NOT EXISTS(SELECT 1 FROM receipt_archive p WHERE p.kind='command'
              AND json_extract(p.value,'$.intent.callId')=json_extract(a.value,'$.intent.callId') AND json_extract(p.value,'$.phase')!='result')
            AND NOT EXISTS(SELECT 1 FROM receipt_archive u,json_tree(u.value) j WHERE u.kind='command'
              AND json_extract(u.value,'$.intent.callId')=json_extract(a.value,'$.intent.callId')
              AND (j.key IN ('phase','state','ownerPhase','outcome','execution') AND j.value IN ('unknown','outcome_unknown','needs_attention') OR
                j.key IN ('error','errorCode','code') AND (j.value GLOB '*outcome_unknown*' OR j.value IN ('native_connection_lost','native_epoch_lost'))))))
        ORDER BY a.rowid LIMIT 128").map_err(error)?.query_map([], |row| row.get::<_, String>(0)).map_err(error)?.collect::<Result<Vec<_>,_>>().map_err(error)?;
            for key in released { db.archive.release("command", &key)?; }
            for (key, sql) in [("count", "SELECT count(*) FROM commands"), ("epochs", "SELECT count(*) FROM epochs"), ("calls", "SELECT count(*) FROM calls"), ("targets", "SELECT count(*) FROM targets")] {
                let count: i64 = db.db.query_row(sql, [], |row| row.get(0)).map_err(error)?;
                db.set_meta(key, &count.to_string())?;
            }
            Ok(())
        })
    }
}
