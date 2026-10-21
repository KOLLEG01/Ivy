//! Receipt-backed Object moves preserve immutable revisions and lifecycle owners.
use super::{
    Location, mutations::refresh_navigation, navigation, raw_object, read, stat,
    writing::update_search,
};
use crate::store::{AuthenticatedContext, LedgerResult, Store};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use rusqlite::params;

struct Entry {
    id: JsString,
    parent_id: Option<JsString>,
    depth: i64,
    position: i64,
    icon: Option<String>,
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn string<'a>(value: &'a IvyJson, field: &str) -> IvyResult<&'a JsString> {
    value
        .get(field)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn subtree(store: &Store, id: &JsString) -> IvyResult<Vec<Entry>> {
    let mut statement = store.connection().prepare_cached(
        "WITH RECURSIVE descendants(id) AS (SELECT ? UNION ALL SELECT o.id FROM objects o JOIN descendants d ON o.parent_id=d.id)
         SELECT o.* FROM objects o JOIN descendants d ON o.id=d.id WHERE ivy_budget() ORDER BY o.depth,o.id",
    ).map_err(|e| store.query_error(e))?;
    statement
        .query_map([text(id)], |row| {
            Ok(Entry {
                id: JsString::from(row.get::<_, String>("id")?),
                parent_id: row
                    .get::<_, Option<String>>("parent_id")?
                    .map(JsString::from),
                depth: row.get("depth")?,
                position: row.get("position")?,
                icon: row.get("icon")?,
            })
        })
        .map_err(|e| store.query_error(e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| store.query_error(e))
}
fn normalize_positions(store: &Store, parent: Option<&JsString>) -> IvyResult<()> {
    let siblings = store
        .connection()
        .prepare_cached("SELECT id,position FROM objects WHERE parent_id IS ? ORDER BY position,id")
        .map_err(|e| store.query_error(e))?
        .query_map([parent.map(text)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| store.query_error(e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| store.query_error(e))?;
    for (position, (id, previous)) in siblings.into_iter().enumerate() {
        if previous != position as i64 {
            store
                .connection()
                .prepare_cached("UPDATE objects SET position=? WHERE id=?")
                .map_err(|e| store.query_error(e))?
                .execute(params![position as i64, id])
                .map_err(|e| store.query_error(e))?;
        }
    }
    Ok(())
}

/// Context and clocks come from admitted dispatch, never the request body.
pub fn move_object(
    store: &mut Store,
    context: &AuthenticatedContext,
    parameters: &IvyJson,
    now_ms: u64,
    now: &JsString,
) -> IvyResult<IvyJson> {
    store.mutate(
        context,
        &"objects.move".into(),
        parameters,
        now_ms,
        |store, principal| {
            let id = string(parameters, "objectId")?;
            let object = raw_object(store, Location::Id(id))?;
            let rows = subtree(store, string(&object, "id")?)?;
            let source = rows.first().ok_or_else(IvyError::internal)?;
            let parent_id = match parameters.get("parentId") {
                Some(IvyJson::Null) => None,
                Some(value) => Some(value.as_js_string().ok_or_else(IvyError::internal)?),
                None => return Err(IvyError::internal()),
            };
            let parent = navigation::parent(store, parent_id)?;
            let name = navigation::name(string(parameters, "name")?)?;
            if let Some(parent) = &parent {
                require(
                    navigation::object_mode(store, &parent.contract_key)?.as_deref()
                        == Some("retain"),
                    "invalid_arguments",
                    "Owned artifacts cannot be navigation parents.",
                )?;
            }
            require(
                !rows.iter().any(|row| Some(&row.id) == parent_id),
                "invalid_arguments",
                "An Object cannot be moved below itself.",
            )?;
            let (_, depth) = navigation::path(parent.as_ref(), &name)?;
            let maximum_depth = rows.iter().map(|row| row.depth).fold(0, i64::max);
            require(
                maximum_depth + depth - source.depth <= 64,
                "limit_exceeded",
                "Move would exceed maximum hierarchy depth.",
            )?;
            navigation::ensure_name(store, parent_id, &name, Some(id))?;
            let parent_changed = source.parent_id.as_ref() != parent_id;
            let position = if parent_changed {
                navigation::next_position(store, parent_id)?
            } else {
                source.position
            };
            let icon = match parameters.get("icon") {
                None => source.icon.clone(),
                Some(IvyJson::Null) => None,
                Some(value) => {
                    navigation::icon(Some(value.as_js_string().ok_or_else(IvyError::internal)?))?
                }
            };
            store.connection().execute(
                "UPDATE objects SET parent_id=?,name=?,position=?,icon=?,updated_at=? WHERE id=?",
                params![parent_id.map(text),name,position,icon,text(now),text(id)],
            ).map_err(|e| store.query_error(e))?;
            if parent_changed {
                normalize_positions(store, source.parent_id.as_ref())?;
            }
            refresh_navigation(store, id)?;
            let document = read(store, Location::Id(id), None)?;
            let content = document.get("content").ok_or_else(IvyError::internal)?;
            let search_text = if content.get("encoding") == Some(&"text".into()) {
                text(string(content, "value")?)
            } else {
                String::new()
            };
            update_search(store, &text(id), &name, &search_text)?;
            let result = stat(store, Location::Id(id))?;
            store.append_core_event(
                principal,
                string(&result, "id")?,
                result
                    .get("currentRevision")
                    .and_then(IvyJson::as_f64)
                    .unwrap() as u64,
                &"move".into(),
                string(parameters, "mutationId")?,
                now,
                None,
            )?;
            Ok(LedgerResult::snapshot(result))
        },
    )
}
