//! JavaScript property operations used by the persistent read call sites.
use ivy_contracts::{IvyJson as Json, JsString, json::js_string};
use ivy_sdk::task_comments::CommentException;

pub(super) fn type_error<E: From<CommentException>>(message: impl Into<JsString>) -> E {
    CommentException {
        kind: "TypeError",
        message: message.into(),
    }
    .into()
}
pub(super) fn field<'a, E: From<CommentException>>(
    value: Option<&'a Json>,
    key: &str,
) -> Result<Option<&'a Json>, E> {
    match value {
        None | Some(Json::Null) => Err(type_error(format!(
            "Cannot read properties of {} (reading '{key}')",
            if value.is_none() { "undefined" } else { "null" }
        ))),
        Some(value) => Ok(value.get(key)),
    }
}
pub(super) fn has<E: From<CommentException>>(value: Option<&Json>, key: &str) -> Result<bool, E> {
    if let Some(value @ (Json::Object(_) | Json::Array(_))) = value {
        return Ok(value.get(key).is_some());
    }
    let mut message = JsString::from(format!(
        "Cannot use 'in' operator to search for '{key}' in "
    ));
    message
        .0
        .extend(value.map(js_string).unwrap_or_else(|| "undefined".into()).0);
    Err(type_error(message))
}
pub(super) fn same(a: Option<&Json>, b: Option<&Json>) -> bool {
    match (a, b) {
        (None, None) | (Some(Json::Null), Some(Json::Null)) => true,
        (Some(Json::String(a)), Some(Json::String(b))) => a == b,
        (Some(Json::Number(a)), Some(Json::Number(b))) => a == b,
        (Some(Json::Bool(a)), Some(Json::Bool(b))) => a == b,
        (Some(a @ (Json::Object(_) | Json::Array(_))), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}
pub(super) fn insert(target: &mut Json, key: &str, value: Option<&Json>) {
    if let Some(value) = value {
        target.insert(key, value.clone());
    }
}
