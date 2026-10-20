//! Node Buffer compatibility at existing JavaScript text/byte boundaries.
use crate::JsString;

/// Decode like Buffer.from(text, "base64url"), including its permissive legacy
/// path: both alphabets, ignored non-alphabet bytes, early padding and partial
/// final groups. Node truncates every UTF-16 unit to its low byte on that path.
/// This is not a strict token validator; admission policy belongs to callers.
pub fn base64url_decode(text: &JsString) -> Vec<u8> {
    let mut output = Vec::with_capacity(text.0.len() / 4 * 3);
    let mut pending = 0u16;
    let mut bits = 0;
    for unit in &text.0 {
        let digit = match *unit as u8 {
            b'A'..=b'Z' => (*unit as u8) - b'A',
            b'a'..=b'z' => (*unit as u8) - b'a' + 26,
            b'0'..=b'9' => (*unit as u8) - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        pending = (pending << 6) | u16::from(digit);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((pending >> bits) as u8);
            pending &= (1 << bits) - 1;
        }
    }
    output
}

/// Buffer.from(text, "base64url").toString("utf8"), retaining replacement
/// characters for malformed UTF-8 rather than rejecting the decoded bytes.
pub fn base64url_text(text: &JsString) -> JsString {
    JsString::from(String::from_utf8_lossy(&base64url_decode(text)).as_ref())
}

/// Accept exactly standard padded base64 whose decoded length fits the caller's
/// bound, as in Buffer.from(text, "base64").toString("base64") == text.
/// Rejected input is never decoded or allocated. The empty string is canonical.
pub fn canonical_base64_decode(text: &JsString, maximum_bytes: usize) -> Option<Vec<u8>> {
    let units = &text.0;
    if units.len() % 4 != 0 {
        return None;
    }
    let padding = if units.ends_with(&[61, 61]) {
        2
    } else {
        usize::from(units.ends_with(&[61]))
    };
    let length = units.len() / 4 * 3 - padding;
    if length > maximum_bytes {
        return None;
    }
    let body = &units[..units.len() - padding];
    if !body
        .iter()
        .all(|unit| matches!(unit, 65..=90 | 97..=122 | 48..=57 | 43 | 47))
    {
        return None;
    }
    if padding != 0 {
        let last = match *body.last()? {
            unit @ 65..=90 => unit - 65,
            unit @ 97..=122 => unit - 97 + 26,
            unit @ 48..=57 => unit - 48 + 52,
            43 => 62,
            47 => 63,
            _ => return None,
        };
        // Unused low bits must be zero for decode/re-encode to be identical.
        if last & if padding == 2 { 15 } else { 3 } != 0 {
            return None;
        }
    }
    Some(base64url_decode(text))
}
