//! Hive's local storage portion of runtime reset. The caller owns reset identity,
//! durable capture, admission, file clearing and phase ordering. These functions
//! run synchronously on the storage execution context and close their own Stores.
use crate::{objects, store::Store};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::js_string};
use rusqlite::{
    OptionalExtension, Row, params,
    types::{Value, ValueRef},
};
use std::path::Path;

fn string(row: &Row<'_>, column: &str) -> rusqlite::Result<IvyJson> {
    let value = match row.get_ref(column)? {
        ValueRef::Null => IvyJson::Null,
        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
        ValueRef::Integer(v) => IvyJson::Number(v as f64),
        ValueRef::Real(v) => IvyJson::Number(v),
        // Node returns Uint8Array for BLOBs; String(Uint8Array) joins decimal bytes.
        ValueRef::Blob(v) => {
            return Ok(v
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",")
                .into());
        }
    };
    Ok(IvyJson::String(js_string(&value)))
}
fn nullable_string(row: &Row<'_>, column: &str) -> rusqlite::Result<IvyJson> {
    if matches!(row.get_ref(column)?, ValueRef::Null) {
        Ok(IvyJson::Null)
    } else {
        string(row, column)
    }
}
fn captured(row: &Row<'_>) -> rusqlite::Result<IvyJson> {
    Ok(IvyJson::object([
        (
            "family",
            IvyJson::object([
                ("key", "agent/instructions".into()),
                ("owner", string(row, "owner")?),
                ("mediaType", string(row, "media_type")?),
                ("objectMode", string(row, "object_mode")?),
            ]),
        ),
        (
            "contract",
            IvyJson::object([
                ("version", string(row, "contract_version")?),
                ("definition", string(row, "definition")?),
            ]),
        ),
        (
            "object",
            IvyJson::object([
                ("id", string(row, "id")?),
                ("parentId", nullable_string(row, "parent_id")?),
                ("ownerObjectId", nullable_string(row, "owner_object_id")?),
                ("name", string(row, "name")?),
                ("path", string(row, "path")?),
                ("position", IvyJson::Number(row.get("position")?)),
                ("icon", nullable_string(row, "icon")?),
                ("depth", IvyJson::Number(row.get("depth")?)),
                (
                    "currentRevision",
                    IvyJson::Number(row.get("current_revision")?),
                ),
                ("createdAt", string(row, "created_at")?),
                ("updatedAt", string(row, "updated_at")?),
            ]),
        ),
        (
            "revision",
            IvyJson::object([
                ("contractVersion", string(row, "contract_version")?),
                ("encoding", string(row, "encoding")?),
                ("content", string(row, "content")?),
                ("contentHash", string(row, "content_hash")?),
                ("byteLength", IvyJson::Number(row.get("byte_length")?)),
                ("createdAt", string(row, "revision_created_at")?),
                ("referencesJson", string(row, "references_json")?),
            ]),
        ),
    ]))
}

/// Capture the original SavedInstruction[] shape without parsing/canonicalizing
/// its raw owner, definition, content or references strings. Like the original,
/// this opens the normal writable Store (including its startup normalization).
pub fn saved_instructions(path: &Path) -> IvyResult<IvyJson> {
    if !path.exists() {
        return Ok(IvyJson::Array(vec![]));
    }
    let store = Store::open(path)?;
    let result = (|| {
        let mut statement = store.connection().prepare(
            "SELECT o.*,f.owner,f.media_type,f.object_mode,r.contract_version,r.encoding,r.content,r.content_hash,r.byte_length,r.created_at AS revision_created_at,r.references_json,c.definition FROM objects o JOIN contract_families f ON f.key=o.contract_key JOIN revisions r ON r.object_id=o.id AND r.revision=o.current_revision JOIN contracts c ON c.key=r.contract_key AND c.version=r.contract_version WHERE o.contract_key='agent/instructions' AND o.effective_archive=0",
        ).map_err(|e| store.query_error(e))?;
        let rows = statement
            .query_map([], captured)
            .map_err(|e| store.query_error(e))?;
        let values = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| store.query_error(e))?;
        Ok(IvyJson::Array(values))
    })();
    store.close()?;
    result
}

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn parameter(value: &IvyJson, key: &str) -> IvyResult<Value> {
    Ok(match field(value, key)? {
        IvyJson::Null => Value::Null,
        IvyJson::Number(v) => Value::Real(*v),
        IvyJson::String(v) => Value::Text(String::from_utf16_lossy(&v.0)),
        _ => return Err(IvyError::internal()),
    })
}

/// Restore only the captured current revision and original identities. The
/// complete batch rolls back on failure; no retention recount or schema rewrite
/// is added to the original SQL boundary.
pub fn restore_instructions(store: &mut Store, values: &IvyJson) -> IvyResult<()> {
    store.transaction(|store| {
        let values = values.as_array().ok_or_else(IvyError::internal)?;
        for value in values {
            let object = field(value, "object")?;
            if object.is_null() { return Err(IvyError::internal()); }
            if object.get("parentId") != Some(&IvyJson::Null)
                || object.get("ownerObjectId") != Some(&IvyJson::Null) {
                return Err(IvyError::new("storage_invalid", "Instruction configuration must be a root document without runtime references."));
            }
            let revision = field(value, "revision")?;
            if revision.is_null() { return Err(IvyError::internal()); }
            if revision.get("referencesJson") != Some(&"{}".into()) {
                return Err(IvyError::new("storage_invalid", "Instruction configuration must be a root document without runtime references."));
            }
            let family = field(value, "family")?;
            let contract = field(value, "contract")?;
            store.connection().execute("INSERT OR IGNORE INTO contract_families VALUES(?,?,?,?)",
                params![parameter(family,"key")?,parameter(family,"owner")?,parameter(family,"mediaType")?,parameter(family,"objectMode")?],
            ).map_err(|e| store.query_error(e))?;
            store.connection().execute("INSERT OR IGNORE INTO contracts VALUES(?,?,?)",
                params![parameter(family,"key")?,parameter(contract,"version")?,parameter(contract,"definition")?],
            ).map_err(|e| store.query_error(e))?;
            store.connection().execute(
                "INSERT INTO objects(search_id,id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at) VALUES(NULL,?,?,?,?,?,?,?,?,?,?,NULL,0,?,?)",
                params![parameter(object,"id")?,parameter(object,"parentId")?,parameter(object,"ownerObjectId")?,
                    parameter(object,"name")?,parameter(object,"path")?,parameter(object,"position")?,parameter(object,"icon")?,
                    parameter(object,"depth")?,parameter(family,"key")?,parameter(object,"currentRevision")?,
                    parameter(object,"createdAt")?,parameter(object,"updatedAt")?],
            ).map_err(|e| store.query_error(e))?;
            store.connection().execute("INSERT INTO revisions VALUES(?,?,?,?,?,?,?,?,?,?)",
                params![parameter(object,"id")?,parameter(object,"currentRevision")?,parameter(family,"key")?,
                    parameter(revision,"contractVersion")?,parameter(revision,"encoding")?,parameter(revision,"content")?,
                    parameter(revision,"contentHash")?,parameter(revision,"byteLength")?,parameter(revision,"createdAt")?,parameter(revision,"referencesJson")?],
            ).map_err(|e| store.query_error(e))?;
            let search = store.connection().query_row("SELECT search_id FROM objects WHERE id=?",
                [parameter(object,"id")?], |row| row.get::<_,i64>(0),
            ).optional().map_err(|e| store.query_error(e))?
                .ok_or_else(|| IvyError::new("storage_invalid", "Restored instruction metadata is unavailable."))?;
            store.connection().execute("INSERT INTO object_fts(rowid,object_id,name,text) VALUES(?,?,?,?)",
                params![search,parameter(object,"id")?,parameter(object,"name")?,""],
            ).map_err(|e| store.query_error(e))?;
        }
        Ok(())
    })
}

/// Run after Host has durably captured instructions and cleared its selected
/// paths. Epoch, restore, root bootstrap and service registrations intentionally
/// retain their separate failure boundaries. Host supplies the clock at the
/// original root-bootstrap call boundary, not before the storage work.
pub fn rebootstrap(
    path: &Path,
    epoch: &JsString,
    instructions: &IvyJson,
    roots: &[JsString],
    service_contracts: &[IvyJson],
    now: impl FnOnce() -> IvyResult<JsString>,
) -> IvyResult<()> {
    let mut store = Store::open(path)?;
    let result = (|| {
        store.set_metadata("runtime_epoch", &String::from_utf16_lossy(&epoch.0))?;
        restore_instructions(&mut store, instructions)?;
        objects::bootstrap_scope_roots(&mut store, roots, &now()?)?;
        objects::bootstrap_service_contracts(&mut store, service_contracts)
    })();
    store.close()?;
    result
}
