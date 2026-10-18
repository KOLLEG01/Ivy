use super::{
    Component, escaped_byte, hex, normalize_component, normalize_percent, remove_dot_segments,
    serialize_component, unreserved,
};
use crate::{IvyError, IvyResult, JsString};
#[path = "schema_uri_host.rs"]
mod host;

#[derive(Clone, Default)]
struct Reference {
    scheme: Option<JsString>,
    userinfo: Option<JsString>,
    host: Option<JsString>,
    port: Option<JsString>,
    path: JsString,
    query: Option<JsString>,
    fragment: Option<JsString>,
}
impl Reference {
    fn authority(&self) -> bool {
        self.userinfo.is_some() || self.host.is_some() || self.port.is_some()
    }
}
fn invalid() -> IvyError {
    IvyError::new("registry_invalid", "Invalid or unsupported JSON schema.")
}
fn part(units: &[u16]) -> JsString {
    JsString(units.to_vec())
}
fn lower(input: &JsString) -> JsString {
    let mut output = Vec::new();
    let mut segment = String::new();
    for c in char::decode_utf16(input.0.iter().copied()) {
        match c {
            Ok(c) => segment.push(c),
            Err(e) => {
                output.extend(segment.to_lowercase().encode_utf16());
                segment.clear();
                output.push(e.unpaired_surrogate());
            }
        }
    }
    output.extend(segment.to_lowercase().encode_utf16());
    JsString(output)
}
fn scheme(input: &[u16]) -> IvyResult<JsString> {
    // The original resolver uses legacy unescape, including %uXXXX, for schemes.
    let mut decoded = Vec::new();
    let mut i = 0;
    while i < input.len() {
        if input[i..].starts_with(&[37, 117]) && input.len() - i >= 6 {
            if let Some(value) = input[i + 2..i + 6]
                .iter()
                .try_fold(0u16, |n, u| hex(*u).map(|v| n * 16 + v as u16))
            {
                decoded.push(value);
                i += 6;
                continue;
            }
        }
        if let Some(value) = escaped_byte(&input[i..]) {
            decoded.push(value as u16);
            i += 3;
        } else {
            decoded.push(input[i]);
            i += 1;
        }
    }
    if !decoded
        .first()
        .is_some_and(|c| matches!(c, 65..=90 | 97..=122))
        || !decoded
            .iter()
            .all(|c| matches!(c, 65..=90 | 97..=122 | 48..=57 | 43 | 45 | 46))
    {
        return Err(invalid());
    }
    Ok(lower(&JsString(decoded)))
}
fn check_percent(input: &JsString) -> IvyResult<()> {
    let mut i = 0;
    while i < input.0.len() {
        if input.0[i] == 37 {
            if escaped_byte(&input.0[i..]).is_none() {
                return Err(invalid());
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    Ok(())
}
fn parse(input: &JsString) -> IvyResult<Reference> {
    let units = &input.0;
    let mut out = Reference::default();
    let mut i = 0;
    if let Some(end) = units.iter().position(|c| matches!(c, 35 | 47 | 58 | 63)) {
        if end > 0 && units[end] == 58 {
            out.scheme = Some(scheme(&units[..end])?);
            i = end + 1;
        }
    }
    let region: Vec<_> = units[i..]
        .iter()
        .copied()
        .take_while(|c| matches!(c, 47 | 92 | 9 | 10 | 13))
        .collect();
    let stripped: Vec<_> = region
        .iter()
        .copied()
        .filter(|c| !matches!(c, 9 | 10 | 13))
        .collect();
    if stripped.len() >= 2 && (!stripped.starts_with(&[47, 47]) || stripped.len() != region.len()) {
        return Err(invalid());
    }
    if units[i..].starts_with(&[47, 47]) {
        i += 2;
        let end = i + units[i..]
            .iter()
            .position(|c| matches!(c, 35 | 47 | 63))
            .unwrap_or(units.len() - i);
        if units[i..end].contains(&92) {
            return Err(invalid());
        }
        if let Some(at) = units[i..end].iter().position(|c| *c == 64) {
            out.userinfo = Some(part(&units[i..i + at]));
            i += at + 1;
        }
        // A well-formed bracket capture takes precedence over a reg-name. On an
        // unmatched bracket, the original parser falls back to the reg-name arm.
        let bracket_end = (units.get(i) == Some(&91))
            .then(|| units[i + 1..end].iter().position(|c| *c == 93))
            .flatten()
            .filter(|n| *n > 0)
            .map(|n| i + n + 2);
        let host_end = bracket_end.unwrap_or_else(|| {
            i + units[i..end]
                .iter()
                .position(|c| *c == 58)
                .unwrap_or(end - i)
        });
        out.host = Some(part(&units[i..host_end]));
        i = host_end;
        if units.get(i) == Some(&58) {
            i += 1;
            let start = i;
            while units.get(i).is_some_and(|c| matches!(c, 48..=57)) {
                i += 1;
            }
            let digits = &units[start..i];
            out.port = Some(if digits.is_empty() {
                JsString::default()
            } else {
                let mut number = 0u32;
                for digit in digits {
                    number = number * 10 + (*digit - 48) as u32;
                    if number > 65535 {
                        return Err(invalid());
                    }
                }
                number.to_string().into()
            });
        }
    }
    let path_end = i + units[i..]
        .iter()
        .position(|c| matches!(c, 35 | 63))
        .unwrap_or(units.len() - i);
    out.path = part(&units[i..path_end]);
    i = path_end;
    if out.authority() && !out.path.0.is_empty() && out.path.0[0] != 47 {
        return Err(invalid());
    }
    if units.get(i) == Some(&63) {
        i += 1;
        let end = i + units[i..]
            .iter()
            .position(|c| *c == 35)
            .unwrap_or(units.len() - i);
        out.query = Some(part(&units[i..end]));
        i = end;
    }
    if units.get(i) == Some(&35) {
        i += 1;
        // JavaScript dot excludes line/paragraph separators even with the
        // resolver's explicit CR/LF alternative; the pattern is not end-anchored.
        let end = i + units[i..]
            .iter()
            .position(|c| matches!(c, 0x2028 | 0x2029))
            .unwrap_or(units.len() - i);
        out.fragment = Some(part(&units[i..end]));
    }
    for component in [&out.userinfo, &out.query, &out.fragment]
        .into_iter()
        .flatten()
    {
        check_percent(component)?;
    }
    check_percent(&out.path)?;
    if let Some(value) = &mut out.host {
        let bracketed = value.0.first() == Some(&91) && value.0.last() == Some(&93);
        if !bracketed {
            check_percent(value)?;
        }
        let ip = host::ip_literal(value);
        if (value.0.contains(&91) || value.0.contains(&93)) && (!bracketed || ip.is_none()) {
            return Err(invalid());
        }
        if let Some(ip) = ip {
            *value = ip.host;
        } else {
            *value = lower(value);
        }
        if units.contains(&37) {
            let ip = host::ip_literal(value).is_some() || host::ipv4(value);
            if !ip {
                *value = normalize_percent(value, true);
            }
            *value = host::escape_delimiters(value, ip);
        }
    }
    out.path = normalize_component(&out.path, Component::Path);
    out.query = out
        .query
        .map(|v| normalize_component(&v, Component::QueryOrFragment));
    out.fragment = out
        .fragment
        .map(|v| normalize_component(&v, Component::QueryOrFragment));
    Ok(out)
}

/// Resolve a schema resource reference using the compiler's URI semantics.
/// Invalid URIs become the compiler's registry_invalid error. Resource indexing
/// and duplicate-ID checks are separate; this does not compile a schema.
pub fn resolve(base: &JsString, relative: &JsString) -> IvyResult<JsString> {
    let base = parse(base)?;
    let relative = parse(relative)?;
    let mut target = relative.clone();
    if relative.scheme.is_some() {
        target.path = remove_dot_segments(&relative.path);
    } else {
        target.scheme = base.scheme.clone();
        if relative.authority() {
            target.path = remove_dot_segments(&relative.path);
        } else {
            target.userinfo = base.userinfo.clone();
            target.host = base.host.clone();
            target.port = base.port.clone();
            if relative.path.0.is_empty() {
                target.path = base.path.clone();
                if target.query.is_none() {
                    target.query = base.query.clone();
                }
            } else if relative.path.0.first() == Some(&47) {
                target.path = remove_dot_segments(&relative.path);
            } else {
                let mut merged = if base.authority() && base.path.0.is_empty() {
                    vec![47]
                } else {
                    base.path.0[..base
                        .path
                        .0
                        .iter()
                        .rposition(|c| *c == 47)
                        .map_or(0, |n| n + 1)]
                        .to_vec()
                };
                merged.extend(&relative.path.0);
                target.path = remove_dot_segments(&JsString(merged));
            }
        }
    }
    if target.scheme.as_ref().is_some_and(|v| {
        ["http", "https", "ws", "wss"]
            .iter()
            .any(|s| v == &JsString::from(*s))
    }) {
        if let Some(value) = &mut target.host {
            if !value.0.is_empty()
                && !host::ipv4(value)
                && host::ip_literal(value).is_none()
                && value
                    .0
                    .iter()
                    .any(|c| !matches!(c, 33|34|36|38..=46|59|61|95|96|97..=122|123|125|126))
            {
                let mut url = JsString::from("http://");
                url.0.extend(&value.0);
                match crate::whatwg_url::parse_url(&url) {
                    Ok(parsed) => *value = parsed.hostname.into(),
                    Err(_) if value.0.contains(&37) && value.0.iter().all(|c| *c < 128) => {}
                    Err(_) => return Err(invalid()),
                }
            }
        }
    }
    serialize(&target)
}

fn serialize(value: &Reference) -> IvyResult<JsString> {
    let mut out = Vec::new();
    if let Some(scheme) = &value.scheme {
        out.extend(&scheme.0);
        out.push(58);
    }
    if value.authority() {
        out.extend([47, 47]);
        if let Some(userinfo) = &value.userinfo {
            out.extend(serialize_component(userinfo, Component::Userinfo, false).0);
            out.push(64);
        }
        if let Some(value) = &value.host {
            let mut normalized = value.clone();
            if !host::ipv4(&normalized) {
                if host::ip_literal(&normalized).is_none() {
                    normalized = normalize_percent(&normalized, true);
                }
                normalized = host::ip_literal(&normalized).map_or_else(
                    || host::escape_delimiters(&normalized, false),
                    |v| v.authority,
                );
            }
            out.extend(normalized.0);
        }
        if let Some(port) = &value.port {
            out.push(58);
            out.extend(&port.0);
        }
        if !value.path.0.is_empty() && value.path.0[0] != 47 {
            out.push(47);
        }
    }
    let path = remove_dot_segments(&normalize_percent(&value.path, false));
    if !value.authority() && path.0.starts_with(&[47, 47]) {
        out.extend("/%2F".encode_utf16());
        out.extend(&path.0[2..]);
    } else {
        out.extend(path.0);
    }
    if let Some(query) = &value.query {
        out.push(63);
        out.extend(serialize_component(query, Component::QueryOrFragment, false).0);
    }
    if let Some(fragment) = &value.fragment {
        out.push(35);
        out.extend(serialize_component(fragment, Component::QueryOrFragment, false).0);
    }
    Ok(JsString(out))
}
