//! Stable Object FTS pages; quoted words remain data, never FTS query syntax.
use super::{
    Location, complete_object, object_row,
    pages::{archived, cursor, identity, limit},
    stat,
};
use crate::{paging::page, store::Store};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::try_js_string};
use rusqlite::params;

fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
fn words(input: &JsString, letter_or_number: &dyn Fn(u32) -> bool) -> Vec<JsString> {
    let mut result = vec![];
    let mut word = vec![];
    for decoded in char::decode_utf16(input.0.iter().copied()) {
        let point = decoded
            .as_ref()
            .map_or_else(|e| u32::from(e.unpaired_surrogate()), |c| u32::from(*c));
        if point == 95 || letter_or_number(point) {
            if let Ok(character) = decoded {
                word.extend_from_slice(character.encode_utf16(&mut [0; 2]));
            }
        } else if !word.is_empty() {
            result.push(JsString(std::mem::take(&mut word)));
        }
    }
    if !word.is_empty() {
        result.push(JsString(word));
    }
    result
}

/// Input is admitted by the owning operation. The runtime supplies exactly the
/// Unicode Letter/Number predicate used by Discovery's /[\p{L}\p{N}]/u boundary.
/// Hive adds underscore, word grouping, the word limit and all search policy.
pub fn search(
    store: &Store,
    parameters: &IvyJson,
    letter_or_number: &dyn Fn(u32) -> bool,
) -> IvyResult<IvyJson> {
    let identity = identity("objects.search", parameters)?;
    let limit = limit(parameters, 50);
    let after = match cursor(parameters) {
        Some(cursor) => match store.read_cursor(&identity, cursor)? {
            Some(value) => try_js_string(&value)?,
            None => "undefined".into(),
        },
        None => JsString::default(),
    };
    let root = parameters
        .get("rootId")
        .and_then(IvyJson::as_js_string)
        .filter(|v| !v.0.is_empty())
        .map(|id| stat(store, Location::Id(id)))
        .transpose()?;
    let tokens = words(string(parameters, "text")?, letter_or_number);
    if tokens.len() > 32 {
        return Err(IvyError::new(
            "limit_exceeded",
            "Search supports at most 32 words.",
        ));
    }
    if tokens.is_empty() {
        return Ok(page(vec![], None));
    }
    let query = tokens
        .iter()
        .map(|token| format!("\"{}\"", text(token).replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");
    let contract = parameters
        .get("contractKey")
        .and_then(IvyJson::as_js_string)
        .map(text);
    let excluded = parameters
        .get("excludeContractKey")
        .and_then(IvyJson::as_js_string)
        .map(text);
    let root_id = root
        .as_ref()
        .map(|v| string(v, "id").map(text))
        .transpose()?;
    let root_path = root
        .as_ref()
        .map(|v| string(v, "path").map(text))
        .transpose()?;
    let mut statement = store.connection().prepare_cached(
        "SELECT o.* FROM object_fts f JOIN objects o ON o.id=f.object_id
         WHERE object_fts MATCH ? AND (? IS NULL OR o.contract_key=?) AND (? IS NULL OR o.contract_key<>?) AND (? OR o.effective_archive=0)
         AND (? IS NULL OR o.id=? OR substr(o.path,1,length(?)+1)=?||'/') AND o.id>? AND ivy_budget() ORDER BY o.id LIMIT ?"
    ).map_err(|e| store.query_error(e))?;
    let rows = statement
        .query_map(
            params![
                query,
                contract,
                contract,
                excluded,
                excluded,
                i64::from(archived(parameters)),
                root_id,
                root_id,
                root_path,
                root_path,
                text(&after),
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
        Some(
            store.cursor(
                &identity,
                items
                    .last()
                    .ok_or_else(IvyError::internal)?
                    .get("id")
                    .ok_or_else(IvyError::internal)?,
            )?,
        )
    } else {
        None
    };
    Ok(page(items, next))
}
