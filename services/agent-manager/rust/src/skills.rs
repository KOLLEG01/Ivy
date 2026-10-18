//! Managed skill document preflight, before touching any output directory.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::{collections::HashSet, path::Path};

pub struct SkillFiles {
    pub name: JsString,
    /// Preserve document insertion order, like the original nested Maps.
    pub files: Vec<(JsString, Vec<u8>)>,
}

fn text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}

/// The document has already passed the shared SkillsDocument schema. Keep the
/// original path identity in UTF-16; Node encodes file contents as replacement UTF-8.
pub fn normalize_skills(document: &IvyJson) -> IvyResult<Vec<SkillFiles>> {
    if document.get("enabled") != Some(&IvyJson::Bool(true)) {
        return Ok(Vec::new());
    }
    let entries = document
        .get("files")
        .and_then(IvyJson::as_array)
        .ok_or_else(IvyError::internal)?;
    let mut result: Vec<SkillFiles> = Vec::new();
    let mut seen = HashSet::new();
    let mut total = 0;
    for file in entries {
        let path = text(file, "path")?;
        let parts: Vec<_> = path.0.split(|c| *c == 47).collect();
        if path.0.contains(&92)
            || parts
                .iter()
                .any(|p| p.is_empty() || *p == [46] || *p == [46, 46])
            || Path::new(&String::from_utf16_lossy(&path.0)).is_absolute()
        {
            return Err(invalid(
                "Managed skill paths must be relative slash-separated file paths.",
            ));
        }
        let skill = parts[0];
        if parts.len() < 2
            || skill.len() > 64
            || !skill.first().is_some_and(|c| (97..=122).contains(c))
            || !skill.iter().all(|c| matches!(c, 45 | 48..=57 | 97..=122))
        {
            return Err(invalid(
                "Managed skill files require a safe top-level skill directory.",
            ));
        }
        if !seen.insert(path.clone()) {
            return Err(invalid("Managed skill file paths must be unique."));
        }
        let content = String::from_utf16_lossy(&text(file, "content")?.0).into_bytes();
        total += content.len();
        if content.len() > 256 * 1024 || total > 4 * 1024 * 1024 {
            return Err(IvyError::new(
                "limit_exceeded",
                "Managed skill files exceed their bounded size.",
            ));
        }
        let group = match result.iter().position(|group| group.name.0 == skill) {
            Some(index) => index,
            None => {
                result.push(SkillFiles {
                    name: JsString(skill.to_vec()),
                    files: Vec::new(),
                });
                result.len() - 1
            }
        };
        let local = JsString(path.0[skill.len() + 1..].to_vec());
        result[group].files.push((local, content));
    }
    for group in &result {
        if !group
            .files
            .iter()
            .any(|(path, _)| *path == JsString::from("SKILL.md"))
        {
            return Err(invalid(&format!(
                "Managed skill {} requires SKILL.md.",
                group.name.to_utf8()?
            )));
        }
    }
    Ok(result)
}
