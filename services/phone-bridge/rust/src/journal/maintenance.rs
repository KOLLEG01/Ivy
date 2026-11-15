use super::contracts::{field, uuid};
use super::*;
use ivy_contracts::{
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};

pub(super) const PAGE: i64 = 16;
pub(super) const INTERVAL: Duration = Duration::from_millis(25);

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
            db.set_meta("epochOperations", "0")?;
            db.maintenance_pending.set(true);
            Ok(())
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
                db.db.execute_batch("DELETE FROM active_calls;").map_err(error)?;
                db.maintenance_pending.set(true);
                Ok(())
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
    /// One small transaction between original jobs, never inside submit/admit.
    /// Partial calls retain their admission until their final command is moved.
    pub(super) fn compact_page(&self) -> IvyResult<bool> {
        let progress = self.transaction(|db| {
            let mut progress = false;
            let call: Option<(String, String, String)> = db.db.query_row(
                "SELECT c.call_id,c.value,c.operation_id FROM calls c
                 WHERE NOT EXISTS(SELECT 1 FROM active_calls a WHERE a.call_id=c.call_id)
                   AND NOT EXISTS(SELECT 1 FROM phone_command_facts f WHERE f.call_id=c.call_id AND f.active=1
                     AND (f.submitted=1 AND f.epoch=? OR f.archive_pending=1))
                 ORDER BY c.rowid LIMIT 1", [db.epoch()?.unwrap_or_default()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).optional().map_err(error)?;
            if let Some((id, value, operation_id)) = call {
                let commands = db.db.prepare_cached(
                    "SELECT operation_id,method,value FROM commands WHERE call_id=? ORDER BY rowid LIMIT ?"
                ).map_err(error)?.query_map(params![id, PAGE], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
                }).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
                for (operation, method, text) in commands {
                    db.retire_command(&operation, Some((&id, &method)), &text)?;
                    progress = true;
                }
                let remaining = db.db.query_row("SELECT 1 FROM commands WHERE call_id=? LIMIT 1",
                    [&id], |_| Ok(())).optional().map_err(error)?.is_some();
                if !remaining {
                    db.archive.retain("call", &id, &value)?;
                    db.archive.retain("call-operation", &operation_id, &id)?;
                    let target: Option<String> = db.db.query_row("SELECT value FROM targets WHERE call_id=?",
                        [&id], |row| row.get(0)).optional().map_err(error)?;
                    if let Some(target) = target { db.archive.retain("target", &id, &target)?; }
                    let targets = db.db.execute("DELETE FROM targets WHERE call_id=?", [&id]).map_err(error)?;
                    db.subtract("targets", targets)?;
                    let calls = db.db.execute("DELETE FROM calls WHERE call_id=?", [&id]).map_err(error)?;
                    db.subtract("calls", calls)?;
                    progress = true;
                }
            }
            let commands = db.db.prepare_cached(
                "SELECT c.operation_id,c.value FROM phone_command_facts f JOIN commands c ON c.operation_id=f.operation_id
                 WHERE f.active=1 AND f.call_id IS NULL AND f.submitted=0 ORDER BY c.rowid LIMIT ?"
            ).map_err(error)?.query_map([PAGE], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
            for (id, text) in commands {
                db.retire_command(&id, None, &text)?;
                progress = true;
            }
            let epochs = db.db.prepare_cached("SELECT epoch FROM epochs WHERE epoch!=? ORDER BY rowid LIMIT ?")
                .map_err(error)?.query_map(params![db.epoch()?.unwrap_or_default(), PAGE], |row| row.get::<_, String>(0))
                .map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
            for epoch in epochs {
                db.archive.retain("epoch", &epoch, &epoch)?;
                let removed = db.db.execute("DELETE FROM epochs WHERE epoch=?", [&epoch]).map_err(error)?;
                db.subtract("epochs", removed)?;
                progress = true;
            }
            // Every command in a partly moved call still participates in its
            // uncertainty/release fence. No JSON trees or whole-table counts.
            let released = db.db.prepare_cached(
                "SELECT f.operation_id FROM phone_command_facts f
                 WHERE f.active=0 AND f.ordinary=1 AND f.released=0
                   AND (f.call_id IS NULL OR (
                     NOT EXISTS(SELECT 1 FROM phone_command_facts a WHERE a.call_id=f.call_id AND a.active=1)
                     AND EXISTS(SELECT 1 FROM phone_command_facts r WHERE r.call_id=f.call_id AND r.positive_release=1)
                     AND NOT EXISTS(SELECT 1 FROM phone_command_facts u WHERE u.call_id=f.call_id AND u.unresolved=1)))
                 ORDER BY f.archive_order LIMIT ?"
            ).map_err(error)?.query_map([PAGE], |row| row.get::<_, String>(0))
                .map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
            for key in released {
                db.archive.release("command", &key)?;
                db.db.execute("UPDATE phone_command_facts SET released=1 WHERE operation_id=?", [&key]).map_err(error)?;
                progress = true;
            }
            Ok(progress)
        })?;
        // Rollback leaves canonical rows and the TEMP projection in the same
        // transaction. A stable blocked set sleeps until an actual write.
        self.maintenance_pending.set(progress);
        Ok(progress)
    }
    fn subtract(&self, key: &str, removed: usize) -> IvyResult<()> {
        if removed != 0 {
            self.set_counter(key, self.counter(key)? - removed as f64)?;
        }
        Ok(())
    }
    fn retire_command(&self, id: &str, call: Option<(&str, &str)>, text: &str) -> IvyResult<()> {
        self.archive.retain("command", id, text)?;
        let command = parse(text, usize::MAX).map_err(|_| IvyError::internal())?;
        if let Some((call_id, method)) = call {
            let intent = field(&command, "intent")?;
            let generation = intent
                .get("voiceGeneration")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(IvyJson::Number(0.0));
            let mut key = vec![call_id.into(), method.into()];
            if generation != IvyJson::Number(0.0) {
                key.push(generation);
            }
            let key = canonical(&IvyJson::Array(key), MANAGEMENT_FRAME_BYTES)?;
            if self.archive.read("call-command", &key)?.is_none() {
                self.archive.retain("call-command", &key, id)?;
            }
            if intent
                .get("commandSequence")
                .and_then(IvyJson::as_f64)
                .is_some_and(|n| n > 0.0)
            {
                let kind = match method {
                    "call.command.feedback" => Some("command-feedback"),
                    "call.selectVoice" => Some("voice-selection"),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let key = canonical(
                        &IvyJson::Array(vec![
                            call_id.into(),
                            field(intent, "commandSequence")?.clone(),
                        ]),
                        MANAGEMENT_FRAME_BYTES,
                    )?;
                    self.archive.retain(kind, &key, id)?;
                }
            }
        }
        self.project_command(&command, false)?;
        let removed = self
            .db
            .execute("DELETE FROM commands WHERE operation_id=?", [id])
            .map_err(error)?;
        self.subtract("count", removed)
    }
}
