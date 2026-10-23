//! Small JSON accessors shared by persistence and mutation code.
pub(crate) use ivy_contracts::IvyJson as Value;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
pub(crate) fn invalid(message: impl Into<JsString>) -> IvyError {
    IvyError::new_js("invalid_arguments".into(), message.into())
}
pub(crate) fn field(value: &Json, key: &str) -> IvyResult<Json> {
    Ok(value.get(key).cloned().unwrap_or(Json::Null))
}
pub(crate) fn has(value: &Json, key: &str) -> IvyResult<bool> {
    Ok(value.get(key).is_some())
}
pub(crate) fn nullish(value: &Json) -> bool {
    value.is_null()
}
pub(crate) fn fields(value: &Json) -> Vec<(JsString, Json)> {
    value.as_object().map(|v| v.to_vec()).unwrap_or_default()
}
pub(crate) fn object(fields: impl IntoIterator<Item = (JsString, Json)>) -> Json {
    Json::object(fields)
}
pub(crate) fn array(values: impl IntoIterator<Item = Json>) -> Json {
    Json::Array(values.into_iter().collect())
}
pub(crate) fn items(value: &Json) -> IvyResult<Vec<Json>> {
    value
        .as_array()
        .map(<[Json]>::to_vec)
        .ok_or_else(|| invalid("Expected an array."))
}
pub(crate) fn at(value: &Json, index: usize) -> IvyResult<Json> {
    Ok(value
        .as_array()
        .ok_or_else(|| invalid("Expected an array."))?
        .get(index)
        .cloned()
        .unwrap_or(Json::Null))
}
pub(crate) fn length(value: &Json) -> IvyResult<Json> {
    Ok(Json::Number(
        value
            .as_array()
            .ok_or_else(|| invalid("Expected an array."))?
            .len() as f64,
    ))
}
pub(crate) fn string(value: &Json) -> IvyResult<JsString> {
    value
        .as_js_string()
        .cloned()
        .ok_or_else(|| invalid("Expected a string."))
}
pub(crate) fn number(value: &Json) -> IvyResult<f64> {
    value
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| invalid("Expected a finite number."))
}
pub(crate) fn same_key(left: &Json, right: &Json) -> bool {
    left == right
}
pub(crate) fn decoded_comment<E: From<IvyError>>(value: &Json) -> Result<Json, E> {
    Ok(value.clone())
}
pub(crate) fn present(value: &Json) -> bool {
    !value.is_null()
}
pub(crate) fn is_true(value: &Json) -> bool {
    value == &Json::Bool(true)
}
