//! Select compact catalog identities before reading a contract body.
use crate::{objects, paging, store::Store};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, compare_versions, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags, params};

pub(crate) fn install_functions(connection: &Connection) -> rusqlite::Result<()> {
    connection.create_scalar_function(
        "ivy_contract_version_key",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let version = context.get::<String>(0)?;
            compare_versions(&version, &version)
                .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))?;
            // Numeric components have no leading zeroes and at most 128 bytes.
            // Length followed by digits preserves their exact numeric order.
            let mut key = Vec::with_capacity(version.len() + 1);
            for part in version.split('.') {
                key.push(part.len() as u8);
                key.extend_from_slice(part.as_bytes());
            }
            Ok(key)
        },
    )
}

pub(crate) fn latest(store: &Store, key: &JsString) -> IvyResult<IvyJson> {
    let encoded = store
        .connection()
        .prepare_cached(
            "SELECT definition FROM contracts WHERE key=?1 AND version=(
                 SELECT version FROM contracts WHERE key=?1
                 ORDER BY ivy_contract_version_key(version) DESC LIMIT 1
             )",
        )
        .and_then(|mut statement| {
            statement
                .query_row([String::from_utf16_lossy(&key.0)], |row| {
                    row.get::<_, String>(0)
                })
                .optional()
        })
        .map_err(|error| store.query_error(error))?
        .ok_or_else(|| IvyError::new("contract_not_found", "Data Contract not found."))?;
    parse(&encoded, MANAGEMENT_FRAME_BYTES).map_err(|_| IvyError::internal())
}

struct ContractId {
    key: String,
    version: String,
}

fn read(
    store: &Store,
    id: &ContractId,
    summaries: bool,
    remaining: usize,
) -> IvyResult<Option<IvyJson>> {
    let query = if summaries {
        // -> keeps JSON string escapes, including lone UTF-16 units. Do not use
        // json_extract's conversion to SQLite text for external string values.
        "SELECT json_object('key',definition -> '$.key','version',definition -> '$.version',
             'description',definition -> '$.specMarkdown','owner',definition -> '$.owner',
             'mediaType',definition -> '$.mediaType'),
             json_type(definition,'$.jsonSchema') IS NOT NULL,0
         FROM contracts WHERE key=? AND version=?"
    } else {
        "SELECT definition,0,length(CAST(definition AS BLOB)) FROM contracts WHERE key=? AND version=?"
    };
    let row = store
        .connection()
        .prepare_cached(query)
        .and_then(|mut statement| {
            statement.query_row(params![id.key, id.version], |row| {
                // Registered definitions are canonical JSON. Check their exact
                // byte length before copying or decoding a body that cannot fit.
                if row.get::<_, i64>(2)? as usize > remaining {
                    return Ok(None);
                }
                Ok(Some((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)))
            })
        })
        .map_err(|error| store.query_error(error))?;
    let Some((encoded, has_schema)) = row else {
        return Ok(None);
    };
    let mut record = parse(&encoded, MANAGEMENT_FRAME_BYTES).map_err(|_| IvyError::internal())?;
    if summaries {
        let spec = record
            .get("description")
            .and_then(IvyJson::as_js_string)
            .ok_or_else(IvyError::internal)?;
        let mut description = crate::discovery::collapse_whitespace(spec);
        description.0.truncate(512);
        if description.0.is_empty() {
            description = record
                .get("key")
                .and_then(IvyJson::as_js_string)
                .cloned()
                .ok_or_else(IvyError::internal)?;
        }
        record.insert("description", IvyJson::String(description));
        record.insert("hasJsonSchema", has_schema.into());
    }
    Ok(Some(record))
}

pub(crate) fn list(store: &Store, method: &str, params: &IvyJson) -> IvyResult<IvyJson> {
    let identity = objects::page_identity(method, params)?;
    let after = match params
        .get("cursor")
        .and_then(IvyJson::as_js_string)
        .filter(|cursor| !cursor.0.is_empty())
    {
        Some(cursor) => paging::catalog_boundary(store, &identity, cursor)?,
        None => JsString::default(),
    };
    let limit = params
        .get("limit")
        .and_then(IvyJson::as_f64)
        .unwrap_or(50.0) as usize;
    let filter = params
        .get("key")
        .and_then(IvyJson::as_js_string)
        .map(|value| String::from_utf16_lossy(&value.0));
    let all_versions = params.get("allVersions").and_then(IvyJson::as_bool) == Some(true);
    let query = if all_versions {
        "SELECT key,version FROM contracts WHERE (?1 IS NULL OR key=?1)
         AND key||'@'||version > ?2 ORDER BY key||'@'||version LIMIT ?3"
    } else {
        // SQLite selects bare columns from the row contributing the unique max.
        // Only key/version metadata enters this grouping and sort.
        "SELECT key,version FROM (
             SELECT key,version,max(ivy_contract_version_key(version)) FROM contracts
             WHERE (?1 IS NULL OR key=?1) GROUP BY key
         ) WHERE key||'@'||version > ?2 ORDER BY key||'@'||version LIMIT ?3"
    };
    let ids = store
        .connection()
        .prepare_cached(query)
        .and_then(|mut statement| {
            statement
                .query_map(
                    params![
                        filter,
                        String::from_utf16_lossy(&after.0),
                        (limit + 1) as i64
                    ],
                    |row| {
                        Ok(ContractId {
                            key: row.get(0)?,
                            version: row.get(1)?,
                        })
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|error| store.query_error(error))?;
    let mut items = Vec::new();
    let mut used = 128;
    let mut boundary = JsString::default();
    let mut more = false;
    'records: for id in ids {
        if items.len() == limit {
            more = true;
            break;
        }
        let Some(record) = read(
            store,
            &id,
            method == "contracts.summaries",
            paging::PAGE_BYTES - used,
        )?
        else {
            if items.is_empty() {
                return Err(IvyError::new(
                    "result_too_large",
                    "A catalog entry exceeds the response limit.",
                ));
            }
            more = true;
            break;
        };
        // The SQL key replaces lone surrogates. Preserve the old catalog's
        // repeated latest result when different original keys share that key.
        let copies = if !all_versions && id.key.contains('\u{fffd}') {
            store
                .connection()
                .query_row(
                    "SELECT count(DISTINCT definition -> '$.key') FROM contracts WHERE key=?",
                    [&id.key],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| store.query_error(error))? as usize
        } else {
            1
        };
        let size = canonical(&record, MANAGEMENT_FRAME_BYTES)?.len();
        let mut key = record
            .get("key")
            .and_then(IvyJson::as_js_string)
            .cloned()
            .ok_or_else(IvyError::internal)?;
        key.0.push(64);
        key.0.extend(
            record
                .get("version")
                .and_then(IvyJson::as_js_string)
                .ok_or_else(IvyError::internal)?
                .0
                .iter()
                .copied(),
        );
        let mut record = Some(record);
        for index in 0..copies {
            if items.len() == limit || used + size > paging::PAGE_BYTES {
                if items.is_empty() {
                    return Err(IvyError::new(
                        "result_too_large",
                        "A catalog entry exceeds the response limit.",
                    ));
                }
                more = true;
                break 'records;
            }
            used += size;
            items.push(if index + 1 == copies {
                record.take().unwrap()
            } else {
                record.as_ref().unwrap().clone()
            });
            boundary = key.clone();
        }
    }
    let next = if more && !items.is_empty() {
        Some(store.cursor(&identity, &IvyJson::String(boundary))?)
    } else {
        None
    };
    Ok(paging::page(items, next))
}

#[cfg(test)]
#[path = "../../../../tests/rust/hive_contract_catalog.rs"]
mod tests;
