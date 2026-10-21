//! Exact local document projections for current reads. Parsing and native SQL
//! failure classification are mandatory bindings; neither is normalized here.
use super::values::{field, insert, type_error};
use crate::{journal_store::Contracts, local_journal::LocalJournal};
use ivy_contracts::{
    IvyError, IvyJson as Json, JsString,
    hash::hash_json,
    json::{js_string, number_from_string, object_entries, try_js_string},
};
use ivy_sdk::task_comments::CommentException;
use ivy_store::rusqlite::{
    OptionalExtension, params_from_iter,
    types::{Value, ValueRef},
};

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
// These statements have anonymous parameters. Node's first object argument is
// a named-parameter bag, including arrays; unbound parameters remain SQL NULL.
fn parameters<E: From<CommentException>>(args: &[Option<&Json>]) -> Result<Vec<Value>, E> {
    let named = matches!(args.first(), Some(Some(Json::Object(_) | Json::Array(_))));
    let first_key = match args.first() {
        Some(Some(value @ Json::Object(_))) => {
            object_entries(value).first().map(|(key, _)| (*key).clone())
        }
        Some(Some(Json::Array(items))) if !items.is_empty() => Some("0".into()),
        _ => None,
    };
    if let Some(key) = first_key {
        let mut message = JsString::from("Unknown named parameter '");
        message.0.extend(key.0);
        message.0.push(b'\'' as u16);
        return Err(CommentException {
            kind: "Error",
            message,
        }
        .into());
    }
    let mut values = args[usize::from(named)..]
        .iter()
        .enumerate()
        .map(|(index, value)| parameter::<E>(*value, index + 1))
        .collect::<Result<Vec<_>, _>>()?;
    values.resize(args.len(), Value::Null);
    Ok(values)
}

enum Column {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}
impl Column {
    fn string(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Integer(value) => {
                String::from_utf16_lossy(&js_string(&Json::Number(*value as f64)).0)
            }
            Self::Real(value) => String::from_utf16_lossy(&js_string(&Json::Number(*value)).0),
            Self::Text(value) => value.clone(),
            Self::Blob(value) => value
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(","),
        }
    }
    fn number(&self) -> f64 {
        match self {
            Self::Null => 0.0,
            Self::Integer(value) => *value as f64,
            Self::Real(value) => *value,
            _ => number_from_string(&self.string().into()),
        }
    }
    fn is_root(&self, root: Option<&JsString>) -> bool {
        match self {
            Self::Null => root.is_none(),
            Self::Text(value) => root == Some(&JsString::from(value.as_str())),
            _ => false,
        }
    }
}
fn columns(row: &ivy_store::rusqlite::Row<'_>) -> ivy_store::rusqlite::Result<Vec<Column>> {
    (0..row.as_ref().column_count())
        .map(|index| {
            Ok(match row.get_ref(index)? {
                ValueRef::Null => Column::Null,
                ValueRef::Integer(value) => Column::Integer(value),
                ValueRef::Real(value) => Column::Real(value),
                ValueRef::Text(value) => Column::Text(String::from_utf8_lossy(value).into_owned()),
                ValueRef::Blob(value) => Column::Blob(value.to_vec()),
            })
        })
        .collect()
}
fn address<'a, E: From<CommentException>>(value: &'a Json) -> Result<Option<&'a Json>, E> {
    if matches!(value, Json::String(_)) {
        Ok(Some(value))
    } else {
        field(Some(value), "objectId")
    }
}
impl<C: Contracts, Parse, Failure> SqliteReads<'_, C, Parse, Failure> {
    fn row<E>(&self, sql: &str, args: &[Option<&Json>]) -> Result<Option<Vec<Column>>, E>
    where
        E: From<CommentException>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        // Preparing precedes argument binding. Materialize the entire returned
        // row before scope checks, including Node's integer range failure.
        let mut statement = self
            .journal
            .connection
            .prepare(sql)
            .map_err(&self.sqlite_failure)?;
        let values = parameters::<E>(args)?;
        let row = statement
            .query_row(params_from_iter(values), columns)
            .optional()
            .map_err(&self.sqlite_failure)?;
        if let Some(row) = &row {
            for column in row {
                if let Column::Integer(value) = column {
                    if !(-9007199254740991..=9007199254740991).contains(value) {
                        return Err(CommentException { kind: "RangeError", message: format!(
                            "Value is too large to be represented as a JavaScript number: {value}"
                        ).into() }.into());
                    }
                }
            }
        }
        Ok(row)
    }
    pub fn key<E>(&self, id: Option<&Json>) -> Result<Option<String>, E>
    where
        E: From<CommentException>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        let document = self.row::<E>("SELECT key FROM documents WHERE id=?", &[id])?;
        if document.is_some() {
            return Ok(document.map(|row| row[0].string()));
        }
        Ok(self
            .row::<E>("SELECT key FROM blobs WHERE id=?", &[id])?
            .map(|row| row[0].string()))
    }
    pub fn document<E>(&self, key: &str, reference: &Json, primary: bool) -> Result<Json, E>
    where
        E: From<IvyError> + From<CommentException>,
        Parse: Fn(&str) -> Result<Json, E>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        let id = address::<E>(reference)?;
        let head = self.row::<E>(
            "SELECT * FROM documents WHERE id=? AND key=?",
            &[id, Some(&key.into())],
        )?;
        if !head
            .as_ref()
            .is_some_and(|row| !primary || row[3].is_root(self.journal.root_object_id.as_ref()))
        {
            return Err(IvyError::new(
                "task_board_scope_mismatch",
                "The local workflow record has another scope.",
            )
            .into());
        }
        let head = head.unwrap();
        let current_revision = Json::Number(head[4].number());
        let revision = if matches!(reference, Json::String(_)) {
            Some(&current_revision)
        } else {
            field::<E>(Some(reference), "revision")?
        };
        let row = self.row::<E>(
            "SELECT * FROM revisions WHERE id=? AND revision=?",
            &[id, revision],
        )?;
        let Some(row) = row else {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "The local workflow revision is unavailable.",
            )
            .into());
        };
        let encoded = row[2].string();
        let value = (self.parse)(&encoded)?;
        let mut pin = Json::Object(vec![]);
        insert(&mut pin, "objectId", id);
        insert(&mut pin, "revision", revision);
        let mut metadata = Json::Object(vec![]);
        insert(&mut metadata, "id", id);
        for (key, value) in [
            (
                "parentId",
                if matches!(head[3], Column::Null) {
                    Json::Null
                } else {
                    head[3].string().into()
                },
            ),
            ("ownerObjectId", Json::Null),
            ("name", head[2].string().into()),
            ("path", "".into()),
            ("position", Json::Number(0.0)),
            ("icon", Json::Null),
            ("contractKey", key.into()),
            ("currentRevision", Json::Number(head[4].number())),
            ("contractVersion", self.contracts.version(key)?.into()),
            ("archivedAt", Json::Null),
            ("effectivelyArchived", false.into()),
            ("createdAt", head[5].string().into()),
            ("updatedAt", head[6].string().into()),
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
            ("createdAt", row[3].string().into()),
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
