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
fn parse(input: &JsString, strict: bool) -> IvyResult<Reference> {
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
    if strict
        && stripped.len() >= 2
        && (!stripped.starts_with(&[47, 47]) || stripped.len() != region.len())
    {
        return Err(invalid());
    }
    if units[i..].starts_with(&[47, 47]) {
        i += 2;
        let end = i + units[i..]
            .iter()
            .position(|c| matches!(c, 35 | 47 | 63))
            .unwrap_or(units.len() - i);
        if strict && units[i..end].contains(&92) {
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
                let text: String = digits.iter().map(|digit| *digit as u8 as char).collect();
                if strict {
                    text.parse::<u16>()
                        .map_err(|_| invalid())?
                        .to_string()
                        .into()
                } else {
                    // Resource IDs retain their canonical numeric spelling;
                    // strict reference resolution admits only actual TCP ports.
                    let number = text.parse::<f64>().map_err(|_| invalid())?;
                    if !number.is_finite() {
                        return Err(invalid());
                    }
                    let mut buffer = ryu_js::Buffer::new();
                    let normalized = buffer.format(number);
                    if !normalized.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(invalid());
                    }
                    normalized.into()
                }
            });
        }
    }
    let path_end = i + units[i..]
        .iter()
        .position(|c| matches!(c, 35 | 63))
        .unwrap_or(units.len() - i);
    out.path = part(&units[i..path_end]);
    i = path_end;
    if strict && out.authority() && !out.path.0.is_empty() && out.path.0[0] != 47 {
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
        if strict {
            check_percent(component)?;
        }
    }
    if strict {
        check_percent(&out.path)?;
    }
    let urn = !strict && out.scheme.as_ref() == Some(&JsString::from("urn"));
    let domain = !strict && domain_scheme(&out);
    if let Some(value) = &mut out.host {
        let bracketed = value.0.first() == Some(&91) && value.0.last() == Some(&93);
        if strict && !bracketed {
            check_percent(value)?;
        }
        let ip = host::ip_literal(value);
        let malformed =
            (value.0.contains(&91) || value.0.contains(&93)) && (!bracketed || ip.is_none());
        if strict && malformed {
            return Err(invalid());
        }
        if let Some(ip) = ip {
            *value = ip.host;
        } else {
            *value = lower(value);
        }
        if domain && !malformed {
            let _ = canonicalize_domain(value);
        }
        if !urn && !malformed && units.contains(&37) {
            let ip = host::ip_literal(value).is_some() || host::ipv4(value);
            if !ip {
                *value = normalize_percent(value, true);
            }
            *value = host::escape_delimiters(value, ip);
        }
    }
    if !urn {
        out.path = normalize_component(&out.path, Component::Path);
        out.query = out
            .query
            .map(|v| normalize_component(&v, Component::QueryOrFragment));
        out.fragment = out
            .fragment
            .map(|v| normalize_component(&v, Component::QueryOrFragment));
    }
    Ok(out)
}

/// Resolve a schema resource reference using the compiler's URI semantics.
/// Invalid URIs become the compiler's registry_invalid error. Resource indexing
/// and duplicate-ID checks are separate; this does not compile a schema.
pub fn resolve(base: &JsString, relative: &JsString) -> IvyResult<JsString> {
    let base = parse(base, true)?;
    let relative = parse(relative, true)?;
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
    if domain_scheme(&target) {
        if let Some(value) = &mut target.host {
            if canonicalize_domain(value).is_err()
                && !(value.0.contains(&37) && value.0.iter().all(|c| *c < 128))
            {
                return Err(invalid());
            }
        }
    }
    serialize(&target, true)
}

fn domain_scheme(value: &Reference) -> bool {
    value.scheme.as_ref().is_some_and(|v| {
        ["http", "https", "ws", "wss"]
            .iter()
            .any(|s| v == &JsString::from(*s))
    })
}
fn canonicalize_domain(value: &mut JsString) -> IvyResult<()> {
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
        *value = crate::whatwg_url::parse_url(&url)
            .map_err(|_| invalid())?
            .hostname
            .into();
    }
    Ok(())
}

/// Normalize a compiler resource ID with the scheme handlers enabled and return
/// its document prefix, ending in `#`. Unlike resolve, parser diagnostics alone
/// do not reject the ID; serialization still enforces scheme/port/URN syntax.
pub fn full_path(id: &JsString) -> IvyResult<JsString> {
    let mut value = parse(id, false)?;
    let scheme = value.scheme.clone().unwrap_or_default();
    let mut skip_escape = false;
    if ["http", "https", "ws", "wss"]
        .iter()
        .any(|s| scheme == JsString::from(*s))
    {
        let secure = scheme == JsString::from("https") || scheme == JsString::from("wss");
        if value.port.as_ref().is_some_and(|p| {
            p.0.is_empty() || *p == JsString::from(if secure { "443" } else { "80" })
        }) {
            value.port = None;
        }
        if scheme == JsString::from("http") || scheme == JsString::from("https") {
            if value.path.0.is_empty() {
                value.path = "/".into();
            }
        } else {
            if value.path == JsString::from("/") {
                value.path = JsString::default();
            }
            if value.query.as_ref().is_some_and(|q| q.0.is_empty()) {
                value.query = None;
            }
            value.fragment = None;
        }
    } else if scheme == JsString::from("urn") {
        let Some(colon) = value.path.0.iter().position(|c| *c == 58) else {
            return Err(invalid());
        };
        let nid = &value.path.0[..colon];
        let nss = &value.path.0[colon + 1..];
        let letter = |c: u16| matches!(c,65..=90|97..=122|48..=57|0x17f|0x212a);
        if nid.is_empty()
            || nid.len() > 32
            || !letter(nid[0])
            || !nid.iter().all(|c| letter(*c) || *c == 45)
            || nss.is_empty()
        {
            return Err(invalid());
        }
        let mut i = 0;
        while i < nss.len() {
            if letter(nss[i]) || matches!(nss[i], 95 | 33 | 36 | 39..=47 | 58 | 59 | 61 | 64) {
                i += 1;
            } else if escaped_byte(&nss[i..]).is_some() {
                i += 3;
            } else {
                return Err(invalid());
            }
        }
        let mut path = lower(&part(nid));
        let nss = if path == JsString::from("uuid") {
            lower(&part(nss))
        } else {
            part(nss)
        };
        path.0.push(58);
        path.0.extend(nss.0);
        value.path = path;
        skip_escape = true;
    }
    let mut result = serialize(&value, skip_escape)?;
    if let Some(hash) = result.0.iter().position(|c| *c == 35) {
        result.0.truncate(hash);
    }
    result.0.push(35);
    Ok(result)
}

pub(crate) fn reference_parts(id: &JsString) -> IvyResult<(JsString, Option<JsString>)> {
    // The pinned serializer copies its input, so scheme serialization does not
    // erase the parsed fragment used by compiler pointer selection.
    Ok((full_path(id)?, parse(id, false)?.fragment))
}

fn serialize(value: &Reference, skip_escape: bool) -> IvyResult<JsString> {
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
            if !port.0.iter().all(|c| matches!(c, 48..=57)) {
                return Err(invalid());
            }
            out.push(58);
            out.extend(&port.0);
        }
        if !value.path.0.is_empty() && value.path.0[0] != 47 {
            out.push(47);
        }
    }
    let no_scheme = value.scheme.is_none() && !value.authority() && !skip_escape;
    let path = if skip_escape {
        normalize_percent(&value.path, false)
    } else {
        serialize_component(&value.path, Component::Path, no_scheme)
    };
    let mut path = remove_dot_segments(&path);
    if no_scheme {
        path = serialize_component(&path, Component::Path, true);
    }
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
