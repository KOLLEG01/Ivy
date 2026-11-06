//! Borrow the configured native owners without copying their service settings.
use crate::{IvyError, IvyJson, IvyResult};
use ivy_contracts::{
    hash::hash_json,
    native_backend::{BackendIdentity, NativeProvider},
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct AgentBackend<'a> {
    pub settings: &'a IvyJson,
    pub data_root: Option<&'a IvyJson>,
    pub identity: Option<BackendIdentity>,
}
impl AgentBackend<'_> {
    /// A storage/reference identity cannot silently follow a reused backend ID
    /// into a different provider or account.
    pub fn storage_key(&self) -> IvyResult<Option<String>> {
        self.identity
            .as_ref()
            .map(|identity| {
                Ok(format!(
                    "{}-{}",
                    identity.id(),
                    &hash_json(&identity.json())?[7..]
                ))
            })
            .transpose()
    }
}
fn identity(settings: &IvyJson) -> IvyResult<Option<BackendIdentity>> {
    let identity = BackendIdentity::optional(settings.get("backend"))?;
    let provider = if settings
        .get("appServer")
        .and_then(|value| value.get("mode"))
        == Some(&"claude-adapter".into())
    {
        NativeProvider::Claude
    } else {
        NativeProvider::Codex
    };
    if identity
        .as_ref()
        .is_some_and(|identity| identity.provider() != provider)
    {
        return Err(IvyError::new(
            "native_backend_mismatch",
            "Backend identifiers must be unique and identify their configured engine and account.",
        ));
    }
    Ok(identity)
}

/// Settings validation belongs to the input boundary. This view checks the
/// ownership selectors needed by host operations, including a retained flat owner.
pub fn configured_backends<'a>(
    settings: &'a IvyJson,
    default_data: Option<&'a IvyJson>,
) -> IvyResult<Vec<AgentBackend<'a>>> {
    let Some(entries) = settings.get("backends") else {
        return Ok(vec![AgentBackend {
            settings,
            data_root: default_data,
            identity: identity(settings)?,
        }]);
    };
    if settings.as_object().is_none_or(|fields| fields.len() != 1) {
        return Err(IvyError::new(
            "invalid_arguments",
            "Hosted AgentManager settings contain only their configured backends.",
        ));
    }
    let entries = entries
        .as_array()
        .filter(|entries| (1..=8).contains(&entries.len()))
        .ok_or_else(|| {
            IvyError::new(
                "invalid_arguments",
                "AgentManager requires one to eight configured backends.",
            )
        })?;
    let mut ids = HashSet::new();
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        let settings = entry.required("settings")?;
        if entry.as_object().is_none_or(|fields| fields.len() != 2)
            || settings.as_object().is_none()
            || settings.get("backends").is_some()
        {
            return Err(IvyError::new(
                "invalid_arguments",
                "Each native backend requires its own settings and data root.",
            ));
        }
        let identity = identity(settings)?.ok_or_else(|| {
            IvyError::new(
                "native_backend_mismatch",
                "Configured backends require their complete engine and account identity.",
            )
        })?;
        if !ids.insert(identity.id().to_owned()) {
            return Err(IvyError::new(
                "native_backend_mismatch",
                "Backend identifiers must be unique and identify their configured engine and account.",
            ));
        }
        let data = entry.required("dataRoot")?;
        if data.as_js_string().is_none_or(|value| value.0.is_empty()) {
            return Err(IvyError::new(
                "invalid_arguments",
                "Backend data roots must be nonempty paths.",
            ));
        }
        result.push(AgentBackend {
            settings,
            data_root: Some(data),
            identity: Some(identity),
        });
    }
    Ok(result)
}

/// Recheck filesystem ownership at host admission and immediately before native
/// startup. A shared server home remains external to every backend's data.
pub fn validate_native_homes(
    backends: &[AgentBackend<'_>],
    artifact: &Path,
    context: &crate::codex_home::HomeContext<'_>,
) -> IvyResult<()> {
    let resolved = |value: &ivy_contracts::JsString| -> IvyResult<PathBuf> {
        Ok(crate::node_path::filesystem_path(
            &crate::paths::future_path(value)?,
        ))
    };
    let artifact = resolved(&crate::node_path::from_native(artifact))?;
    let data = backends
        .iter()
        .map(|backend| {
            backend
                .data_root
                .map(|value| resolved(value.as_js_string().ok_or_else(IvyError::internal)?))
                .transpose()
        })
        .collect::<IvyResult<Vec<_>>>()?;
    let overlap = |a: &Path, b: &Path| -> IvyResult<bool> {
        Ok(
            crate::paths::inside(a, b).map_err(|_| IvyError::internal())?
                || crate::paths::inside(b, a).map_err(|_| IvyError::internal())?,
        )
    };
    let mut homes: Vec<(usize, PathBuf)> = Vec::new();
    for (owner, backend) in backends.iter().enumerate() {
        let settings = backend.settings;
        let home = crate::codex_home::resolve_codex_home_json(settings, context)?;
        let mut own = vec![resolved(&home.path)?];
        if crate::codex_home::uses_external_app_server_json(settings)? {
            let home = crate::codex_home::expected_server_home_json(settings, context)?;
            own.push(resolved(
                home.as_js_string().ok_or_else(IvyError::internal)?,
            )?);
        }
        for home in own {
            let mut conflict = overlap(&artifact, &home)?;
            for (other, root) in data.iter().enumerate() {
                if other != owner {
                    if let Some(root) = root {
                        conflict |= overlap(root, &home)?;
                    }
                }
            }
            for (other, previous) in &homes {
                if *other != owner {
                    conflict |= overlap(previous, &home)?;
                }
            }
            if conflict {
                return Err(IvyError::new(
                    "native_backend_mismatch",
                    "Native backend homes must be separate from other accounts, backend data and executable artifacts.",
                ));
            }
            homes.push((owner, home));
        }
    }
    Ok(())
}
