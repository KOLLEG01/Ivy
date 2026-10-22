//! Service configuration boundary. Filesystem work runs on blocking workers.
use crate::{IvyError, IvyJson, IvyResult, node_runtime::NodeRuntime, paths::inside};
use ivy_contracts::{JsString, core_schema::validate_host, json};
use std::{ffi::OsString, fs, path::{Path, PathBuf}};

pub const CONFIGURATION_BYTES: usize = 1024 * 1024;

pub(crate) enum FileError { Missing, Invalid(IvyError) }
impl FileError {
    pub(crate) fn error(self) -> IvyError {
        match self { Self::Missing => IvyError::internal(), Self::Invalid(error) => error }
    }
}
fn file_error(error: std::io::Error) -> FileError {
    if error.kind() == std::io::ErrorKind::NotFound { FileError::Missing }
    else { FileError::Invalid(IvyError::internal()) }
}
pub(crate) fn read_json(path: &Path, limit: usize) -> Result<IvyJson, FileError> {
    let metadata = fs::metadata(path).map_err(file_error)?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(FileError::Invalid(IvyError::new("invalid_arguments", "Configuration must be a bounded regular file.")));
    }
    let bytes = fs::read(path).map_err(file_error)?;
    // Node readFile(..., 'utf8') replaces malformed input before JSON.parse.
    json::parse(&String::from_utf8_lossy(&bytes), usize::MAX)
        .map_err(|_| FileError::Invalid(IvyError::internal()))
}
pub async fn json_file(path: impl Into<PathBuf>, limit: usize) -> IvyResult<IvyJson> {
    let path = path.into();
    tokio::task::spawn_blocking(move || read_json(&path, limit).map_err(FileError::error))
        .await.map_err(|_| IvyError::internal())?
}
pub async fn atomic_json(path: impl Into<PathBuf>, value: IvyJson) -> IvyResult<()> {
    let path = path.into();
    tokio::task::spawn_blocking(move || write_json(&path, &value))
        .await.map_err(|_| IvyError::internal())?
}
pub(crate) fn write_json(path: &Path, value: &IvyJson) -> IvyResult<()> {
    let mut bytes = json::canonical(value, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES)?.into_bytes();
    bytes.push(b'\n');
    ivy_store::atomic::atomic_write(path, &bytes).map_err(|_| IvyError::internal())
}
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
    load_instance_config(path.into(), Some(node.version.clone())).await
}

/// Native services validate the same public configuration and build identity.
/// A Node runtime is required only when launching a Node workload.
pub async fn native_instance_config(path: impl Into<PathBuf>) -> IvyResult<InstanceConfig> {
    load_instance_config(path.into(), None).await
}

async fn load_instance_config(path: PathBuf, node_version: Option<String>) -> IvyResult<InstanceConfig> {
    tokio::task::spawn_blocking(move || {
        let config = InstanceConfig::from_json(read_json(&path, CONFIGURATION_BYTES).map_err(FileError::error)?)?;
        if !absolute(&config.data_root) || !absolute(&config.artifact_root) || !separate(&config.data_root, &config.artifact_root)? {
            return Err(IvyError::new("invalid_arguments", "Instance data and artifact roots must be separate absolute paths."));
        }
        if let Some(version) = node_version {
            let mut parts = version.split('.');
            if parts.next() != Some("24") || parts.next().and_then(|part| part.parse::<u64>().ok()).is_none_or(|minor| minor < 18) {
                return Err(IvyError::new("unsupported_runtime", "This distribution requires pinned Node 24.18 or newer in the 24.x line."));
            }
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
