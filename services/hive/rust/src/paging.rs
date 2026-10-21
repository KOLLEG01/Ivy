//! Bounded registry catalogs. Object scans use the database query keyset.
use crate::store::Store;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, try_js_string},
    limits::MANAGEMENT_FRAME_BYTES,
};

pub const PAGE_BYTES: usize = 2 * 1024 * 1024 - 8192;

/// Hive's exact UTC calendar admission; date parsing/formatting belongs to the
/// bound runtime rather than an alternative service-local date implementation.
pub fn utc(value: &IvyJson, runtime: &dyn crate::retention::Runtime) -> IvyResult<IvyJson> {
    let string = try_js_string(value)?;
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
    if !milliseconds.is_finite() || normalize(runtime.iso_date(milliseconds)?) != normalize(string)
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

pub fn catalog_page(
    store: &Store,
    records: &mut [IvyJson],
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
    records: &mut [IvyJson],
    identity: &IvyJson,
    key: impl Fn(&IvyJson) -> IvyResult<JsString>,
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
    let keys = records
        .iter()
        .map(|record| {
            let key = key(record)?;
            Ok((bytes(key.clone()), key))
        })
        .collect::<IvyResult<Vec<_>>>()?;
    let mut keyed = keys
        .into_iter()
        .zip(
            records
                .iter_mut()
                .map(|record| std::mem::replace(record, IvyJson::Null)),
        )
        .collect::<Vec<_>>();
    keyed.sort_by(|a, b| a.0.0.cmp(&b.0.0));
    let mut keys = Vec::with_capacity(records.len());
    for (record, (key, value)) in records.iter_mut().zip(keyed) {
        *record = value;
        keys.push(key);
    }
    let after = bytes(after);
    let remaining = records
        .iter()
        .enumerate()
        .filter(|(index, _)| keys[*index].0 > after)
        .collect::<Vec<_>>();
    let mut items = vec![];
    let mut used = 128;
    let mut boundary = None;
    for (index, record) in &remaining {
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
        boundary = Some(*index);
    }
    let next = if !items.is_empty() && remaining.len() > items.len() {
        Some(store.cursor(
            identity,
            &IvyJson::String(keys[boundary.unwrap()].1.clone()),
        )?)
    } else {
        None
    };
    Ok(page(items, next))
}
