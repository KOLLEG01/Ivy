//! Storage locations shared by host admission and runtime supervision.
use crate::{IvyError, IvyJson, IvyResult};
use std::path::{Path, PathBuf};

pub(crate) fn path(value: &IvyJson) -> IvyResult<PathBuf> { crate::config::path_value(value) }
pub(crate) fn path_json(path: &Path) -> IvyJson {
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        IvyJson::String(ivy_contracts::JsString(path.as_os_str().encode_wide().collect()))
    }
    #[cfg(not(windows))] { path.to_string_lossy().as_ref().into() }
}
pub fn host_configuration_path(config: &IvyJson) -> IvyResult<PathBuf> {
    match config.get("configPath").filter(|v| !v.is_null()) {
        Some(value) => path(value),
        None => Ok(path(config.required("runtimeRoot")?)?.join("host.json")),
    }
}
/// `instance` is either a configured instance object or its instance ID.
pub fn service_paths(config: &IvyJson, instance: &IvyJson) -> IvyResult<IvyJson> {
    let instance = if instance.as_js_string().is_some() {
        config.required("instances")?.as_array().ok_or_else(IvyError::internal)?.iter()
            .find(|v| v.get("instanceId") == Some(instance))
            .ok_or_else(|| IvyError::new("not_found", "Unknown service instance for path resolution."))?
    } else { instance };
    if let Some(paths) = instance.get("paths").filter(|v| !v.is_null()) { return Ok(paths.clone()); }
    let base = match config.get("servicesRoot").filter(|v| v.as_js_string().is_some_and(|s| !s.0.is_empty())) {
        Some(root) => path(root)?.join(path(instance.required("componentId")?)?),
        None => path(config.required("runtimeRoot")?)?.join("instances").join(path(instance.required("instanceId")?)?),
    };
    Ok(IvyJson::object([("data", path_json(&base.join("data"))), ("work", path_json(&base.join("work"))), ("logs", path_json(&base.join("logs")))]))
}
