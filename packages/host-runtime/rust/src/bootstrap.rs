//! Bootstrap plan composition and exact Linux direct-unit bytes.
use crate::{
    IvyError, IvyJson, IvyResult,
    linux_host::trimmed,
    linux_resources::{self, installation_identity},
    linux_service_identity::linux_process_user,
    node_path::{self, Context, Style},
};
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json, json};
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn string(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn field(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    string(value.required(key)?)
}
fn concat(parts: &[JsString]) -> JsString {
    JsString(
        parts
            .iter()
            .flat_map(|part| part.0.iter().copied())
            .collect(),
    )
}
fn selected<'a>(value: &'a IvyJson, key: &str) -> Option<&'a IvyJson> {
    value.get(key).filter(|value| !value.is_null())
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
pub fn configuration_bootstrap_plan(
    config: &IvyJson,
    path: &JsString,
    candidate: &IvyJson,
    resources: Option<&IvyJson>,
    components: &[IvyJson],
) -> IvyResult<IvyJson> {
    let identity = installation_identity(config)?;
    let os = candidate.required("platform")?.required("os")?;
    require(
        resources.is_none_or(|value| !truthy(value)) || os == &IvyJson::from("linux"),
        "unsupported_runtime",
        "A Linux resource budget requires a Linux bootstrap candidate.",
    )?;
    let mut processes = Vec::new();
    for instance in config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        let component = instance.required("componentId")?;
        if instance.get("engine") != Some(&"process".into())
            || !(component == &"host-executor".into()
                || os == &"win32".into() && component == &"service-manager".into())
            || !(Some(component) == candidate.get("componentId")
                || components.iter().any(|value| {
                    value
                        .get("manifest")
                        .and_then(|manifest| manifest.get("componentId"))
                        == Some(component)
                }))
        {
            continue;
        }
        let id = instance.required("instanceId")?;
        let mut process = IvyJson::object([
            ("instanceId", id.clone()),
            ("componentId", component.clone()),
            (
                "name",
                format!("{identity}-{}", &hash_json(id)?[7..19]).into(),
            ),
        ]);
        if let Some(user) = linux_process_user(&string(component)?, &os.as_str()?) {
            process.insert("user", user.into());
        }
        if let Some(selected) = components.iter().find(|value| {
            value
                .get("manifest")
                .and_then(|manifest| manifest.get("componentId"))
                == Some(component)
        }) {
            let manifest = selected.required("manifest")?;
            let candidate = selected.required("candidate")?;
            for (key, value) in [
                ("candidateId", candidate.required("candidateId")?),
                ("artifactRoot", candidate.required("artifactRoot")?),
            ] {
                process.insert(key, value.clone());
            }
            if let Some(entrypoint) = manifest.get("entrypoint") {
                process.insert("entrypoint", entrypoint.clone());
            }
            if let Some(restart) = manifest.get("restart").filter(|value| truthy(value)) {
                process.insert("restart", restart.clone());
            }
            process.insert(
                "configPath",
                IvyJson::String(node_path::join(
                    Style::native(),
                    &[
                        field(config, "runtimeRoot")?,
                        "bootstrap".into(),
                        identity.clone().into(),
                        "instances".into(),
                        concat(&[string(id)?, ".json".into()]),
                    ],
                )),
            );
        }
        if os == &"win32".into()
            && instance
                .get("process")
                .and_then(|value| value.get("allowWindowsBreakaway"))
                .is_some_and(truthy)
        {
            process.insert("allowWindowsBreakaway", true.into());
        }
        processes.push(process);
    }
    let mut plan = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("os", os.clone()),
        ("hostId", config.required("hostId")?.clone()),
        ("installationId", identity.clone().into()),
        ("candidateId", candidate.required("candidateId")?.clone()),
        ("artifactRoot", candidate.required("artifactRoot")?.clone()),
        (
            "configPath",
            IvyJson::String(
                Context::current()
                    .map_err(|_| IvyError::internal())?
                    .resolve(&[path.clone()]),
            ),
        ),
        ("configHash", hash_json(config)?.into()),
        ("runtimeRoot", config.required("runtimeRoot")?.clone()),
        ("processes", IvyJson::Array(processes)),
    ]);
    if let Some(node) = config.required("executables")?.get("node") {
        plan.insert("nodeExecutable", node.clone());
    }
    if let Some(resources) = resources.filter(|value| truthy(value)) {
        plan.insert(
            "linuxResources",
            linux_resources::linux_resource_scope(&identity, resources)?,
        );
    }
    validate_host("BootstrapPlan", &plan)?;
    Ok(plan)
}
fn unit_arg(value: &JsString) -> IvyResult<JsString> {
    require(
        !value.0.iter().any(|unit| *unit <= 31),
        "invalid_arguments",
        "Unit arguments cannot contain control characters.",
    )?;
    let mut result = vec![34];
    for unit in &value.0 {
        if [92, 34, 37].contains(unit) {
            result.push(if *unit == 37 { 37 } else { 92 });
        }
        result.push(*unit);
    }
    result.push(34);
    Ok(JsString(result))
}
pub fn linux_unit(
    plan: &IvyJson,
    instance: &IvyJson,
    launch_id: Option<JsString>,
) -> IvyResult<JsString> {
    let launch = match launch_id {
        Some(id) => id,
        None => {
            crate::journal::bootstrap_launch_id_js(plan, &field(instance, "instanceId")?)?.into()
        }
    };
    if let Some(resources) = plan.get("linuxResources").filter(|value| truthy(value)) {
        linux_resources::validate_linux_resource_scope(
            resources,
            Some(&plan.required("installationId")?.as_str()?),
        )?;
    }
    let artifact =
        string(selected(instance, "artifactRoot").unwrap_or(plan.required("artifactRoot")?))?;
    let component = field(instance, "componentId")?;
    let host = component == JsString::from("host-executor");
    let entry = selected(instance, "entrypoint");
    let executable = if entry.is_none()
        || entry.and_then(|value| value.get("executable")) == Some(&"node".into())
    {
        field(plan, "nodeExecutable")?
    } else {
        field(entry.unwrap(), "executable")?
    };
    let args = match entry {
        Some(entry) => entry
            .required("args")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(string)
            .collect::<IvyResult<Vec<_>>>()?,
        None => vec![if host {
            "dist/packages/host-runtime/src/executor.js".into()
        } else {
            concat(&["dist/services/".into(), component, "/src/main.js".into()])
        }],
    };
    let config = if host {
        field(plan, "configPath")?
    } else {
        string(selected(instance, "configPath").unwrap_or(plan.required("configPath")?))?
    };
    let mut argv = vec![executable];
    argv.extend(args);
    argv.extend(["--config".into(), config]);
    if host && instance.get("configPath").is_some_and(truthy) {
        argv.extend(["--instance-config".into(), field(instance, "configPath")?]);
    }
    let mut arguments = Vec::new();
    for (arg_index, arg) in argv.into_iter().enumerate() {
        if arg_index > 0 {
            arguments.push(32);
        }
        arguments.extend(unit_arg(&arg)?.0);
    }
    require(
        artifact.0.first() == Some(&47)
            && trimmed(&artifact) == artifact
            && !artifact.0.iter().any(|unit| *unit <= 31)
            && artifact.0.last() != Some(&92),
        "invalid_arguments",
        "Invalid systemd working directory.",
    )?;
    let directory = JsString(
        artifact
            .0
            .iter()
            .flat_map(|unit| {
                if *unit == 37 {
                    vec![37, 37]
                } else {
                    vec![*unit]
                }
            })
            .collect(),
    );
    let restart = instance.get("restart");
    let policy = restart.and_then(|restart| selected(restart, "policy"));
    let policy = if policy == Some(&IvyJson::from("never")) {
        "no".into()
    } else {
        policy
            .map(json::try_js_string)
            .transpose()?
            .unwrap_or_else(|| "always".into())
    };
    let delay = restart.and_then(|restart| selected(restart, "minimumDelayMs"));
    let delay = match delay {
        None => 2000.0,
        Some(IvyJson::Number(value)) => *value,
        Some(IvyJson::Bool(value)) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Some(value) => json::number_from_string(&json::try_js_string(value)?),
    };
    let delay = if delay.is_nan() {
        f64::NAN
    } else {
        (delay / 1000.0).ceil().max(1.0)
    };
    let mut parts = vec![
        "[Unit]\nDescription=IvyNext ".into(),
        json::try_js_string(instance.required("name")?)?,
        "\nAfter=network.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\n".into(),
    ];
    if let Some(user) = instance.get("user").filter(|value| truthy(value)) {
        let user = json::try_js_string(user)?;
        parts.extend([
            "User=".into(),
            user.clone(),
            "\nGroup=".into(),
            user,
            "\n".into(),
        ]);
    }
    parts.extend(["ExecStart=:".into(),JsString(arguments),"\nWorkingDirectory=".into(),directory,"\nRestart=".into(),policy,"\nRestartSec=".into(),json::try_js_string(&IvyJson::Number(delay))?,"\nKillMode=control-group\nTimeoutStopSec=630\nUMask=0077\nNoNewPrivileges=true\nEnvironment=NODE_NO_WARNINGS=1\nEnvironment=IVY_LAUNCH_ID=".into(),launch,"\n".into()]);
    if let Some(config) = instance.get("configPath").filter(|value| truthy(value)) {
        parts.extend([
            "Environment=IVY_INSTANCE_CONFIG=".into(),
            unit_arg(&string(config)?)?,
            "\n".into(),
        ]);
    }
    if let Some(scope) = plan.get("linuxResources").filter(|value| truthy(value)) {
        parts.extend(["Slice=".into(), field(scope, "slice")?, "\n".into()]);
    }
    parts.push("\n[Install]\nWantedBy=multi-user.target\n".into());
    Ok(concat(&parts))
}
pub fn replaceable_systemd_owner(content: &JsString, allow_legacy: bool) -> bool {
    let prefix = JsString::from("[Unit]\nDescription=IvyNext ");
    let owned = content.0.starts_with(&prefix.0);
    owned
        && (content.contains_ascii("ExecStart=:")
            && content.contains_ascii("WorkingDirectory=")
            && content.contains_ascii("Environment=NODE_NO_WARNINGS=1\n")
            && !content.contains_ascii("guardian.js")
            || allow_legacy
                && content.contains_ascii("guardian.js")
                && content.contains_ascii("Environment="))
}
