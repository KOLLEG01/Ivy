//! Node-compatible lexical paths. No filesystem access or process cwd mutation.
//! UTF-16 is retained until an explicit native filesystem conversion.
use ivy_contracts::JsString;
use std::{collections::BTreeMap, io, path::{Path, PathBuf}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style { Windows, Posix }
impl Style {
    fn separator(self, c: u16) -> bool { c == 47 || self == Self::Windows && c == 92 }
    fn slash(self) -> u16 { if self == Self::Windows { 92 } else { 47 } }
    pub fn native() -> Self { if cfg!(windows) { Self::Windows } else { Self::Posix } }
}
fn letter(c: u16) -> bool { (65..=90).contains(&c) || (97..=122).contains(&c) }
fn lower(value: &[u16]) -> Vec<u16> {
    let mut output = Vec::new(); let mut segment = String::new();
    for c in char::decode_utf16(value.iter().copied()) {
        match c {
            Ok(c) => segment.push(c),
            Err(e) => { output.extend(segment.to_lowercase().encode_utf16()); segment.clear(); output.push(e.unpaired_surrogate()); }
        }
    }
    output.extend(segment.to_lowercase().encode_utf16()); output
}
fn reserved(value: &[u16]) -> bool {
    let upper: Vec<_> = value.iter().map(|c| if (97..=122).contains(c) { c - 32 } else { *c }).collect();
    ["CON", "PRN", "AUX", "NUL"].iter().any(|s| upper == s.encode_utf16().collect::<Vec<_>>())
        || upper.len() == 4 && (upper.starts_with(&[67,79,77]) || upper.starts_with(&[76,80,84]))
            && ((49..=57).contains(&upper[3]) || [185,178,179].contains(&upper[3]))
}
fn before_colon(value: &[u16]) -> &[u16] {
    // String.slice(0, -1) is observable in Node's reserved-name normalization.
    &value[..value.iter().position(|c| *c == 58).unwrap_or(value.len().saturating_sub(1))]
}
struct Root { end: usize, device: Vec<u16>, absolute: bool }
fn windows_root(path: &[u16], normalize: bool) -> Root {
    let mut root = Root { end: 0, device: Vec::new(), absolute: false };
    if path.is_empty() { return root; }
    if Style::Windows.separator(path[0]) {
        root.absolute = true;
        if path.len() == 1 || !Style::Windows.separator(path[1]) { root.end = 1; return root; }
        let mut end = 2;
        while end < path.len() && !Style::Windows.separator(path[end]) { end += 1; }
        if end == 2 || end == path.len() { return root; }
        let server = &path[2..end];
        while end < path.len() && Style::Windows.separator(path[end]) { end += 1; }
        let share = end;
        while end < path.len() && !Style::Windows.separator(path[end]) { end += 1; }
        if share == end { return root; }
        root.device.extend_from_slice(&[92,92]); root.device.extend_from_slice(server);
        if server == [46] || server == [63] {
            root.end = 4;
            if normalize {
                if let Some(colon) = path.iter().position(|c| *c == 58).filter(|i| *i >= 4) {
                    if reserved(&path[4..colon]) {
                        root.device = vec![92,92,63,92]; root.device.extend_from_slice(&path[4..=colon]); root.end = colon + 1;
                    }
                }
            }
        } else {
            root.device.push(92); root.device.extend_from_slice(&path[share..end]); root.end = end;
        }
    } else if path.len() >= 2 && letter(path[0]) && path[1] == 58 {
        root.device.extend_from_slice(&path[..2]); root.end = 2;
        if path.get(2).is_some_and(|c| Style::Windows.separator(*c)) { root.end = 3; root.absolute = true; }
    } else if normalize {
        if let Some(colon) = path.iter().position(|c| *c == 58).filter(|i| *i > 0) {
            if reserved(&path[..colon]) { root.device.extend_from_slice(&path[..=colon]); root.end = colon + 1; }
        }
    }
    root
}
fn tail(style: Style, path: &[u16], above_root: bool) -> Vec<u16> {
    let mut parts: Vec<&[u16]> = Vec::new();
    for part in path.split(|c| style.separator(*c)) {
        if part.is_empty() || part == [46] { continue; }
        if part == [46,46] {
            if parts.last().is_some_and(|last| *last != [46,46]) { parts.pop(); }
            else if above_root { parts.push(part); }
        } else { parts.push(part); }
    }
    parts.join(&style.slash())
}
pub fn is_absolute(style: Style, path: &JsString) -> bool {
    path.0.first().is_some_and(|c| style.separator(*c)) || style == Style::Windows && path.0.len() > 2
        && letter(path.0[0]) && path.0[1] == 58 && style.separator(path.0[2])
}
pub fn normalize(style: Style, path: &JsString) -> JsString {
    let path = &path.0;
    if path.is_empty() { return JsString::from("."); }
    if style == Style::Windows && path.len() == 1 { return JsString(vec![if path[0] == 47 { 92 } else { path[0] }]); }
    let root = if style == Style::Windows { windows_root(path, true) }
        else { Root {end: usize::from(path[0] == 47), device: Vec::new(), absolute: path[0] == 47} };
    let mut rest = tail(style, &path[root.end..], !root.absolute);
    if rest.is_empty() && !root.absolute { rest.push(46); }
    if !rest.is_empty() && style.separator(*path.last().unwrap()) { rest.push(style.slash()); }
    if style == Style::Windows && !root.absolute && root.device.is_empty() && path.contains(&58) {
        if rest.len() >= 2 && letter(rest[0]) && rest[1] == 58
            || path.iter().enumerate().any(|(i,c)| *c == 58 && (i + 1 == path.len() || style.separator(path[i+1]))) {
            let mut result = vec![46,92]; result.extend(rest); return JsString(result);
        }
    }
    let mut result = Vec::new();
    if style == Style::Windows && reserved(before_colon(path)) { result.extend_from_slice(&[46,92]); }
    result.extend(root.device);
    if root.absolute { result.push(style.slash()); }
    result.extend(rest); JsString(result)
}
pub fn join(style: Style, parts: &[JsString]) -> JsString {
    let parts: Vec<_> = parts.iter().filter(|part| !part.0.is_empty()).map(|part| part.0.as_slice()).collect();
    if parts.is_empty() { return JsString::from("."); }
    let mut joined = parts.join(&style.slash());
    if style == Style::Windows {
        let first = parts[0];
        let unc = first.len() > 2 && style.separator(first[0]) && style.separator(first[1]) && !style.separator(first[2]);
        if !unc {
            let count = joined.iter().take_while(|c| style.separator(**c)).count();
            if count >= 2 { joined.drain(..count-1); joined[0] = 92; }
        }
        // Reserved device paths intentionally bypass dot-segment normalization.
        if joined.split(|c| *c == 92).any(|part| part.iter().position(|c| *c == 58).is_some_and(|colon| reserved(&part[..colon]))) {
            for c in &mut joined { if *c == 47 { *c = 92; } }
            return JsString(joined);
        }
    }
    normalize(style, &JsString(joined))
}

/// Explicit cwd and per-drive cwd make path resolution deterministic. Drive keys
/// are `C:` etc.; their lookup follows Windows' case-insensitive environment.
pub struct Context { pub style: Style, pub cwd: JsString, pub drive_cwds: BTreeMap<JsString, JsString> }
impl Context {
    pub fn current() -> io::Result<Self> {
        let mut drive_cwds = BTreeMap::new();
        #[cfg(windows)]
        for drive in b'A'..=b'Z' {
            let key = format!("{}:", drive as char);
            if let Some(value) = std::env::var_os(format!("={key}")) {
                drive_cwds.insert(JsString::from(key), from_native(Path::new(&value)));
            }
        }
        Ok(Self { style: Style::native(), cwd: from_native(&std::env::current_dir()?), drive_cwds })
    }
    pub fn resolve(&self, parts: &[JsString]) -> JsString {
        let mut device = Vec::new(); let mut rest = Vec::new(); let mut absolute = false;
        for part in parts.iter().map(|part| &part.0).rev().map(Some).chain(std::iter::once(None)) {
            let fallback;
            let path = match part {
                Some(path) if path.is_empty() => continue,
                Some(path) => path,
                None => {
                    fallback = if self.style == Style::Windows && !device.is_empty() {
                        let mut cwd = self.drive_cwds.iter().find(|(key,_)| lower(&key.0) == lower(&device))
                            .map(|(_,path)| path.0.clone()).filter(|path| !path.is_empty()).unwrap_or_else(|| self.cwd.0.clone());
                        if lower(&cwd[..cwd.len().min(2)]) != lower(&device) && cwd.get(2) == Some(&92) {
                            cwd = device.clone(); cwd.push(92);
                        }
                        cwd
                    } else { self.cwd.0.clone() };
                    &fallback
                }
            };
            let root = if self.style == Style::Windows { windows_root(path, false) }
                else { Root { end: 0, device: Vec::new(), absolute: path.first() == Some(&47) } };
            if !root.device.is_empty() {
                if !device.is_empty() && lower(&device) != lower(&root.device) { continue; }
                if device.is_empty() { device = root.device; }
            }
            if absolute { if !device.is_empty() { break; } }
            else {
                let mut combined = path[root.end..].to_vec(); combined.push(self.style.slash()); combined.extend(rest); rest = combined;
                absolute = root.absolute;
                if absolute && (self.style == Style::Posix || !device.is_empty()) { break; }
            }
        }
        let rest = tail(self.style, &rest, !absolute);
        if absolute { device.push(self.style.slash()); }
        device.extend(rest);
        if device.is_empty() { device.push(46); }
        JsString(device)
    }
    pub fn relative(&self, from: &JsString, to: &JsString) -> JsString {
        if from == to { return JsString::default(); }
        let from = self.resolve(std::slice::from_ref(from)); let to = self.resolve(std::slice::from_ref(to));
        let windows = self.style == Style::Windows;
        let a = if windows { lower(&from.0) } else { from.0.clone() };
        let b = if windows { lower(&to.0) } else { to.0.clone() };
        if a == b { return JsString::default(); }
        let slash = self.style.slash();
        let expanded = a.len() != from.0.len() || b.len() != to.0.len();
        let split = |value: &[u16]| -> Vec<Vec<u16>> {
            let mut parts: Vec<_> = value.split(|c| *c == slash).map(|p| p.to_vec()).collect();
            if parts.last().is_some_and(|p| p.is_empty()) { parts.pop(); }
            if !expanded { while parts.first().is_some_and(|p| p.is_empty()) { parts.remove(0); } }
            parts
        };
        let from_parts = split(&from.0); let to_parts = split(&to.0);
        let common = from_parts.iter().zip(&to_parts).take_while(|(a,b)| if windows { lower(a) == lower(b) } else { a == b }).count();
        if windows && common == 0 && !from_parts.is_empty() && !to_parts.is_empty() { return to; }
        let mut result = vec![vec![46,46]; from_parts.len()-common]; result.extend_from_slice(&to_parts[common..]);
        JsString(result.join(&slash))
    }
}
pub fn from_native(path: &Path) -> JsString {
    #[cfg(windows)] { use std::os::windows::ffi::OsStrExt; JsString(path.as_os_str().encode_wide().collect()) }
    #[cfg(not(windows))] { JsString::from(path.to_string_lossy().as_ref()) }
}
pub fn to_native(path: &JsString) -> PathBuf {
    #[cfg(windows)] { use std::os::windows::ffi::OsStringExt; std::ffi::OsString::from_wide(&path.0).into() }
    #[cfg(not(windows))] { String::from_utf16_lossy(&path.0).into() }
}

/// Node fs string arguments cross a UTF-8 boundary before native filesystem I/O.
/// Lexical paths, JSON and direct native arguments still use lossless to_native.
pub fn filesystem_path(path: &JsString) -> PathBuf { String::from_utf16_lossy(&path.0).into() }
