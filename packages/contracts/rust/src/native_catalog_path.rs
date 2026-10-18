use crate::{IvyError, IvyResult, JsString, checked_native_contract::native_versions};

fn separator(c: u16, windows: bool) -> bool {
    c == 47 || windows && c == 92
}
fn letter(c: u16) -> bool {
    (65..=90).contains(&c) || (97..=122).contains(&c)
}
fn reserved(value: &[u16]) -> bool {
    let value: Vec<_> = value
        .iter()
        .map(|c| if (97..=122).contains(c) { c - 32 } else { *c })
        .collect();
    ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|s| value == s.encode_utf16().collect::<Vec<_>>())
        || value.len() == 4
            && (value.starts_with(&[67, 79, 77]) || value.starts_with(&[76, 80, 84]))
            && ((49..=57).contains(&value[3]) || [185, 178, 179].contains(&value[3]))
}
// This local adapter preserves the pinned Node path.join contract. It does not
// resolve the filesystem, case-fold paths or apply native project membership rules.
fn join(root: &JsString, parts: &[&str], windows: bool) -> JsString {
    let slash = if windows { 92 } else { 47 };
    let mut first = root.0.clone();
    let mut path = first.clone();
    for part in parts {
        if !path.is_empty() {
            path.push(slash);
        }
        path.extend(part.encode_utf16());
        if first.is_empty() {
            first = part.encode_utf16().collect();
        }
    }
    if windows {
        let unc = first.len() > 2
            && separator(first[0], true)
            && separator(first[1], true)
            && !separator(first[2], true);
        if !unc {
            let count = path.iter().take_while(|c| separator(**c, true)).count();
            if count >= 2 {
                path.drain(..count);
                path.insert(0, 92);
            }
        }
        if path.split(|c| *c == 92).any(|part| {
            part.iter()
                .position(|c| *c == 58)
                .is_some_and(|i| reserved(&part[..i]))
        }) {
            for c in &mut path {
                if *c == 47 {
                    *c = 92;
                }
            }
            return JsString(path);
        }
    }
    let mut absolute = path.first().is_some_and(|c| separator(*c, windows));
    let mut start = usize::from(absolute);
    let mut device = Vec::new();
    if windows {
        if absolute && path.get(1).is_some_and(|c| separator(*c, true)) {
            start = 0;
            let mut i = 2;
            let server_start = i;
            while i < path.len() && !separator(path[i], true) {
                i += 1;
            }
            if i < path.len() && i > server_start {
                let server = &path[server_start..i];
                while i < path.len() && separator(path[i], true) {
                    i += 1;
                }
                let share_start = i;
                while i < path.len() && !separator(path[i], true) {
                    i += 1;
                }
                if i > share_start {
                    device.extend_from_slice(&[92, 92]);
                    device.extend_from_slice(server);
                    if server == [46] || server == [63] {
                        start = 4;
                        if let Some(colon) = path.iter().position(|c| *c == 58).filter(|i| *i >= 4)
                        {
                            if reserved(&path[4..colon]) {
                                device = vec![92, 92, 63, 92];
                                device.extend_from_slice(&path[4..=colon]);
                                start = colon + 1;
                            }
                        }
                    } else {
                        device.push(92);
                        device.extend_from_slice(&path[share_start..i]);
                        start = i;
                    }
                }
            }
        } else if !absolute {
            if let Some(colon) = path.iter().position(|c| *c == 58) {
                if colon == 1 && letter(path[0]) {
                    device.extend_from_slice(&path[..2]);
                    start = 2;
                    if path.get(2).is_some_and(|c| separator(*c, true)) {
                        absolute = true;
                        start = 3;
                    }
                } else if reserved(&path[..colon]) {
                    device.extend_from_slice(&path[..=colon]);
                    start = colon + 1;
                }
            }
        }
    }
    let mut components: Vec<&[u16]> = Vec::new();
    for part in path[start..].split(|c| separator(*c, windows)) {
        if part.is_empty() || part == [46] {
            continue;
        }
        if part == [46, 46] {
            if components.last().is_some_and(|v| *v != [46, 46]) {
                components.pop();
            } else if !absolute {
                components.push(part);
            }
        } else {
            components.push(part);
        }
    }
    let mut tail = Vec::new();
    for (i, part) in components.iter().enumerate() {
        if i > 0 {
            tail.push(slash);
        }
        tail.extend_from_slice(part);
    }
    if tail.is_empty() && !absolute {
        tail.push(46);
    }
    if !tail.is_empty() && path.last().is_some_and(|c| separator(*c, windows)) {
        tail.push(slash);
    }
    if windows && !absolute && device.is_empty() && path.contains(&58) {
        if tail.len() >= 2 && letter(tail[0]) && tail[1] == 58
            || path
                .iter()
                .enumerate()
                .any(|(i, c)| *c == 58 && (i + 1 == path.len() || separator(path[i + 1], true)))
        {
            let mut out = vec![46, 92];
            out.extend(tail);
            return JsString(out);
        }
    }
    let reserved_root = windows
        && path
            .iter()
            .position(|c| *c == 58)
            .is_some_and(|i| reserved(&path[..i]));
    let mut out = if reserved_root { vec![46, 92] } else { vec![] };
    out.extend(device);
    if absolute && !reserved_root {
        out.push(slash);
    }
    out.extend(tail);
    JsString(out)
}
fn os_path(value: &JsString) -> std::path::PathBuf {
    // Node passes filesystem strings through UTF-8 even on Windows. Preserve
    // the original UTF-16 in the returned path, but replace lone surrogates here.
    String::from_utf16_lossy(&value.0).into()
}
/// Select an existing platform catalog, otherwise return the default path.
/// Platform/architecture defaults describe this process; path syntax is always
/// that of the owning OS, independent of the requested catalog platform.
pub fn native_catalog_path(
    root: &JsString,
    version: &JsString,
    platform: Option<&JsString>,
    architecture: Option<&JsString>,
) -> IvyResult<JsString> {
    let default_platform = JsString::from(match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        v => v,
    });
    let default_architecture = JsString::from(match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        v => v,
    });
    let platform = platform.unwrap_or(&default_platform);
    let architecture = architecture.unwrap_or(&default_architecture);
    let valid = |value: &JsString| {
        !value.0.is_empty()
            && value
                .0
                .iter()
                .all(|c| (97..=122).contains(c) || (48..=57).contains(c))
    };
    if !native_versions()?.contains(version) || !valid(platform) || !valid(architecture) {
        return Err(IvyError::new(
            "native_version_unsupported",
            "Supported native platform and version required.",
        ));
    }
    let directory = join(
        root,
        &["specs", "native", &format!("codex-{}", version.to_utf8()?)],
        cfg!(windows),
    );
    let selected = join(
        &directory,
        &[&format!(
            "catalog.{}-{}.json",
            platform.to_utf8()?,
            architecture.to_utf8()?
        )],
        cfg!(windows),
    );
    Ok(if os_path(&selected).exists() {
        selected
    } else {
        join(&directory, &["catalog.json"], cfg!(windows))
    })
}
