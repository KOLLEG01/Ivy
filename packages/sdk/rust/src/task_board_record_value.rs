//! Typed JSON access used only by stored-record migrations.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome, json};
use std::borrow::Cow;

pub(super) type Result<T> = IvyResult<T>;
pub(super) type Value<'a> = Option<Cow<'a, Json>>;

pub(super) fn invalid() -> IvyError {
    IvyError::new(
        "internal_error",
        "Stored TaskBoard record has an invalid field type.",
    )
    .with_outcome(Outcome::Unknown)
}
pub(super) fn borrow(value: &Json) -> Value<'_> {
    Some(Cow::Borrowed(value))
}
pub(super) fn owned<'a>(value: Json) -> Value<'a> {
    Some(Cow::Owned(value))
}
pub(super) fn retain(value: Value<'_>) -> Json {
    value.map(Cow::into_owned).unwrap_or(Json::Null)
}
pub(super) fn missing<'a>() -> Value<'a> {
    None
}
pub(super) fn null<'a>() -> Value<'a> {
    owned(Json::Null)
}
pub(super) fn num<'a>(value: f64) -> Value<'a> {
    owned(Json::Number(value))
}
pub(super) fn js<'a>(value: JsString) -> Value<'a> {
    owned(Json::String(value))
}
pub(super) fn str<'a>(value: &str) -> Value<'a> {
    owned(Json::from(value))
}
pub(super) fn string(value: &str) -> Json {
    Json::from(value)
}
pub(super) fn string_js(value: JsString) -> Json {
    Json::String(value)
}
pub(super) fn nullish(value: &Value<'_>) -> bool {
    value.as_deref().is_none_or(Json::is_null)
}
pub(super) fn is_missing(value: &Value<'_>) -> bool {
    value.is_none()
}
pub(super) fn present(value: &Value<'_>) -> bool {
    !nullish(value)
}
pub(super) fn is_text(value: &Value<'_>, text: &str) -> bool {
    value.as_deref().is_some_and(|value| value.is_str(text))
}
pub(super) fn field<'a>(value: &Value<'a>, key: &str) -> Result<Value<'a>> {
    match value {
        Some(Cow::Borrowed(value)) if value.as_object().is_some() => {
            Ok(value.get(key).map(Cow::Borrowed))
        }
        Some(Cow::Owned(value)) if value.as_object().is_some() => {
            Ok(value.get(key).cloned().map(Cow::Owned))
        }
        _ => Err(invalid()),
    }
}
pub(super) fn copy_object(value: &Value<'_>) -> Result<Json> {
    value
        .as_deref()
        .filter(|v| v.as_object().is_some())
        .cloned()
        .ok_or_else(invalid)
}
pub(super) fn object_rest(value: &Value<'_>, removed: &str) -> Result<Json> {
    let mut object = copy_object(value)?;
    object.remove(removed);
    Ok(object)
}
pub(super) fn elements<'a>(value: &Value<'a>) -> Option<Vec<Value<'a>>> {
    match value {
        Some(Cow::Borrowed(value)) => value
            .as_array()
            .map(|items| items.iter().map(borrow).collect()),
        Some(Cow::Owned(value)) => value
            .as_array()
            .map(|items| items.iter().cloned().map(owned).collect()),
        None => None,
    }
}
pub(super) fn array_items<'a>(value: &Value<'a>) -> Result<Vec<Value<'a>>> {
    elements(value).ok_or_else(invalid)
}
pub(super) fn has_items(value: &Value<'_>) -> Result<bool> {
    if nullish(value) {
        return Ok(false);
    }
    Ok(!array_items(value)?.is_empty())
}
pub(super) fn array(values: Vec<Value<'_>>) -> Json {
    Json::Array(values.into_iter().map(retain).collect())
}
pub(super) fn empty() -> Json {
    Json::Object(Vec::new())
}
pub(super) fn put(object: &mut Json, key: &str, value: Value<'_>) {
    if let Some(value) = value {
        object.insert(key, value.into_owned());
    } else {
        object.remove(key);
    }
}
pub(super) fn insert(object: &mut Json, key: &str, value: Json) {
    object.insert(key, value);
}
pub(super) fn finish(object: Json) -> Json {
    object
}
pub(super) fn object<'a>(fields: impl IntoIterator<Item = (&'static str, Value<'a>)>) -> Json {
    let mut object = empty();
    for (key, value) in fields {
        put(&mut object, key, value);
    }
    object
}
pub(super) fn text(value: &Value<'_>) -> Result<JsString> {
    match value.as_deref() {
        Some(Json::String(text)) => Ok(text.clone()),
        Some(Json::Number(number)) if number.is_finite() => Ok(number.to_string().into()),
        _ => Err(invalid()),
    }
}
pub(super) fn number(value: &Value<'_>) -> Result<f64> {
    value
        .as_deref()
        .and_then(Json::as_f64)
        .filter(|n| n.is_finite())
        .ok_or_else(invalid)
}
pub(super) fn serialized(value: &Value<'_>) -> Result<Option<String>> {
    value
        .as_deref()
        .map(|value| json::stringify(value, usize::MAX))
        .transpose()
}
pub(super) fn equal(left: &Value<'_>, right: &Value<'_>) -> bool {
    match (left.as_deref(), right.as_deref()) {
        (None, None) => true,
        (Some(left), Some(right)) => ivy_contracts::builtin_schema::json_equal(left, right),
        _ => false,
    }
}
pub(super) fn concat(parts: impl IntoIterator<Item = JsString>) -> JsString {
    JsString(parts.into_iter().flat_map(|part| part.0).collect())
}
