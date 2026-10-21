//! Exact local document projections for current reads. Parsing and native SQL
//! failure classification are mandatory bindings; neither is normalized here.
use super::values::{field, insert, type_error};
use crate::{journal_store::Contracts, local_journal::LocalJournal};
use ivy_contracts::{IvyError, IvyJson as Json, JsString, hash::hash_json, json::try_js_string};
use ivy_sdk::task_comments::CommentException;
use ivy_store::rusqlite::{OptionalExtension, params, types::Value};

pub struct SqliteReads<'a, C, Parse, Failure> {
    pub journal: LocalJournal<'a>,
    pub contracts: &'a C,
    pub parse: Parse,
    pub sqlite_failure: Failure,
}

fn parameter<E: From<CommentException>>(value: Option<&Json>, index: usize) -> Result<Value, E> {
    Ok(match value {
        Some(Json::Null) => Value::Null,
        Some(Json::String(value)) => Value::Text(String::from_utf16_lossy(&value.0)),
        Some(Json::Number(value)) => Value::Real(*value),
        _ => {
            return Err(type_error(format!(
                "Provided value cannot be bound to SQLite parameter {index}."
            )));
        }
    })
}
fn address<'a, E: From<CommentException>>(value: &'a Json) -> Result<Option<&'a Json>, E> {
    if matches!(value, Json::String(_)) {
        Ok(Some(value))
    } else {
        field(Some(value), "objectId")
    }
}
impl<C: Contracts, Parse, Failure> SqliteReads<'_, C, Parse, Failure> {
    pub fn key<E>(&self, id: Option<&Json>) -> Result<Option<String>, E>
    where
        E: From<CommentException>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        let id = parameter::<E>(id, 1)?;
        let document = self
            .journal
            .connection
            .query_row("SELECT key FROM documents WHERE id=?", [&id], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .map_err(&self.sqlite_failure)?;
        if document.is_some() {
            return Ok(document);
        }
        self.journal
            .connection
            .query_row("SELECT key FROM blobs WHERE id=?", [&id], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .map_err(&self.sqlite_failure)
    }
    pub fn document<E>(&self, key: &str, reference: &Json, primary: bool) -> Result<Json, E>
    where
        E: From<IvyError> + From<CommentException>,
        Parse: Fn(&str) -> Result<Json, E>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        let id = address::<E>(reference)?;
        let sql_id = parameter::<E>(id, 1)?;
        let head=self.journal.connection.query_row(
            "SELECT parent_id,current_revision,name,created_at,updated_at FROM documents WHERE id=? AND key=?",params![&sql_id,key],
            |r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,f64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)),
        ).optional().map_err(&self.sqlite_failure)?;
        if !head.as_ref().is_some_and(|(parent, ..)| {
            !primary
                || parent.as_deref().map(JsString::from).as_ref()
                    == self.journal.root_object_id.as_ref()
        }) {
            return Err(IvyError::new(
                "task_board_scope_mismatch",
                "The local workflow record has another scope.",
            )
            .into());
        }
        let (parent, current, name, created, updated) = head.unwrap();
        let current_revision = Json::Number(current);
        let revision = if matches!(reference, Json::String(_)) {
            Some(&current_revision)
        } else {
            field::<E>(Some(reference), "revision")?
        };
        let sql_revision = parameter::<E>(revision, 2)?;
        let row = self
            .journal
            .connection
            .query_row(
                "SELECT value_json,created_at FROM revisions WHERE id=? AND revision=?",
                params![sql_id, sql_revision],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(&self.sqlite_failure)?;
        let Some((encoded, revision_created)) = row else {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "The local workflow revision is unavailable.",
            )
            .into());
        };
        let value = (self.parse)(&encoded)?;
        let mut pin = Json::Object(vec![]);
        insert(&mut pin, "objectId", id);
        insert(&mut pin, "revision", revision);
        let mut metadata = Json::Object(vec![]);
        insert(&mut metadata, "id", id);
        for (key, value) in [
            ("parentId", parent.map(Json::from).unwrap_or(Json::Null)),
            ("ownerObjectId", Json::Null),
            ("name", name.into()),
            ("path", "".into()),
            ("position", Json::Number(0.0)),
            ("icon", Json::Null),
            ("contractKey", key.into()),
            ("currentRevision", Json::Number(current)),
            ("contractVersion", self.contracts.version(key)?.into()),
            ("archivedAt", Json::Null),
            ("effectivelyArchived", false.into()),
            ("createdAt", created.into()),
            ("updatedAt", updated.into()),
        ] {
            metadata.insert(key, value);
        }
        let mut revision_record = Json::Object(vec![]);
        insert(&mut revision_record, "objectId", id);
        insert(&mut revision_record, "revision", revision);
        for (key, value) in [
            ("contractVersion", self.contracts.version(key)?.into()),
            ("contentHash", hash_json(&value)?.into()),
            ("mediaType", "application/json".into()),
            ("byteLength", Json::Number(encoded.len() as f64)),
            ("createdAt", revision_created.into()),
            ("references", Json::Object(vec![])),
        ] {
            revision_record.insert(key, value);
        }
        Ok(Json::object([
            ("key", key.into()),
            ("pin", pin),
            ("value", value),
            ("metadata", metadata),
            ("revision", revision_record),
        ]))
    }
    pub fn failed_handoff<E>(&self, run_id: &JsString) -> Result<Option<Json>, E>
    where
        E: From<IvyError> + From<CommentException>,
        Parse: Fn(&str) -> Result<Json, E>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        failed_handoff_with(
            self.journal.connection,
            run_id,
            &self.parse,
            &self.sqlite_failure,
        )
    }
}

impl<C, Parse, Failure, E> crate::engine_read::condition_policy::ConditionStore<E>
    for SqliteReads<'_, C, Parse, Failure>
where
    C: Contracts,
    E: From<IvyError> + From<CommentException>,
    Parse: Fn(&str) -> Result<Json, E>,
    Failure: Fn(ivy_store::rusqlite::Error) -> E,
{
    fn failed_handoff(&self, run_id: &JsString) -> Result<Option<Json>, E> {
        self.failed_handoff(run_id)
    }
}

pub fn failed_handoff_with<E>(
    connection: &ivy_store::rusqlite::Connection,
    run_id: &JsString,
    parse: &impl Fn(&str) -> Result<Json, E>,
    failure: &impl Fn(ivy_store::rusqlite::Error) -> E,
) -> Result<Option<Json>, E>
where
    E: From<IvyError> + From<CommentException>,
{
    let raw=connection.query_row("SELECT r.value_json FROM documents d
         JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision
         WHERE d.key='task-board/operation' AND json_extract(r.value_json,'$.handoffRun.objectId')=?
           AND json_extract(r.value_json,'$.phase')='failed'
           AND json_extract(r.value_json,'$.error.code') IN ('content_too_large','storage_full','quota_exceeded','database_error')
         ORDER BY d.updated_at DESC LIMIT 1",[crate::local_journal::text(run_id)],|r|r.get::<_,String>(0))
         .optional().map_err(failure)?;
    let Some(raw) = raw else { return Ok(None) };
    let operation = parse(&raw)?;
    let error = field::<E>(Some(&operation), "error")?;
    // Object property evaluation is code, detail, then operationId.
    let code = field::<E>(error, "code")?;
    let message = field::<E>(error, "message")?;
    let truncated = match message {
        Some(Json::String(message)) => {
            Json::String(JsString(message.0.iter().copied().take(1800).collect()))
        }
        Some(Json::Array(message)) => Json::Array(message.iter().take(1800).cloned().collect()),
        _ => {
            field::<E>(message, "slice")?;
            return Err(type_error(
                "operation.error.message.slice is not a function",
            ));
        }
    };
    let text = try_js_string(&truncated)
        .map_err(|_| type_error::<E>("Cannot convert object to primitive value"))?;
    let mut detail = JsString::from("Die Übergabe konnte nicht gespeichert werden: ");
    detail.0.extend(text.0);
    let mut result = Json::Object(vec![]);
    insert(&mut result, "code", code);
    result.insert("detail", Json::String(detail));
    insert(
        &mut result,
        "operationId",
        field::<E>(Some(&operation), "operationId")?,
    );
    Ok(Some(result))
}
