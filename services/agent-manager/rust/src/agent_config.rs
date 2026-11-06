//! One settings boundary, before reset, state files or native process creation.
use crate::native_launch::NativeHomeInputs;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, agent_schema::validate_agent, native_backend::BackendIdentity,
};
use ivy_host_runtime::{
    agent_backends::configured_backends, config::InstanceConfig, node_path, paths,
};

pub(crate) struct BackendConfig {
    pub config: InstanceConfig,
    pub identity: Option<BackendIdentity>,
}
pub(crate) struct ValidatedAgentConfig {
    config: InstanceConfig,
    pub backends: Vec<BackendConfig>,
    pub hosted: bool,
}
impl ValidatedAgentConfig {
    pub(crate) fn new(config: InstanceConfig) -> IvyResult<Self> {
        let value = config.json();
        let settings = value.required("settings")?;
        validate_agent("ServiceSettings", settings)?;
        if value.get("componentId") != Some(&"agent-manager".into())
            || value
                .get("credential")
                .and_then(IvyJson::as_js_string)
                .is_none_or(|v| v.0.is_empty())
        {
            return Err(IvyError::new(
                "invalid_arguments",
                "AgentManager requires its own component identity and Hive credential.",
            ));
        }
        let hosted = settings.get("backends").is_some();
        let entries = configured_backends(settings, Some(value.required("dataRoot")?))?;
        let artifact = node_path::filesystem_path(&paths::future_path(&node_path::from_native(
            config.artifact_root(),
        ))?);
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        let mut backends = Vec::with_capacity(entries.len());
        for entry in entries {
            let settings = entry.settings;
            let identity = entry.identity;
            let root = entry.data_root.cloned().ok_or_else(IvyError::internal)?;
            let root_text = root.as_js_string().ok_or_else(IvyError::internal)?;
            if !node_path::is_absolute(node_path::Style::native(), root_text) {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "Backend data roots must be absolute.",
                ));
            }
            let resolved = paths::future_path(root_text)?;
            let path = node_path::filesystem_path(&resolved);
            if paths::inside(&artifact, &path).map_err(|_| IvyError::internal())?
                || paths::inside(&path, &artifact).map_err(|_| IvyError::internal())?
                || roots.iter().any(|previous| {
                    paths::inside(previous, &path).unwrap_or(true)
                        || paths::inside(&path, previous).unwrap_or(true)
                })
            {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "Backend data roots must be separate from each other and executable artifacts.",
                ));
            }
            roots.push(path);
            let mut backend = IvyJson::object(
                value
                    .as_object()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .filter(|(key, _)| !key.eq_str("settings") && !key.eq_str("dataRoot"))
                    .cloned(),
            );
            backend.insert("dataRoot", root);
            backend.insert("settings", settings.clone());
            backends.push(BackendConfig {
                config: InstanceConfig::from_json(backend)?,
                identity,
            });
        }
        Ok(Self {
            config,
            backends,
            hosted,
        })
    }
    pub(crate) fn config(&self) -> &InstanceConfig {
        &self.config
    }
    pub(crate) fn validate_homes(&self, home: &NativeHomeInputs) -> IvyResult<()> {
        let backends = self
            .backends
            .iter()
            .map(|backend| {
                Ok(ivy_host_runtime::agent_backends::AgentBackend {
                    settings: backend.config.json().required("settings")?,
                    data_root: Some(backend.config.json().required("dataRoot")?),
                    identity: backend.identity.clone(),
                })
            })
            .collect::<IvyResult<Vec<_>>>()?;
        ivy_host_runtime::agent_backends::validate_native_homes(
            &backends,
            self.config.artifact_root(),
            &home.context(),
        )
    }
}
