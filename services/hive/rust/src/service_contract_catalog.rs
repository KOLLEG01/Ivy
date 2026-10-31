//! Stream the existing snapshot identity, then load only the selected page.
use super::*;

const RECORDS: &str = "FROM registries AS r,json_each(r.registry_json,'$.requiredContracts') AS e
    WHERE r.node_id=?1 AND e.key IS NOT NULL AND (?2 IS NULL OR CASE WHEN e.type='object' THEN
        json_type(e.value,'$.key')='text'
        AND json_extract(e.value,'$.key')=json_extract(?2,'$') END)";

fn record(index: usize, source: &str) -> IvyResult<IvyJson> {
    Ok(IvyJson::object([
        ("index", IvyJson::Number(index as f64)), ("value", decode(source)?),
    ]))
}

pub(super) fn list(store: &Store, params: &IvyJson, node: &IvyJson) -> IvyResult<IvyJson> {
    let id = text(string(params, "serviceNodeId")?);
    // Admit the retained catalog before hashing or checking the cursor, as before.
    let kinds = sql(store, store.connection().query_row(
        "SELECT json_type(registry_json),json_type(registry_json,'$.requiredContracts')
         FROM registries WHERE node_id=?", [&id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    ).optional())?;
    let has_catalog = match kinds {
        None => false,
        Some((kind, _)) if kind == "null" => false,
        Some((kind, requirements)) if kind == "object"
            && requirements.as_deref().is_none_or(|kind| matches!(kind, "array" | "null")) => true,
        _ => return Err(IvyError::internal()),
    };
    let filter = optional_string(params, "key").filter(|key| !key.0.is_empty())
        .map(|key| canonical(&IvyJson::String(key.clone()), MANAGEMENT_FRAME_BYTES)).transpose()?;
    let captured = node.get("lastSuccessfulSyncAt");
    let captured_key = JsString::from("capturedAt");
    let records_key = JsString::from("records");
    let snapshot = {
        let mut statement = sql(store, store.connection().prepare_cached(&format!(
            // json_each traverses this one array in index order. Sorting these
            // rows would make SQLite retain every projected body before hashing.
            "SELECT e.key,r.registry_json -> ('$.requiredContracts['||e.key||']')
             {RECORDS}",
        )))?;
        let rows = sql(store, statement.query_map(params![id, filter], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        }))?;
        // One record at a time preserves canonical depth/node/byte budgets and
        // error priority without retaining the complete requirement array.
        hash_object_with_array(captured.map(|value| (&captured_key, value)), &records_key,
            rows.map(|row| {
                let (index, source) = sql(store, row)?;
                record(index as usize, &source)
            }))?
    };
    let mut identity = objects::page_identity("serviceNodes.contracts", params)?;
    identity.insert("snapshot", snapshot.into());
    let after = match optional_string(params, "cursor").filter(|cursor| !cursor.0.is_empty()) {
        Some(cursor) => text(&crate::paging::catalog_boundary(store, &identity, cursor)?),
        None => String::new(),
    };
    let limit = params.get("limit").and_then(IvyJson::as_f64).unwrap_or(50.0) as usize;
    let indices = {
        let mut statement = sql(store, store.connection().prepare_cached(&format!(
            "SELECT e.key {RECORDS} AND printf('%08d',e.key)>?3
             ORDER BY printf('%08d',e.key) LIMIT ?4",
        )))?;
        sql(store, sql(store, statement.query_map(params![id, filter, after, (limit + 1) as i64],
            |row| row.get::<_, i64>(0)))?.collect::<rusqlite::Result<Vec<_>>>())?
    };
    let mut result = crate::paging::catalog_page_with(store, indices, &identity,
        |index| Ok(format!("{index:08}").into()),
        |index| {
            let source = sql(store, store.connection().query_row(
                "SELECT registry_json -> ('$.requiredContracts['||?2||']')
                 FROM registries WHERE node_id=?1", params![id, index as i64],
                |row| row.get::<_, String>(0),
            ))?;
            record(index as usize, &source)
        }, Some(limit), None)?;
    let items = result.remove("items").and_then(IvyJson::into_array)
        .ok_or_else(IvyError::internal)?;
    result.insert("items", IvyJson::Array(items.into_iter()
        .map(|mut record| record.remove("value").unwrap_or(IvyJson::Null)).collect()));
    result.insert("provider", provider(node));
    result.insert("hasCatalog", has_catalog.into());
    if let Some(captured) = captured { result.insert("capturedAt", captured.clone()); }
    Ok(result)
}
