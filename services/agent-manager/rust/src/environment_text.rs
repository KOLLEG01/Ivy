//! Existing managed TOML marker rules, without rewriting unrelated TOML syntax.
use ivy_contracts::{IvyError, IvyResult, hash::digest};
use std::{collections::HashSet, ops::Range};

pub const BEGIN_CODEX: &str =
    "# >>> Ivy AgentManager Codex configuration (managed; edit in AgentUI)";
pub const END_CODEX: &str = "# <<< Ivy AgentManager Codex configuration";
pub const BEGIN_MCP: &str = "# >>> Ivy AgentManager MCP configuration (managed; edit in AgentUI)";
pub const END_MCP: &str = "# <<< Ivy AgentManager MCP configuration";
pub const BEGIN_PERMISSIONS: &str =
    "# >>> Ivy AgentManager Codex permissions (managed; edit in AgentUI)";
pub const END_PERMISSIONS: &str = "# <<< Ivy AgentManager Codex permissions";
const APPROVAL: &str = "approval_policy = \"never\"";
const PERMISSIONS: &str = "default_permissions = \":danger-full-access\"";

pub struct ManagedBlock {
    pub block: Option<String>,
    pub outside: String,
}
fn conflict(message: &str) -> IvyError {
    IvyError::new("target_conflict", message)
}
fn malformed() -> IvyError {
    conflict("Codex configuration has malformed Ivy management markers.")
}
fn after_first_char(source: &str, start: usize) -> usize {
    start + source[start..].chars().next().map_or(0, char::len_utf8)
}

/// File text has already passed the original fatal UTF-8 decoder.
pub fn managed_block(
    source: &str,
    begin_marker: &str,
    end_marker: &str,
) -> IvyResult<ManagedBlock> {
    let begin = source.find(begin_marker);
    let end = source.find(end_marker);
    match (begin, end) {
        (None, None) => Ok(ManagedBlock {
            block: None,
            outside: source.into(),
        }),
        (Some(begin), Some(end))
            if end > begin
                && !source[after_first_char(source, begin)..].contains(begin_marker)
                && !source[after_first_char(source, end)..].contains(end_marker) =>
        {
            let through = end + end_marker.len();
            Ok(ManagedBlock {
                block: Some(source[begin..through].into()),
                outside: format!("{}{}", &source[..begin], &source[through..]),
            })
        }
        _ => Err(malformed()),
    }
}

// Match ECMAScript String.trim and multiline anchors, including their different
// treatment of U+0085 versus U+FEFF. TOML's own whitespace grammar is not used here.
fn whitespace(c: char) -> bool {
    matches!(c,'\u{9}'..='\u{d}'|'\u{20}'|'\u{a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
fn line_starts(source: &str) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0).chain(source.char_indices().filter_map(|(i, c)| {
        matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}').then_some(i + c.len_utf8())
    }))
}
fn table_positions(source: &str) -> Vec<usize> {
    line_starts(source)
        .filter(|start| {
            source[*start..]
                .trim_start_matches([' ', '\t'])
                .starts_with('[')
        })
        .collect()
}

// The original permission regex permits non-CR/LF characters (including Unicode
// line separators) in its value and ends before a bare CR via the multiline `$`.
fn permission_lines(source: &str, key: &str) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut consumed = 0;
    for start in line_starts(source) {
        if start < consumed {
            continue;
        }
        let tail = source[start..].trim_start_matches([' ', '\t']);
        let Some(tail) = tail.strip_prefix(key) else {
            continue;
        };
        let tail = tail.trim_start_matches([' ', '\t']);
        let Some(value) = tail.strip_prefix('=') else {
            continue;
        };
        let value_start = source.len() - value.len();
        let mut end = value
            .find(['\r', '\n'])
            .map_or(source.len(), |index| value_start + index);
        if source[end..].starts_with("\r\n") {
            end += 2;
        } else if source[end..].starts_with('\n') {
            end += 1;
        }
        result.push(start..end);
        consumed = end;
    }
    result
}
fn remove_lines(source: &str, ranges: &[Range<usize>]) -> String {
    let mut result = String::new();
    let mut previous = 0;
    for range in ranges {
        result.push_str(&source[previous..range.start]);
        previous = range.end;
    }
    result.push_str(&source[previous..]);
    result
}

pub fn render_codex(sections: Option<&[String]>) -> Option<String> {
    sections.map(|sections| {
        let mut parts = vec![BEGIN_CODEX, APPROVAL, PERMISSIONS, ""];
        parts.extend(sections.iter().map(String::as_str));
        parts.push(END_CODEX);
        parts.join("\n")
    })
}

pub fn codex_block(source: &str, accepted_hashes: &HashSet<String>) -> IvyResult<ManagedBlock> {
    let begin = source.find(BEGIN_CODEX);
    let end = source.find(END_CODEX);
    if begin.is_none() && end.is_none() {
        return Ok(ManagedBlock {
            block: None,
            outside: source.into(),
        });
    }
    let begin = begin.ok_or_else(malformed)?;
    if end.is_some_and(|end| end <= begin)
        || source[begin + 1..].contains(BEGIN_CODEX)
        || end.is_some_and(|end| source[end + 1..].contains(END_CODEX))
    {
        return Err(malformed());
    }
    let through = end.map_or(source.len(), |end| end + END_CODEX.len());
    let current = end.map(|_| &source[begin..through]);
    if let Some(current) = current.filter(|current| accepted_hashes.contains(&digest(current))) {
        return Ok(ManagedBlock {
            block: Some(current.into()),
            outside: format!("{}{}", &source[..begin], &source[through..]),
        });
    }
    let region = &source[begin + BEGIN_CODEX.len()..end.unwrap_or(source.len())];
    let positions = table_positions(region);
    let root = &region[..positions.first().copied().unwrap_or(region.len())];
    let approval = permission_lines(root, "approval_policy");
    let permissions = permission_lines(root, "default_permissions");
    if approval.len() != 1
        || permissions.len() != 1
        || root[approval[0].clone()].trim_matches(whitespace) != APPROVAL
        || root[permissions[0].clone()].trim_matches(whitespace) != PERMISSIONS
    {
        return Err(conflict(
            "The Ivy-managed Codex permissions have local changes.",
        ));
    }
    let extra = remove_lines(root, &approval);
    let extra = remove_lines(&extra, &permission_lines(&extra, "default_permissions"));
    let raw: Vec<_> = positions
        .iter()
        .enumerate()
        .map(|(i, start)| &region[*start..positions.get(i + 1).copied().unwrap_or(region.len())])
        .collect();
    for count in 0..=raw.len() {
        let mut sections = Vec::new();
        for table in &raw[..count] {
            let table = table.trim_matches(whitespace);
            sections.push(table.into());
            if table.starts_with("[apps.") {
                sections.push(String::new());
            }
        }
        let candidate = render_codex(Some(&sections)).unwrap();
        if accepted_hashes.contains(&digest(&candidate)) {
            return Ok(ManagedBlock {
                block: Some(candidate),
                outside: format!(
                    "{}{}{}{}",
                    &source[..begin],
                    extra,
                    raw[count..].concat(),
                    &source[through..]
                ),
            });
        }
    }
    Err(conflict("The Ivy-managed Codex block has local changes."))
}

fn collapse_blank_lines(source: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < source.len() {
        let start = i;
        let mut count = 0;
        loop {
            if source[i..].starts_with("\r\n") {
                i += 2;
            } else if source[i..].starts_with('\n') {
                i += 1;
            } else {
                break;
            }
            count += 1;
        }
        if count >= 3 {
            out.push_str("\n\n");
        } else {
            out.push_str(&source[start..i]);
        }
        if i < source.len() {
            let c = source[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

pub fn with_block(source: &str, block: Option<&str>) -> String {
    let (bom, source) = source
        .strip_prefix('\u{feff}')
        .map_or(("", source), |tail| ("\u{feff}", tail));
    let collapsed = collapse_blank_lines(source);
    let outside = collapsed.trim_matches(whitespace);
    let first = table_positions(outside)
        .first()
        .copied()
        .unwrap_or(outside.len());
    let root = outside[..first].trim_end_matches(whitespace);
    let tables = outside[first..].trim_start_matches(whitespace);
    let parts: Vec<_> = [Some(root), block, Some(tables)]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect();
    format!(
        "{bom}{}{}",
        parts.join("\n\n"),
        if parts.is_empty() { "" } else { "\n" }
    )
}

fn section_boundary(tail: &str) -> bool {
    tail.starts_with('.') || tail.starts_with(']')
}

/// Original case-insensitive section regex preserves captured name case.
pub fn server_sections(source: &str) -> Vec<String> {
    const PREFIX: &str = "[mcp_servers.";
    let mut names = Vec::new();
    let mut consumed = 0;
    for start in line_starts(source) {
        if start < consumed {
            continue;
        }
        let tail = source[start..].trim_start_matches(whitespace);
        if !tail
            .get(..PREFIX.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(PREFIX))
        {
            continue;
        }
        let mut tail = &tail[PREFIX.len()..];
        let quoted = tail.starts_with('"');
        if quoted {
            tail = &tail[1..];
        }
        let bytes = tail.as_bytes();
        if !bytes.first().is_some_and(u8::is_ascii_alphabetic) {
            continue;
        }
        let count = bytes
            .iter()
            .take(64)
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(**b, b'_' | b'-'))
            .count();
        let name = &tail[..count];
        let mut rest = &tail[count..];
        if quoted {
            let Some(value) = rest.strip_prefix('"') else {
                continue;
            };
            rest = value;
        }
        if !section_boundary(rest) {
            continue;
        }
        consumed = source.len() - rest.len() + 1;
        if !names.iter().any(|value| value == name) {
            names.push(name.into());
        }
    }
    names
}

pub fn has_app_section(source: &str, app_id: &str) -> bool {
    let alternatives = [
        format!("\"{app_id}\""),
        format!("'{app_id}'"),
        app_id.into(),
    ];
    line_starts(source).any(|start| {
        source[start..]
            .trim_start_matches(whitespace)
            .strip_prefix("[apps.")
            .is_some_and(|tail| {
                alternatives.iter().any(|name| {
                    tail.strip_prefix(name.as_str())
                        .is_some_and(section_boundary)
                })
            })
    })
}

const ROOT_KEYS: [&str; 4] = [
    "approval_policy",
    "default_permissions",
    "sandbox_mode",
    "sandbox_workspace_write",
];
fn key_tail<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    if let Some(tail) = source.strip_prefix('"') {
        tail.strip_prefix(key)?.strip_prefix('"')
    } else if let Some(tail) = source.strip_prefix('\'') {
        tail.strip_prefix(key)?.strip_prefix('\'')
    } else {
        source.strip_prefix(key)
    }
}
fn root_boundary(source: &str) -> usize {
    table_positions(source)
        .first()
        .copied()
        .unwrap_or(source.len())
}

pub fn root_configuration_keys(source: &str) -> Vec<String> {
    let root = &source[..root_boundary(source)];
    let mut keys = Vec::new();
    let mut consumed = 0;
    for start in line_starts(root) {
        if start < consumed {
            continue;
        }
        let tail = root[start..].trim_start_matches(whitespace);
        for key in ROOT_KEYS {
            let Some(rest) = key_tail(tail, key) else {
                continue;
            };
            let Some(rest) = rest.trim_start_matches(whitespace).strip_prefix('=') else {
                continue;
            };
            consumed = root.len() - rest.len();
            if !keys.iter().any(|value| value == key) {
                keys.push(key.into());
            }
            break;
        }
    }
    keys
}

pub fn has_legacy_sandbox_table(source: &str) -> bool {
    line_starts(source).any(|start| {
        source[start..]
            .trim_start_matches(whitespace)
            .strip_prefix("[sandbox_workspace_write")
            .is_some_and(section_boundary)
    })
}

/// Adopt only one exact matching assignment per key in the root region.
/// Comments, duplicate assignments and changed values retain the original errors.
pub fn adopt_matching_permissions(source: &str) -> IvyResult<String> {
    let boundary = root_boundary(source);
    let root = &source[..boundary];
    let mut ranges = Vec::new();
    let mut seen = HashSet::new();
    let mut consumed = 0;
    for start in line_starts(root) {
        if start < consumed {
            continue;
        }
        let tail = root[start..].trim_start_matches([' ', '\t']);
        for (key, expected) in [
            ("approval_policy", "never"),
            ("default_permissions", ":danger-full-access"),
        ] {
            let Some(rest) = key_tail(tail, key) else {
                continue;
            };
            let Some(rest) = rest.trim_start_matches([' ', '\t']).strip_prefix('=') else {
                continue;
            };
            let value = rest.trim_start_matches([' ', '\t']);
            let value_start = root.len() - value.len();
            let value_end = value
                .find(['\r', '\n'])
                .map_or(root.len(), |i| value_start + i);
            let mut end = value_end;
            if root[end..].starts_with("\r\n") {
                end += 2;
            } else if root[end..].starts_with('\n') {
                end += 1;
            }
            let value = root[value_start..value_end].trim_matches(whitespace);
            if !seen.insert(key)
                || (value != format!("\"{expected}\"") && value != format!("'{expected}'"))
            {
                return Err(conflict(&format!(
                    "Codex {key} already has a different or ambiguous unmanaged configuration."
                )));
            }
            ranges.push(start..end);
            consumed = end;
            break;
        }
    }
    Ok(format!(
        "{}{}",
        remove_lines(root, &ranges),
        &source[boundary..]
    ))
}
