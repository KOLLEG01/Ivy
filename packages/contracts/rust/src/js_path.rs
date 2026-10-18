//! Lexical Node path operations over retained UTF-16 units. Host selection,
//! filesystem access and workspace reservation policy belong to the caller.
use crate::JsString;

fn separator(unit: u16) -> bool {
    unit == 47 || unit == 92
}
fn letter(unit: u16) -> bool {
    (65..=90).contains(&unit) || (97..=122).contains(&unit)
}
fn reserved(path: &[u16], colon: Option<usize>) -> bool {
    // The pinned implementation passes -1 to slice when no colon is present.
    let end = colon.unwrap_or_else(|| path.len().saturating_sub(1));
    let name = JsString(path[..end].to_vec()).to_uppercase();
    ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|value| name == JsString::from(*value))
        || (name.0.len() == 4
            && (name.0.starts_with(&[67, 79, 77]) || name.0.starts_with(&[76, 80, 84]))
            && ((49..=57).contains(&name.0[3]) || [185, 178, 179].contains(&name.0[3])))
}
fn unc(server: &[u16], share: &[u16]) -> Vec<u16> {
    let mut result = vec![92, 92];
    result.extend_from_slice(server);
    result.push(92);
    result.extend_from_slice(share);
    result
}

/// Node path.win32.normalize, including relative/drive/UNC/device roots,
/// trailing separators and colon/reserved-name handling. It does not resolve
/// against process/drive cwd, strip extended prefixes or change letter case.
pub fn normalize_win32(value: &JsString) -> JsString {
    let path = &value.0;
    if path.is_empty() {
        return JsString::from(".");
    }
    if path.len() == 1 {
        return if path[0] == 47 {
            JsString(vec![92])
        } else {
            value.clone()
        };
    }
    let colon = path.iter().position(|unit| *unit == 58);
    let mut root_end = 0;
    let mut device = None;
    let mut absolute = false;
    if separator(path[0]) {
        absolute = true;
        if separator(path[1]) {
            let mut end = 2;
            while end < path.len() && !separator(path[end]) {
                end += 1;
            }
            if end > 2 && end < path.len() {
                let server = &path[2..end];
                while end < path.len() && separator(path[end]) {
                    end += 1;
                }
                if end < path.len() {
                    let start = end;
                    while end < path.len() && !separator(path[end]) {
                        end += 1;
                    }
                    if server == [46] || server == [63] {
                        let mut root = vec![92, 92];
                        root.extend_from_slice(server);
                        device = Some(root);
                        root_end = 4;
                        let candidate = colon
                            .filter(|index| *index >= 4)
                            .map(|index| &path[4..=index])
                            .unwrap_or(&[]);
                        if reserved(candidate, candidate.len().checked_sub(1)) {
                            let mut name = vec![92, 92, 63, 92];
                            name.extend_from_slice(candidate);
                            device = Some(name);
                            root_end = 4 + candidate.len();
                        }
                    } else if end == path.len() {
                        let mut root = unc(server, &path[start..]);
                        root.push(92);
                        return JsString(root);
                    } else {
                        device = Some(unc(server, &path[start..end]));
                        root_end = end;
                    }
                }
            }
        } else {
            root_end = 1;
        }
    } else if let Some(colon) = colon.filter(|index| *index > 0) {
        if letter(path[0]) && colon == 1 {
            device = Some(path[..2].to_vec());
            root_end = 2;
            if path.len() > 2 && separator(path[2]) {
                absolute = true;
                root_end = 3;
            }
        } else if reserved(path, Some(colon)) {
            device = Some(path[..=colon].to_vec());
            root_end = colon + 1;
        }
    }
    let mut components: Vec<&[u16]> = Vec::new();
    for part in path[root_end..].split(|unit| separator(*unit)) {
        if part.is_empty() || part == [46] {
            continue;
        }
        if part == [46, 46] {
            if components.last().is_some_and(|last| *last != [46, 46]) {
                components.pop();
            } else if !absolute {
                components.push(part);
            }
        } else {
            components.push(part);
        }
    }
    let mut tail = Vec::new();
    for (index, part) in components.iter().enumerate() {
        if index > 0 {
            tail.push(92);
        }
        tail.extend_from_slice(part);
    }
    if tail.is_empty() && !absolute {
        tail.push(46);
    }
    if !tail.is_empty() && separator(*path.last().unwrap()) {
        tail.push(92);
    }
    if !absolute
        && device.is_none()
        && colon.is_some()
        && ((tail.len() >= 2 && letter(tail[0]) && tail[1] == 58)
            || path.iter().enumerate().any(|(index, unit)| {
                *unit == 58 && (index + 1 == path.len() || separator(path[index + 1]))
            }))
    {
        let mut result = vec![46, 92];
        result.extend(tail);
        return JsString(result);
    }
    let mut result = if reserved(path, colon) {
        let mut result = vec![46, 92];
        result.extend(device.unwrap_or_default());
        result
    } else {
        let mut result = device.unwrap_or_default();
        if absolute {
            result.push(92);
        }
        result
    };
    result.extend(tail);
    JsString(result)
}
