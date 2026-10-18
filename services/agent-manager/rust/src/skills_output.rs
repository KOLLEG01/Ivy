//! Existing skills ownership rules, separate from locked filesystem effects.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::object_entries};

pub struct SkillsOwnership {
    before: Vec<(JsString, IvyJson)>,
    authority: String,
}
pub struct SkillReplacement {
    pub name: JsString,
    pub owned: bool,
    pub unchanged: bool,
}
pub struct SkillsOutput {
    pub replacements: Vec<SkillReplacement>,
    pub state: IvyJson,
}
fn conflict(message: &str) -> IvyError {
    IvyError::new("target_conflict", message)
}
fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null => false,
        IvyJson::Bool(value) => *value,
        IvyJson::Number(value) => *value != 0.0 && !value.is_nan(),
        IvyJson::String(value) => !value.0.is_empty(),
        _ => true,
    }
}
pub(crate) fn valid_name(name: &JsString) -> bool {
    name.0.len() <= 64
        && name.0.first().is_some_and(|c| (97..=122).contains(c))
        && name.0.iter().all(|c| matches!(c, 45 | 48..=57 | 97..=122))
}

/// Read state and call this under the output lock, before staging files. Tree
/// hashes must use the original per-directory locale ordering. Dropped skills
/// remain on disk and are deliberately omitted from the next ownership state.
pub fn inspect_ownership(
    state: Option<&IvyJson>,
    authority: &str,
    desired_names: &[JsString],
    mut tree_hash: impl FnMut(&JsString) -> IvyResult<Option<String>>,
) -> IvyResult<SkillsOwnership> {
    let version = state.and_then(|s| s.get("schemaVersion"));
    if state.is_some_and(truthy)
        && (!matches!(version, Some(IvyJson::Number(v)) if *v == 2.0)
            || state.and_then(|s| s.get("authority")) != Some(&IvyJson::from(authority)))
    {
        return Err(conflict("Another Hive or host owns these managed skills."));
    }
    let applied = state.and_then(|s| s.get("applied"));
    // Object.entries boxes primitives. Nonempty arrays/strings have numeric
    // keys, which fail the exact skill-name rule before any directory read.
    if matches!(applied, Some(IvyJson::Array(v)) if !v.is_empty())
        || matches!(applied, Some(IvyJson::String(v)) if !v.0.is_empty())
    {
        return Err(conflict(
            "Managed skill state contains an invalid directory name.",
        ));
    }
    let before: Vec<_> = applied
        .map(object_entries)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (name, hash) in &before {
        if !valid_name(name) {
            return Err(conflict(
                "Managed skill state contains an invalid directory name.",
            ));
        }
        if desired_names.contains(name) {
            let actual = tree_hash(name)?.map(IvyJson::from).unwrap_or(IvyJson::Null);
            if &actual != hash {
                return Err(conflict(&format!(
                    "Managed skill {} has local changes.",
                    name.to_utf8()?
                )));
            }
        }
    }
    Ok(SkillsOwnership {
        before,
        authority: authority.into(),
    })
}

impl SkillsOwnership {
    /// Call after staging and hashing every desired directory, still holding
    /// the same lock. All collisions are checked before the first replacement.
    /// `after` is the insertion-ordered result of normalized skill groups.
    pub fn plan(
        &self,
        after: &[(JsString, String)],
        desired_version: &str,
        mut tree_hash: impl FnMut(&JsString) -> IvyResult<Option<String>>,
    ) -> IvyResult<SkillsOutput> {
        let mut replacements = Vec::new();
        for (name, hash) in after {
            let previous = self
                .before
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, v)| v);
            if previous.is_none() && tree_hash(name)?.is_some() {
                return Err(conflict(&format!(
                    "Skill directory {} already exists outside Ivy management.",
                    name.to_utf8()?
                )));
            }
            replacements.push(SkillReplacement {
                name: name.clone(),
                unchanged: previous == Some(&IvyJson::from(hash.clone())),
                owned: previous.is_some(),
            });
        }
        Ok(SkillsOutput {
            replacements,
            state: IvyJson::object([
                ("schemaVersion", IvyJson::Number(2.0)),
                ("authority", self.authority.clone().into()),
                (
                    "applied",
                    IvyJson::object(
                        after
                            .iter()
                            .map(|(name, hash)| (name.clone(), IvyJson::from(hash.clone()))),
                    ),
                ),
                ("appliedVersion", desired_version.into()),
            ]),
        })
    }
}
