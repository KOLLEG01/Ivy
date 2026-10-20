//! Current SDK model projection; no static provider-model fallback.
use super::{
    error::{AdapterError, Result},
    value::{coalesce, field, is, object, string, truthy},
};
use ivy_contracts::{IvyJson, JsString};

pub(super) fn json_value(value: &IvyJson) -> Option<IvyJson> {
    let Some(text) = value.as_js_string() else {
        return None;
    };
    // JSON.parse accepts literal UTF-16 surrogates inside strings. Escape code
    // units before the shared UTF-8 parser, retaining paired and lone values.
    let mut source = String::new();
    for unit in &text.0 {
        if (0xd800..=0xdfff).contains(unit) {
            source.push_str(&format!("\\u{unit:04x}"))
        } else {
            source.push(char::from_u32(u32::from(*unit)).unwrap())
        }
    }
    ivy_contracts::json::parse(&source, usize::MAX).ok()
}
fn json_object(value: &IvyJson) -> IvyJson {
    json_value(value)
        .filter(|v| v.as_object().is_some())
        .unwrap_or_else(object)
}
pub fn resolve_model(
    raw: &IvyJson,
    summary: bool,
    environment: &IvyJson,
    discovered: &[JsString],
) -> IvyJson {
    let raw = string(&coalesce(raw, &"".into())).trim();
    let default = || {
        let value = field(environment, "CLAUDE_CODEX_DEFAULT_MODEL");
        if truthy(value) {
            value.clone()
        } else {
            IvyJson::Null
        }
    };
    if summary
        && (raw.0.is_empty()
            || super::protocol::Adapter::codex_model(&IvyJson::String(raw.clone())))
    {
        for name in ["CLAUDE_CODEX_SUMMARY_MODEL", "CLAUDE_CODEX_TITLE_MODEL"] {
            let value = field(environment, name);
            if truthy(value) {
                return value.clone();
            }
        }
        return "haiku".into();
    }
    if raw.0.is_empty()
        || ["default", "claude-default", "claude-code", "custom"]
            .iter()
            .any(|v| raw == JsString::from(*v))
    {
        return default();
    }
    let aliases = json_object(field(environment, "CLAUDE_CODEX_MODEL_ALIASES"));
    let custom = aliases.get_key(&raw);
    let alias = if let Some(value) = custom.filter(|v| v.as_js_string().is_some()) {
        value.clone()
    } else {
        let built_in = match raw.to_utf8().ok().as_deref() {
            Some("claude-sonnet") => Some("sonnet"),
            Some("claude-opus") => Some("opus"),
            Some("claude-fable") => Some("fable"),
            Some("claude-haiku") => Some("haiku"),
            Some("sonnet-1m" | "claude-sonnet-1m") => Some("sonnet[1m]"),
            Some("opus-plan" | "claude-opus-plan") => Some("opusplan"),
            _ => None,
        };
        built_in.map(IvyJson::from).unwrap_or(IvyJson::Null)
    };
    if truthy(&alias) {
        return alias;
    }
    if let Some(configured) = field(environment, "CLAUDE_CODEX_MODELS").as_js_string() {
        for value in configured
            .0
            .split(|c| *c == 44)
            .map(|part| JsString(part.to_vec()).trim())
            .filter(|s| !s.0.is_empty())
        {
            if value.to_english_lowercase() == raw.to_english_lowercase() {
                return IvyJson::String(value);
            }
        }
    }
    for value in discovered {
        if value.to_english_lowercase() == raw.to_english_lowercase() {
            return IvyJson::String(value.clone());
        }
    }
    if super::protocol::Adapter::codex_model(&IvyJson::String(raw.clone())) {
        default()
    } else {
        IvyJson::String(raw)
    }
}
pub fn resolve_effort(raw: &IvyJson, environment: &IvyJson) -> IvyJson {
    let raw = string(&coalesce(raw, &"".into())).trim();
    let aliases = json_object(field(environment, "CLAUDE_CODEX_EFFORT_ALIASES"));
    let mapped = aliases
        .get_key(&raw)
        .filter(|v| v.as_js_string().is_some())
        .cloned()
        .unwrap_or_else(|| IvyJson::String(raw));
    if ["low", "medium", "high", "xhigh", "max"]
        .iter()
        .any(|v| is(&mapped, v))
    {
        mapped
    } else if is(&mapped, "minimal") {
        "low".into()
    } else {
        IvyJson::Null
    }
}
pub fn allowed_tools(environment: &IvyJson) -> IvyJson {
    let Some(raw) = field(environment, "CLAUDE_CODEX_ALLOWED_TOOLS").as_js_string() else {
        return IvyJson::Null;
    };
    let raw = raw.trim();
    if raw.0.is_empty()
        || raw == JsString::from("*")
        || raw.to_english_lowercase() == JsString::from("default")
    {
        return IvyJson::Null;
    }
    IvyJson::Array(
        raw.0
            .split(|c| *c == 44)
            .map(|part| JsString(part.to_vec()).trim())
            .filter(|s| !s.0.is_empty())
            .map(IvyJson::String)
            .collect(),
    )
}
pub fn add_dirs(environment: &IvyJson) -> IvyJson {
    let Some(raw) = field(environment, "CLAUDE_CODEX_ADD_DIRS").as_js_string() else {
        return IvyJson::Array(vec![]);
    };
    if let Some(IvyJson::Array(ref values)) = json_value(&IvyJson::String(raw.clone())) {
        let values: std::result::Result<Vec<_>, _> = values
            .iter()
            .map(ivy_contracts::json::try_js_string)
            .collect();
        if let Ok(values) = values {
            return IvyJson::Array(
                values
                    .into_iter()
                    .filter(|s| !s.0.is_empty())
                    .map(IvyJson::String)
                    .collect(),
            );
        }
    }
    IvyJson::Array(
        raw.0
            .split(|c| *c == 44)
            .map(|part| JsString(part.to_vec()).trim())
            .filter(|s| !s.0.is_empty())
            .map(IvyJson::String)
            .collect(),
    )
}

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
