//! Retained JSON value-to-text conversion for cursors, headers and cookies.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::stringify};

pub(crate) fn value_text(value: Option<&IvyJson>) -> IvyResult<JsString> {
    Ok(match value {
        None => "undefined".into(),
        Some(IvyJson::String(value)) => value.clone(),
        Some(IvyJson::Array(values)) => {
            let mut text = JsString::default();
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    text.0.push(u16::from(b','));
                }
                if !value.is_null() {
                    text.0.extend(value_text(Some(value))?.0);
                }
            }
            text
        }
        Some(value @ IvyJson::Object(_)) => {
            if value.get("toString").is_some() {
                return Err(IvyError::internal());
            }
            "[object Object]".into()
        }
        Some(value) => stringify(value, usize::MAX)?.into(),
    })
}
