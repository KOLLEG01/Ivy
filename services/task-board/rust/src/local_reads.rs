//! Native SQLite document projections and retained handoff failures.
use crate::{journal_store::Contracts, local_journal::LocalJournal};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
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
fn parameters(args: &[Option<&Json>]) -> IvyResult<Vec<Value>> {
    args.iter()
        .map(|value| {
            Ok(match value {
                None | Some(Json::Null) => Value::Null,
                Some(Json::String(value)) => Value::Text(String::from_utf16_lossy(&value.0)),
                Some(Json::Number(value)) if value.is_finite() => Value::Real(*value),
                Some(Json::Bool(value)) => Value::Integer(i64::from(*value)),
                _ => {
                    return Err(IvyError::new(
                        "invalid_arguments",
                        "SQLite parameters must be scalar JSON values.",
                    ));
                }
            })
        })
        .collect()
}
pub(super) enum Column {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob,
}
impl Column {
    pub(super) fn string(&self) -> IvyResult<String> {
        match self {
            Self::Text(value) => Ok(value.clone()),
            Self::Integer(value) => Ok(value.to_string()),
            Self::Real(value) => Ok(value.to_string()),
            _ => Err(IvyError::new(
                "database_error",
                "The stored workflow column is not text.",
            )),
        }
    }
    pub(super) fn number(&self) -> IvyResult<f64> {
        match self {
            Self::Integer(value) if (-9007199254740991..=9007199254740991).contains(value) => {
                Ok(*value as f64)
            }
            Self::Real(value) if value.is_finite() => Ok(*value),
            _ => Err(IvyError::new(
                "database_error",
                "The stored workflow column is not a JSON number.",
            )),
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
                ValueRef::Blob(_) => Column::Blob,
            })
        })
        .collect()
}
impl<C: Contracts, Parse, Failure> SqliteReads<'_, C, Parse, Failure> {
    pub(super) fn row<E>(&self, sql: &str, args: &[Option<&Json>]) -> Result<Option<Vec<Column>>, E>
    where
        E: From<IvyError>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        let mut statement = self
            .journal
            .connection
            .prepare(sql)
            .map_err(&self.sqlite_failure)?;
        statement
            .query_row(params_from_iter(parameters(args)?), columns)
            .optional()
            .map_err(&self.sqlite_failure)
    }
    pub(super) fn rows<E>(&self, sql: &str, args: &[Option<&Json>]) -> Result<Vec<Vec<Column>>, E>
    where
        E: From<IvyError>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        let mut statement = self
            .journal
            .connection
            .prepare(sql)
            .map_err(&self.sqlite_failure)?;
        statement
            .query_map(params_from_iter(parameters(args)?), columns)
            .map_err(&self.sqlite_failure)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(&self.sqlite_failure)
    }
    pub(super) fn execute<E>(&self, sql: &str, args: &[Option<&Json>]) -> Result<(), E>
    where
        E: From<IvyError>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        self.run(sql, args).map(|_| ())
    }
    pub(super) fn run<E>(&self, sql: &str, args: &[Option<&Json>]) -> Result<usize, E>
    where
        E: From<IvyError>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        let mut statement = self
            .journal
            .connection
            .prepare(sql)
            .map_err(&self.sqlite_failure)?;
        statement
            .execute(params_from_iter(parameters(args)?))
            .map_err(&self.sqlite_failure)
    }
    pub fn retired_operation_receipt<E>(
        &self,
        caller: Option<&Json>,
        operation_id: Option<&Json>,
    ) -> Result<Option<Json>, E>
    where
        E: From<IvyError>,
        Parse: Fn(&str) -> Result<Json, E>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        self.row::<E>("SELECT receipt_json FROM retired_operation_receipts WHERE caller_id=? AND operation_id=?", &[caller, operation_id])?
            .map(|row| (self.parse)(&row[0].string()?)).transpose()
    }
    pub fn key<E>(&self, id: Option<&Json>) -> Result<Option<String>, E>
    where
        E: From<IvyError>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        if let Some(row) = self.row::<E>("SELECT key FROM documents WHERE id=?", &[id])? {
            return Ok(Some(row[0].string()?));
        }
        self.row::<E>("SELECT key FROM blobs WHERE id=?", &[id])?
            .map(|row| row[0].string().map_err(E::from))
            .transpose()
    }
    pub fn document<E>(&self, key: &str, reference: &Json, primary: bool) -> Result<Json, E>
    where
        E: From<IvyError>,
        Parse: Fn(&str) -> Result<Json, E>,
        Failure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        let id = if reference.as_js_string().is_some() {
            reference
        } else {
            reference.required("objectId")?
        };
        let head = self.row::<E>(
            "SELECT * FROM documents WHERE id=? AND key=?",
            &[Some(id), Some(&key.into())],
        )?;
        let head = head
            .filter(|row| !primary || row[3].is_root(self.journal.root_object_id.as_ref()))
            .ok_or_else(|| {
                IvyError::new(
                    "task_board_scope_mismatch",
                    "The local workflow record has another scope.",
                )
            })?;
        let current_revision = Json::Number(head[4].number()?);
        let revision = if reference.as_js_string().is_some() {
            &current_revision
        } else {
            reference.required("revision")?
        };
        let row = self
            .row::<E>(
                "SELECT * FROM revisions WHERE id=? AND revision=?",
                &[Some(id), Some(revision)],
            )?
            .ok_or_else(|| {
                IvyError::new(
                    "task_board_evidence_mismatch",
                    "The local workflow revision is unavailable.",
                )
            })?;
        let encoded = row[2].string()?;
        let value = (self.parse)(&encoded)?;
        let pin = Json::object([("objectId", id.clone()), ("revision", revision.clone())]);
        let parent = if matches!(head[3], Column::Null) {
            Json::Null
        } else {
            head[3].string()?.into()
        };
        let metadata = Json::object([
            ("id", id.clone()),
            ("parentId", parent),
            ("ownerObjectId", Json::Null),
            ("name", head[2].string()?.into()),
            ("path", "".into()),
            ("position", Json::Number(0.0)),
            ("icon", Json::Null),
            ("contractKey", key.into()),
            ("currentRevision", current_revision.clone()),
            ("contractVersion", self.contracts.version(key)?.into()),
            ("archivedAt", Json::Null),
            ("effectivelyArchived", false.into()),
            ("createdAt", head[5].string()?.into()),
            ("updatedAt", head[6].string()?.into()),
        ]);
        let revision_record = Json::object([
            ("objectId", id.clone()),
            ("revision", revision.clone()),
            ("contractVersion", self.contracts.version(key)?.into()),
            ("contentHash", hash_json(&value)?.into()),
            ("mediaType", "application/json".into()),
            ("byteLength", Json::Number(encoded.len() as f64)),
            ("createdAt", row[3].string()?.into()),
            ("references", Json::Object(vec![])),
        ]);
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
        E: From<IvyError>,
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
impl<C: Contracts, Parse, Failure, E> crate::engine_read::condition_policy::ConditionStore<E>
    for SqliteReads<'_, C, Parse, Failure>
where
    E: From<IvyError>,
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
    E: From<IvyError>,
{
    let raw = connection.query_row("SELECT r.value_json FROM documents d
         JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision
         WHERE d.key='task-board/operation' AND json_extract(r.value_json,'$.handoffRun.objectId')=?
           AND json_extract(r.value_json,'$.phase')='failed'
           AND json_extract(r.value_json,'$.error.code') IN ('content_too_large','storage_full','quota_exceeded','database_error')
         ORDER BY d.updated_at DESC LIMIT 1", [crate::local_journal::text(run_id)], |r| r.get::<_, String>(0))
        .optional().map_err(failure)?;
    let Some(raw) = raw else { return Ok(None) };
    let operation = parse(&raw)?;
    let error = operation.required("error")?;
    let message = error
        .required("message")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    let mut detail = JsString::from("Die Übergabe konnte nicht gespeichert werden: ");
    detail.0.extend(message.0.iter().copied().take(1800));
    Ok(Some(Json::object([
        ("code", error.required("code")?.clone()),
        ("detail", Json::String(detail)),
        ("operationId", operation.required("operationId")?.clone()),
    ])))
}
