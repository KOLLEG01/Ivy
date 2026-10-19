//! The pinned message wrapper/text and BufferJSON byte boundaries.
use crate::main_context::truthy;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{number_from_string, parse, try_js_string},
    re2::Re2Pattern,
};
use std::sync::OnceLock;

pub fn content(encoded: &str) -> IvyResult<Json> {
    let message = parse(encoded, usize::MAX).map_err(|_| IvyError::internal())?;
    if message.is_null() {
        return Err(IvyError::internal());
    }
    let mut content = message.get("message").cloned().unwrap_or(Json::Null);
    if !truthy(&content) {
        return Ok(Json::Null);
    }
    // normalizeMessageContent in the package-lock-pinned protocol dependency.
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
        .find_map(|key| content.get(key).filter(|v| truthy(v)));
        match inner {
            Some(value) => content = value.get("message").cloned().unwrap_or(Json::Null),
            None => break,
        }
    }
    Ok(content)
}

pub fn text(content: &Json, command: bool) -> Json {
    content
        .get("conversation")
        .filter(|v| !v.is_null())
        .or_else(|| {
            content
                .get("extendedTextMessage")
                .and_then(|v| v.get("text"))
                .filter(|v| !v.is_null())
        })
        .cloned()
        .or_else(|| {
            (!command)
                .then(|| {
                    ["imageMessage", "documentMessage"].iter().find_map(|key| {
                        content
                            .get(key)
                            .and_then(|v| v.get("caption"))
                            .filter(|v| !v.is_null())
                            .cloned()
                    })
                })
                .flatten()
        })
        .unwrap_or_else(|| "".into())
}

pub fn number(value: Option<&Json>) -> IvyResult<f64> {
    Ok(match value {
        None => f64::NAN,
        Some(Json::Null) => 0.0,
        Some(Json::Number(n)) => *n,
        Some(Json::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(v)
            if numeric_buffer(v)
                || (v.get("type") == Some(&"Buffer".into())
                    && matches!(v.get("data"), Some(Json::String(_)))) =>
        {
            number_from_string(&String::from_utf8_lossy(&bytes(v)?).as_ref().into())
        }
        Some(v) => number_from_string(&try_js_string(v)?),
    })
}
fn numeric_buffer(value: &Json) -> bool {
    value
        .as_object()
        .filter(|v| !v.is_empty())
        .is_some_and(|values| {
            values.iter().all(|(k, v)| {
                let key = k.trim_start();
                let units = if matches!(key.0.first(), Some(43 | 45)) {
                    &key.0[1..]
                } else {
                    &key.0
                };
                units.first().is_some_and(|u| (48..=57).contains(u)) && matches!(v, Json::Number(_))
            })
        })
}

/// Buffer.from(text, 'base64') accepts URL alphabet, ignored characters,
/// missing padding and nonzero discarded bits. It stops at the first '='.
fn base64(value: &JsString) -> Vec<u8> {
    let mut bits = 0_u32;
    let mut count = 0;
    let mut output = Vec::new();
    for unit in &value.0 {
        let value = match (*unit & 255) as u8 {
            b'=' => break,
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'a'..=b'z' => c - b'a' + 26,
            c @ b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            output.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    output
}

pub fn bytes(value: &Json) -> IvyResult<Vec<u8>> {
    if let Json::String(value) = value {
        return Ok(base64(value));
    }
    if value.get("type") == Some(&"Buffer".into()) {
        return bytes(value.required("data")?);
    }
    let convert = |values: &[Json]| -> IvyResult<Vec<u8>> {
        values
            .iter()
            .map(|v| {
                let n = number(Some(v))?;
                Ok(if n.is_finite() {
                    n.trunc().rem_euclid(256.0) as u8
                } else {
                    0
                })
            })
            .collect()
    };
    if let Some(values) = value.as_array() {
        return convert(values);
    }
    // BufferJSON.reviver also admits numeric-key objects as Uint8Array data.
    if numeric_buffer(value) {
        let values = ivy_contracts::json::object_entries(value)
            .into_iter()
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>();
        return convert(&values);
    }
    Err(IvyError::internal())
}

pub fn filename(value: Option<&Json>) -> IvyResult<JsString> {
    let name = match value.filter(|v| !v.is_null()) {
        None => JsString::from("attachment"),
        Some(name) => name
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?,
    };
    let separator = |u: u16| u == 47 || (cfg!(windows) && u == 92);
    let drive = cfg!(windows)
        && name.0.len() >= 2
        && name.0[1] == 58
        && ((65..=90).contains(&name.0[0]) || (97..=122).contains(&name.0[0]));
    let start = if drive { 2 } else { 0 };
    let end = name
        .0
        .iter()
        .rposition(|u| !separator(*u))
        .map_or(start, |i| i + 1);
    let start = name.0[start..end]
        .iter()
        .rposition(|u| separator(*u))
        .map_or(start, |i| start + i + 1);
    static ALLOWED: OnceLock<IvyResult<Re2Pattern>> = OnceLock::new();
    let allowed = ALLOWED
        .get_or_init(|| Re2Pattern::compile(&r"^[\p{L}\p{N}._ -]$".into()))
        .as_ref()
        .map_err(Clone::clone)?;
    let mut clean = Vec::new();
    for point in char::decode_utf16(name.0[start..end].iter().copied()) {
        match point {
            Ok(c) => {
                let mut buffer = [0; 2];
                let units = c.encode_utf16(&mut buffer);
                if allowed.is_match(&JsString(units.to_vec())) {
                    clean.extend_from_slice(units);
                } else {
                    clean.push(95);
                }
            }
            Err(_) => clean.push(95),
        }
    }
    clean.truncate(120);
    Ok(if clean.is_empty() {
        "attachment".into()
    } else {
        JsString(clean)
    })
}
