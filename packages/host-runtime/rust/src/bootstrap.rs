//! Bootstrap plan composition and exact Linux direct-unit bytes.
use crate::{
    IvyError, IvyJson, IvyResult,
    linux_host::trimmed,
    linux_resources::{self, installation_identity},
    linux_service_identity::linux_process_user,
    node_path::{self, Context, Style},
};
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json};
use std::{fs, path::Path};
mod installation;
pub(crate) use installation::BootstrapInstallation;

fn fs_path(path: &Path) -> std::path::PathBuf {
    node_path::filesystem_path(&node_path::from_native(path))
}

/// Selected authority precedes immutable history. Pending atomic writes are
/// retained but never interpreted; stale contract/hash/installation plans have
/// no execution authority. Other I/O and syntax errors remain failures.
pub async fn retained_bootstrap_plans(config: IvyJson) -> IvyResult<Vec<IvyJson>> {
    tokio::task::spawn_blocking(move || {
        let identity = installation_identity(&config)?;
        let runtime = crate::config::path_value(config.required("runtimeRoot")?)?;
        let selected = runtime.join("bootstrap").join(format!("{identity}.json"));
        let root = runtime.join("bootstrap-plans");
        let mut entries = match fs::read_dir(fs_path(&root)) {
            Ok(entries) => entries.collect::<Result<Vec<_>, _>>().map_err(|_| IvyError::internal())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err(IvyError::internal()),
        };
        let accepted = |name: &str| name.len() == 69 && name.ends_with(".json")
            && name.as_bytes()[..64].iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b));
        let pending = |name: &str| name.len() == 110 && name.ends_with(".tmp")
            && name.as_bytes().get(69) == Some(&b'.') && accepted(&name[..69])
            && name.as_bytes()[70..106].iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b) || *b == b'-');
        // Name admission is ASCII, so rejecting non-ASCII before slicing also
        // preserves Node's regexp behavior for malformed native filename bytes.
        require(entries.len() <= 1000 && entries.iter().all(|entry| {
            let name = entry.file_name();
            let name = name.to_str().unwrap_or("");
            name.is_ascii() && entry.file_type().is_ok_and(|kind| kind.is_file())
                && (accepted(name) || pending(name))
        }), "storage_invalid", "Invalid retained bootstrap plan set.")?;
        entries.sort_by_key(|entry| entry.file_name());
        let mut paths = Vec::new();
        match fs::symlink_metadata(fs_path(&selected)) {
            Ok(_) => paths.push(selected.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => return Err(IvyError::internal()),
        }
        for entry in entries {
            if entry.file_name().to_str().is_some_and(accepted) {
                paths.push(root.join(entry.file_name()));
            }
        }
        let mut plans = Vec::new();
        let mut hashes = std::collections::BTreeSet::new();
        for path in paths {
            let read = || -> IvyResult<Option<(String, IvyJson)>> {
                let metadata = fs::symlink_metadata(fs_path(&path)).map_err(|_| IvyError::internal())?;
                require(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 65536,
                    "storage_invalid", "Retained bootstrap plan is not a bounded regular file.")?;
                let plan = crate::config::read_json(&path, crate::config::CONFIGURATION_BYTES)
                    .map_err(crate::config::FileError::error)?;
                validate_host("BootstrapPlan", &plan)?;
                let hash = hash_json(&plan)?;
                require(path == selected || path == root.join(format!("{}.json", &hash[7..])),
                    "storage_invalid", "Retained bootstrap plan hash changed.")?;
                let same = plan.get("installationId") == Some(&identity.clone().into())
                    && plan.get("hostId") == config.get("hostId") && plan.get("runtimeRoot") == config.get("runtimeRoot");
                require(path != selected || same, "storage_invalid", "Selected bootstrap plan belongs to a different installation.")?;
                if !same { return Ok(None); }
                if let Some(scope) = plan.get("linuxResources").filter(|v| truthy(v)) {
                    linux_resources::validate_linux_resource_scope(scope, Some(&identity))?;
                }
                Ok(Some((hash, plan)))
            };
            match read() {
                Ok(Some((hash, plan))) => { if hashes.insert(hash) { plans.push(plan); } },
                Ok(_) => {},
                Err(error) if ["invalid_arguments", "storage_invalid", "artifact_changed"].iter().any(|code| error.code == *code) => {},
                Err(error) => return Err(error),
            }
        }
        Ok(plans)
    }).await.map_err(|_| IvyError::internal())?
}
pub async fn retain_bootstrap_plan(plan: &IvyJson, job_launcher: std::path::PathBuf) -> IvyResult<()> {
    validate_host("BootstrapPlan", plan)?;
    if let Some(scope) = plan.get("linuxResources").filter(|value| truthy(value)) {
        linux_resources::validate_linux_resource_scope(scope, Some(&plan.required("installationId")?.as_str()?))?;
    }
    let root = crate::config::path_value(plan.required("runtimeRoot")?)?.join("bootstrap-plans");
    let path = root.join(format!("{}.json", &hash_json(plan)?[7..]));
    if !crate::backup_files::exists(&root).await? { crate::backup_files::private_directory(root, job_launcher).await?; }
    if crate::backup_files::exists(&path).await? {
        require(hash_json(&crate::config::json_file(path, crate::config::CONFIGURATION_BYTES).await?)? == hash_json(plan)?, "storage_invalid", "Retained bootstrap plan changed.")
    } else { crate::config::atomic_json(path, plan.clone()).await }
}

/// Publish only while all bootstrap owners are stopped. Each protected launch
/// file selects the same exact release as its OS definition.
pub async fn publish_bootstrap_instance_configurations(plan: &IvyJson, config: &IvyJson, journal: &crate::journal::HostJournal) -> IvyResult<()> {
    for process in plan.required("processes")?.as_array().ok_or_else(IvyError::internal)? {
        let Some(path) = process.get("configPath").filter(|value| truthy(value)) else { continue; };
        let instance = journal.instance(&process.required("instanceId")?.as_str()?).await?;
        let candidate = journal.candidate(&process.get("candidateId").filter(|value| !value.is_null()).unwrap_or(plan.required("candidateId")?).as_str()?).await?;
        let manifest = journal.manifest(&candidate.required("candidateId")?.as_str()?).await?;
        let paths = crate::layout::service_paths(config, &instance)?;
        let component = instance.required("componentId")?;
        let mut settings = instance.required("settings")?.clone();
        if component == &"agent-manager".into() {
            let context = crate::host_config::HostContext::current()?;
            settings = crate::layout::agent_manager_account_settings(&settings, &context.account_home, context.paths.style)?;
        } else if ["host-executor", "service-manager"].iter().any(|name| component == &IvyJson::from(*name)) {
            if settings.get("hostConfigPath").is_none_or(|value| value.is_null()) {
                settings.insert("hostConfigPath", crate::layout::host_configuration_path_for_style(config, Style::native())?);
            }
        }
        let mut value = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)), ("instanceId", instance.required("instanceId")?.clone()),
            ("serviceNodeId", instance.required("serviceNodeId")?.clone()), ("componentId", component.clone()),
            ("hostId", config.required("hostId")?.clone()), ("publicBaseUrl", config.required("publicBaseUrl")?.clone()),
            ("dataRoot", paths.required("data")?.clone()), ("workRoot", paths.required("work")?.clone()), ("logsRoot", paths.required("logs")?.clone()),
            ("artifactRoot", candidate.required("artifactRoot")?.clone()), ("buildId", candidate.required("buildId")?.clone()),
            ("version", manifest.required("version")?.clone()), ("settings", settings),
        ]);
        if let Some(credential) = instance.get("credential").filter(|value| truthy(value)) { value.insert("credential", credential.clone()); }
        validate_host("InstanceConfig", &value)?;
        crate::config::atomic_json(crate::config::path_value(path)?, value).await?;
    }
    Ok(())
}
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
/// Pick one retained release for each bootstrap component. The caller supplies
/// the shared default-locale comparator; no byte-order replacement is assumed.
pub async fn bootstrap_components(
    config: &IvyJson,
    journal: &crate::journal::HostJournal,
    host_candidate: &IvyJson,
    preferred: &[IvyJson],
    compare: &crate::artifact::FileCompare,
) -> IvyResult<Vec<IvyJson>> {
    let mut result = Vec::new();
    for instance in config.required("instances")?.as_array().ok_or_else(IvyError::internal)? {
        let component = instance.required("componentId")?;
        if instance.get("engine") != Some(&"process".into())
            || !(component == &"host-executor".into()
                || host_candidate.required("platform")?.get("os") == Some(&"win32".into()) && component == &"service-manager".into()) { continue; }
        let installed = journal.installed(&instance.required("instanceId")?.as_str()?).await?;
        let selected = preferred.iter().find(|value| value.get("candidate").and_then(|value| value.get("componentId")) == Some(component));
        let candidate = if Some(component) == host_candidate.get("componentId") { Some(host_candidate.clone()) }
            else if let Some(selected) = selected { Some(selected.required("candidate")?.clone()) }
            else if let Some(installed) = installed { Some(journal.candidate(&installed.required("candidateId")?.as_str()?).await?) }
            else {
                let mut rows = journal.retained_candidates(&component.as_str()?, 1000).await?;
                if rows.len() > 1 && rows.iter().any(|row| row.get("createdAt").and_then(IvyJson::as_js_string).is_none()) { return Err(IvyError::internal()); }
                // Both timestamps are schema strings, and stable ties retain the
                // journal's candidate-id order just as Array.sort does.
                rows.sort_by(|left, right| compare(right.get("createdAt").and_then(IvyJson::as_js_string).unwrap(), left.get("createdAt").and_then(IvyJson::as_js_string).unwrap()));
                rows.into_iter().next()
            };
        let Some(candidate) = candidate else {
            if instance.get("enabled").is_some_and(truthy) {
                return Err(IvyError::new("not_found", format!("No retained bootstrap candidate exists for {}.", component.as_str()?)));
            }
            continue;
        };
        let manifest = if selected.is_some_and(|selected| selected.get("candidate").and_then(|value| value.get("candidateId")) == candidate.get("candidateId")) {
            selected.unwrap().required("manifest")?.clone()
        } else { journal.manifest(&candidate.required("candidateId")?.as_str()?).await? };
        result.push(IvyJson::object([("candidate", candidate), ("manifest", manifest)]));
    }
    Ok(result)
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
            .map(string)
            .transpose()?
            .unwrap_or_else(|| "always".into())
    };
    let delay = restart.and_then(|restart| selected(restart, "minimumDelayMs"));
    let delay = match delay {
        None => 2000.0,
        Some(IvyJson::Number(value)) => *value,
        Some(_) => return Err(IvyError::new("invalid_arguments", "Restart delay must be a number.")),
    };
    require(delay.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&delay), "invalid_arguments", "Restart delay must be finite and nonnegative.")?;
    let delay = (delay / 1000.0).ceil().max(1.0);
    let mut parts = vec![
        "[Unit]\nDescription=IvyNext ".into(),
        string(instance.required("name")?)?,
        "\nAfter=network.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\n".into(),
    ];
    if let Some(user) = instance.get("user").filter(|value| truthy(value)) {
        let user = string(user)?;
        parts.extend([
            "User=".into(),
            user.clone(),
            "\nGroup=".into(),
            user,
            "\n".into(),
        ]);
    }
    parts.extend(["ExecStart=:".into(),JsString(arguments),"\nWorkingDirectory=".into(),directory,"\nRestart=".into(),policy,"\nRestartSec=".into(),delay.to_string().into(),"\nKillMode=control-group\nTimeoutStopSec=630\nUMask=0077\nNoNewPrivileges=true\nEnvironment=NODE_NO_WARNINGS=1\nEnvironment=IVY_LAUNCH_ID=".into(),launch,"\n".into()]);
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
