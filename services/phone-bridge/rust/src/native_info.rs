//! Original SIP INFO grammar. The admitted call owns sequence suppression.
use rsipstack::sip::{Header, Request};

pub(crate) fn digit(request: &Request) -> Option<char> {
    let content_type = request.headers.iter().find_map(|header| match header {
        Header::ContentType(value) => Some(value.value()),
        _ => None,
    })?;
    let media_type = content_type.split(';').next()?.trim();
    let body = String::from_utf8_lossy(&request.body);
    // System.String.Length counts UTF-16 units, rather than UTF-8 bytes.
    if body.encode_utf16().take(1025).count() > 1024 {
        return None;
    }
    if media_type.eq_ignore_ascii_case("application/dtmf") {
        let mut chars = body.trim().chars();
        let digit = normalized(chars.next()?)?;
        return chars.next().is_none().then_some(digit);
    }
    if !media_type.eq_ignore_ascii_case("application/dtmf-relay") {
        return None;
    }
    // The original multiline regex permits whitespace across newlines, takes
    // the first valid Signal line, and recognizes only LF as an anchor.
    for start in std::iter::once(0).chain(body.match_indices('\n').map(|(i, _)| i + 1)) {
        let line = body[start..].trim_start();
        let Some(name) = line.get(..6) else {
            continue;
        };
        if !name.eq_ignore_ascii_case("signal") {
            continue;
        }
        let rest = line[6..].trim_start();
        let Some(rest) = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':')) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(value) = rest.chars().next() else {
            continue;
        };
        let Some(digit) = normalized(value) else {
            continue;
        };
        let rest = &rest[value.len_utf8()..];
        if rest.trim().is_empty()
            || rest
                .chars()
                .take_while(|c| c.is_whitespace())
                .any(|c| c == '\n')
        {
            return Some(digit);
        }
    }
    None
}
fn normalized(value: char) -> Option<char> {
    let value = value.to_ascii_uppercase();
    matches!(value, '0'..='9' | '*' | '#' | 'A'..='D').then_some(value)
}
