use super::JsString;
use crate::IvyError;

#[derive(Clone, Copy)]
pub(super) enum Diagnostic {
    Unexpected,
    UnterminatedString,
    ObjectStart,
    ObjectKey,
    Colon,
    ArrayDelimiter,
    ObjectDelimiter,
    Minus,
    Fraction,
    Exponent,
    Trailing,
    Escape,
    Control,
    Unicode,
}

pub(super) fn error(source: &JsString, at: usize, diagnostic: Diagnostic) -> IvyError {
    use Diagnostic::*;
    let text = match diagnostic {
        Unexpected => {
            return IvyError::new("SyntaxError", "Invalid JSON syntax.")
                .with_js_message(unexpected(source, at));
        }
        UnterminatedString => "Unterminated string in JSON",
        ObjectStart => "Expected property name or '}' in JSON",
        ObjectKey => "Expected double-quoted property name in JSON",
        Colon => "Expected ':' after property name in JSON",
        ArrayDelimiter => "Expected ',' or ']' after array element in JSON",
        ObjectDelimiter => "Expected ',' or '}' after property value in JSON",
        Minus => "No number after minus sign in JSON",
        Fraction => "Unterminated fractional number in JSON",
        Exponent => "Exponent part is missing a number in JSON",
        Trailing => "Unexpected non-whitespace character after JSON",
        Escape => "Bad escaped character in JSON",
        Control => "Bad control character in string literal in JSON",
        Unicode => "Bad Unicode escape in JSON",
    };
    IvyError::new("SyntaxError", "Invalid JSON syntax.")
        .with_js_message(position(source, at, text).into())
}

fn position(source: &JsString, at: usize, text: &str) -> String {
    // Positions/columns count UTF-16 units. Only CR/LF are JSON line breaks;
    // CRLF counts once, including when the error itself is the following LF.
    let (mut line, mut start, mut cursor) = (1, 0, 0);
    while cursor < at {
        if source.0[cursor] == 13 && cursor + 1 < at && source.0[cursor + 1] == 10 {
            cursor += 1;
        }
        if matches!(source.0[cursor], 10 | 13) {
            line += 1;
            start = cursor + 1;
        }
        cursor += 1;
    }
    format!(
        "{text} at position {at} (line {line} column {})",
        at - start + 1
    )
}

fn unexpected(source: &JsString, at: usize) -> JsString {
    let Some(&unit) = source.0.get(at) else {
        return "Unexpected end of JSON input".into();
    };
    if unit == 45 || (48..=57).contains(&unit) {
        return position(source, at, "Unexpected number in JSON").into();
    }
    if unit == 34 {
        return position(source, at, "Unexpected string in JSON").into();
    }
    if ["[object Object]", "undefined", "Infinity", "NaN"]
        .iter()
        .any(|s| source.0.iter().copied().eq(s.encode_utf16()))
    {
        let mut result = JsString::from("\"");
        result.0.extend_from_slice(&source.0);
        result.0.extend("\" is not valid JSON".encode_utf16());
        return result;
    }
    // Node 24's context is sliced by UTF-16 code unit, even across a surrogate
    // pair. Keep those exact units in wire_message instead of replacing them.
    let (start, end, before, after) = if source.0.len() < 21 {
        (0, source.0.len(), "", "")
    } else if at < 10 {
        (0, at + 10, "", "...")
    } else if at < source.0.len() - 10 {
        (at - 10, at + 10, "...", "...")
    } else {
        (at - 10, source.0.len(), "...", "")
    };
    let mut result = JsString::from("Unexpected token '");
    result.0.push(unit);
    result.0.extend(format!("', {before}\"").encode_utf16());
    result.0.extend_from_slice(&source.0[start..end]);
    result
        .0
        .extend(format!("\"{after} is not valid JSON").encode_utf16());
    result
}
