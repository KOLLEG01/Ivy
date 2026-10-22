//! Secretary's configuration values. Generic protected-file and host instance
//! loading are supplied by HostExecutor; this module owns document policy only.
use crate::{
    schema::{Schema, validate},
    triage_schema::{self, TriageSchema},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::config::protected_json_file;
use std::path::{Path, PathBuf};

pub const CONFIGURATION_BYTES: usize = 1024 * 1024;

pub fn service_configuration_path(data_root: &JsString) -> IvyResult<JsString> {
    let root = PathBuf::from(data_root.to_utf8()?);
    let absolute = std::path::absolute(root).map_err(|_| IvyError::internal())?;
    let parent = absolute.parent().unwrap_or(&absolute);
    Ok(parent.join("config.json").to_string_lossy().as_ref().into())
}

/// Host instance validation precedes this service-specific admission. Settings
/// are checked before the exact component/credential/host/service identity.
pub fn startup_settings(config: &IvyJson) -> IvyResult<&IvyJson> {
    let settings = config
        .get("settings")
        .ok_or_else(|| IvyError::new("invalid_arguments", "Value is outside the JSON domain."))?;
    validate(Schema::Settings, settings)?;
    let identity = settings.required("identity")?;
    if config.get("componentId") == Some(&"secretary".into())
        && config
            .get("credential")
            .is_some_and(|value| !value.is_null())
        && config.get("serviceNodeId") == identity.get("serviceNodeId")
        && config.get("hostId") == identity.get("hostId")
    {
        Ok(settings)
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Secretary needs its own component, credential and exact configured host/service identity.",
        ))
    }
}

/// Preserve absence separately from JSON null. The generic protected reader
/// returns None only for an absent file/ancestor, before this envelope check.
pub fn service_configuration(
    value: Option<IvyJson>,
    invalid: IvyError,
) -> IvyResult<Option<IvyJson>> {
    match value {
        None => Ok(None),
        Some(value)
            if value.as_object().is_some()
                && value.get("schemaVersion") == Some(&IvyJson::Number(1.0)) =>
        {
            Ok(Some(value))
        }
        Some(_) => Err(invalid),
    }
}
pub async fn load_service_configuration(
    path: impl Into<PathBuf>,
    maximum_bytes: usize,
    invalid: IvyError,
) -> IvyResult<Option<IvyJson>> {
    let value = protected_json_file(path, maximum_bytes, invalid.clone()).await?;
    service_configuration(value, invalid)
}
pub fn minimum_message_age_minutes(value: Option<&IvyJson>) -> IvyResult<f64> {
    let minutes = match value {
        None | Some(IvyJson::Null) => Some(5.0),
        Some(IvyJson::Number(value))
            if value.is_finite() && value.fract() == 0.0 && (0.0..=1440.0).contains(value) =>
        {
            Some(*value)
        }
        _ => None,
    };
    minutes.ok_or_else(||IvyError::new("secretary_delivery_config_invalid","Secretary minimum message age must be a whole number of minutes between zero and one day."))
}
pub fn delivery_configuration(config: Option<&IvyJson>) -> IvyResult<f64> {
    minimum_message_age_minutes(config.and_then(|value| value.get("minimumMessageAgeMinutes")))
}
pub fn triage_configuration(value: &IvyJson) -> IvyResult<IvyJson> {
    triage_schema::validate(TriageSchema::Settings, value)?;
    triage_schema::check(
        Path::new(&value.required("threadCwd")?.as_str()?).is_absolute(),
        "secretary_triage_config_invalid",
    )?;
    Ok(value.clone())
}
pub fn configured_triage(config: Option<&IvyJson>) -> IvyResult<Option<IvyJson>> {
    config
        .and_then(|value| value.get("triage"))
        .map(triage_configuration)
        .transpose()
}
pub fn unprotected_configuration(section: &str) -> IvyError {
    IvyError::new(
        format!("secretary_{section}_config_unprotected"),
        "Secretary service configuration must be a bounded protected regular file.",
    )
}
pub async fn load_minimum_message_age_minutes(path: impl Into<PathBuf>) -> IvyResult<f64> {
    let config = load_service_configuration(
        path,
        CONFIGURATION_BYTES,
        unprotected_configuration("delivery"),
    )
    .await?;
    delivery_configuration(config.as_ref())
}
pub async fn load_triage_configuration(path: impl Into<PathBuf>) -> IvyResult<Option<IvyJson>> {
    let config = load_service_configuration(
        path,
        CONFIGURATION_BYTES,
        unprotected_configuration("triage"),
    )
    .await?;
    configured_triage(config.as_ref())
}

pub fn microsoft_configuration(value: &IvyJson, settings: &IvyJson) -> IvyResult<IvyJson> {
    use crate::{
        microsoft_schema::validate_microsoft,
        outlook_projection::{microsoft_profile, outlook_arguments},
    };
    use ivy_sdk::clock::{date_to_iso, parse_date};
    use std::collections::HashSet;
    validate_microsoft("MicrosoftConfiguration", value)?;
    let collectors = value
        .required("collectors")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let mut ids = HashSet::new();
    if !collectors.iter().all(|v| {
        ids.insert(
            v.required("sourceId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
        )
    }) {
        return Err(IvyError::new(
            "microsoft_source_config_invalid",
            "Microsoft source IDs must be unique.",
        ));
    }
    for collector in collectors {
        let source = settings
            .required("sources")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|v| v.get("sourceId") == collector.get("sourceId"));
        microsoft_profile(collector.required("profile")?)?;
        let admitted = source.is_some_and(|s| {
            s.get("kind") == Some(&"email".into())
                && s.get("accountId") == collector.get("profile").and_then(|p| p.get("id"))
                && s.get("producerPrincipalIds")
                    .and_then(IvyJson::as_array)
                    .is_some_and(|v| {
                        v.iter().any(|id| {
                            Some(id) == settings.get("identity").and_then(|v| v.get("principalId"))
                        })
                    })
        });
        if !admitted || !Path::new(&collector.required("threadCwd")?.as_str()?).is_absolute() {
            return Err(IvyError::new(
                "microsoft_source_config_invalid",
                "An Outlook collector requires this admitted producer, its exact account and an absolute thread directory.",
            ));
        }
        let since = collector.required("since")?;
        let later =
            date_to_iso(parse_date(since.as_js_string().ok_or_else(IvyError::internal)?) + 1.0)
                .map_err(|_| IvyError::internal())?;
        outlook_arguments(&IvyJson::object([
            ("since", since.clone()),
            ("until", IvyJson::String(later)),
            ("skip", IvyJson::Number(0.0)),
            ("pageSize", collector.required("pageSize")?.clone()),
        ]))?;
    }
    Ok(value.clone())
}
pub fn teams_configuration(value: &IvyJson, _settings: &IvyJson) -> IvyResult<IvyJson> {
    crate::microsoft_schema::validate_microsoft("TeamsConfiguration", value)?;
    if !value
        .required("collectors")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .is_empty()
    {
        return Err(IvyError::new(
            "teams_native_source_not_ready",
            "Use an admitted source producer. Native Teams collection is disabled until complete pagination and media coverage are supported.",
        ));
    }
    Ok(value.clone())
}
fn empty_collectors() -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("collectors", IvyJson::Array(vec![])),
    ])
}
pub fn configured_microsoft(config: Option<&IvyJson>, settings: &IvyJson) -> IvyResult<IvyJson> {
    config.and_then(|v| v.get("microsoft")).map_or_else(
        || Ok(empty_collectors()),
        |v| microsoft_configuration(v, settings),
    )
}
pub fn configured_teams(config: Option<&IvyJson>, settings: &IvyJson) -> IvyResult<IvyJson> {
    config.and_then(|v| v.get("teams")).map_or_else(
        || Ok(empty_collectors()),
        |v| teams_configuration(v, settings),
    )
}
pub async fn load_microsoft_configuration(
    path: impl Into<PathBuf>,
    settings: &IvyJson,
) -> IvyResult<IvyJson> {
    let config = load_service_configuration(
        path,
        CONFIGURATION_BYTES,
        IvyError::new(
            "microsoft_config_unprotected",
            "Secretary service configuration must be a bounded protected regular file.",
        ),
    )
    .await?;
    configured_microsoft(config.as_ref(), settings)
}
pub async fn load_teams_configuration(
    path: impl Into<PathBuf>,
    settings: &IvyJson,
) -> IvyResult<IvyJson> {
    let config = load_service_configuration(
        path,
        CONFIGURATION_BYTES,
        IvyError::new(
            "teams_config_unprotected",
            "Secretary service configuration must be a bounded protected regular file.",
        ),
    )
    .await?;
    configured_teams(config.as_ref(), settings)
}
