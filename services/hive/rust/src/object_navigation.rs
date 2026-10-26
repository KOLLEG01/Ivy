//! Shared Object name, icon and navigation rules.
use crate::store::Store;
use ivy_contracts::{IvyError, IvyResult, JsString};
use rusqlite::{OptionalExtension, params};

fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
pub(super) fn name(value: &JsString) -> IvyResult<String> {
    require(
        String::from_utf16(&value.0).is_ok(),
        "invalid_arguments",
        "Object name must be valid Unicode.",
    )?;
    let name = value.normalize_nfc();
    require(
        !name.0.is_empty()
            && name.0.len() <= 255
            && !name.0.iter().any(|unit| matches!(unit, 47 | 92 | 0))
            && name != JsString::from(".")
            && name != JsString::from(".."),
        "invalid_arguments",
        "Invalid Object name.",
    )?;
    name.to_utf8()
}
pub(super) fn icon(value: Option<&JsString>) -> IvyResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    require(
        String::from_utf16(&value.0).is_ok(),
        "invalid_arguments",
        "Object icon must be valid Unicode.",
    )?;
    let icon = value.normalize_nfc().trim_ecmascript();
    require(
        !icon.0.is_empty()
            && icon.0.len() <= 32
            && !icon.0.iter().any(|unit| matches!(unit, 13 | 10 | 0)),
        "invalid_arguments",
        "Object icon must be a short single-line value.",
    )?;
    Ok(Some(icon.to_utf8()?))
}
pub(super) struct Parent {
    pub path: String,
    pub depth: i64,
    pub archived: i64,
    pub contract_key: JsString,
}
pub(super) fn parent(store: &Store, id: Option<&JsString>) -> IvyResult<Option<Parent>> {
    let Some(id) = id else {
        return Ok(None);
    };
    store
        .connection()
        .query_row(
            "SELECT path,depth,effective_archive,contract_key FROM objects WHERE id=?",
            [String::from_utf16_lossy(&id.0)],
            |row| {
                Ok(Parent {
                    path: row.get(0)?,
                    depth: row.get(1)?,
                    archived: row.get(2)?,
                    contract_key: JsString::from(row.get::<_, String>(3)?),
                })
            },
        )
        .optional()
        .map_err(|e| store.query_error(e))?
        .map(Some)
        .ok_or_else(|| IvyError::new("not_found", "Object not found."))
}
pub(super) fn object_mode(store: &Store, key: &JsString) -> IvyResult<Option<String>> {
    store
        .connection()
        .query_row(
            "SELECT object_mode FROM contract_families WHERE key=?",
            [String::from_utf16_lossy(&key.0)],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| store.query_error(e))
}
pub(super) fn path(parent: Option<&Parent>, name: &str) -> IvyResult<(String, i64)> {
    let depth = parent.map_or(1, |parent| parent.depth + 1);
    require(
        depth <= 64,
        "limit_exceeded",
        "Object hierarchy exceeds its maximum depth.",
    )?;
    Ok((
        format!(
            "{}/{}",
            parent.map_or("", |parent| parent.path.as_str()),
            name
        ),
        depth,
    ))
}
pub(super) fn ensure_name(
    store: &Store,
    parent: Option<&JsString>,
    name: &str,
    excluding: Option<&JsString>,
) -> IvyResult<()> {
    let existing: Option<String> = store
        .connection()
        .query_row(
            "SELECT id FROM objects WHERE parent_id IS ? AND name=?",
            params![parent.map(|id| String::from_utf16_lossy(&id.0)), name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| store.query_error(e))?;
    require(
        existing.is_none_or(|id| excluding.is_some_and(|excluded| &JsString::from(id) == excluded)),
        "revision_conflict",
        "An Object already uses that sibling name.",
    )
}
pub(super) fn next_position(store: &Store, parent: Option<&JsString>) -> IvyResult<i64> {
    let current: f64 = store
        .connection()
        .query_row(
            "SELECT COALESCE(MAX(position),-1) AS position FROM objects WHERE parent_id IS ?",
            [parent.map(|id| String::from_utf16_lossy(&id.0))],
            |row| row.get(0),
        )
        .map_err(|e| store.query_error(e))?;
    require(
        current.is_finite() && current.fract() == 0.0 && current < 9_007_199_254_740_991.0,
        "limit_exceeded",
        "Object sibling position limit reached.",
    )?;
    Ok(current as i64 + 1)
}
