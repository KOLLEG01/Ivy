//! Storage locations shared by host admission and runtime supervision.
use crate::{IvyError, IvyJson, IvyResult};
use crate::node_path::{self, Style};
use ivy_contracts::JsString;
use std::{collections::BTreeMap, path::{Path, PathBuf}};
mod storage_api;
pub use storage_api::{StorageArea, StorageContext, host_storage_areas, storage_path, storage_file, service_paths, service_paths_for_style};
use storage_api::{text, joined, service_base};

pub(crate) fn path(value: &IvyJson) -> IvyResult<PathBuf> { crate::config::path_value(value) }
pub(crate) fn path_json(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn selected<'a>(value: &'a IvyJson, key: &str) -> Option<&'a IvyJson> { value.get(key).filter(|v| !v.is_null()) }
pub fn host_configuration_path(config: &IvyJson) -> IvyResult<PathBuf> {
    path(&host_configuration_path_for_style(config, Style::native())?)
}
pub fn host_configuration_path_for_style(config: &IvyJson, style: Style) -> IvyResult<IvyJson> {
    match selected(config, "configPath") {
        Some(value) => Ok(value.clone()),
        None => joined(style, &[config.required("runtimeRoot")?.clone(), "host.json".into()]),
    }
}
/// Unreduced permanent storage union for bidirectional overlap checks only.
/// Selecting stable storage keys/order is a separate locale-sensitive operation.
pub(crate) fn storage_overlap_roots(config: &IvyJson, account_home: impl Fn() -> IvyResult<JsString>) -> IvyResult<Vec<JsString>> {
    let mut roots = vec![text(config.required("runtimeRoot")?)?];
    for instance in config.required("instances")?.as_array().ok_or_else(IvyError::internal)? {
        roots.extend(service_paths(config, instance)?.as_object().ok_or_else(IvyError::internal)?.iter()
            .map(|(_, value)| text(value)).collect::<IvyResult<Vec<_>>>()?);
        if instance.get("componentId") == Some(&"agent-manager".into()) {
            roots.push(match instance.required("settings")?.get("internalProjectRoot").and_then(IvyJson::as_js_string) {
                Some(root) => root.clone(),
                None => node_path::join(Style::native(), &[account_home()?, ".ivy".into(), "codex-projects".into()]),
            });
        }
    }
    Ok(roots)
}

/// The owning process supplies its OS homedir result separately from the
/// environment overrides. This also permits resolving another host's config
/// without reading or changing this process's account or environment.
pub fn account_home(environment: &BTreeMap<JsString, JsString>, style: Style, os_home: &JsString) -> IvyResult<JsString> {
    let key = if style == Style::Windows { "USERPROFILE" } else { "HOME" };
    let home = environment.get(&JsString::from(key)).filter(|home| !home.0.is_empty()).unwrap_or(os_home);
    if !node_path::is_absolute(style, home) {
        return Err(IvyError::new("invalid_arguments", "The executing account must have an absolute user home."));
    }
    Ok(node_path::normalize(style, home))
}
pub fn default_host_config_path(environment: &BTreeMap<JsString, JsString>, style: Style, os_home: &JsString) -> IvyResult<JsString> {
    Ok(node_path::join(style, &[account_home(environment, style, os_home)?, ".ivy".into(), "config.json".into()]))
}
fn claude(settings: &IvyJson) -> bool { settings.get("appServer").and_then(|v| v.get("mode")) == Some(&"claude-adapter".into()) }
pub fn agent_manager_account_settings(settings: &IvyJson, home: &JsString, style: Style) -> IvyResult<IvyJson> {
    let mut result = settings.clone();
    for (key, suffix) in [("projectRoot", vec!["projects"]), ("internalProjectRoot", vec![".ivy", "codex-projects"])] {
        if selected(settings, key).is_none() {
            let mut parts = vec![home.clone()]; parts.extend(suffix.into_iter().map(JsString::from));
            result.insert(key, IvyJson::String(node_path::join(style, &parts)));
        }
    }
    if !claude(settings) {
        let skills = match selected(settings, "skillsRoot") {
            Some(value) => value.clone(),
            None => {
                let native_home = match selected(settings, "codexHome").or_else(|| selected(settings, "nativeHome")) {
                    Some(value) => value.as_js_string().cloned().ok_or_else(IvyError::internal)?,
                    None => node_path::join(style, &[home.clone(), ".codex".into()]),
                };
                IvyJson::String(node_path::join(style, &[native_home, "skills".into()]))
            }
        };
        result.insert("skillsRoot", skills);
    }
    Ok(result)
}

/// Resolve the accepted input exactly once at the host boundary. Optional old
/// roots retain the original layout; account defaults use the selected flavor.
pub fn resolve_host_configuration(input: &IvyJson, source_path: &JsString, environment: &BTreeMap<JsString, JsString>, style: Style, os_home: &JsString) -> IvyResult<IvyJson> {
    let home = account_home(environment, style, os_home)?;
    let modern = input.get("ivyRoot").is_some() || input.get("servicesRoot").is_some() || input.get("runtimeRoot").is_none()
        || node_path::normalize(style, source_path) == default_host_config_path(environment, style, os_home)?;
    let ivy_root = selected(input, "ivyRoot").cloned().unwrap_or_else(|| IvyJson::String(node_path::join(style, &[home.clone(), ".ivy".into()])));
    let mut config = input.clone();
    for (key, suffix) in [("runtimeRoot","runtime"),("artifactRoot","artifacts"),("stagingRoot","staging")] {
        if selected(input, key).is_none() { config.insert(key, joined(style, &[ivy_root.clone(), suffix.into()])?); }
    }
    if modern {
        config.insert("ivyRoot", ivy_root.clone());
        config.insert("servicesRoot", match selected(input, "servicesRoot") { Some(v) => v.clone(), None => joined(style, &[ivy_root.clone(), "services".into()])? });
        config.insert("configPath", selected(input, "configPath").cloned().unwrap_or_else(|| IvyJson::String(source_path.clone())));
    }
    for key in ["packageUpdates", "configurationUpdates"] {
        if selected(&config, key).is_none() { config.insert(key, IvyJson::object([("intervalSeconds", IvyJson::Number(60.0))])); }
    }
    let mut instances = Vec::new();
    for instance in input.required("instances")?.as_array().ok_or_else(IvyError::internal)? {
        let component = instance.required("componentId")?.as_str()?;
        let mut settings = crate::default_settings::default_service_settings_for_paths(&component, instance.required("settings")?, &text(&ivy_root)?, source_path, style)?;
        if component == "agent-manager" {
            let is_claude = claude(&settings);
            let data = match instance.get("paths").and_then(|p| selected(p, "data")) {
                Some(value) => value.clone(),
                None => joined(style, &[service_base(&config, instance, style)?, "data".into()])?,
            };
            let native_home = if is_claude { joined(style, &[data, "codex-home".into()])? }
                else { joined(style, &[ivy_root.clone(), "codex".into(), instance.required("instanceId")?.clone()])? };
            // Undefined and explicit null differ here in the original resolver.
            if settings.get("codexHome").is_none() && settings.get("nativeHome").is_none() { settings.insert("codexHome", native_home); }
            if selected(&settings, "internalProjectRoot").is_none() { settings.insert("internalProjectRoot", joined(style, &[ivy_root.clone(), "codex-projects".into()])?); }
            if selected(&settings, "appServer").is_none() { settings.insert("appServer", IvyJson::object([("mode", if style == Style::Windows { "external-proxy".into() } else { "owned-stdio".into() })])); }
            settings = agent_manager_account_settings(&settings, &home, style)?;
        }
        let mut instance = instance.clone(); instance.insert("settings", settings);
        let process_is_falsy = match instance.get("process") {
            None | Some(IvyJson::Null) | Some(IvyJson::Bool(false)) => true,
            Some(IvyJson::Number(n)) => *n == 0.0 || n.is_nan(),
            Some(IvyJson::String(s)) => s.0.is_empty(),
            _ => false,
        };
        if component == "service-manager" && instance.get("engine") == Some(&"process".into()) && style == Style::Windows && process_is_falsy {
            instance.insert("process", IvyJson::object([("allowWindowsBreakaway", true.into())]));
        }
        instances.push(instance);
    }
    config.insert("instances", IvyJson::Array(instances));
    for key in ["runtimeRoot","artifactRoot","stagingRoot","ivyRoot","servicesRoot","configPath"] {
        if let Some(value) = config.get(key) {
            if !node_path::is_absolute(style, &text(value)?) {
                return Err(IvyError::new("invalid_arguments", "Host directories and configuration path must be absolute."));
            }
        }
    }
    Ok(config)
}
