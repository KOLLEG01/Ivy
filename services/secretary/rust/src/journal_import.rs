//! Explicit import mutations preserve original technical revision history.
use super::*;

impl Journal {
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
