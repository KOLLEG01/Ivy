use ivy_contracts::{
    IvyJson as Json, JsString,
    error::{JavaScriptException, JavaScriptExceptionKind},
    json,
};

pub(super) type Result<T> = std::result::Result<T, JavaScriptException>;

pub(super) fn type_error(message: impl Into<JsString>) -> JavaScriptException {
    JavaScriptException {
        kind: JavaScriptExceptionKind::TypeError,
        message: message.into(),
    }
}
pub(super) fn field<'a>(value: Option<&'a Json>, key: &str) -> Result<Option<&'a Json>> {
    match value {
        None | Some(Json::Null) => Err(type_error(format!(
            "Cannot read properties of {} (reading '{key}')",
            if value.is_none() { "undefined" } else { "null" }
        ))),
        Some(value) => Ok(value.get(key)),
    }
}
pub(super) fn optional<'a>(value: Option<&'a Json>, key: &str) -> Result<Option<&'a Json>> {
    if nullish(value) {
        Ok(None)
    } else {
        field(value, key)
    }
}
pub(super) fn nullish(value: Option<&Json>) -> bool {
    matches!(value, None | Some(Json::Null))
}
pub(super) fn truthy(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null | Json::Bool(false)) => false,
        Some(Json::Number(value)) => *value != 0.0 && !value.is_nan(),
        Some(Json::String(value)) => !value.0.is_empty(),
        _ => true,
    }
}
pub(super) fn is_text(value: Option<&Json>, text: &str) -> bool {
    value.and_then(Json::as_js_string) == Some(&JsString::from(text))
}
pub(super) fn has_length(value: Option<&Json>) -> bool {
    match value {
        Some(Json::Array(values)) => !values.is_empty(),
        Some(Json::String(value)) => !value.0.is_empty(),
        Some(value) => truthy(value.get("length")),
        None => false,
    }
}
pub(super) fn string(value: Option<&Json>) -> Result<JsString> {
    match value {
        Some(value) => json::try_js_string(value)
            .map_err(|_| type_error("Cannot convert object to primitive value")),
        None => Ok("undefined".into()),
    }
}
pub(super) fn number(value: Option<&Json>) -> Result<f64> {
    Ok(match value {
        None => f64::NAN,
        Some(Json::Null) => 0.0,
        Some(Json::Bool(value)) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Some(Json::Number(value)) => *value,
        _ => json::number_from_string(&string(value)?),
    })
}
pub(super) fn concat(parts: impl IntoIterator<Item = JsString>) -> JsString {
    JsString(parts.into_iter().flat_map(|part| part.0).collect())
}
pub(super) fn spread(value: Option<&Json>) -> Json {
    match value {
        Some(Json::Object(_)) => value.unwrap().clone(),
        Some(Json::Array(values)) => Json::object(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value.clone())),
        ),
        Some(Json::String(value)) => Json::object(
            value
                .0
                .iter()
                .enumerate()
                .map(|(index, unit)| (index.to_string(), Json::String(JsString(vec![*unit])))),
        ),
        _ => Json::object::<&str>([]),
    }
}
pub(super) fn put(target: &mut Json, key: &str, value: Option<&Json>) {
    if let Some(value) = value {
        target.insert(key, value.clone());
    } else {
        target.remove(key);
    }
}
pub(super) fn strict_equal(left: Option<&Json>, right: Option<&Json>) -> bool {
    match (left, right) {
        (None, None) | (Some(Json::Null), Some(Json::Null)) => true,
        (Some(Json::Bool(a)), Some(Json::Bool(b))) => a == b,
        (Some(Json::Number(a)), Some(Json::Number(b))) => a == b,
        (Some(Json::String(a)), Some(Json::String(b))) => a == b,
        (Some(a @ (Json::Array(_) | Json::Object(_))), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}

// String iteration follows code points; object spread above follows code units.
pub(super) fn iterable(value: &Json, error: &str) -> Result<Vec<Json>> {
    match value {
        Json::Array(values) => Ok(values.clone()),
        Json::String(value) => {
            let mut result = Vec::new();
            let mut index = 0;
            while index < value.0.len() {
                let start = index;
                index += 1;
                if (0xd800..=0xdbff).contains(&value.0[start])
                    && index < value.0.len()
                    && (0xdc00..=0xdfff).contains(&value.0[index])
                {
                    index += 1;
                }
                result.push(Json::String(JsString(value.0[start..index].to_vec())));
            }
            Ok(result)
        }
        _ => Err(type_error(error)),
    }
}

pub(super) fn object_rest(value: Option<&Json>, binding: &str, removed: &str) -> Result<Json> {
    if nullish(value) {
        return Err(type_error(format!(
            "Cannot destructure property '{removed}' of '{binding}' as it is {}.",
            if value.is_none() { "undefined" } else { "null" }
        )));
    }
    let mut rest = spread(value);
    rest.remove(removed);
    Ok(rest)
}
