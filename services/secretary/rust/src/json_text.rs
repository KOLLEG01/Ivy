//! Original string interpolation for values in retained JSON evidence.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::stringify};

pub fn text(value: Option<&IvyJson>) -> IvyResult<JsString> {
    Ok(match value {
        None => "undefined".into(),
        Some(IvyJson::String(value)) => value.clone(),
        Some(IvyJson::Array(values)) => {
            let mut output = JsString::default();
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.0.push(u16::from(b','));
                }
                if !value.is_null() {
                    output.0.extend(text(Some(value))?.0);
                }
            }
            output
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
