//! Service configuration boundary. Filesystem work runs on blocking workers.
use crate::{IvyError, IvyJson, IvyResult, node_runtime::NodeRuntime, paths::inside};
use ivy_contracts::{JsString, core_schema::validate_host};
use std::{ffi::OsString, fs, path::{Path, PathBuf}};
pub use crate::protected_config::protected_json_file;
pub use crate::config_file::json_file;
pub use crate::config_write::atomic_json;
pub(crate) use crate::config_file::{FileError, read_json};
pub(crate) use crate::config_write::write_json;

pub const CONFIGURATION_BYTES: usize = 1024 * 1024;

fn absolute(path: &Path) -> bool {
    // node:path.win32 admits root-relative paths as absolute (e.g. \\data).
    #[cfg(windows)] { path.has_root() }
    #[cfg(not(windows))] { path.is_absolute() }
}

/// Supply argv after the executable and the current IVY_INSTANCE_CONFIG value.
/// The first --config wins, including its missing/invalid argument.
pub fn configuration_path(args: &[OsString], environment: Option<OsString>) -> IvyResult<PathBuf> {
    let path = match args.iter().position(|arg| arg == "--config") {
        Some(position) => args.get(position + 1).cloned(),
        None => environment,
    }.map(PathBuf::from);
    path.filter(|p| absolute(p)).ok_or_else(|| IvyError::new("invalid_arguments", "Use --config ABSOLUTE_PATH or IVY_INSTANCE_CONFIG."))
}

/// Preserve the entire validated JSON, including credentials and UTF-16 settings.
/// Deliberately has no Debug implementation: diagnostics must not print secrets.
#[derive(Clone)]
pub struct InstanceConfig { value: IvyJson, data_root: PathBuf, artifact_root: PathBuf }
impl InstanceConfig {
    pub fn from_json(value: IvyJson) -> IvyResult<Self> {
        validate_host("InstanceConfig", &value)?;
        let data_root = path_value(value.required("dataRoot")?)?;
        let artifact_root = path_value(value.required("artifactRoot")?)?;
        Ok(Self { value, data_root, artifact_root })
    }
    pub fn json(&self) -> &IvyJson { &self.value }
    pub fn data_root(&self) -> &Path { &self.data_root }
    pub fn artifact_root(&self) -> &Path { &self.artifact_root }
}
pub(crate) fn path_value(value: &IvyJson) -> IvyResult<PathBuf> {
    let text = value.as_js_string().ok_or_else(IvyError::internal)?;
    #[cfg(windows)]
    { use std::os::windows::ffi::OsStringExt; Ok(OsString::from_wide(&text.0).into()) }
    #[cfg(not(windows))]
    { Ok(String::from_utf16_lossy(&text.0).into()) }
}
fn separate(a: &Path, b: &Path) -> IvyResult<bool> {
    Ok(!inside(a, b).map_err(|_| IvyError::internal())? && !inside(b, a).map_err(|_| IvyError::internal())?)
}
/// The caller supplies the discovered dispatcher/host Node runtime; no PATH lookup.
pub async fn instance_config(path: impl Into<PathBuf>, node: &NodeRuntime) -> IvyResult<InstanceConfig> {
    let path = path.into();
    let version = node.version.clone();
    tokio::task::spawn_blocking(move || {
        let config = InstanceConfig::from_json(read_json(&path, CONFIGURATION_BYTES).map_err(FileError::error)?)?;
        if !absolute(&config.data_root) || !absolute(&config.artifact_root) || !separate(&config.data_root, &config.artifact_root)? {
            return Err(IvyError::new("invalid_arguments", "Instance data and artifact roots must be separate absolute paths."));
        }
        let mut parts = version.split('.');
        if parts.next() != Some("24") || parts.next().and_then(|part| part.parse::<u64>().ok()).is_none_or(|minor| minor < 18) {
            return Err(IvyError::new("unsupported_runtime", "This distribution requires pinned Node 24.18 or newer in the 24.x line."));
        }
        let build = read_json(&config.artifact_root.join("dist/build-info.json"), CONFIGURATION_BYTES).map_err(FileError::error)?;
        if build.is_null() { return Err(IvyError::internal()); }
        if build.get("buildId") != config.value.get("buildId") || build.get("version") != config.value.get("version") {
            return Err(IvyError::new("build_mismatch", "Configuration identity does not match the prepared executable artifact."));
        }
        fs::create_dir_all(&config.data_root).map_err(|_| IvyError::internal())?;
        for key in ["workRoot", "logsRoot"] {
            if let Some(value) = config.value.get(key) {
                let root = path_value(value)?;
                if !absolute(&root) || !separate(&config.artifact_root, &root)? {
                    return Err(IvyError::new("invalid_arguments", "Service work/logs must stay outside executable artifacts."));
                }
                fs::create_dir_all(root).map_err(|_| IvyError::internal())?;
            }
        }
        Ok(config)
    }).await.map_err(|_| IvyError::internal())?
}

pub(crate) fn js_environment(name: &str) -> Option<JsString> {
    std::env::var_os(name).map(|value| {
        #[cfg(windows)]
        { use std::os::windows::ffi::OsStrExt; JsString(value.encode_wide().collect()) }
        #[cfg(not(windows))]
        { value.to_string_lossy().as_ref().into() }
    })
}
