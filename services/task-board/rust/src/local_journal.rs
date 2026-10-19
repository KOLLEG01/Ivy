//! TaskBoard's local document journal. Workflow validation and remote publication
//! remain with the owner; short SQLite transactions never span an await.
use crate::operations::Document;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::{digest, hash_json},
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_store::{
    rusqlite::{Connection, OptionalExtension, params},
    sqlite::error,
};

pub fn initialize(connection: &Connection) -> IvyResult<()> {
    connection
        .execute_batch(include_str!("journal.sql"))
        .map_err(error)
}

pub(crate) fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
pub(crate) fn count_column(
    row: &ivy_store::rusqlite::Row<'_>,
    index: usize,
) -> ivy_store::rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value)
        .map_err(|_| ivy_store::rusqlite::Error::IntegralValueOutOfRange(index, value))
}
pub(crate) fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
pub(crate) fn transaction<T>(
    connection: &Connection,
    work: impl FnOnce() -> IvyResult<T>,
) -> IvyResult<T> {
    connection.execute_batch("BEGIN IMMEDIATE").map_err(error)?;
    match work() {
        Ok(value) => match connection.execute_batch("COMMIT").map_err(error) {
            Ok(()) => Ok(value),
            Err(failure) => {
                connection.execute_batch("ROLLBACK").map_err(error)?;
                Err(failure)
            }
        },
        Err(failure) => {
            connection.execute_batch("ROLLBACK").map_err(error)?;
            Err(failure)
        }
    }
}

pub struct LocalJournal<'a> {
    pub connection: &'a Connection,
    pub root_object_id: Option<JsString>,
}

pub struct LocalArtifact {
    pub contract_key: String,
    pub parent_id: Option<JsString>,
    pub content_hash: String,
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
}

impl LocalJournal<'_> {
    pub fn metadata(&self, id: &JsString, version: &str) -> IvyResult<Json> {
        self.connection.query_row(
            "SELECT key,parent_id,name,current_revision,created_at,updated_at FROM documents WHERE id=?",
            [text(id)],
            |row| Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,String>(2)?,count_column(row,3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?)),
        ).map_err(error).map(|(key,parent,name,revision,created,updated)| Json::object([
            ("id",Json::String(id.clone())),("parentId",parent.map(Json::from).unwrap_or(Json::Null)),
            ("ownerObjectId",Json::Null),("name",name.into()),("path","".into()),("position",Json::Number(0.0)),
            ("icon",Json::Null),("contractKey",key.into()),("currentRevision",Json::Number(revision as f64)),
            ("contractVersion",version.into()),("archivedAt",Json::Null),("effectivelyArchived",false.into()),
            ("createdAt",created.into()),("updatedAt",updated.into()),
        ]))
    }
    pub fn name(&self, id: &JsString) -> IvyResult<JsString> {
        self.connection
            .query_row("SELECT name FROM documents WHERE id=?", [text(id)], |row| {
                row.get::<_, String>(0)
            })
            .map(Into::into)
            .map_err(error)
    }

    pub fn artifact(&self, pin: &Json) -> IvyResult<Option<LocalArtifact>> {
        let id = pin
            .required("objectId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let revision = pin
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)?;
        let blob: Option<(String, Option<String>, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT key,parent_id,bytes FROM blobs WHERE id=?",
                [text(id)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        if let Some((key, parent, bytes)) = blob {
            require(
                revision == 1.0,
                "task_board_evidence_mismatch",
                "Local evidence blobs have one revision.",
            )?;
            return Ok(Some(LocalArtifact {
                contract_key: key,
                parent_id: parent.map(Into::into),
                content_hash: digest(&bytes),
                media_type: "application/octet-stream",
                bytes,
            }));
        }
        let head: Option<(String, Option<String>)> = self
            .connection
            .query_row(
                "SELECT key,parent_id FROM documents WHERE id=?",
                [text(id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((key, parent)) = head else {
            return Ok(None);
        };
        let encoded: Option<String> = self
            .connection
            .query_row(
                "SELECT value_json FROM revisions WHERE id=? AND revision=?",
                params![text(id), revision],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        require(
            encoded.is_some(),
            "task_board_evidence_mismatch",
            "The local artifact revision is unavailable.",
        )?;
        let encoded = encoded.unwrap();
        let value = parse(&encoded, MANAGEMENT_FRAME_BYTES)?;
        Ok(Some(LocalArtifact {
            contract_key: key,
            parent_id: parent.map(Into::into),
            content_hash: hash_json(&value)?,
            media_type: "application/json",
            bytes: encoded.into_bytes(),
        }))
    }
    pub fn key(&self, id: &JsString) -> IvyResult<Option<String>> {
        let id = text(id);
        let document = self
            .connection
            .query_row("SELECT key FROM documents WHERE id=?", [&id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(error)?;
        if document.is_some() {
            return Ok(document);
        }
        self.connection
            .query_row("SELECT key FROM blobs WHERE id=?", [&id], |row| row.get(0))
            .optional()
            .map_err(error)
    }

    pub fn read(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        let id = address
            .as_js_string()
            .or_else(|| address.get("objectId").and_then(Json::as_js_string))
            .ok_or_else(IvyError::internal)?;
        let head: Option<(Option<String>, u64)> = self
            .connection
            .query_row(
                "SELECT parent_id,current_revision FROM documents WHERE id=? AND key=?",
                params![text(id), key],
                |row| Ok((row.get(0)?, count_column(row, 1)?)),
            )
            .optional()
            .map_err(error)?;
        require(
            head.as_ref().is_some_and(|(parent, _)| {
                !primary
                    || parent.as_deref().map(JsString::from).as_ref()
                        == self.root_object_id.as_ref()
            }),
            "task_board_scope_mismatch",
            "The local workflow record has another scope.",
        )?;
        let (parent, current) = head.unwrap();
        let revision = if address.as_js_string().is_some() {
            current as f64
        } else {
            address
                .required("revision")?
                .as_f64()
                .ok_or_else(IvyError::internal)?
        };
        let encoded: Option<String> = self
            .connection
            .query_row(
                "SELECT value_json FROM revisions WHERE id=? AND revision=?",
                params![text(id), revision],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        require(
            encoded.is_some(),
            "task_board_evidence_mismatch",
            "The local workflow revision is unavailable.",
        )?;
        let value = parse(&encoded.unwrap(), MANAGEMENT_FRAME_BYTES)?;
        Ok(Document {
            pin: Json::object([
                ("objectId", Json::String(id.clone())),
                ("revision", Json::Number(revision)),
            ]),
            content_hash: hash_json(&value)?,
            value,
            parent_id: parent.map(Into::into),
        })
    }

    pub fn named(
        &self,
        key: &str,
        name: &JsString,
        parent: Option<&JsString>,
    ) -> IvyResult<Option<Document>> {
        let id: Option<String> = self
            .connection
            .query_row(
                "SELECT id FROM documents WHERE key=? AND name=? AND parent_id IS ?",
                params![key, text(name), parent.map(text)],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        id.map(|id| self.read(key, &id.into(), parent == self.root_object_id.as_ref()))
            .transpose()
    }

    /// The owner runs retention admission before entering this synchronous write.
    /// `now` is its already serialized UTC clock, and `id` its fresh UUID.
    pub fn create(
        &self,
        key: &str,
        name: &JsString,
        parent: Option<&JsString>,
        value: &Json,
        now: &JsString,
        id: &JsString,
    ) -> IvyResult<Json> {
        let encoded = canonical(value, MANAGEMENT_FRAME_BYTES)?;
        if let Some(existing) = self.named(key, name, parent)? {
            require(
                existing.content_hash == hash_json(value)?,
                "revision_conflict",
                "The local TaskBoard identity differs.",
            )?;
            return Ok(existing.pin);
        }
        let written = transaction(self.connection, || {
            self.connection
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![
                        text(id),
                        key,
                        text(name),
                        parent.map(text),
                        1,
                        text(now),
                        text(now)
                    ],
                )
                .map_err(error)?;
            self.connection
                .execute(
                    "INSERT INTO revisions VALUES(?,?,?,?)",
                    params![text(id), 1, &encoded, text(now)],
                )
                .map_err(error)?;
            if key == "task-board/coordination" {
                if let Some(active) = value.get("activeOperation").filter(|v| **v != Json::Null) {
                    self.connection
                        .execute(
                            "INSERT INTO active_gates VALUES(?,?)",
                            params![
                                text(id),
                                text(
                                    active
                                        .required("objectId")?
                                        .as_js_string()
                                        .ok_or_else(IvyError::internal)?
                                )
                            ],
                        )
                        .map_err(error)?;
                }
            }
            Ok(())
        });
        if let Err(failure) = written {
            if let Some(winner) = self.named(key, name, parent)? {
                require(
                    winner.content_hash == hash_json(value)?,
                    "revision_conflict",
                    "The local TaskBoard identity already exists.",
                )?;
                return Ok(winner.pin);
            }
            return Err(failure);
        }
        Ok(Json::object([
            ("objectId", Json::String(id.clone())),
            ("revision", Json::Number(1.0)),
        ]))
    }

    pub fn update(
        &self,
        key: &str,
        id: &JsString,
        expected: f64,
        value: &Json,
        now: &JsString,
    ) -> IvyResult<Json> {
        let encoded = canonical(value, MANAGEMENT_FRAME_BYTES)?;
        let next = expected + 1.0;
        transaction(self.connection, || {
            let changed = self.connection.execute("UPDATE documents SET current_revision=?,updated_at=? WHERE id=? AND key=? AND current_revision=?",params![next,text(now),text(id),key,expected]).map_err(error)?;
            require(
                changed == 1,
                "revision_conflict",
                "The local TaskBoard row changed.",
            )?;
            self.connection
                .execute(
                    "INSERT INTO revisions VALUES(?,?,?,?)",
                    params![text(id), next, &encoded, text(now)],
                )
                .map_err(error)?;
            if key == "task-board/coordination" {
                if let Some(active) = value.get("activeOperation").filter(|v| **v != Json::Null) {
                    self.connection.execute("INSERT INTO active_gates VALUES(?,?) ON CONFLICT(gate_id) DO UPDATE SET operation_id=excluded.operation_id",params![text(id),text(active.required("objectId")?.as_js_string().ok_or_else(IvyError::internal)?)]).map_err(error)?;
                } else {
                    self.connection
                        .execute("DELETE FROM active_gates WHERE gate_id=?", [text(id)])
                        .map_err(error)?;
                }
            }
            if matches!(key, "task-board/scan-cursor" | "task-board/coordination") {
                self.connection
                    .execute(
                        "DELETE FROM revisions WHERE id=? AND revision<?",
                        params![text(id), next],
                    )
                    .map_err(error)?;
            }
            Ok(())
        })?;
        Ok(Json::object([
            ("objectId", Json::String(id.clone())),
            ("revision", Json::Number(next)),
        ]))
    }
}
