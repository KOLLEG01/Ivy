//! Durable gate indexes in the existing TaskBoard journal. The caller owns the
//! connection, schema and blocking execution context; no network work occurs here.
use ivy_contracts::JsString;
use ivy_store::rusqlite::{Connection, OptionalExtension, Result, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingGate {
    pub gate_id: JsString,
    pub operation_id: JsString,
}

pub struct LocalGates<'a> {
    connection: &'a Connection,
}

// Match node:sqlite's UTF-8 binding, including replacement of lone surrogates.
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}

impl<'a> LocalGates<'a> {
    pub fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    /// The caller wraps this operation ID in the original revision-1 pin.
    pub fn pending_coordination(&self) -> Result<Option<JsString>> {
        self.connection
            .query_row(
                "SELECT operation_id FROM active_gates ORDER BY gate_id LIMIT 1",
                [],
                |row| row.get::<_, String>(0).map(Into::into),
            )
            .optional()
    }

    pub fn pending_page(&self, after: Option<&JsString>, limit: i64) -> Result<Vec<PendingGate>> {
        self.connection
            .prepare("SELECT gate_id,operation_id FROM active_gates WHERE gate_id>? ORDER BY gate_id LIMIT ?")?
            .query_map(params![after.map(text).unwrap_or_default(), limit], |row| {
                Ok(PendingGate {
                    gate_id: row.get::<_, String>(0)?.into(),
                    operation_id: row.get::<_, String>(1)?.into(),
                })
            })?
            .collect()
    }

    pub fn operation_gates(&self, operation: &JsString) -> Result<Vec<JsString>> {
        self.connection
            .prepare(
                "SELECT gate_name FROM operation_gates WHERE operation_id=? ORDER BY gate_name",
            )?
            .query_map([text(operation)], |row| {
                row.get::<_, String>(0).map(Into::into)
            })?
            .collect()
    }

    /// Retain the whole set atomically. BEGIN IMMEDIATE intentionally rejects a
    /// nested transaction, as the original journal does; it must not roll back
    /// a transaction owned by the caller when admission itself fails.
    pub fn retain_operation_gates(&self, operation: &JsString, names: &[JsString]) -> Result<()> {
        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let mut insert = self
                .connection
                .prepare("INSERT OR IGNORE INTO operation_gates VALUES(?,?)")?;
            for name in names {
                insert.execute(params![text(operation), text(name)])?;
            }
            self.connection.execute_batch("COMMIT")
        })();
        if let Err(error) = result {
            // Preserve the original ordering, including a rollback failure.
            self.connection.execute_batch("ROLLBACK")?;
            return Err(error);
        }
        Ok(())
    }

    pub fn retired_operation(
        &self,
        caller: &JsString,
        operation: &JsString,
    ) -> Result<Option<JsString>> {
        self.connection
            .query_row(
                "SELECT request_hash FROM retired_operations WHERE caller_id=? AND operation_id=?",
                params![text(caller), text(operation)],
                |row| row.get::<_, String>(0).map(Into::into),
            )
            .optional()
    }
}
