//! Immutable launch configuration and release-bound native settings.
use crate::{
    IvyError, IvyJson, IvyResult, accepted_configuration, config,
    journal::HostJournal,
    layout,
    native_path::{self, Style},
    paths, streaming_hash,
};
use ivy_contracts::{JsString, core_schema::validate_host};
use std::{fs, path::PathBuf};

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn now() -> IvyJson {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .into()
}
fn selected<'a>(value: &'a IvyJson, key: &str) -> Option<&'a IvyJson> {
    value.get(key).filter(|value| !value.is_null())
}

/// The caller supplies the selected artifact and original settings. Only the
/// release-owned phone executable is rebound; input values remain unchanged.
pub async fn candidate_bound_settings(
    component: JsString,
    value: IvyJson,
    artifact_root: PathBuf,
) -> IvyResult<IvyJson> {
    internal(
        tokio::spawn(async move {
            if component != JsString::from("phone-bridge") {
                return Ok(value);
            }
            if value.is_null() {
                return Err(IvyError::internal());
            }
            let native = value.get("native");
            require(
                native.is_some_and(|native| native.as_object().is_some()),
                "invalid_arguments",
                "Phone settings require their release-owned native executable.",
            )?;
            let executable = native
                .unwrap()
                .get("executable")
                .and_then(IvyJson::as_js_string)
                .map(native_path::to_native);
            require(
                executable
                    .as_ref()
                    .is_some_and(|path| !path.as_os_str().is_empty() && !path.is_absolute()),
                "invalid_arguments",
                "Phone executable must be relative to its selected artifact.",
            )?;
            let executable = executable.unwrap();
            let actual = internal(
                tokio::task::spawn_blocking(move || -> IvyResult<PathBuf> {
                    let root = internal(paths::realpath(&artifact_root))?;
                    let path = root.join(executable);
                    let path =
                        paths::absolute(&path).unwrap_or_else(|_| native_path::lexical(&path));
                    let actual = internal(paths::realpath(&path))?;
                    require(
                        internal(paths::inside(&root, &actual))?
                            && internal(fs::metadata(&actual))?.is_file(),
                        "target_conflict",
                        "Phone executable leaves its selected artifact.",
                    )?;
                    Ok(actual)
                })
                .await,
            )??;
            let hash = streaming_hash::file_hash(actual).await?;
            let mut settings = value;
            let mut native = settings.required("native")?.clone();
            native.insert("executableHash", hash.into());
            settings.insert("native", native);
            Ok(settings)
        })
        .await,
    )?
}

#[derive(Clone)]
pub struct MaterializedTarget {
    pub target: IvyJson,
    pub config: IvyJson,
}

/// Freeze host/instance/candidate/manifest together before the first filesystem
/// await. Account defaults use the host boundary's explicit selected home.
/// Once admitted, publishing both files survives a dropped waiter.
pub async fn materialize_target(
    journal: HostJournal,
    instance_id: JsString,
    candidate_id: String,
    enabled: bool,
    basis: Option<IvyJson>,
    account_home: JsString,
) -> IvyResult<MaterializedTarget> {
    internal(
        tokio::spawn(async move {
            let (host, instance, candidate, manifest) = journal
                .materialization_snapshot(&instance_id, &candidate_id)
                .await?;
            let basis = basis.filter(|value| !value.is_null());
            let revision = uuid::Uuid::new_v4().to_string();
            let paths = layout::service_paths(&host, &instance)?;
            if let Some(basis) = &basis {
                require(
                    basis.get("instanceId") == Some(&IvyJson::String(instance_id.clone()))
                        && basis.get("componentId") == instance.get("componentId")
                        && basis.get("hostId") == host.get("hostId"),
                    "target_conflict",
                    "Rollback configuration does not belong to this runtime instance.",
                )?;
            }
            let component = text(instance.required("componentId")?)?;
            let settings = if let Some(settings) =
                basis.as_ref().and_then(|basis| selected(basis, "settings"))
            {
                settings.clone()
            } else if component == JsString::from("agent-manager") {
                layout::agent_manager_account_settings(
                    instance.required("settings")?,
                    &account_home,
                    Style::native(),
                )?
            } else if ["host-executor", "service-manager"]
                .iter()
                .any(|name| component == JsString::from(*name))
            {
                let mut settings = instance.required("settings")?.clone();
                settings.insert(
                    "hostConfigPath",
                    match selected(&settings, "hostConfigPath") {
                        Some(value) => value.clone(),
                        None => layout::host_configuration_path_for_style(&host, Style::native())?,
                    },
                );
                settings
            } else {
                instance.required("settings")?.clone()
            };
            let settings = candidate_bound_settings(
                component,
                settings,
                config::path_value(candidate.required("artifactRoot")?)?,
            )
            .await?;
            let mut config = basis
                .clone()
                .unwrap_or_else(|| IvyJson::object(Vec::<(String, IvyJson)>::new()));
            for (key, value) in [
                ("schemaVersion", IvyJson::Number(1.0)),
                ("instanceId", IvyJson::String(instance_id.clone())),
                ("componentId", instance.required("componentId")?.clone()),
                ("hostId", host.required("hostId")?.clone()),
                ("artifactRoot", candidate.required("artifactRoot")?.clone()),
                ("buildId", candidate.required("buildId")?.clone()),
                ("version", manifest.required("version")?.clone()),
                ("settings", settings),
            ] {
                config.insert(key, value);
            }
            for (key, fallback) in [
                ("serviceNodeId", instance.required("serviceNodeId")?),
                ("publicBaseUrl", host.required("publicBaseUrl")?),
                ("dataRoot", paths.required("data")?),
                ("workRoot", paths.required("work")?),
                ("logsRoot", paths.required("logs")?),
            ] {
                config.insert(
                    key,
                    basis
                        .as_ref()
                        .and_then(|basis| selected(basis, key))
                        .unwrap_or(fallback)
                        .clone(),
                );
            }
            let credential = if let Some(basis) = &basis {
                basis.get("credential")
            } else {
                instance.get("credential")
            };
            if let Some(credential) = credential.filter(|value| !value.is_null()) {
                config.insert("credential", credential.clone());
            }
            validate_host("InstanceConfig", &config)?;
            let config_path = native_path::lexical(
                &config::path_value(host.required("runtimeRoot")?)?
                    .join("instances")
                    .join(native_path::to_native(&instance_id))
                    .join("configurations")
                    .join(&revision)
                    .join("config.json"),
            );
            config::atomic_json(config_path.clone(), config.clone()).await?;
            let snapshot_host = host.clone();
            let snapshot_path = config_path.clone();
            let snapshot_revision = revision.clone();
            internal(
                tokio::task::spawn_blocking(move || {
                    accepted_configuration::save_accepted_instance_configuration(
                        &snapshot_host,
                        &snapshot_path,
                        &snapshot_revision,
                    )
                })
                .await,
            )??;
            let target = IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("instanceId", IvyJson::String(instance_id)),
                ("revision", revision.into()),
                ("candidateId", candidate_id.into()),
                (
                    "desired",
                    if enabled { "running" } else { "stopped" }.into(),
                ),
                (
                    "configPath",
                    IvyJson::String(native_path::from_native(&config_path)),
                ),
                ("requestedAt", now()),
            ]);
            validate_host("RuntimeTarget", &target)?;
            Ok(MaterializedTarget { target, config })
        })
        .await,
    )?
}

/// Retain the selected candidate/configuration and invalidate only its launch.
pub fn stopped_target(target: &IvyJson) -> IvyJson {
    let mut target = target.clone();
    target.insert("revision", uuid::Uuid::new_v4().to_string().into());
    target.insert("desired", "stopped".into());
    target.insert("requestedAt", now());
    target
}
