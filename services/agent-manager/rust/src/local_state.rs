//! Local current workflow rows and bounded native-operation receipts.
//!
//! The schema and eviction fences match the existing agent-runtime.sqlite.
//! This synchronous owner belongs on the serialized storage worker; transactions
//! must never remain open while awaiting Hive or the native process.

use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_contracts::{
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
    operation::parse_operation_id_js,
};
use ivy_store::receipts::has_unresolved_outcome;
use ivy_store::rusqlite::{
    self, Connection, OptionalExtension, TransactionBehavior, config::DbConfig, params,
};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const RESULT_RESERVATION: i64 = 25 * 1024 * 1024;

const SCHEMA: &str = "
PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=1000;
CREATE TABLE IF NOT EXISTS state (
  kind TEXT NOT NULL, key TEXT NOT NULL, revision INTEGER NOT NULL, value_json TEXT NOT NULL,
  byte_length INTEGER NOT NULL, terminal_at_ms INTEGER, updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(kind,key)
) STRICT;
CREATE TABLE IF NOT EXISTS state_metadata (key TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
INSERT OR IGNORE INTO state_metadata VALUES ('expired_before','0');
CREATE TABLE IF NOT EXISTS completion_receipts(operation_key TEXT PRIMARY KEY,released INTEGER NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS retired_operations(operation_key TEXT PRIMARY KEY,issued_at INTEGER NOT NULL) STRICT;
CREATE TRIGGER IF NOT EXISTS retired_operation_insert BEFORE INSERT ON state
  WHEN NEW.kind='operation' AND EXISTS(SELECT 1 FROM retired_operations WHERE operation_key=NEW.key)
  BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
INSERT OR IGNORE INTO completion_receipts
  SELECT key,0 FROM state WHERE kind='operation' AND json_extract(value_json,'$.method')='turn/start'
    AND json_extract(value_json,'$.phase')='succeeded';
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub object_id: String,
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedState {
    pub value: IvyJson,
    pub pin: Pin,
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub max_operations: i64,
    pub max_journal_bytes: i64,
}

pub struct LocalAgentState {
    db: Connection,
    pub service_node_id: String,
    limits: Limits,
    runtime_epoch: Option<JsString>,
}

fn storage_error(_: rusqlite::Error) -> IvyError {
    IvyError::internal()
}

fn text(value: &IvyJson, key: &str) -> Option<String> {
    value.get(key).and_then(|part| part.as_str().ok())
}

fn phase(value: &IvyJson, kind: &str) -> Option<String> {
    if kind == "operation" {
        text(value, "phase")
    } else {
        None
    }
}

fn reservation(phase: Option<&str>) -> i64 {
    if matches!(phase, Some("accepted" | "dispatched")) {
        RESULT_RESERVATION
    } else {
        0
    }
}

impl LocalAgentState {
    pub fn open(filename: &Path, service_node_id: String, limits: Limits) -> IvyResult<Self> {
        if let Some(parent) = filename
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|_| IvyError::internal())?;
        }
        let db = Connection::open(filename).map_err(storage_error)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false)
            .map_err(storage_error)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)
            .map_err(storage_error)?;
        db.execute_batch(SCHEMA).map_err(storage_error)?;
        Ok(Self {
            db,
            service_node_id,
            limits,
            runtime_epoch: None,
        })
    }

    pub fn set_runtime_epoch(&mut self, epoch: JsString) -> IvyResult<()> {
        if epoch.0.is_empty() || epoch.0.contains(&(b':' as u16)) {
            return Err(IvyError::new("invalid_arguments", "Invalid runtime epoch."));
        }
        self.runtime_epoch = Some(epoch);
        Ok(())
    }

    pub fn close(self) -> IvyResult<()> {
        self.db.close().map_err(|(_, error)| storage_error(error))
    }

    fn id(&self, kind: &str, key: &str) -> IvyResult<String> {
        Ok(hash_json(&IvyJson::Array(vec![
            self.service_node_id.clone().into(),
            kind.into(),
            key.into(),
        ]))?[7..]
            .to_owned())
    }

    pub fn read(&self, kind: &str, key: &str) -> IvyResult<Option<SavedState>> {
        let row: Option<(i64, String)> = self
            .db
            .query_row(
                "SELECT revision,value_json FROM state WHERE kind=? AND key=?",
                params![kind, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(storage_error)?;
        row.map(|(revision, json)| {
            Ok(SavedState {
                value: parse(&json, MANAGEMENT_FRAME_BYTES)?,
                pin: Pin {
                    object_id: self.id(kind, key)?,
                    revision,
                },
            })
        })
        .transpose()
    }

    pub fn write(
        &mut self,
        kind: &str,
        key: &str,
        value: &IvyJson,
        previous: Option<&Pin>,
    ) -> IvyResult<SavedState> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| IvyError::internal())?
            .as_millis();
        self.write_at(
            kind,
            key,
            value,
            previous,
            i64::try_from(now).map_err(|_| IvyError::internal())?,
        )
    }

    /// Explicit clock for parity fixtures. CAS, reservations, eviction and the
    /// insertion are one IMMEDIATE transaction; failed admission rolls all back.
    pub fn write_at(
        &mut self,
        kind: &str,
        key: &str,
        value: &IvyJson,
        previous: Option<&Pin>,
        now: i64,
    ) -> IvyResult<SavedState> {
        let encoded = canonical(value, MANAGEMENT_FRAME_BYTES)?;
        let bytes = encoded.len() as i64;
        if bytes > 32 * 1024 * 1024 {
            return Err(IvyError::new(
                "limit_exceeded",
                "Agent state exceeds its byte limit.",
            ));
        }
        let object_id = self.id(kind, key)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let row: Option<(i64, i64, String)> = tx
            .query_row(
                "SELECT revision,byte_length,value_json FROM state WHERE kind=? AND key=?",
                params![kind, key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(storage_error)?;
        let matches = match previous {
            Some(pin) => {
                pin.object_id == object_id && row.as_ref().is_some_and(|row| row.0 == pin.revision)
            }
            None => row.is_none(),
        };
        if !matches {
            return Err(IvyError::new(
                "revision_conflict",
                "Agent workflow row changed.",
            ));
        }
        if row.is_none() && kind == "operation" {
            let id = value.get("operationId").and_then(IvyJson::as_js_string);
            let (Some(id), Some(epoch)) = (id, self.runtime_epoch.as_ref()) else {
                return Err(IvyError::new(
                    "operation_expired",
                    "AgentManager has no current Hive runtime epoch.",
                ));
            };
            let identity = parse_operation_id_js(id)?;
            let expired_before: String = tx
                .query_row(
                    "SELECT value FROM state_metadata WHERE key='expired_before'",
                    [],
                    |row| row.get(0),
                )
                .map_err(storage_error)?;
            let boundary = expired_before
                .parse::<u64>()
                .map_err(|_| IvyError::internal())?;
            if &identity.runtime_epoch != epoch
                || identity.issued_at_unix_ms as i128 > now as i128 + 60_000
                || identity.issued_at_unix_ms <= boundary
            {
                return Err(IvyError::new(
                    "operation_expired",
                    "Native operation identity belongs to another runtime or predates the receipt ring.",
                ));
            }
        }
        let next_phase = phase(value, kind);
        let prior_phase = row
            .as_ref()
            .map(|row| parse(&row.2, MANAGEMENT_FRAME_BYTES))
            .transpose()?
            .as_ref()
            .and_then(|value| phase(value, kind));
        make_room(
            &tx,
            self.limits,
            bytes - row.as_ref().map_or(0, |row| row.1) + reservation(next_phase.as_deref())
                - reservation(prior_phase.as_deref()),
            if row.is_some() { 0 } else { 1 },
            key,
        )?;
        let terminal_at = if matches!(next_phase.as_deref(), Some("succeeded" | "failed")) {
            Some(now)
        } else {
            None
        };
        let revision = row.as_ref().map_or(0, |row| row.0) + 1;
        tx.execute("INSERT INTO state(kind,key,revision,value_json,byte_length,terminal_at_ms,updated_at_ms) VALUES(?,?,?,?,?,?,?)
            ON CONFLICT(kind,key) DO UPDATE SET revision=excluded.revision,value_json=excluded.value_json,byte_length=excluded.byte_length,
            terminal_at_ms=COALESCE(state.terminal_at_ms,excluded.terminal_at_ms),updated_at_ms=excluded.updated_at_ms",
            params![kind, key, revision, encoded, bytes, terminal_at, now]).map_err(storage_error)?;
        if kind == "operation"
            && next_phase.as_deref() == Some("succeeded")
            && text(value, "method").as_deref() == Some("turn/start")
        {
            tx.execute(
                "INSERT OR IGNORE INTO completion_receipts VALUES(?,0)",
                [key],
            )
            .map_err(storage_error)?;
        }
        tx.commit().map_err(storage_error)?;
        Ok(SavedState {
            value: value.clone(),
            pin: Pin {
                object_id,
                revision,
            },
        })
    }

    pub fn completion_caller(&self, thread_id: &str, turn_id: &str) -> IvyResult<Option<String>> {
        let mut statement = self.db.prepare("SELECT DISTINCT json_extract(value_json,'$.callerPrincipalId') AS caller FROM state
            WHERE kind='operation' AND json_extract(value_json,'$.method')='turn/start'
              AND json_extract(value_json,'$.phase')='succeeded'
              AND json_extract(value_json,'$.params.threadId')=?
              AND json_extract(value_json,'$.reply.result.turn.id')=? LIMIT 2").map_err(storage_error)?;
        let mut rows = statement
            .query(params![thread_id, turn_id])
            .map_err(storage_error)?;
        let caller = rows
            .next()
            .map_err(storage_error)?
            .and_then(|row| row.get::<_, String>(0).ok());
        if rows.next().map_err(storage_error)?.is_some() {
            Ok(None)
        } else {
            Ok(caller)
        }
    }

    pub fn release_completion(&self, thread_id: &str, turn_id: &str) -> IvyResult<()> {
        self.db.execute("UPDATE completion_receipts SET released=1 WHERE operation_key IN
            (SELECT key FROM state WHERE kind='operation' AND json_extract(value_json,'$.params.threadId')=?
              AND json_extract(value_json,'$.reply.result.turn.id')=?)", params![thread_id, turn_id]).map_err(storage_error)?;
        Ok(())
    }
}

fn make_room(
    db: &Connection,
    limits: Limits,
    extra_bytes: i64,
    extra_count: i64,
    protected_key: &str,
) -> IvyResult<()> {
    let (count, bytes): (i64, i64) = db.query_row("SELECT COUNT(*),COALESCE(SUM(byte_length),0)+
        COALESCE(SUM(CASE WHEN kind='operation' AND json_extract(value_json,'$.phase') IN ('accepted','dispatched') THEN ? ELSE 0 END),0) FROM state",
        [RESULT_RESERVATION], |row| Ok((row.get(0)?, row.get(1)?))).map_err(storage_error)?;
    let (mut count, mut bytes) = (count + extra_count, bytes + extra_bytes);
    let boundary: String = db
        .query_row(
            "SELECT value FROM state_metadata WHERE key='expired_before'",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    let mut boundary = boundary.parse::<u64>().map_err(|_| IvyError::internal())?;
    if count <= limits.max_operations && bytes <= limits.max_journal_bytes {
        return Ok(());
    }
    let mut statement = db.prepare("SELECT key,value_json,byte_length FROM state s WHERE kind='operation' AND key<>?
        AND json_extract(value_json,'$.phase') IN ('succeeded','failed')
        AND NOT EXISTS(SELECT 1 FROM completion_receipts c WHERE c.operation_key=s.key AND c.released=0)
        ORDER BY terminal_at_ms,rowid").map_err(storage_error)?;
    // Materialize candidates before deleting, matching SQLite Statement.all.
    let candidates = statement
        .query_map([protected_key], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(storage_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage_error)?;
    for (key, json, size) in candidates {
        let value = parse(&json, MANAGEMENT_FRAME_BYTES)?;
        if has_unresolved_outcome(&value)? {
            continue;
        }
        let Some(id) = value.get("operationId").and_then(IvyJson::as_js_string) else {
            continue;
        };
        let Ok(identity) = parse_operation_id_js(id) else {
            continue;
        };
        let issued_at = identity.issued_at_unix_ms;
        db.execute(
            "INSERT OR IGNORE INTO retired_operations VALUES(?,?)",
            params![key, issued_at as i64],
        )
        .map_err(storage_error)?;
        db.execute("DELETE FROM state WHERE kind='operation' AND key=?", [&key])
            .map_err(storage_error)?;
        db.execute(
            "DELETE FROM completion_receipts WHERE operation_key=?",
            [&key],
        )
        .map_err(storage_error)?;
        boundary = boundary.max(issued_at);
        count -= 1;
        bytes -= size;
        if count <= limits.max_operations && bytes <= limits.max_journal_bytes {
            break;
        }
    }
    db.execute(
        "UPDATE state_metadata SET value=? WHERE key='expired_before'",
        [boundary.to_string()],
    )
    .map_err(storage_error)?;
    db.execute(
        "DELETE FROM retired_operations WHERE issued_at<?",
        [boundary as i64],
    )
    .map_err(storage_error)?;
    if count > limits.max_operations || bytes > limits.max_journal_bytes {
        return Err(IvyError::new(
            "native_journal_capacity",
            "Agent journal capacity is occupied by live or unresolved work; no native request sent.",
        ));
    }
    Ok(())
}
