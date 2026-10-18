use super::{JsString, escaped_byte, unreserved};

pub(super) struct IpLiteral {
    pub host: JsString,
    pub authority: JsString,
}

pub(super) fn ipv4(value: &JsString) -> bool {
    let Ok(value) = String::from_utf16(&value.0) else {
        return false;
    };
    ipv4_text(&value)
}
fn ipv4_text(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|c| c.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
                && part.parse::<u8>().is_ok()
        })
}
fn zone(value: &str) -> bool {
    let units: Vec<_> = value.encode_utf16().collect();
    let mut i = 0;
    while i < units.len() {
        if unreserved(units[i]) {
            i += 1;
        } else if escaped_byte(&units[i..]).is_some() {
            i += 3;
        } else {
            return false;
        }
    }
    !units.is_empty()
}
pub(super) fn ip_literal(value: &JsString) -> Option<IpLiteral> {
    let value = String::from_utf16(&value.0).ok()?;
    let bracketed = value.starts_with('[') && value.ends_with(']');
    if (value.starts_with('[') || value.ends_with(']')) && !bracketed {
        return None;
    }
    let body = if bracketed {
        &value[1..value.len() - 1]
    } else {
        &value
    };
    if bracketed && (body.starts_with('v') || body.starts_with('V')) {
        if let Some((version, address)) = body[1..].split_once('.') {
            if !version.is_empty()
                && version.bytes().all(|c| c.is_ascii_hexdigit())
                && !address.is_empty()
                && address
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:".contains(&c))
            {
                let authority = format!("[{}]", body.to_ascii_lowercase());
                return Some(IpLiteral {
                    host: authority.clone().into(),
                    authority: authority.into(),
                });
            }
        }
        return None;
    }
    if body.bytes().filter(|c| *c == b':').count() < 2 {
        return None;
    }
    let (address, zone_id) = if let Some((address, suffix)) = body.split_once('%') {
        let zone_id = suffix.strip_prefix("25").unwrap_or(suffix);
        if !zone(zone_id) {
            return None;
        }
        (address, Some(zone_id))
    } else {
        (body, None)
    };
    let compressed = address.contains("::");
    let (left, right) = if let Some((left, right)) = address.split_once("::") {
        if right.contains("::") {
            return None;
        }
        (left, right)
    } else {
        (address, "")
    };
    let left_parts: Vec<_> = if left.is_empty() && compressed {
        vec![]
    } else {
        left.split(':').collect()
    };
    let right_parts: Vec<_> = if right.is_empty() {
        vec![]
    } else {
        right.split(':').collect()
    };
    let parts: Vec<_> = left_parts.iter().chain(&right_parts).copied().collect();
    let mut normalized = Vec::new();
    let mut count = 0;
    for (index, part) in parts.iter().enumerate() {
        if part.contains('.') {
            if index + 1 != parts.len() || (compressed && right.is_empty()) || !ipv4_text(part) {
                return None;
            }
            count += 2;
            normalized.push((*part).to_string());
        } else {
            if part.is_empty() || part.len() > 4 || !part.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            count += 1;
            normalized.push(format!("{:x}", u16::from_str_radix(part, 16).ok()?));
        }
    }
    if (!compressed && count != 8) || (compressed && count >= 8) {
        return None;
    }
    if compressed {
        normalized.splice(
            left_parts.len()..left_parts.len(),
            std::iter::repeat_n("0".to_string(), 8 - count),
        );
    }
    let mut best = (0, 0);
    let mut index = 0;
    while index < normalized.len() {
        if normalized[index] != "0" {
            index += 1;
            continue;
        }
        let start = index;
        while index < normalized.len() && normalized[index] == "0" {
            index += 1;
        }
        if index - start > best.1 {
            best = (start, index - start);
        }
    }
    let address = if best.1 >= 2 {
        format!(
            "{}::{}",
            normalized[..best.0].join(":"),
            normalized[best.0 + best.1..].join(":")
        )
    } else {
        normalized.join(":")
    };
    let host = format!(
        "{address}{}",
        zone_id.map_or(String::new(), |z| format!("%{z}"))
    );
    let authority = format!(
        "[{address}{}]",
        zone_id.map_or(String::new(), |z| format!("%25{z}"))
    );
    Some(IpLiteral {
        host: host.into(),
        authority: authority.into(),
    })
}

pub(super) fn escape_delimiters(value: &JsString, ip: bool) -> JsString {
    let mut output = Vec::new();
    for unit in &value.0 {
        let escape = match *unit {
            64 => Some("%40"),
            47 => Some("%2F"),
            63 => Some("%3F"),
            35 => Some("%23"),
            58 if !ip => Some("%3A"),
            _ => None,
        };
        if let Some(escape) = escape {
            output.extend(escape.encode_utf16());
        } else {
            output.push(*unit);
        }
    }
    JsString(output)
}
