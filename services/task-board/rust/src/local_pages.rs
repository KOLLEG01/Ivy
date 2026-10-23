//! Internal journal queries over current SQLite heads. Locale ordering is
//! supplied by the host owner; this module does not select a fallback locale.
use crate::{
    category_projection::CategoryLocale,
    local_journal::{LocalJournal, count_column, require},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_store::sqlite::error;
use std::cmp::Ordering;

struct Row {
    id: Json,
    parent: Json,
    revision: u64,
    value: Json,
}
fn string(value: &Json) -> IvyResult<&JsString> {
    value.as_js_string().ok_or_else(IvyError::internal)
}
fn field<'a>(row: &'a Row, path: &JsString) -> Option<&'a Json> {
    if path == &JsString::from("object.id") {
        return Some(&row.id);
    }
    if path == &JsString::from("object.parentId") {
        return Some(&row.parent);
    }
    let parts = path.0.strip_prefix(&[100, 97, 116, 97, 58, 47])?;
    let mut value = &row.value;
    for part in parts.split(|unit| *unit == 47) {
        value = match value {
            Json::Object(_) => value.get_key(&JsString(part.to_vec()))?,
            Json::Array(values) => {
                // Query selectors address JSON array elements, never inherited
                // language properties or coercion hooks.
                let text = String::from_utf16(part).ok()?;
                let index = text.parse::<usize>().ok()?;
                if index.to_string() != text {
                    return None;
                }
                values.get(index)?
            }
            _ => return None,
        };
    }
    Some(value)
}
fn same(left: &Json, right: &Json) -> IvyResult<bool> {
    Ok(canonical(left, MANAGEMENT_FRAME_BYTES)? == canonical(right, MANAGEMENT_FRAME_BYTES)?)
}
fn matches(row: &Row, predicate: &Json) -> IvyResult<bool> {
    let op = predicate.required("op")?.as_str()?;
    match op.as_str() {
        "and" | "or" => {
            let conjunction = op == "and";
            for item in predicate
                .required("args")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                if matches(row, item)? != conjunction {
                    return Ok(!conjunction);
                }
            }
            Ok(conjunction)
        }
        "not" => Ok(!matches(row, predicate.required("arg")?)?),
        _ => {
            let value = field(row, string(predicate.required("field")?)?);
            match op.as_str() {
                "eq" | "ne" => match value {
                    Some(value) => Ok(same(value, predicate.required("value")?)? == (op == "eq")),
                    None => Ok(false),
                },
                "in" => {
                    if let (Some(value), Some(items)) =
                        (value, predicate.get("value").and_then(Json::as_array))
                    {
                        for item in items {
                            if same(value, item)? {
                                return Ok(true);
                            }
                        }
                    }
                    Ok(false)
                }
                "contains" => Ok(
                    match (
                        value.and_then(Json::as_js_string),
                        predicate.get("value").and_then(Json::as_js_string),
                    ) {
                        (Some(value), Some(needle)) => {
                            needle.0.is_empty()
                                || value.0.windows(needle.0.len()).any(|part| part == needle.0)
                        }
                        _ => false,
                    },
                ),
                "isNull" => Ok(value.is_none_or(Json::is_null)),
                _ => Ok(false),
            }
        }
    }
}
enum Scalar {
    Number(f64, JsString),
    Text(JsString),
}
impl Scalar {
    fn read(value: Option<&Json>) -> IvyResult<Self> {
        Ok(match value {
            None | Some(Json::Null) => Self::Text(JsString::default()),
            Some(Json::String(value)) => Self::Text(value.clone()),
            Some(value @ Json::Number(number)) => {
                Self::Number(*number, canonical(value, MANAGEMENT_FRAME_BYTES)?.into())
            }
            Some(Json::Bool(value)) => Self::Text(if *value { "true" } else { "false" }.into()),
            // Native query definitions select scalar order fields. Do not add
            // a general object/array-to-language-string execution model here.
            _ => return Err(IvyError::internal()),
        })
    }
    fn text(&self) -> &JsString {
        match self {
            Self::Number(_, text) | Self::Text(text) => text,
        }
    }
    fn compare(&self, right: &Self, locale: &impl CategoryLocale) -> Ordering {
        match (self, right) {
            (Self::Number(left, _), Self::Number(right, _)) => {
                left.partial_cmp(right).unwrap_or(Ordering::Equal)
            }
            _ => locale.compare_default(self.text(), right.text()),
        }
    }
}
fn cursor_error() -> IvyError {
    IvyError::new("invalid_cursor", "The local workflow cursor is invalid.")
}
fn offset(options: &Json, length: usize) -> IvyResult<usize> {
    let cursor = options.get("cursor").filter(|value| !value.is_null());
    let Some(cursor) = cursor else { return Ok(0) };
    let cursor = string(cursor)?;
    if cursor.0.is_empty() {
        return Ok(0);
    }
    // These internal cursors contain the unsigned decimal offset emitted below.
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor.to_utf8().map_err(|_| cursor_error())?)
        .map_err(|_| cursor_error())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| cursor_error())?;
    let value = text.parse::<usize>().map_err(|_| cursor_error())?;
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) || value > length {
        return Err(cursor_error());
    }
    Ok(value)
}

impl LocalJournal<'_> {
    /// Page current local records using internal native query definitions.
    /// Root/version filters and projections belong to the remote store; local
    /// records retain their existing key-only scope and empty projection map.
    pub fn page(&self, key: &str, options: &Json, locale: &impl CategoryLocale) -> IvyResult<Json> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT d.id,d.parent_id,d.current_revision,r.value_json FROM documents d
             JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision
             WHERE d.key=? ORDER BY d.id",
            )
            .map_err(error)?;
        let rows = statement
            .query_map([key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    count_column(row, 2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(error)?;
        let default_order = Json::Array(vec![Json::object([
            ("field", "object.id".into()),
            ("direction", "asc".into()),
        ])]);
        let orders = options
            .get("orderBy")
            .filter(|value| !value.is_null())
            .unwrap_or(&default_order)
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let mut selected = Vec::new();
        for row in rows {
            let (id, parent, revision, encoded) = row.map_err(error)?;
            let row = Row {
                id: id.into(),
                parent: parent.map(Json::from).unwrap_or(Json::Null),
                revision,
                value: parse(&encoded, MANAGEMENT_FRAME_BYTES)?,
            };
            if let Some(predicate) = options.get("where").filter(|value| !value.is_null()) {
                if !matches(&row, predicate)? {
                    continue;
                }
            }
            let keys = orders
                .iter()
                .map(|order| Scalar::read(field(&row, string(order.required("field")?)?)))
                .collect::<IvyResult<Vec<_>>>()?;
            selected.push((row, keys));
        }
        selected.sort_by(|(left, left_keys), (right, right_keys)| {
            for ((left, right), order) in left_keys.iter().zip(right_keys).zip(orders) {
                let result = left.compare(right, locale);
                if result != Ordering::Equal {
                    return if order.get("direction") == Some(&Json::from("desc")) {
                        result.reverse()
                    } else {
                        result
                    };
                }
            }
            locale.compare_default(
                left.id.as_js_string().unwrap(),
                right.id.as_js_string().unwrap(),
            )
        });
        let offset = offset(options, selected.len())?;
        let limit = options
            .get("limit")
            .filter(|value| !value.is_null())
            .map(|value| value.as_f64().ok_or_else(IvyError::internal))
            .transpose()?
            .unwrap_or(50.0);
        require(
            limit.is_finite() && limit >= 0.0 && limit.fract() == 0.0,
            "invalid_arguments",
            "The internal workflow page limit must be a nonnegative integer.",
        )?;
        let end = ((offset as f64 + limit).min(selected.len() as f64)) as usize;
        let items = selected[offset..end]
            .iter()
            .map(|(row, _)| {
                Json::object([
                    ("objectId", row.id.clone()),
                    ("revision", Json::Number(row.revision as f64)),
                    ("values", Json::Object(vec![])),
                ])
            })
            .collect();
        Ok(Json::object([
            ("items", Json::Array(items)),
            (
                "nextCursor",
                if end < selected.len() {
                    URL_SAFE_NO_PAD.encode(end.to_string()).into()
                } else {
                    Json::Null
                },
            ),
        ]))
    }
}
