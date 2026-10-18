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
