//! Existing skills ownership rules, separate from locked filesystem effects.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};

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
    if state.is_some_and(|state| !state.is_null())
        && (!matches!(version, Some(IvyJson::Number(v)) if *v == 2.0)
            || state.and_then(|s| s.get("authority")) != Some(&IvyJson::from(authority)))
    {
        return Err(conflict("Another Hive or host owns these managed skills."));
    }
    let applied = state.and_then(|s| s.get("applied"));
    if applied.is_some_and(|value| !value.is_null() && value.as_object().is_none()) {
        return Err(conflict("Managed skill ownership must be an object."));
    }
    let before: Vec<_> = applied.and_then(IvyJson::as_object).unwrap_or(&[]).to_vec();
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
