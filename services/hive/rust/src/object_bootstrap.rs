//! Local reset bootstrap. Host owns admission and the reset record; no network
//! operation exposes these schema operations.
use crate::store::Store;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::digest, json::parse};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeSet;

const ROOT_KEY: &str = "ivy-reference-rollout/root";
const ROOT_CONTRACT: &str = r#"{
  "key":"ivy-reference-rollout/root","version":"1.0.0",
  "owner":{"kind":"hive"},"mediaType":"application/json",
  "retention":{"objects":{"mode":"retain"},"revisions":{"mode":"current"}},
  "specMarkdown":"Empty stable scope anchor recreated by a full Reference Rollout reset.",
  "jsonSchema":{"type":"object","properties":{"schemaVersion":{"const":1}},
    "required":["schemaVersion"],"additionalProperties":false}
}"#;

/// Register the fixed root contract, then atomically create the selected roots.
/// Existing roots are checked using the original reset metadata guard only.
pub fn bootstrap_scope_roots(store: &mut Store, ids: &[JsString], at: &JsString) -> IvyResult<()> {
    let roots: BTreeSet<_> = ids.iter().collect();
    for id in &roots {
        if !(1..=256).contains(&id.0.len()) {
            return Err(IvyError::new(
                "invalid_arguments",
                "Invalid configured scope root Object ID.",
            ));
        }
    }
    let definition = parse(ROOT_CONTRACT, usize::MAX)?;
    super::register(store, &definition, definition.get("owner").unwrap())?;
    let content = r#"{"schemaVersion":1}"#;
    let content_hash = digest(content);
    let at = String::from_utf16_lossy(&at.0);
    store.transaction(|store| {
        for id in roots {
            let id = String::from_utf16_lossy(&id.0);
            let existing = store.connection().query_row(
                "SELECT parent_id,owner_object_id,contract_key,current_revision,archived_at FROM objects WHERE id=?",
                [&id], |row| Ok((row.get::<_,Option<String>>(0)?, row.get::<_,Option<String>>(1)?,
                    row.get::<_,String>(2)?,row.get::<_,i64>(3)?,row.get::<_,Option<String>>(4)?)),
            ).optional().map_err(|e| store.query_error(e))?;
            if let Some((parent, owner, key, revision, archived)) = existing {
                if parent.is_some() || owner.is_some() || key != ROOT_KEY || revision != 1 || archived.is_some() {
                    return Err(IvyError::new("target_conflict", "Configured scope root ID is already used by another Object."));
                }
                continue;
            }
            let name = format!("Ivy scope {}", &digest(&id)[7..23]);
            let position = super::navigation::next_position(store, None)?;
            store.connection().execute(
                "INSERT INTO objects(id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at) VALUES (?,NULL,NULL,?,?,?,NULL,1,?,1,NULL,0,?,?)",
                params![id,name,format!("/{name}"),position,ROOT_KEY,at,at],
            ).map_err(|e| store.query_error(e))?;
            store.connection().execute("INSERT INTO revisions VALUES (?,?,?,?,?,?,?,?,?,?)",
                params![id,1,ROOT_KEY,"1.0.0","json",content,content_hash,content.len() as i64,at,"{}"],
            ).map_err(|e| store.query_error(e))?;
            store.adjust_retention_usage(&ROOT_KEY.into(), 1, 1, content.len() as i64)?;
            super::writing::update_search(store, &id, &name, "")?;
        }
        Ok(())
    })
}

/// Preserve each registration's own boundary: an earlier service contract is
/// retained if a later definition fails. This is not a batch transaction.
pub fn bootstrap_service_contracts(store: &mut Store, definitions: &[IvyJson]) -> IvyResult<()> {
    for definition in definitions {
        let owner = definition
            .get("owner")
            .filter(|v| !v.is_null())
            .ok_or_else(IvyError::internal)?;
        if owner.get("kind") != Some(&"service".into()) {
            return Err(IvyError::new(
                "contract_owner_mismatch",
                "Reset bootstrap accepts only service-owned contracts.",
            ));
        }
        super::register(store, definition, owner)?;
    }
    Ok(())
}
