//! Rebuildable facts owned by the exclusive journal connection. Canonical
//! format-8 rows remain the authority; no older owner can leave durable facts
//! stale. Opaque receipts are inspected once when written or on owner startup.
use super::{contracts::field, *};
use ivy_contracts::json::parse;
use ivy_store::receipts::has_unresolved_outcome;

impl JournalDb<'_> {
    pub(super) fn rebuild_projection(&self) -> IvyResult<()> {
        self.db.execute_batch("CREATE TEMP TABLE IF NOT EXISTS phone_command_facts(
          operation_id TEXT PRIMARY KEY,call_id TEXT,epoch TEXT NOT NULL,active INTEGER NOT NULL,
          submitted INTEGER NOT NULL,archive_pending INTEGER NOT NULL,unresolved INTEGER NOT NULL,
          ordinary INTEGER NOT NULL,positive_release INTEGER NOT NULL,released INTEGER NOT NULL,
          archive_order INTEGER NOT NULL) STRICT;
          CREATE INDEX IF NOT EXISTS temp.phone_facts_by_call ON phone_command_facts(call_id,active,submitted,epoch);
          CREATE INDEX IF NOT EXISTS temp.phone_facts_unresolved ON phone_command_facts(call_id,unresolved);
          CREATE INDEX IF NOT EXISTS temp.phone_facts_release ON phone_command_facts(call_id,positive_release);
          CREATE INDEX IF NOT EXISTS temp.phone_facts_history ON phone_command_facts(active,ordinary,released,archive_order);
          CREATE INDEX IF NOT EXISTS temp.phone_facts_archive_pending ON phone_command_facts(active,archive_pending);
          CREATE TEMP TRIGGER IF NOT EXISTS phone_archive_fact_delete AFTER DELETE ON main.receipt_archive
            WHEN OLD.kind='command' BEGIN DELETE FROM phone_command_facts WHERE operation_id=OLD.key AND active=0; END;
          DELETE FROM phone_command_facts;").map_err(error)?;
        // Bounded startup pages also accept the existing canonical archive. The
        // active row wins if an interrupted external fixture contains both.
        for (sql, active) in [
            (
                "SELECT rowid,value FROM receipt_archive WHERE kind='command' AND rowid>? ORDER BY rowid LIMIT ?",
                false,
            ),
            (
                "SELECT rowid,value FROM commands WHERE rowid>? ORDER BY rowid LIMIT ?",
                true,
            ),
        ] {
            let mut after = 0i64;
            loop {
                let rows = self
                    .db
                    .prepare_cached(sql)
                    .map_err(error)?
                    .query_map(params![after, maintenance::PAGE], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(error)?;
                if rows.is_empty() {
                    break;
                }
                for (order, text) in rows {
                    after = order;
                    self.project_command(&parse(&text, usize::MAX)?, active)?;
                }
            }
        }
        self.maintenance_pending.set(true);
        Ok(())
    }

    pub(super) fn project_command(&self, value: &IvyJson, active: bool) -> IvyResult<()> {
        let intent = field(value, "intent")?;
        let id = field(intent, "operationId")?.as_str()?;
        let phase = field(value, "phase")?.as_str()?;
        let method = field(intent, "method")?.as_str()?;
        let call = intent
            .get("callId")
            .and_then(IvyJson::as_js_string)
            .map(JsString::to_utf8)
            .transpose()?;
        let archive_pending = method == "call.archiveVoice"
            && (phase != "result"
                || value
                    .get("receipt")
                    .and_then(|v| v.get("result"))
                    .and_then(|v| v.get("state"))
                    != Some(&"archived".into()))
            && value.get("archiveResolution").and_then(|v| v.get("state"))
                != Some(&"observed_archived".into());
        let unresolved = phase != "result" || archive_pending || has_unresolved_outcome(value)?;
        let ordinary = !unresolved
            && ![
                "call.bindVoice",
                "call.createVoice",
                "call.selectSpeakerVoice",
                "call.archiveVoice",
                "call.release",
            ]
            .contains(&method.as_str());
        let positive_release = phase == "result"
            && method == "call.release"
            && value.get("receipt").and_then(|v| v.get("ok")) == Some(&true.into());
        self.db.execute("INSERT INTO phone_command_facts VALUES(?,?,?,?,?,?,?,?,?,
            EXISTS(SELECT 1 FROM receipt_archive_history WHERE kind='command' AND key=?),
            COALESCE((SELECT rowid FROM receipt_archive WHERE kind='command' AND key=?),0))
          ON CONFLICT(operation_id) DO UPDATE SET call_id=excluded.call_id,epoch=excluded.epoch,
            active=excluded.active,submitted=excluded.submitted,archive_pending=excluded.archive_pending,
            unresolved=excluded.unresolved,ordinary=excluded.ordinary,positive_release=excluded.positive_release,
            released=excluded.released,archive_order=excluded.archive_order",
            params![id, call, field(intent, "epoch")?.as_str()?, active, phase == "submitted", archive_pending,
                unresolved, ordinary, positive_release, id, id]).map_err(error)?;
        self.maintenance_pending.set(true);
        Ok(())
    }
}
