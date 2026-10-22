//! Assignment rule inheritance, trigger admission and durable names.
use crate::schema::{Schema, validate};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::clock::TimeZone;
use std::collections::HashSet;

fn need(condition: bool, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new("secretary_assignment_invalid", message))
    }
}

fn time_zone(value: &IvyJson) -> IvyResult<()> {
    need(
        value
            .as_str()
            .is_ok_and(|zone| TimeZone::local_parts(0, &zone).is_ok()),
        "Secretary rules and schedules need a supported IANA time zone.",
    )
}

fn window(value: Option<&IvyJson>) -> IvyResult<()> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(());
    };
    time_zone(value.required("timeZone")?)?;
    need(
        value.get("start") != value.get("end"),
        "A Secretary contact window needs distinct start and end times.",
    )
}

pub fn validate_rules(value: &IvyJson) -> IvyResult<()> {
    validate(Schema::GlobalRules, value)?;
    for channel in ["main", "whatsapp", "voice"] {
        window(value.required(channel)?.get("window"))?;
    }
    Ok(())
}

pub fn validate_execution_target(value: &IvyJson) -> IvyResult<()> {
    validate(Schema::ExecutionTarget, value)?;
    let path = value.required("threadCwd")?.as_str()?;
    let absolute = std::path::Path::new(&path).is_absolute();
    need(
        absolute,
        "Secretary task working directories must be absolute.",
    )
}

pub fn validate_configuration(value: &IvyJson) -> IvyResult<()> {
    validate(Schema::SecretaryConfiguration, value)?;
    validate_rules(value.required("rules")?)?;
    if let Some(execution) = value.get("execution").filter(|v| !v.is_null()) {
        validate_execution_target(execution)?;
    }
    Ok(())
}

/// Settings have already been admitted by the service. Retain the original
/// inverted silent window and validate the completed configuration afterward.
pub fn default_configuration(
    settings: &IvyJson,
    now: &JsString,
    execution: Option<&IvyJson>,
) -> IvyResult<IvyJson> {
    let policy = settings.required("policy")?;
    let window = match policy.get("silentTime").filter(|v| !v.is_null()) {
        Some(silent) => IvyJson::object([
            ("timeZone", silent.required("timeZone")?.clone()),
            ("start", silent.required("end")?.clone()),
            ("end", silent.required("start")?.clone()),
        ]),
        None => IvyJson::Null,
    };
    let main = IvyJson::object([
        ("enabled", true.into()),
        (
            "minimumUrgency",
            policy.required("minimumMainUrgency")?.clone(),
        ),
        ("window", window.clone()),
    ]);
    let mut whatsapp = main.clone();
    whatsapp.insert("questionsOnly", false.into());
    let value = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("execution", execution.cloned().unwrap_or(IvyJson::Null)),
        ("browserNotifications", true.into()),
        ("updatedAt", IvyJson::String(now.clone())),
        (
            "rules",
            IvyJson::object([
                ("main", main),
                ("whatsapp", whatsapp),
                (
                    "voice",
                    IvyJson::object([
                        ("enabled", true.into()),
                        ("minimumUrgency", "critical".into()),
                        ("window", window),
                        ("immediateOnly", true.into()),
                    ]),
                ),
                (
                    "researchMaxMinutes",
                    policy.required("researchMaxMinutes")?.clone(),
                ),
            ]),
        ),
    ]);
    validate_configuration(&value)?;
    Ok(value)
}

/// Inputs are admitted GlobalRules/RuleOverrides records; merges keep the same
/// shallow override semantics and detached values as the original structuredClone.
pub fn unified_main_rules(value: &IvyJson) -> IvyJson {
    let mut result = value.clone();
    let mut main = value.get("main").cloned().unwrap_or(IvyJson::Null);
    result.insert("main", main.clone());
    main.insert("questionsOnly", false.into());
    result.insert("whatsapp", main);
    result
}

pub fn unified_rule_overrides(value: &IvyJson) -> IvyJson {
    let mut result = value.clone();
    result.remove("whatsapp");
    result
}

fn merge(base: &IvyJson, override_value: Option<&IvyJson>) -> IvyJson {
    let mut result = base.clone();
    if let Some(pairs) = override_value.and_then(IvyJson::as_object) {
        for (key, value) in pairs {
            result.insert(key.clone(), value.clone());
        }
    }
    result
}

pub fn effective_rules(defaults: &IvyJson, overrides: &IvyJson) -> IvyResult<IvyJson> {
    let main = merge(defaults.required("main")?, overrides.get("main"));
    let mut whatsapp = main.clone();
    whatsapp.insert("questionsOnly", false.into());
    let result = IvyJson::object([
        ("main", main),
        ("whatsapp", whatsapp),
        (
            "voice",
            merge(defaults.required("voice")?, overrides.get("voice")),
        ),
        (
            "researchMaxMinutes",
            overrides
                .get("researchMaxMinutes")
                .filter(|value| !value.is_null())
                .unwrap_or(defaults.required("researchMaxMinutes")?)
                .clone(),
        ),
    ]);
    validate_rules(&result)?;
    Ok(result)
}

fn unique(values: &[IvyJson]) -> bool {
    // Admission has already limited these arrays to strings or small integers.
    let mut strings = HashSet::new();
    let mut numbers = HashSet::new();
    values.iter().all(|value| match value {
        IvyJson::String(text) => strings.insert(text),
        IvyJson::Number(number) => numbers.insert(*number as i64),
        _ => false,
    })
}

fn pointer(path: &JsString) -> bool {
    if path.0.is_empty() {
        return true;
    }
    path.0[0] == u16::from(b'/')
        && path.0.iter().enumerate().all(|(index, unit)| {
            *unit != u16::from(b'~') || matches!(path.0.get(index + 1), Some(48 | 49))
        })
}

pub fn validate_trigger(value: &IvyJson) -> IvyResult<()> {
    validate(Schema::AssignmentTrigger, value)?;
    match value.required("kind")?.as_str()?.as_str() {
        "event" => return Ok(()),
        "object-change" => {
            let ids = value.required("objectIds")?.as_array().unwrap();
            let paths = value.required("paths")?.as_array().unwrap();
            need(
                unique(ids) && unique(paths),
                "Observed objects and paths must be unique.",
            )?;
            let mut paths: Vec<&IvyJson> = paths.iter().collect();
            if let Some(observation) = value.get("observation").filter(|v| !v.is_null()) {
                paths.push(observation.required("completePath")?);
                paths.push(observation.required("observedAtPath")?);
            }
            return need(
                paths
                    .iter()
                    .all(|path| pointer(path.as_js_string().unwrap())),
                "Observed paths must be JSON Pointers.",
            );
        }
        _ => {}
    }
    time_zone(value.required("timeZone")?)?;
    let cadence = value.required("cadence")?.as_str()?;
    let interval = value.required("intervalMinutes")?;
    let local_time = value.required("localTime")?;
    let weekdays = value.required("weekdays")?.as_array().unwrap();
    need(
        if cadence == "interval" {
            !interval.is_null() && local_time.is_null() && weekdays.is_empty()
        } else {
            interval.is_null()
                && !local_time.is_null()
                && if cadence == "daily" {
                    weekdays.is_empty()
                } else {
                    !weekdays.is_empty()
                }
        },
        "The Secretary schedule fields must match its selected cadence.",
    )?;
    need(
        unique(weekdays),
        "Secretary schedule weekdays must be unique.",
    )
}

pub fn validate_assignment(value: &IvyJson) -> IvyResult<()> {
    validate(Schema::Assignment, value)?;
    validate_trigger(value.required("trigger")?)?;
    let rules = value.required("rules")?;
    for channel in ["main", "voice"] {
        window(rules.get(channel).and_then(|rule| rule.get("window")))?;
    }
    need(
        value
            .required("assignmentId")?
            .as_js_string()
            .unwrap()
            .utf16_len()
            <= 256,
        "Secretary assignment identities are too long.",
    )
}

pub fn assignment_name(assignment_id: &JsString) -> IvyResult<String> {
    Ok(format!(
        "assignment-{}",
        &hash_json(&IvyJson::String(assignment_id.clone()))?[7..]
    ))
}

pub fn execution_name(assignment_id: &JsString, trigger_key: &JsString) -> IvyResult<String> {
    Ok(format!(
        "execution-{}",
        &hash_json(&IvyJson::Array(vec![
            IvyJson::String(assignment_id.clone()),
            IvyJson::String(trigger_key.clone())
        ]))?[7..]
    ))
}
