//! Sampled diagnostics are evictable and remain separate from effect receipts.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::digest,
    json::{canonical, parse},
};
use ivy_store::{
    rusqlite::{Connection, OptionalExtension, params},
    sqlite::error,
};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

struct Signature {
    call: IvyJson,
    closed: Option<bool>,
    failed: Option<bool>,
    audio: Option<JsString>,
}
impl Signature {
    fn new(call: &IvyJson, media: Option<&IvyJson>) -> Self {
        Self {
            call: call.clone(),
            closed: media
                .and_then(|m| m.get("closed"))
                .and_then(IvyJson::as_bool),
            failed: media
                .and_then(|m| m.get("failed"))
                .and_then(IvyJson::as_bool),
            audio: media
                .and_then(|m| m.get("audio"))
                .and_then(|a| a.get("state"))
                .and_then(IvyJson::as_js_string)
                .cloned(),
        }
    }
    fn matches(&self, call: &IvyJson, media: Option<&IvyJson>) -> bool {
        self.call == *call
            && self.closed
                == media
                    .and_then(|m| m.get("closed"))
                    .and_then(IvyJson::as_bool)
            && self.failed
                == media
                    .and_then(|m| m.get("failed"))
                    .and_then(IvyJson::as_bool)
            && self.audio.as_ref()
                == media
                    .and_then(|m| m.get("audio"))
                    .and_then(|a| a.get("state"))
                    .and_then(IvyJson::as_js_string)
    }
}
struct Sample {
    call_id: JsString,
    signature: Signature,
    value_hash: String,
    at: Instant,
}
struct Store {
    db: Option<Connection>,
    sampled: VecDeque<Sample>,
}
impl Store {
    fn sampled(
        &mut self,
        id: &JsString,
        call: &IvyJson,
        media: Option<&IvyJson>,
        value_hash: String,
        at: Instant,
    ) {
        if let Some(sample) = self.sampled.iter_mut().find(|s| &s.call_id == id) {
            if !sample.signature.matches(call, media) {
                sample.signature = Signature::new(call, media);
            }
            sample.value_hash = value_hash;
            sample.at = at;
        } else {
            self.sampled.push_back(Sample {
                call_id: id.clone(),
                signature: Signature::new(call, media),
                value_hash,
                at,
            });
            if self.sampled.len() > 64 {
                self.sampled.pop_front();
            }
        }
    }
}
#[derive(Clone)]
pub struct PhoneCallLogs(Arc<Mutex<Store>>);

impl PhoneCallLogs {
    pub fn open(root: &Path) -> IvyResult<Self> {
        let db = Connection::open(root.join("phone-diagnostics.sqlite")).map_err(error)?;
        db.execute_batch("PRAGMA busy_timeout=100; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA max_page_count=65536;
            CREATE TABLE IF NOT EXISTS snapshots(id INTEGER PRIMARY KEY,call_id TEXT NOT NULL,at TEXT NOT NULL,value TEXT NOT NULL) STRICT;
            CREATE INDEX IF NOT EXISTS snapshots_call ON snapshots(call_id,id);").map_err(error)?;
        Ok(Self(Arc::new(Mutex::new(Store {
            db: Some(db),
            sampled: VecDeque::new(),
        }))))
    }
    pub fn record(&self, call: &IvyJson, status: &IvyJson) -> IvyResult<()> {
        let call_id = call
            .get("callId")
            .and_then(IvyJson::as_js_string)
            .ok_or_else(IvyError::internal)?;
        let observed = status
            .get("call")
            .filter(|v| v.get("id").and_then(IvyJson::as_js_string) == Some(call_id));
        let Some(observed) = observed else {
            return Ok(());
        };
        let media = status.get("media");
        let mut store = self.0.lock().unwrap();
        let now = Instant::now();
        if store.sampled.iter().any(|s| {
            &s.call_id == call_id
                && s.signature.matches(observed, media)
                && now.duration_since(s.at).as_millis() < 5000
        }) {
            return Ok(());
        }
        let mut snapshot = IvyJson::object([("call", observed.clone())]);
        if let Some(media) = media {
            snapshot.insert("media", media.clone());
        }
        let value = canonical(&snapshot, 32768)?;
        let value_hash = digest(&value);
        if store
            .sampled
            .iter()
            .any(|s| &s.call_id == call_id && s.value_hash == value_hash)
        {
            store.sampled(call_id, observed, media, value_hash, now);
            return Ok(());
        }
        let db = store.db.as_ref().ok_or_else(closed)?;
        let native_id = String::from_utf16_lossy(&call_id.0);
        let previous: Option<String> = if store.sampled.iter().any(|s| &s.call_id == call_id) {
            None // The cached hash already identifies this owner's last committed value.
        } else {
            db.query_row(
                "SELECT value FROM snapshots WHERE call_id=? ORDER BY id DESC LIMIT 1",
                [&native_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?
        };
        if previous.as_deref() == Some(&value) {
            store.sampled(call_id, observed, media, value_hash, now);
            return Ok(());
        }
        db.execute_batch("BEGIN IMMEDIATE").map_err(error)?;
        let result = (|| {
            let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            db.execute(
                "INSERT INTO snapshots(call_id,at,value) VALUES (?,?,?)",
                params![native_id, at, value],
            )
            .map_err(error)?;
            db.execute("DELETE FROM snapshots WHERE call_id=? AND id NOT IN (SELECT id FROM snapshots WHERE call_id=? ORDER BY id DESC LIMIT 64)", params![native_id,native_id]).map_err(error)?;
            db.execute_batch("DELETE FROM snapshots WHERE id NOT IN (SELECT id FROM snapshots ORDER BY id DESC LIMIT 4096); COMMIT;").map_err(error)?;
            Ok(())
        })();
        if let Err(cause) = result {
            db.execute_batch("ROLLBACK").map_err(error)?;
            return Err(cause);
        }
        store.sampled(call_id, observed, media, value_hash, now);
        Ok(())
    }
    pub fn read(&self, call_id: &JsString, limit: u32) -> IvyResult<Vec<IvyJson>> {
        if !(1..=64).contains(&limit) {
            return Err(IvyError::new(
                "invalid_arguments",
                "Call log limit must be between 1 and 64.",
            ));
        }
        let store = self.0.lock().unwrap();
        let db = store.db.as_ref().ok_or_else(closed)?;
        let mut statement = db
            .prepare("SELECT at,value FROM snapshots WHERE call_id=? ORDER BY id DESC LIMIT ?")
            .map_err(error)?;
        let rows = statement
            .query_map(
                params![String::from_utf16_lossy(&call_id.0), limit],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(error)?;
        let mut entries = Vec::new();
        let mut bytes = 0;
        for row in rows {
            let (at, value) = row.map_err(error)?;
            bytes += value.len() + 128;
            if bytes > 128 * 1024 {
                break;
            }
            let mut entry = IvyJson::object([("at", at.into())]);
            let snapshot = parse(&value, 32768)?;
            for (key, value) in snapshot.as_object().ok_or_else(IvyError::internal)? {
                entry.insert(key.clone(), value.clone());
            }
            entries.push(entry);
        }
        Ok(entries)
    }
    pub fn close(&self) -> IvyResult<()> {
        if let Some(db) = self.0.lock().unwrap().db.take() {
            db.close().map_err(|(_, cause)| error(cause))?;
        }
        Ok(())
    }
}
fn closed() -> IvyError {
    IvyError::new("storage_unavailable", "Call diagnostics are closed.")
}

#[cfg(test)]
#[path = "../../../../tests/rust/phone_call_logs.rs"]
mod tests;
