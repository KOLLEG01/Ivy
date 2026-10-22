//! Explicit import mutations preserve original technical revision history.
use super::*;

pub struct ImportedCaptureAcknowledgements {
    /// Original successful checkpoints, with their pins, names and values.
    pub sources: Vec<Document>,
    pub inserted: Vec<String>,
    pub next_cursor: Option<String>,
}
impl Journal {
    /// One bounded explicit import page. The source checkpoint rows/revisions
    /// are unchanged. Ordinary opens never backfill old acknowledgements; new
    /// checkpoint completion records them in the existing atomic transaction.
    pub fn import_capture_acknowledgements(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> IvyResult<ImportedCaptureAcknowledgements> {
        if !(1..=1000).contains(&limit) {
            return Err(IvyError::new(
                "invalid_request",
                "Select an import page of 1 to 1000 checkpoints.",
            ));
        }
        self.transaction(|| {
            let rows = self.db.prepare("SELECT id,name,current_revision,value_json FROM documents WHERE key='secretary/operation' AND json_extract(value_json,'$.action')='checkpoint' AND json_extract(value_json,'$.phase')='succeeded' AND (? IS NULL OR id>?) ORDER BY id LIMIT ?")
                .map_err(storage)?
                .query_map(params![after, after, limit as i64 + 1], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?)))
                .map_err(storage)?.collect::<rusqlite::Result<Vec<_>>>().map_err(storage)?;
            let next_cursor = (rows.len() > limit).then(|| rows[limit-1].0.clone());
            let mut sources = vec![];
            let mut inserted = vec![];
            for (id,name,revision,json) in rows.into_iter().take(limit) {
                let value = parse(&json, FRAME)?;
                if let Some(captures) = value.get("checkpointCaptures").filter(|v| !v.is_null()) {
                    let invalid = || IvyError::new("secretary_import_checkpoint_invalid", "The retained checkpoint has no valid capture identity.");
                    for capture in captures.as_array().ok_or_else(invalid)? {
                        let id = capture.get("objectId").and_then(IvyJson::as_js_string).and_then(|s| s.to_utf8().ok()).ok_or_else(invalid)?;
                        if self.db.execute("INSERT OR IGNORE INTO capture_acknowledgements VALUES(?)", [&id]).map_err(storage)? != 0 {
                            inserted.push(id);
                        }
                    }
                }
                sources.push(Document { pin: Pin {object_id:id,revision},name,value });
            }
            Ok(ImportedCaptureAcknowledgements {sources, inserted, next_cursor})
        })
    }
    /// Explicit import appends a converted revision without normal workflow
    /// compaction or quota cleanup. Refuse insufficient room without changing
    /// any original history, expiry fence or unrelated retained work.
    pub fn import_technical_revision(
        &self,
        key: &str,
        pin: &Pin,
        value: &IvyJson,
    ) -> IvyResult<Document> {
        let encoded = canonical(value, FRAME)?;
        self.transaction(|| {
            let current = self.read(key, &pin.object_id, None, true)?;
            if current.pin != *pin {
                return Err(IvyError::new("revision_conflict", "The local workflow row changed."));
            }
            if canonical(&current.value, FRAME)? == encoded { return Ok(current); }
            let usage = self.usage()?;
            if usage.count >= self.limits.count || usage.bytes + encoded.len() as i64 > self.limits.bytes {
                return Err(IvyError::new("capacity_exceeded", "Explicit import needs room to retain the original journal history."));
            }
            let next = pin.revision.checked_add(1).ok_or_else(IvyError::internal)?;
            let changed = self.db.execute("UPDATE technical_documents SET current_revision=?,value_json=? WHERE id=? AND key=? AND current_revision=?", params![next, encoded, pin.object_id, key, pin.revision]).map_err(storage)?;
            if changed != 1 { return Err(IvyError::new("revision_conflict", "The local workflow row changed.")); }
            self.db.execute("INSERT INTO technical_revisions VALUES(?,?,?)", params![pin.object_id, next, encoded]).map_err(storage)?;
            self.read(key, &pin.object_id, Some(next), true)
        })
    }
}
