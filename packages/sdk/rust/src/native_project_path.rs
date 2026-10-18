use crate::{IvyError, IvyResult, JsString};

fn separator(c: u16) -> bool {
    c == 47 || c == 92
}
fn letter(c: u16) -> bool {
    (65..=90).contains(&c) || (97..=122).contains(&c)
}
pub(crate) fn windows_path(path: &[u16]) -> bool {
    path.starts_with(&[92, 92])
        || (path.len() >= 3 && letter(path[0]) && path[1] == 58 && separator(path[2]))
}
pub(crate) fn strip_extended(path: &[u16]) -> Vec<u16> {
    let mut path = if path.len() >= 8
        && path[..8]
            .iter()
            .zip([92, 92, 63, 92, 85, 78, 67, 92])
            .all(|(a, b)| ascii_upper(*a) == b)
    {
        let mut value = vec![92, 92];
        value.extend_from_slice(&path[8..]);
        value
    } else {
        path.to_vec()
    };
    if path.starts_with(&[92, 92, 63, 92]) {
        path.drain(..4);
    }
    path
}
fn ascii_upper(c: u16) -> u16 {
    if (97..=122).contains(&c) { c - 32 } else { c }
}
/// ECMAScript default lowercase retains unpaired UTF-16 units as non-cased boundaries.
pub(crate) fn lowercase(value: &[u16]) -> JsString {
    let mut output = Vec::new();
    let mut segment = String::new();
    for c in char::decode_utf16(value.iter().copied()) {
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
fn reserved_device(value: &[u16]) -> bool {
    let value: Vec<_> = value.iter().copied().map(ascii_upper).collect();
    ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|s| value == s.encode_utf16().collect::<Vec<_>>())
        || (value.len() == 4
            && (value.starts_with(&[67, 79, 77]) || value.starts_with(&[76, 80, 84]))
            && ((49..=57).contains(&value[3]) || [185, 178, 179].contains(&value[3])))
}
/// Only absolute paths reach this adapter; no process cwd, drive cwd or filesystem is consulted.
pub(crate) fn path_key(value: &JsString) -> IvyResult<JsString> {
    let path = strip_extended(&value.0);
    let windows = windows_path(&path);
    if !windows && !path.starts_with(&[47]) {
        return Err(IvyError::new(
            "native_project_path_invalid",
            "Native project paths must be absolute on their owning host.",
        ));
    }
    let mut device = Vec::new();
    let mut start = 1;
    if windows {
        start = 0;
        if path.len() >= 3 && letter(path[0]) && path[1] == 58 {
            device.extend_from_slice(&path[..2]);
            start = 3;
        } else if path.len() > 1 && separator(path[0]) && separator(path[1]) {
            let mut j = 2;
            let first_start = j;
            while j < path.len() && !separator(path[j]) {
                j += 1;
            }
            if j < path.len() && j > first_start {
                let first = &path[first_start..j];
                while j < path.len() && separator(path[j]) {
                    j += 1;
                }
                if j < path.len() {
                    let second_start = j;
                    while j < path.len() && !separator(path[j]) {
                        j += 1;
                    }
                    if first == [46] || first == [63] {
                        device.extend_from_slice(&[92, 92]);
                        device.extend_from_slice(first);
                        start = 4;
                        if let Some(colon) = path.iter().position(|c| *c == 58).filter(|i| *i >= 4)
                        {
                            let possible = &path[4..colon];
                            if reserved_device(possible) {
                                device = vec![92, 92, 63, 92];
                                device.extend_from_slice(&path[4..=colon]);
                                start = colon + 1;
                            }
                        }
                    } else {
                        device.extend_from_slice(&[92, 92]);
                        device.extend_from_slice(first);
                        device.push(92);
                        device.extend_from_slice(&path[second_start..j]);
                        start = j;
                    }
                }
            }
        }
    }
    let mut components = Vec::<&[u16]>::new();
    for part in path[start..].split(|c| *c == 47 || (windows && *c == 92)) {
        if part.is_empty() || part == [46] {
            continue;
        }
        if part == [46, 46] {
            components.pop();
        } else {
            components.push(part);
        }
    }
    let slash = if windows { 92 } else { 47 };
    let mut normalized = device;
    normalized.push(slash);
    for (i, part) in components.iter().enumerate() {
        if i > 0 {
            normalized.push(slash);
        }
        normalized.extend_from_slice(part);
    }
    while normalized.last().is_some_and(|c| separator(*c)) {
        normalized.pop();
    }
    if normalized.is_empty() {
        normalized.push(47);
    }
    if windows {
        let mut value = lowercase(&normalized);
        for c in &mut value.0 {
            if *c == 92 {
                *c = 47;
            }
        }
        Ok(value)
    } else {
        Ok(JsString(normalized))
    }
}
pub(crate) fn contains(root: &[u16], cwd: &[u16]) -> bool {
    cwd == root
        || cwd.starts_with(root) && (root.last() == Some(&47) || cwd.get(root.len()) == Some(&47))
}
