//! Optional JSON fields used by retained operation receipts.
use ivy_contracts::{IvyError, IvyJson as Json, JsString};
pub(super) fn type_error<E: From<IvyError>>(message: impl Into<JsString>) -> E {
    IvyError::new_js("invalid_arguments".into(), message.into()).into()
}
pub(super) fn field<'a, E: From<IvyError>>(
    value: Option<&'a Json>,
    key: &str,
) -> Result<Option<&'a Json>, E> {
    Ok(value.and_then(|v| v.get(key)))
}
pub(super) fn has<E: From<IvyError>>(value: Option<&Json>, key: &str) -> Result<bool, E> {
    Ok(value.is_some_and(|v| v.get(key).is_some()))
}
pub(super) fn same(a: Option<&Json>, b: Option<&Json>) -> bool {
    a == b
}
pub(super) fn insert(target: &mut Json, key: &str, value: Option<&Json>) {
    if let Some(value) = value {
        target.insert(key, value.clone());
    }
}
