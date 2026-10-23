//! Native automatic-maintenance entry. Node is an optional workload capability.
use ivy_contracts::{IvyError, IvyResult};
use ivy_host_runtime::{
    artifact::native_file_compare, bootstrap_maintenance::BootstrapMaintenance,
    bootstrap_os::bootstrap_os, node_runtime::NodeRuntime, paths,
};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

// Match ASCII option prefixes while retaining the OS filename representation.
#[cfg(windows)]
fn inline_value(argument: &OsStr, flag: &str) -> Option<OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let mut units = argument.encode_wide();
    for unit in flag.encode_utf16().chain([b'=' as u16]) {
        if units.next() != Some(unit) {
            return None;
        }
    }
    Some(OsString::from_wide(&units.collect::<Vec<_>>()))
}
#[cfg(unix)]
fn inline_value(argument: &OsStr, flag: &str) -> Option<OsString> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let value = argument
        .as_bytes()
        .strip_prefix(flag.as_bytes())?
        .strip_prefix(b"=")?;
    Some(OsString::from_vec(value.to_vec()))
}

pub struct WorkerPaths {
    pub config: PathBuf,
    pub request: PathBuf,
    pub distribution: PathBuf,
    /// Private native-launch input. Legacy dispatchers supply IVY_NODE_EXECUTABLE.
    pub node_executable: Option<PathBuf>,
}
impl WorkerPaths {
    /// Admit named path options relative to the native working directory.
    /// Repeated options select the last value; no filesystem access is needed.
    pub fn parse(args: &[OsString], cwd: &Path) -> IvyResult<Self> {
        const FLAGS: [&str; 4] = [
            "--config",
            "--request",
            "--distribution",
            "--node-executable",
        ];
        let mut values = BTreeMap::new();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if arg == "--" {
                if args.next().is_some() {
                    return Err(IvyError::internal());
                }
                break;
            }
            let (key, inline) = FLAGS
                .into_iter()
                .find_map(|flag| {
                    if arg == flag {
                        Some((flag, None))
                    } else {
                        inline_value(arg, flag).map(|value| (flag, Some(value)))
                    }
                })
                .ok_or_else(IvyError::internal)?;
            let value = match inline {
                Some(value) => value,
                None => {
                    let value = args.next().ok_or_else(IvyError::internal)?;
                    let bytes = value.as_encoded_bytes();
                    if bytes.len() > 1 && bytes[0] == b'-' {
                        return Err(IvyError::internal());
                    }
                    value.clone()
                }
            };
            values.insert(key, value);
        }
        let required = |key: &str| -> IvyResult<PathBuf> {
            let value = values.get(key).filter(|value| !value.is_empty()).ok_or_else(|| {
                IvyError::new("invalid_arguments", "Automatic bootstrap worker requires config, request and distribution paths.")
            })?;
            paths::absolute(&cwd.join(value)).map_err(|_| IvyError::internal())
        };
        // Do not resolve the workload executable: NodeRuntime::discover retains
        // its absolute-file admission instead of accepting a relative override.
        let node_executable = values.get("--node-executable").map(PathBuf::from);
        Ok(Self {
            config: required("--config")?,
            request: required("--request")?,
            distribution: required("--distribution")?,
            node_executable,
        })
    }
}

/// Assemble the actual maintenance owner, rather than a test OS/readiness seam.
/// No parser or HTTP binding is needed by the independent worker. All admitted
/// effects and dropped-waiter ownership remain in the existing worker policy.
pub async fn run(args: &[OsString]) -> IvyResult<()> {
    let cwd = std::env::current_dir().map_err(|_| IvyError::internal())?;
    let paths = WorkerPaths::parse(args, &cwd)?;
    let node = NodeRuntime::discover_optional(paths.node_executable.as_deref()).await?;
    let compare = native_file_compare()?;
    // The selected native package keeps helpers beside this executable.
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
