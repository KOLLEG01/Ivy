//! Exact unit identity, direct launch rendering and systemctl state decoding.
use crate::{
    IvyError, IvyJson, IvyResult, bootstrap,
    linux_host::trimmed,
    linux_resources::{installation_identity, validate_linux_resource_scope},
    linux_service_identity::linux_process_user,
};
use ivy_contracts::{JsString, hash::hash_json, json};
use std::collections::BTreeMap;
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null => false,
        IvyJson::Bool(value) => *value,
        IvyJson::Number(value) => *value != 0.0 && !value.is_nan(),
        IvyJson::String(value) => !value.0.is_empty(),
        _ => true,
    }
}
fn instance<'a>(config: &'a IvyJson, id: &JsString) -> IvyResult<Option<&'a IvyJson>> {
    Ok(config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .find(|instance| instance.get("instanceId") == Some(&IvyJson::String(id.clone()))))
}
pub fn systemd_unit_name(config: &IvyJson, id: &JsString) -> IvyResult<String> {
    require(
        instance(config, id)?.is_some_and(|value| value.get("engine") == Some(&"process".into())),
        "unsupported_runtime",
        "Only process instances have systemd units.",
    )?;
    Ok(format!(
        "{}-{}.service",
        installation_identity(config)?,
        &hash_json(&IvyJson::String(id.clone()))?[7..19]
    ))
}
#[derive(Clone, Debug, PartialEq)]
pub struct SystemdState {
    pub load_state: JsString,
    pub active_state: JsString,
    pub sub_state: JsString,
    pub main_pid: Option<u64>,
}
impl SystemdState {
    pub fn json(&self) -> IvyJson {
        IvyJson::object([
            ("loadState", IvyJson::String(self.load_state.clone())),
            ("activeState", IvyJson::String(self.active_state.clone())),
            ("subState", IvyJson::String(self.sub_state.clone())),
            (
                "mainPid",
                self.main_pid
                    .map(|pid| IvyJson::Number(pid as f64))
                    .unwrap_or(IvyJson::Null),
            ),
        ])
    }
}
pub fn parse_systemd_state(output: &JsString) -> SystemdState {
    let output = trimmed(output);
    let mut properties = BTreeMap::new();
    for line in output.0.split(|unit| *unit == 10) {
        let line = line.strip_suffix(&[13]).unwrap_or(line);
        if let Some(index) = line
            .iter()
            .position(|unit| *unit == 61)
            .filter(|index| *index > 0)
        {
            properties.insert(
                JsString(line[..index].to_vec()),
                JsString(line[index + 1..].to_vec()),
            );
        }
    }
    let pid = properties
        .get(&JsString::from("MainPID"))
        .map(json::number_from_string)
        .unwrap_or(0.0);
    let get = |key: &str| {
        properties
            .get(&JsString::from(key))
            .cloned()
            .unwrap_or_else(|| "unknown".into())
    };
    SystemdState {
        load_state: get("LoadState"),
        active_state: get("ActiveState"),
        sub_state: get("SubState"),
        main_pid: (pid.is_finite()
            && pid.fract() == 0.0
            && pid > 0.0
            && pid <= 9_007_199_254_740_991.0)
            .then_some(pid as u64),
    }
}
#[derive(Clone)]
pub struct RenderedSystemdUnit {
    pub unit: String,
    pub content: JsString,
}
pub fn render_systemd_unit(
    config: &IvyJson,
    id: &JsString,
    target: &IvyJson,
    candidate: &IvyJson,
    manifest: &IvyJson,
    resources: Option<&IvyJson>,
) -> IvyResult<RenderedSystemdUnit> {
    let resources = resources.filter(|value| truthy(value));
    require(
        cfg!(target_os = "linux")
            || candidate
                .get("platform")
                .and_then(|platform| platform.get("os"))
                == Some(&"linux".into()),
        "unsupported_runtime",
        "Systemd units require a Linux candidate.",
    )?;
    let instance = instance(config, id)?;
    require(
        instance.is_some_and(|instance| instance.get("engine") == Some(&"process".into())),
        "unsupported_runtime",
        "Only process instances can render systemd units.",
    )?;
    let instance = instance.unwrap();
    require(
        candidate
            .get("platform")
            .and_then(|platform| platform.get("os"))
            == Some(&"linux".into())
            && candidate.get("componentId") == instance.get("componentId")
            && target.get("instanceId") == Some(&IvyJson::String(id.clone()))
            && target.get("candidateId") == candidate.get("candidateId"),
        "target_conflict",
        "Systemd unit identity does not match the accepted target.",
    )?;
    require(
        manifest.get("componentId") == instance.get("componentId")
            && manifest.get("entrypoint").is_some_and(truthy),
        "target_conflict",
        "Systemd unit has no matching component entrypoint.",
    )?;
    let installation = installation_identity(config)?;
    if let Some(resources) = resources {
        validate_linux_resource_scope(resources, Some(&installation))?;
    }
    let name = systemd_unit_name(config, id)?;
    let mut process = IvyJson::object([
        ("instanceId", IvyJson::String(id.clone())),
        ("componentId", instance.required("componentId")?.clone()),
        ("name", name.strip_suffix(".service").unwrap().into()),
        ("candidateId", candidate.required("candidateId")?.clone()),
        ("artifactRoot", candidate.required("artifactRoot")?.clone()),
        ("entrypoint", manifest.required("entrypoint")?.clone()),
        ("configPath", target.required("configPath")?.clone()),
    ]);
    if let Some(user) = linux_process_user(&text(instance.required("componentId")?)?, "linux") {
        process.insert("user", user.into());
    }
    if let Some(restart) = manifest.get("restart").filter(|value| truthy(value)) {
        process.insert("restart", restart.clone());
    }
    let mut plan = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("os", "linux".into()),
        ("hostId", config.required("hostId")?.clone()),
        ("installationId", installation.into()),
        ("candidateId", candidate.required("candidateId")?.clone()),
        ("artifactRoot", candidate.required("artifactRoot")?.clone()),
        ("configPath", target.required("configPath")?.clone()),
        ("configHash", hash_json(config)?.into()),
        ("runtimeRoot", config.required("runtimeRoot")?.clone()),
        ("processes", IvyJson::Array(vec![process.clone()])),
    ]);
    if let Some(node) = config.required("executables")?.get("node") {
        plan.insert("nodeExecutable", node.clone());
    }
    if let Some(resources) = resources {
        plan.insert("linuxResources", resources.clone());
    }
    Ok(RenderedSystemdUnit {
        unit: name,
        content: bootstrap::linux_unit(&plan, &process, Some(text(target.required("revision")?)?))?,
    })
}
