//! Bounded registry catalogs. Object scans use the database query keyset.
use crate::store::Store;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};

pub const PAGE_BYTES: usize = 2 * 1024 * 1024 - 8192;

/// Hive's exact UTC calendar admission; date parsing/formatting belongs to the
/// bound runtime rather than an alternative service-local date implementation.
pub fn utc(value: &IvyJson, runtime: &dyn crate::retention::Runtime) -> IvyResult<IvyJson> {
    let string = value
        .as_js_string()
        .ok_or_else(|| IvyError::new("invalid_arguments", "Expected a UTC RFC 3339 timestamp."))?;
    let bytes = &string.0;
    let shape = (bytes.len() == 20 || bytes.len() == 24)
        && bytes.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == 45,
            10 => *c == 84,
            13 | 16 => *c == 58,
            19 if bytes.len() == 24 => *c == 46,
            19 | 23 if i == bytes.len() - 1 => *c == 90,
            _ => (48..=57).contains(c),
        });
    if !shape {
        return Err(IvyError::new(
            "invalid_arguments",
            "Expected a UTC RFC 3339 timestamp.",
        ));
    }
    let milliseconds = runtime.parse_date(&string);
    let normalize = |mut value: JsString| {
        if value
            .0
            .ends_with(&".000Z".encode_utf16().collect::<Vec<_>>())
        {
            value.0.truncate(value.0.len() - 5);
            value.0.push(90);
        }
        value
    };
    if !milliseconds.is_finite()
        || normalize(runtime.iso_date(milliseconds)?) != normalize(string.clone())
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Invalid UTC calendar date.",
        ));
    }
    Ok(value.clone())
}

pub fn page(items: Vec<IvyJson>, next_cursor: Option<String>) -> IvyJson {
    IvyJson::object([
        ("items", IvyJson::Array(items)),
        (
            "nextCursor",
            next_cursor.map(IvyJson::from).unwrap_or(IvyJson::Null),
        ),
    ])
}

pub(crate) fn catalog_boundary(store: &Store, identity: &IvyJson, cursor: &JsString) -> IvyResult<JsString> {
    store.read_cursor(identity, cursor)?
        .and_then(|value| value.as_js_string().cloned())
        .ok_or_else(|| IvyError::new("invalid_cursor", "Invalid catalog cursor boundary."))
}

pub fn catalog_page(
    store: &Store,
    records: Vec<IvyJson>,
    identity: &IvyJson,
    key: impl Fn(&IvyJson) -> JsString,
    limit: Option<usize>,
    cursor: Option<&JsString>,
) -> IvyResult<IvyJson> {
    try_catalog_page(
        store,
        records,
        identity,
        |record| Ok(key(record)),
        limit,
        cursor,
    )
}

/// Catalog keys may decode retained records or hash them. Preserve their error
/// after cursor admission, before response sizing, without a second pager.
pub fn try_catalog_page(
    store: &Store,
    records: Vec<IvyJson>,
    identity: &IvyJson,
    key: impl Fn(&IvyJson) -> IvyResult<JsString>,
    limit: Option<usize>,
    cursor: Option<&JsString>,
) -> IvyResult<IvyJson> {
    catalog_page_with(store, records, identity, key, Ok, limit, cursor)
}

/// Select compact record identities before materializing response bodies.
pub(crate) fn catalog_page_with<T>(
    store: &Store,
    records: Vec<T>,
    identity: &IvyJson,
    key: impl Fn(&T) -> IvyResult<JsString>,
    materialize: impl Fn(T) -> IvyResult<IvyJson>,
    limit: Option<usize>,
    cursor: Option<&JsString>,
) -> IvyResult<IvyJson> {
    let after = match cursor.filter(|c| !c.0.is_empty()) {
        Some(cursor) => catalog_boundary(store, identity, cursor)?,
        None => JsString::default(),
    };
    let bytes = |s: &JsString| String::from_utf16_lossy(&s.0).into_bytes();
    let mut keyed = records
        .into_iter()
        .map(|record| {
            let key = key(&record)?;
            Ok((bytes(&key), key, record))
        })
        .collect::<IvyResult<Vec<_>>>()?;
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let after = bytes(&after);
    let mut items = vec![];
    let mut used = 128;
    let mut boundary = None;
    let mut truncated = false;
    for (bytes, key, record) in keyed {
        if bytes <= after {
            continue;
        }
        if items.len() == limit.unwrap_or(50) {
            truncated = true;
            break;
        }
        let record = materialize(record)?;
        let size = canonical(&record, MANAGEMENT_FRAME_BYTES)?.len();
        if used + size > PAGE_BYTES {
            if items.is_empty() {
                return Err(IvyError::new(
                    "result_too_large",
                    "A catalog entry exceeds the response limit.",
                ));
            }
            truncated = true;
            break;
        }
        used += size;
        items.push(record);
        boundary = Some(key);
    }
    let next = if truncated && let Some(boundary) = boundary {
        Some(store.cursor(identity, &IvyJson::String(boundary))?)
    } else {
        None
    };
    Ok(page(items, next))
}
