//! WhatsApp message wrappers, typed lengths and persisted BufferJSON bytes.
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, json::parse};
use std::path::Path;

pub fn content(encoded: &str) -> IvyResult<Json> {
    let message = parse(encoded, usize::MAX).map_err(|_| IvyError::internal())?;
    if message.as_object().is_none() {
        return Err(IvyError::internal());
    }
    let mut content = message.get("message").cloned().unwrap_or(Json::Null);
    for _ in 0..5 {
        let inner = [
            "ephemeralMessage",
            "viewOnceMessage",
            "documentWithCaptionMessage",
            "viewOnceMessageV2",
            "viewOnceMessageV2Extension",
            "editedMessage",
            "associatedChildMessage",
            "groupStatusMessage",
            "groupStatusMessageV2",
        ]
        .iter()
        .find_map(|key| content.get(key).filter(|value| value.as_object().is_some()));
        match inner {
            Some(value) => content = value.get("message").cloned().unwrap_or(Json::Null),
            None => break,
        }
    }
    Ok(content)
}
pub fn text(content: &Json, command: bool) -> IvyResult<JsString> {
    let mut values = vec![
        content.get("conversation"),
        content
            .get("extendedTextMessage")
            .and_then(|value| value.get("text")),
    ];
    if !command {
        values.extend(
            ["imageMessage", "documentMessage"]
                .map(|key| content.get(key).and_then(|value| value.get("caption"))),
        );
    }
    match values.into_iter().flatten().find(|value| !value.is_null()) {
        Some(value) => value.as_js_string().cloned().ok_or_else(IvyError::internal),
        None => Ok("".into()),
    }
}
pub fn number(value: Option<&Json>) -> IvyResult<f64> {
    match value {
        None | Some(Json::Null) => Ok(f64::NAN),
        Some(Json::Number(value)) => Ok(*value),
        // Protobuf uint64 lengths use decimal strings in the stored JSON.
        Some(Json::String(value)) => value.to_utf8()?.parse().map_err(|_| IvyError::internal()),
        _ => Err(IvyError::internal()),
    }
}
pub fn bytes(value: &Json) -> IvyResult<Vec<u8>> {
    if let Json::String(value) = value {
        let value = value.to_utf8()?;
        return STANDARD
            .decode(&value)
            .or_else(|_| STANDARD_NO_PAD.decode(&value))
            .or_else(|_| URL_SAFE.decode(&value))
            .or_else(|_| URL_SAFE_NO_PAD.decode(&value))
            .map_err(|_| IvyError::internal());
    }
    if value.get("type") == Some(&Json::from("Buffer")) {
        return bytes(value.required("data")?);
    }
    let byte = |value: &Json| {
        value
            .as_f64()
            .filter(|number| number.fract() == 0.0 && (0.0..=255.0).contains(number))
            .map(|value| value as u8)
            .ok_or_else(IvyError::internal)
    };
    if let Some(values) = value.as_array() {
        return values.iter().map(byte).collect();
    }
    if let Some(values) = value.as_object() {
        let mut values = values
            .iter()
            .map(|(index, value)| {
                Ok((
                    index
                        .to_utf8()?
                        .parse::<usize>()
                        .map_err(|_| IvyError::internal())?,
                    byte(value)?,
                ))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        values.sort_unstable_by_key(|(index, _)| *index);
        if values
            .iter()
            .enumerate()
            .all(|(expected, (index, _))| expected == *index)
        {
            return Ok(values.into_iter().map(|(_, value)| value).collect());
        }
    }
    Err(IvyError::internal())
}
pub fn filename(value: Option<&Json>) -> IvyResult<JsString> {
    let value = value
        .filter(|value| !value.is_null())
        .map(Json::as_str)
        .transpose()?
        .unwrap_or_else(|| "attachment".to_owned());
    let leaf = Path::new(&value)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(if value == ".." { ".." } else { "attachment" });
    let clean: String = leaf
        .chars()
        .map(|value| {
            if value.is_alphanumeric() || "._ -".contains(value) {
                value
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    Ok(if clean.is_empty() {
        "attachment".into()
    } else {
        clean.into()
    })
}
