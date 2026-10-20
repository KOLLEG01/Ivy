//! Current SDK model projection; no static provider-model fallback.
use super::{
    error::{AdapterError, Result},
    value::{field, string, truthy},
};
use ivy_contracts::{IvyJson, JsString};

fn context_alias(value: &JsString) -> bool {
    let mut units = value.0.as_slice();
    if units.last() == Some(&10) {
        units = &units[..units.len() - 1];
    }
    units.last() == Some(&93)
        && units.iter().enumerate().any(|(i, c)| {
            *c == 91
                && i + 2 < units.len()
                && !units[i + 1..units.len() - 1]
                    .iter()
                    .any(|c| [10, 13, 0x2028, 0x2029].contains(c))
        })
}
fn id(model: &IvyJson) -> IvyJson {
    if truthy(field(model, "resolvedModel")) && !context_alias(&string(field(model, "value"))) {
        field(model, "resolvedModel").clone()
    } else {
        field(model, "value").clone()
    }
}
fn version(value: &JsString) -> Option<JsString> {
    let mut units = value.0.as_slice();
    if units.last() == Some(&10) {
        units = &units[..units.len() - 1];
    }
    if units.len() >= 9
        && units[units.len() - 9] == 45
        && units[units.len() - 8..]
            .iter()
            .all(|c| (48..=57).contains(c))
    {
        units = &units[..units.len() - 9];
    }
    let prefix = JsString::from("claude-");
    let rest = units.strip_prefix(prefix.0.as_slice())?;
    if !rest.first().is_some_and(|c| (97..=122).contains(c)) {
        return None;
    }
    let start = rest.iter().position(|c| (48..=57).contains(c))?;
    if start == 0
        || rest[start - 1] != 45
        || !rest[..start]
            .iter()
            .all(|c| (97..=122).contains(c) || *c == 45)
    {
        return None;
    }
    let numbers = &rest[start..];
    if numbers.last() == Some(&45)
        || numbers.windows(2).any(|w| w == [45, 45])
        || !numbers.iter().all(|c| (48..=57).contains(c) || *c == 45)
    {
        return None;
    }
    Some(JsString(
        numbers
            .iter()
            .map(|c| if *c == 45 { 46 } else { *c })
            .collect(),
    ))
}
pub fn runtime_model_list(
    models: &[IvyJson],
    configured_model: &JsString,
    configured_effort: &JsString,
) -> Result<IvyJson> {
    let first = models
        .first()
        .ok_or_else(|| AdapterError::new("Claude CLI returned no available models"))?;
    let default = models
        .iter()
        .find(|m| {
            field(m, "value") == &IvyJson::String(configured_model.clone())
                || id(m) == IvyJson::String(configured_model.clone())
        })
        .or_else(|| {
            models
                .iter()
                .find(|m| field(m, "value") == &IvyJson::from("default"))
        })
        .unwrap_or(first);
    let default = id(default);
    let mut rows: Vec<(IvyJson, &IvyJson)> = Vec::new();
    for model in models {
        let key = id(model);
        if let Some(existing) = rows.iter_mut().find(|(id, _)| id == &key) {
            if field(model, "value") != &IvyJson::from("default") {
                existing.1 = model;
            }
        } else {
            rows.push((key, model));
        }
    }
    let data = rows
        .into_iter()
        .map(|(id, model)| {
            let efforts = if field(model, "supportsEffort") == &IvyJson::Bool(false) {
                &[][..]
            } else {
                field(model, "supportedEffortLevels")
                    .as_array()
                    .unwrap_or(&[])
            };
            let resolved = if field(model, "resolvedModel").is_null() {
                &id
            } else {
                field(model, "resolvedModel")
            };
            let mut display = field(model, "displayName")
                .as_js_string()
                .cloned()
                .ok_or_else(|| {
                    AdapterError::typed(
                        "TypeError",
                        "Cannot read properties of undefined (reading 'includes')",
                    )
                })?;
            if let Some(version) = version(&string(resolved)) {
                if !display.0.windows(version.0.len()).any(|v| v == version.0) {
                    display.0.push(32);
                    display.0.extend(version.0);
                }
            }
            let mut row = IvyJson::object([
                ("id", id.clone()),
                ("model", id.clone()),
                ("upgrade", IvyJson::Null),
                ("upgradeInfo", IvyJson::Null),
                ("availabilityNux", IvyJson::Null),
                ("displayName", IvyJson::String(display)),
                ("hidden", false.into()),
                (
                    "supportedReasoningEfforts",
                    IvyJson::Array(
                        efforts
                            .iter()
                            .map(|effort| {
                                IvyJson::object([
                                    ("reasoningEffort", effort.clone()),
                                    ("description", effort.clone()),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "defaultReasoningEffort",
                    if efforts.contains(&IvyJson::String(configured_effort.clone())) {
                        IvyJson::String(configured_effort.clone())
                    } else {
                        efforts.first().cloned().unwrap_or_else(|| "none".into())
                    },
                ),
                (
                    "inputModalities",
                    IvyJson::Array(vec!["text".into(), "image".into()]),
                ),
                ("supportsPersonality", false.into()),
                ("additionalSpeedTiers", IvyJson::Array(vec![])),
                ("isDefault", (id == default).into()),
            ]);
            if let Some(description) = model.get("description") {
                row.insert("description", description.clone());
            }
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(IvyJson::object([
        ("data", IvyJson::Array(data)),
        ("nextCursor", IvyJson::Null),
    ]))
}
