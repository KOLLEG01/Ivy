use super::*;
use crate::paging::page;
use rusqlite::{params, types::Value};

fn argument<T>(value: Option<T>) -> IvyResult<T> {
    value.ok_or_else(|| IvyError::new("invalid_arguments", "Invalid object page arguments."))
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
pub(super) fn identity(method: &str, params: &IvyJson) -> IvyResult<IvyJson> {
    let mut identity = IvyJson::object([("method", IvyJson::from(method))]);
    for (key, value) in argument(params.as_object())? {
        if key != &JsString::from("cursor") {
            identity.insert(key.clone(), value.clone());
        }
    }
    Ok(identity)
}
pub(super) fn cursor<'a>(params: &'a IvyJson) -> Option<&'a JsString> {
    params
        .get("cursor")
        .and_then(IvyJson::as_js_string)
        .filter(|s| !s.0.is_empty())
}
pub(super) fn limit(params: &IvyJson, default: usize) -> usize {
    params
        .get("limit")
        .and_then(IvyJson::as_f64)
        .unwrap_or(default as f64) as usize
}
pub(super) fn archived(params: &IvyJson) -> bool {
    params.get("includeArchived").and_then(IvyJson::as_bool) == Some(true)
}
fn exists(store: &Store, id: &JsString) -> IvyResult<()> {
    let found = store
        .connection()
        .prepare_cached("SELECT 1 FROM objects WHERE id=?")
        .map_err(|e| store.query_error(e))?
        .query_row([text(id)], |row| row.get::<_, i64>(0))
        .optional()
        .map_err(|e| store.query_error(e))?
        .is_some();
    if found {
        Ok(())
    } else {
        Err(IvyError::new("not_found", "Object not found."))
    }
}

/// Input is admitted by the operation contract. Cursors bind the original,
/// pre-default arguments, including explicit false and omitted properties.
pub fn list(store: &Store, params: &IvyJson) -> IvyResult<IvyJson> {
    let parent = argument(params.get("parentId"))?;
    let parent = if parent.is_null() {
        None
    } else {
        Some(argument(parent.as_js_string())?)
    };
    if let Some(parent) = parent {
        exists(store, parent)?;
    }
    let identity = identity("objects.list", params)?;
    let boundary = match cursor(params) {
        Some(cursor) => store.read_cursor(&identity, cursor)?,
        None => Some(IvyJson::Array(vec![
            IvyJson::Number(-1.0),
            IvyJson::from(""),
        ])),
    };
    let boundary = boundary.as_ref().and_then(IvyJson::as_array);
    let position = boundary.and_then(|v| v.first()).and_then(IvyJson::as_f64);
    let id = boundary
        .and_then(|v| v.get(1))
        .and_then(IvyJson::as_js_string);
    let (Some(position), Some(id)) = (position, id) else {
        return Err(IvyError::new("invalid_cursor", "Invalid cursor."));
    };
    let limit = limit(params, 50);
    let mut statement=store.connection().prepare_cached("SELECT * FROM objects WHERE parent_id IS ? AND (? OR effective_archive=0) AND (position>? OR (position=? AND id>?)) AND ivy_budget() ORDER BY position,id LIMIT ?").map_err(|e|store.query_error(e))?;
    let rows = statement
        .query_map(
            params![
                parent.map(text),
                i64::from(archived(params)),
                position,
                position,
                text(id),
                (limit + 1) as i64
            ],
            object_row,
        )
        .map_err(|e| store.query_error(e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| store.query_error(e))?;
    let more = rows.len() > limit;
    let items = rows
        .into_iter()
        .take(limit)
        .map(|row| complete_object(store, row))
        .collect::<IvyResult<Vec<_>>>()?;
    let next = if more {
        let last = items.last().ok_or_else(IvyError::internal)?;
        Some(store.cursor(
            &identity,
            &IvyJson::Array(vec![
                last.get("position").unwrap().clone(),
                last.get("id").unwrap().clone(),
            ]),
        )?)
    } else {
        None
    };
    Ok(page(items, next))
}

pub fn history(store: &Store, params: &IvyJson) -> IvyResult<IvyJson> {
    let id = argument(params.get("objectId").and_then(IvyJson::as_js_string))?;
    let object = stat(store, Location::Id(id))?;
    let identity = identity("objects.history", params)?;
    let after = match cursor(params) {
        Some(cursor) => Value::Real(
            store
                .read_cursor(&identity, cursor)?
                .and_then(|value| value.as_f64())
                .filter(|v| {
                    v.is_finite() && v.fract() == 0.0 && (1.0..=9_007_199_254_740_991.0).contains(v)
                })
                .ok_or_else(|| IvyError::new("invalid_cursor", "Invalid history cursor."))?,
        ),
        None => Value::Null,
    };
    let limit = limit(params, 50);
    let mut statement=store.connection().prepare_cached("SELECT object_id,revision,contract_key,contract_version,content_hash,byte_length,created_at,references_json FROM revisions WHERE object_id=? AND (? IS NULL OR revision<?) ORDER BY revision DESC LIMIT ?").map_err(|e|store.query_error(e))?;
    let rows = statement
        .query_map(
            params![text(id), after, after, (limit + 1) as i64],
            revision_row,
        )
        .map_err(|e| store.query_error(e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| store.query_error(e))?;
    let retained: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM revisions WHERE object_id=?",
            [text(id)],
            |row| row.get(0),
        )
        .map_err(|e| store.query_error(e))?;
    let more = rows.len() > limit;
    let items = rows
        .into_iter()
        .take(limit)
        .map(|(key, row)| complete_revision(store, &key, row))
        .collect::<IvyResult<Vec<_>>>()?;
    let next = if more {
        Some(
            store.cursor(
                &identity,
                items
                    .last()
                    .ok_or_else(IvyError::internal)?
                    .get("revision")
                    .unwrap(),
            )?,
        )
    } else {
        None
    };
    let mut result = page(items, next);
    result.insert(
        "historyComplete",
        IvyJson::Bool(object.get("currentRevision").unwrap().as_f64() == Some(retained as f64)),
    );
    Ok(result)
}

pub fn tree(store: &Store, params: &IvyJson) -> IvyResult<IvyJson> {
    let root = argument(params.get("rootId"))?;
    let root = if root.is_null() {
        None
    } else {
        Some(argument(root.as_js_string())?)
    };
    if let Some(root) = root.filter(|v| !v.0.is_empty()) {
        exists(store, root)?;
    }
    let limit = limit(params, 200);
    let depth = params.get("depth").and_then(IvyJson::as_f64).unwrap_or(3.0) as i64;
    let archived = i64::from(archived(params));
    let mut statement=store.connection().prepare_cached("WITH RECURSIVE subtree(id,level) AS (SELECT id,1 FROM objects WHERE parent_id IS ? AND (? OR effective_archive=0) UNION ALL SELECT o.id,s.level+1 FROM objects o JOIN subtree s ON o.parent_id=s.id WHERE s.level<? AND (? OR o.effective_archive=0)) SELECT o.*,s.level FROM objects o JOIN subtree s ON o.id=s.id WHERE ivy_budget() ORDER BY s.level,o.path LIMIT ?").map_err(|e|store.query_error(e))?;
    let rows = statement
        .query_map(
            params![
                root.map(text),
                archived,
                depth,
                archived,
                (limit + 1) as i64
            ],
            |row| Ok((object_row(row)?, row.get::<_, i64>("level")?)),
        )
        .map_err(|e| store.query_error(e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| store.query_error(e))?;
    let more = rows.len() > limit;
    let mut deeper = false;
    for (row, level) in rows.iter().take(limit) {
        if *level == depth {
            let found=store.connection().prepare_cached("SELECT 1 FROM objects WHERE parent_id=? AND (? OR effective_archive=0) LIMIT 1").map_err(|e|store.query_error(e))?
                .query_row(params![row.get("id").unwrap().as_str()?,archived],|r|r.get::<_,i64>(0)).optional().map_err(|e|store.query_error(e))?.is_some();
            if found {
                deeper = true;
                break;
            }
        }
    }
    let items = rows
        .into_iter()
        .take(limit)
        .map(|(row, _)| complete_object(store, row))
        .collect::<IvyResult<Vec<_>>>()?;
    Ok(IvyJson::object([
        ("items", IvyJson::Array(items)),
        ("truncated", IvyJson::Bool(more || deeper)),
    ]))
}
