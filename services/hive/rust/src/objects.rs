//! Object reads, navigation metadata and transactional mutations.
use crate::store::Store;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use rusqlite::{OptionalExtension, Row, types::ValueRef};

pub enum Location<'a> {
    Id(&'a JsString),
    Path(&'a JsString),
}
#[path = "object_pages.rs"]
mod pages;
pub use pages::{history, list, tree};
#[path = "object_search.rs"]
mod searching;
pub use searching::search;
#[path = "object_mutations.rs"]
mod mutations;
pub use mutations::{archive, reorder};
#[path = "object_delete.rs"]
mod deletion;
pub use deletion::delete;
#[path = "object_retention.rs"]
mod retention;
pub use retention::prune_revisions;
#[path = "object_contracts.rs"]
mod contracts;
pub use contracts::{ContentData, DecodedContent, decode, get_contract, register, register_agent};
#[path = "object_write.rs"]
mod writing;
pub use writing::{
    ExistingObjectWrite, write, write_existing_revision, write_hive, write_hive_prepared,
};
#[path = "object_move.rs"]
mod moving;
#[path = "object_navigation.rs"]
mod navigation;
pub use moving::move_object;
fn value(row: &Row<'_>, name: &str) -> rusqlite::Result<IvyJson> {
    Ok(match row.get_ref(name)? {
        ValueRef::Null => IvyJson::Null,
        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
        ValueRef::Integer(v) => IvyJson::Number(v as f64),
        ValueRef::Real(v) => IvyJson::Number(v),
        ValueRef::Blob(_) => return Err(rusqlite::Error::InvalidQuery),
    })
}
fn object_row(row: &Row<'_>) -> rusqlite::Result<IvyJson> {
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
}
fn complete_object(store: &Store, mut object: IvyJson) -> IvyResult<IvyJson> {
    let version: String = store
        .connection()
        .prepare_cached("SELECT contract_version FROM revisions WHERE object_id=? AND revision=?")
        .map_err(|e| store.query_error(e))?
        .query_row(
            rusqlite::params![
                object.get("id").unwrap().as_str()?,
                object.get("currentRevision").unwrap().as_f64().unwrap() as i64
            ],
            |r| r.get(0),
        )
        .map_err(|e| store.query_error(e))?;
    object.insert("contractVersion", version.into());
    Ok(object)
}
fn revision_row(row: &Row<'_>) -> rusqlite::Result<(String, IvyJson)> {
    let mut revision = IvyJson::object([
        ("objectId", value(row, "object_id")?),
        ("revision", value(row, "revision")?),
        ("contractVersion", value(row, "contract_version")?),
        ("contentHash", value(row, "content_hash")?),
        ("byteLength", value(row, "byte_length")?),
        ("createdAt", value(row, "created_at")?),
    ]);
    revision.insert(
        "references",
        parse(&row.get::<_, String>("references_json")?, usize::MAX)
            .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?,
    );
    Ok((row.get("contract_key")?, revision))
}
fn complete_revision(store: &Store, key: &str, mut revision: IvyJson) -> IvyResult<IvyJson> {
    let version = revision
        .get("contractVersion")
        .unwrap()
        .as_js_string()
        .unwrap();
    let definition = store.contract(&key.into(), version)?;
    if let Some(media) = definition.get("mediaType") {
        revision.insert("mediaType", media.clone());
    }
    Ok(revision)
}
fn raw_object(store: &Store, location: Location<'_>) -> IvyResult<IvyJson> {
    let (column, selected) = match location {
        Location::Id(v) => ("id", v),
        Location::Path(v) => ("path", v),
    };
    let object = store
        .connection()
        .prepare_cached(&format!("SELECT * FROM objects WHERE {column}=?"))
        .map_err(|e| store.query_error(e))?
        .query_row([String::from_utf16_lossy(&selected.0)], object_row)
        .optional()
        .map_err(|e| store.query_error(e))?
        .ok_or_else(|| IvyError::new("not_found", "Object not found."))?;
    Ok(object)
}
pub fn stat(store: &Store, location: Location<'_>) -> IvyResult<IvyJson> {
    complete_object(store, raw_object(store, location)?)
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
                let (key, revision) = revision_row(row)?;
                let encoding: String = row.get("encoding")?;
                let content = if encoding == "base64" {
                    STANDARD.encode(row.get::<_, Vec<u8>>("content")?).into()
                } else if encoding == "json" {
                    parse(&row.get::<_, String>("content")?, usize::MAX)
                        .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?
                } else {
                    value(row, "content")?
                };
                Ok((
                    key,
                    revision,
                    IvyJson::object([("encoding", encoding.into()), ("value", content)]),
                ))
            },
        )
        .optional()
        .map_err(|e| store.query_error(e))?;
    let Some((key, revision_metadata, content)) = row else {
        return Err(if revision.is_some() && selected <= current {
            IvyError::new(
                "revision_pruned",
                "Object revision was pruned by its retention policy.",
            )
        } else {
            IvyError::new("not_found", "Object revision not found.")
        });
    };
    let revision_metadata = complete_revision(store, &key, revision_metadata)?;
    Ok(IvyJson::object([
        ("object", object),
        ("revision", revision_metadata),
        ("content", content),
    ]))
}
