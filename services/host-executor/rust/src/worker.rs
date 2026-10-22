//! Native automatic-maintenance entry. Its package supplies the adjacent Job
//! helper and ICU runtime; the retained plan supplies Node for workload checks.
use ivy_contracts::{IvyError, IvyResult, JsString};
use ivy_host_runtime::{
    artifact::native_file_compare, bootstrap_maintenance::BootstrapMaintenance,
    bootstrap_os::bootstrap_os, node_path, node_runtime::NodeRuntime,
};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

pub struct WorkerPaths {
    pub config: PathBuf,
    pub request: PathBuf,
    pub distribution: PathBuf,
    /// Private native-launch input. Legacy dispatchers supply IVY_NODE_EXECUTABLE.
    pub node_executable: Option<PathBuf>,
}
impl WorkerPaths {
    /// Retain the original strict string options, last-value precedence and
    /// lexical resolve. Parse failures cross IvyError::from as internal errors;
    /// absent/empty required values are the worker's invalid_arguments error.
    pub fn parse(args: &[OsString], paths: &node_path::Context) -> IvyResult<Self> {
        let mut values = BTreeMap::new();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let arg = node_path::from_native(std::path::Path::new(arg));
            if arg == JsString::from("--") {
                if args.next().is_some() {
                    return Err(IvyError::internal());
                }
                break;
            }
            let split = arg.0.iter().position(|unit| *unit == 61);
            let key = JsString(arg.0[..split.unwrap_or(arg.0.len())].to_vec());
            let key = match key.to_utf8().map_err(|_| IvyError::internal())?.as_str() {
                "--config" => "config",
                "--request" => "request",
                "--distribution" => "distribution",
                "--node-executable" => "node-executable",
                _ => return Err(IvyError::internal()),
            };
            let value = match split {
                Some(at) => JsString(arg.0[at + 1..].to_vec()),
                None => {
                    let value = args.next().ok_or_else(IvyError::internal)?;
                    let value = node_path::from_native(std::path::Path::new(value));
                    if value.0.len() > 1 && value.0[0] == 45 {
                        return Err(IvyError::internal());
                    }
                    value
                }
            };
            values.insert(key, value);
        }
        let required = |key: &str| -> IvyResult<PathBuf> {
            let value = values.get(key).filter(|value| !value.0.is_empty()).ok_or_else(|| {
                IvyError::new("invalid_arguments", "Automatic bootstrap worker requires config, request and distribution paths.")
            })?;
            Ok(node_path::to_native(&paths.resolve(&[value.clone()])))
        };
        // Do not resolve the workload executable: NodeRuntime::discover retains
        // its absolute-file admission instead of accepting a relative override.
        let node_executable = values.get("node-executable").map(node_path::to_native);
        Ok(Self {
            config: required("config")?,
            request: required("request")?,
            distribution: required("distribution")?,
            node_executable,
        })
    }
}

/// Assemble the actual maintenance owner, rather than a test OS/readiness seam.
/// No parser or HTTP binding is needed by the independent worker. All admitted
/// effects and dropped-waiter ownership remain in the existing worker policy.
pub async fn run(args: &[OsString]) -> IvyResult<()> {
    let paths = WorkerPaths::parse(
        args,
        &node_path::Context::current().map_err(|_| IvyError::internal())?,
    )?;
    let node = NodeRuntime::discover(paths.node_executable.as_deref()).await?;
    let compare = native_file_compare()?;
    // Universal payloads keep native helpers beside this selected executable.
    // Linux does not use the Windows Job helper; no unused file is required.
    let job_launcher = std::env::current_exe()
        .map_err(|_| IvyError::internal())?
        .with_file_name(if cfg!(windows) {
            "ivy-job.exe"
        } else {
            "ivy-job"
        });
    let os = bootstrap_os(paths.distribution, job_launcher.clone(), node.clone())?;
    let maintenance = BootstrapMaintenance::new(node, compare, os, job_launcher);
    crate::run_worker(maintenance, paths.config, paths.request).await
}
