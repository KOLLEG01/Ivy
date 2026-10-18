//! Immutable object reads and current navigation metadata.
use crate::store::Store;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use rusqlite::{OptionalExtension, Row, types::ValueRef};

pub enum Location<'a> {
    Id(&'a JsString),
    Path(&'a JsString),
}
fn value(row: &Row<'_>, name: &str) -> rusqlite::Result<IvyJson> {
    Ok(match row.get_ref(name)? {
        ValueRef::Null => IvyJson::Null,
        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
        ValueRef::Integer(v) => IvyJson::Number(v as f64),
        ValueRef::Real(v) => IvyJson::Number(v),
        ValueRef::Blob(_) => return Err(rusqlite::Error::InvalidQuery),
    })
}
pub fn stat(store: &Store, location: Location<'_>) -> IvyResult<IvyJson> {
    let (column, selected) = match location {
        Location::Id(v) => ("id", v),
        Location::Path(v) => ("path", v),
    };
    let object = store
        .connection()
        .query_row(
            &format!("SELECT * FROM objects WHERE {column}=?"),
            [String::from_utf16_lossy(&selected.0)],
            |row| {
                let mut pairs = vec![];
                for (name, column) in [
                    ("id", "id"),
                    ("parentId", "parent_id"),
                    ("ownerObjectId", "owner_object_id"),
                    ("name", "name"),
                    ("path", "path"),
                    ("position", "position"),
                    ("icon", "icon"),
                    ("contractKey", "contract_key"),
                    ("currentRevision", "current_revision"),
                    ("archivedAt", "archived_at"),
                    ("createdAt", "created_at"),
                    ("updatedAt", "updated_at"),
                ] {
                    pairs.push((name, value(row, column)?));
                }
                pairs.push((
                    "effectivelyArchived",
                    IvyJson::Bool(row.get::<_, i64>("effective_archive")? != 0),
                ));
                Ok(IvyJson::object(pairs))
            },
        )
        .optional()
        .map_err(|e| store.query_error(e))?
        .ok_or_else(|| IvyError::new("not_found", "Object not found."))?;
    let version: String = store
        .connection()
        .query_row(
            "SELECT contract_version FROM revisions WHERE object_id=? AND revision=?",
            rusqlite::params![
                object.get("id").unwrap().as_str()?,
                object.get("currentRevision").unwrap().as_f64().unwrap() as i64
            ],
            |r| r.get(0),
        )
        .map_err(|e| store.query_error(e))?;
    let mut object = object;
    object.insert("contractVersion", version.into());
    Ok(object)
}
pub fn read(store: &Store, location: Location<'_>, revision: Option<u64>) -> IvyResult<IvyJson> {
    let object = stat(store, location)?;
    let id = object.get("id").unwrap().as_str()?;
    let current = object.get("currentRevision").unwrap().as_f64().unwrap() as u64;
    let selected = revision.unwrap_or(current);
    let row = store
        .connection()
        .query_row(
            "SELECT * FROM revisions WHERE object_id=? AND revision=?",
            rusqlite::params![id, selected as i64],
            |row| {
                let mut revision = IvyJson::object([
                    ("objectId", value(row, "object_id")?),
                    ("revision", value(row, "revision")?),
                    ("contractVersion", value(row, "contract_version")?),
                    ("contentHash", value(row, "content_hash")?),
                    ("byteLength", value(row, "byte_length")?),
                    ("createdAt", value(row, "created_at")?),
                ]);
                let references: String = row.get("references_json")?;
                revision.insert(
                    "references",
                    parse(&references, usize::MAX)
                        .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?,
                );
                let encoding: String = row.get("encoding")?;
                let content = if encoding == "base64" {
                    STANDARD.encode(row.get::<_, Vec<u8>>("content")?).into()
                } else if encoding == "json" {
                    parse(&row.get::<_, String>("content")?, usize::MAX)
                        .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?
                } else {
                    value(row, "content")?
                };
                let key: String = row.get("contract_key")?;
                Ok((
                    key,
                    revision,
                    IvyJson::object([("encoding", encoding.into()), ("value", content)]),
                ))
            },
        )
        .optional()
        .map_err(|e| store.query_error(e))?;
    let Some((key, mut revision_metadata, content)) = row else {
        return Err(if revision.is_some() && selected <= current {
            IvyError::new(
                "revision_pruned",
                "Object revision was pruned by its retention policy.",
            )
        } else {
            IvyError::new("not_found", "Object revision not found.")
        });
    };
    let version = revision_metadata
        .get("contractVersion")
        .unwrap()
        .as_js_string()
        .unwrap();
    let definition = store.contract(&key.into(), version)?;
    if let Some(media) = definition.get("mediaType") {
        revision_metadata.insert("mediaType", media.clone());
    }
    Ok(IvyJson::object([
        ("object", object),
        ("revision", revision_metadata),
        ("content", content),
    ]))
}
