//! JSON pointer access and URI-fragment decoding for schema documents.
use crate::{IvyError, IvyJson, IvyResult, JsString};

pub(crate) fn property<'a>(value: &'a IvyJson, key: &str) -> Option<&'a IvyJson> {
    match value {
        IvyJson::Object(_) => value.get(key),
        IvyJson::Array(items) => key
            .parse::<usize>()
            .ok()
            .filter(|index| index.to_string() == key)
            .and_then(|index| items.get(index)),
        _ => None,
    }
}

pub(crate) fn schema(value: &IvyJson) -> IvyResult<&IvyJson> {
    match value {
        IvyJson::Object(_) | IvyJson::Bool(_) => Ok(value),
        _ => Err(IvyError::new("internal_error", "Expected a JSON schema.")),
    }
}

/// fast-uri preserves invalid percent triplets as literal percent signs, while
/// decodeURIComponent still rejects escaped bytes that are not valid UTF-8.
pub(crate) fn pointer_key(text: &str) -> IvyResult<String> {
    let input = text.as_bytes();
    let mut bytes = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            if let (Some(a), Some(b)) = (
                (input[i + 1] as char).to_digit(16),
                (input[i + 2] as char).to_digit(16),
            ) {
                bytes.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        bytes.push(input[i]);
        i += 1;
    }
    Ok(String::from_utf8(bytes)
        .map_err(|_| IvyError::internal())?
        .replace("~1", "/")
        .replace("~0", "~"))
}

/// URI fragments are distinct from admission's literal JSON pointers. Both AJV
/// entry paths replace unpaired UTF-16 surrogates with U+FFFD. Resolving a $ref
/// rejects malformed percent escapes; direct getSchema lookup escapes them.
pub(crate) fn compiler_pointer_parts(
    fragment: &JsString,
    strict_percent: bool,
) -> IvyResult<Vec<String>> {
    if fragment.0.first() != Some(&47) {
        return Err(IvyError::internal());
    }
    fragment.0[1..]
        .split(|c| *c == 47)
        .map(|part| compiler_pointer_key(part, strict_percent))
        .collect()
}

pub(crate) fn compiler_pointer_key(part: &[u16], strict_percent: bool) -> IvyResult<String> {
    let text = String::from_utf16_lossy(part);
    let input = text.as_bytes();
    let mut index = 0;
    while strict_percent && index < input.len() {
        if input[index] == b'%' {
            let hex = |offset: usize| {
                input
                    .get(offset)
                    .and_then(|byte| (*byte as char).to_digit(16))
            };
            hex(index + 1).ok_or_else(IvyError::internal)?;
            hex(index + 2).ok_or_else(IvyError::internal)?;
            index += 3;
        } else {
            index += 1;
        }
    }
    pointer_key(&text)
}
