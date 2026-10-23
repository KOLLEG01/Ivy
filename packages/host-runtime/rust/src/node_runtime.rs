//! The intentional JavaScript workload runtime is distinct from the Rust backend.
use crate::process::{self, Command, LaunchContext};
use ivy_contracts::{IvyError, IvyResult};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct NodeRuntime {
    pub executable: PathBuf,
    pub version: String,
}

impl NodeRuntime {
    /// Absence is a native-host capability choice, never PATH discovery.
    pub async fn discover_optional(configured: Option<&Path>) -> IvyResult<Option<Self>> {
        if configured.is_none() && std::env::var_os("IVY_NODE_EXECUTABLE").is_none() {
            return Ok(None);
        }
        Self::discover(configured).await.map(Some)
    }
    /// The v1 dispatcher supplies its own absolute process.execPath privately.
    /// Explicit host tool configuration takes precedence for host operations.
    pub async fn discover(configured: Option<&Path>) -> IvyResult<Self> {
        let requested = configured.map(Path::to_path_buf).or_else(|| std::env::var_os("IVY_NODE_EXECUTABLE").map(PathBuf::from))
            .ok_or_else(|| IvyError::new("unsupported_runtime", "The Node workload executable was not supplied by the dispatcher or host configuration."))?;
        if !requested.is_absolute() || !requested.is_file() {
            return Err(IvyError::new(
                "unsupported_runtime",
                "The Node workload executable must be an absolute regular file.",
            ));
        }
        let executable = std::fs::canonicalize(requested).map_err(|_| {
            IvyError::new(
                "unsupported_runtime",
                "The Node workload executable cannot be resolved.",
            )
        })?;
        let root = executable.parent().ok_or_else(|| {
            IvyError::new(
                "unsupported_runtime",
                "The Node workload executable has no containing directory.",
            )
        })?;
        let command = Command {
            executable: "$node".into(),
            args: vec!["--version".into()],
            cwd: None,
            timeout_ms: 5000,
        };
        let result = process::run_command(
            &command,
            root,
            &BTreeMap::from([("$node".into(), executable.clone())]),
            LaunchContext {
                without_job: true,
                ..Default::default()
            },
            None,
        )
        .await?;
        let version = result
            .stdout
            .trim()
            .strip_prefix('v')
            .unwrap_or(result.stdout.trim())
            .to_owned();
        if !crate::package_requirements::runtime_requirements_satisfied(
            &crate::package_requirements::Requirements {
                node: ">=24.18.0 <25.0.0",
                os: None,
                arch: None,
            },
            &crate::package_requirements::Runtime {
                node: &version,
                os: "",
                arch: "",
            },
        )? {
            return Err(IvyError::new(
                "unsupported_runtime",
                "This distribution requires pinned Node 24.18 or newer in the 24.x line.",
            ));
        }
        Ok(Self {
            executable,
            version,
        })
    }

    pub fn npm_cli(&self, configured: Option<&Path>) -> IvyResult<PathBuf> {
        let path = configured.map(Path::to_path_buf).unwrap_or_else(|| {
            self.executable
                .parent()
                .unwrap()
                .join("node_modules/npm/bin/npm-cli.js")
        });
        if !path.is_file() {
            return Err(IvyError::new(
                "unsupported_runtime",
                "Configure npmCliPath for tasks with npm dependencies.",
            ));
        }
        Ok(path)
    }
}
