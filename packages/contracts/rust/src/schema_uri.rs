//! URI component encoding used by schema resource resolution. These operations
//! preserve the compiler's distinction between encoded dots and path segments.
use crate::JsString;
#[path = "schema_uri_resolve.rs"]
mod resolution;
pub use resolution::resolve;

#[derive(Clone, Copy)]
pub enum Component {
    Path,
    QueryOrFragment,
    Userinfo,
}

fn hex(unit: u16) -> Option<u8> {
    match unit {
        48..=57 => Some((unit - 48) as u8),
        65..=70 => Some((unit - 55) as u8),
        97..=102 => Some((unit - 87) as u8),
        _ => None,
    }
}
fn escaped_byte(units: &[u16]) -> Option<u8> {
    if units.len() < 3 || units[0] != 37 {
        return None;
    }
    Some(hex(units[1])? * 16 + hex(units[2])?)
}
fn escape(output: &mut Vec<u16>, byte: u8) {
    const HEX: &[u8] = b"0123456789ABCDEF";
    output.extend([
        37,
        HEX[(byte >> 4) as usize] as u16,
        HEX[(byte & 15) as usize] as u16,
    ]);
}
fn unreserved(unit: u16) -> bool {
    matches!(unit, 48..=57 | 65..=90 | 97..=122 | 45 | 46 | 95 | 126)
}
fn literal(unit: u16, component: Component) -> bool {
    unreserved(unit)
        || matches!(unit, 33 | 36 | 38..=44 | 59 | 61 | 58)
        || (matches!(component, Component::Path | Component::QueryOrFragment)
            && matches!(unit, 47 | 64))
        || (matches!(component, Component::QueryOrFragment) && unit == 63)
}

/// Uppercase valid escapes, optionally decoding unreserved ASCII. Invalid
/// escapes and raw UTF-16 units remain unchanged; URI admission is separate.
pub fn normalize_percent(input: &JsString, decode_unreserved: bool) -> JsString {
    let mut output = Vec::with_capacity(input.0.len());
    let mut index = 0;
    while index < input.0.len() {
        if let Some(byte) = escaped_byte(&input.0[index..]) {
            if decode_unreserved && unreserved(byte as u16) {
                output.push(byte as u16);
            } else {
                escape(&mut output, byte);
            }
            index += 3;
        } else {
            output.push(input.0[index]);
            index += 1;
        }
    }
    JsString(output)
}

/// Normalize a parsed component. Encoded dots stay encoded in paths so they
/// cannot become traversal segments. Raw lone surrogates encode as U+FFFD.
pub fn normalize_component(input: &JsString, component: Component) -> JsString {
    encode(input, component, true, false)
}

/// Serialize a component without decoding escapes. A path without a scheme or
/// authority must escape colons in its first segment.
pub fn serialize_component(
    input: &JsString,
    component: Component,
    path_no_scheme: bool,
) -> JsString {
    encode(input, component, false, path_no_scheme)
}

fn encode(
    input: &JsString,
    component: Component,
    normalize: bool,
    path_no_scheme: bool,
) -> JsString {
    let mut output = Vec::with_capacity(input.0.len());
    let mut index = 0;
    let mut first_segment = true;
    while index < input.0.len() {
        if let Some(byte) = escaped_byte(&input.0[index..]) {
            if normalize
                && unreserved(byte as u16)
                && !(matches!(component, Component::Path) && byte == b'.')
            {
                output.push(byte as u16);
            } else {
                escape(&mut output, byte);
            }
            index += 3;
            continue;
        }
        let unit = input.0[index];
        index += 1;
        if unit == 47 {
            first_segment = false;
        }
        if literal(unit, component)
            && !(matches!(component, Component::Path)
                && path_no_scheme
                && first_segment
                && unit == 58)
        {
            output.push(unit);
        } else if unit < 128 {
            escape(&mut output, unit as u8);
        } else {
            let point = if (0xd800..=0xdbff).contains(&unit)
                && input
                    .0
                    .get(index)
                    .is_some_and(|v| (0xdc00..=0xdfff).contains(v))
            {
                let low = input.0[index];
                index += 1;
                0x10000 + ((unit as u32 - 0xd800) << 10) + low as u32 - 0xdc00
            } else {
                unit as u32
            };
            let character = char::from_u32(point).unwrap_or(char::REPLACEMENT_CHARACTER);
            let mut bytes = [0; 4];
            for byte in character.encode_utf8(&mut bytes).bytes() {
                escape(&mut output, byte);
            }
        }
    }
    JsString(output)
}

/// Remove literal dot segments with the compiler URI resolver's trailing-slash
/// behavior. Percent-encoded dots and raw UTF-16 units are not decoded.
pub fn remove_dot_segments(input: &JsString) -> JsString {
    let mut remaining = input.0.as_slice();
    let mut segments: Vec<&[u16]> = Vec::new();
    const SLASH: &[u16] = &[47];
    while !remaining.is_empty() {
        match remaining {
            [46] | [46, 46] => break,
            [47, 46] | [47, 47] => {
                segments.push(SLASH);
                break;
            }
            [47, 46, 46] => {
                segments.pop();
                segments.push(SLASH);
                break;
            }
            _ => {}
        }
        let skip = if remaining.starts_with(&[46, 46, 47]) {
            3
        } else if remaining.starts_with(&[46, 47]) || remaining.starts_with(&[47, 46, 47]) {
            2
        } else if remaining.starts_with(&[47, 46, 46, 47]) {
            segments.pop();
            3
        } else {
            let end = remaining
                .iter()
                .enumerate()
                .skip(1)
                .find_map(|(index, unit)| (*unit == 47).then_some(index))
                .unwrap_or(remaining.len());
            segments.push(&remaining[..end]);
            end
        };
        remaining = &remaining[skip..];
    }
    JsString(segments.concat())
}
