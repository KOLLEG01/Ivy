//! Bounded registry catalogs. Object scans use the database query keyset.
use crate::store::Store;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, try_js_string},
    limits::MANAGEMENT_FRAME_BYTES,
};

pub const PAGE_BYTES: usize = 2 * 1024 * 1024 - 8192;

pub fn page(items: Vec<IvyJson>, next_cursor: Option<String>) -> IvyJson {
    IvyJson::object([
        ("items", IvyJson::Array(items)),
        (
            "nextCursor",
            next_cursor.map(IvyJson::from).unwrap_or(IvyJson::Null),
        ),
    ])
}

pub fn catalog_page(
    store: &Store,
    records: &mut [IvyJson],
    identity: &IvyJson,
    key: impl Fn(&IvyJson) -> JsString,
    limit: Option<usize>,
    cursor: Option<&JsString>,
) -> IvyResult<IvyJson> {
    let after = match cursor.filter(|c| !c.0.is_empty()) {
        Some(cursor) => match store.read_cursor(identity, cursor)? {
            Some(value) => try_js_string(&value)?,
            None => "undefined".into(),
        },
        None => JsString::default(),
    };
    let bytes = |s: JsString| String::from_utf16_lossy(&s.0).into_bytes();
    records.sort_by_cached_key(|r| bytes(key(r)));
    let after = bytes(after);
    let remaining = records
        .iter()
        .filter(|r| bytes(key(r)) > after)
        .collect::<Vec<_>>();
    let mut items = vec![];
    let mut used = 128;
    for record in &remaining {
        if items.len() == limit.unwrap_or(50) {
            break;
        }
        let size = canonical(record, MANAGEMENT_FRAME_BYTES)?.len();
        if used + size > PAGE_BYTES {
            if items.is_empty() {
                return Err(IvyError::new(
                    "result_too_large",
                    "A catalog entry exceeds the response limit.",
                ));
            }
            break;
        }
        used += size;
        items.push((*record).clone());
    }
    let next = if !items.is_empty() && remaining.len() > items.len() {
        Some(store.cursor(identity, &IvyJson::String(key(items.last().unwrap())))?)
    } else {
        None
    };
    Ok(page(items, next))
}
