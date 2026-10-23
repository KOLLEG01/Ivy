//! Main context composition from object templates and UTF-8 instruction text.
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{parse, stringify},
};
use std::{collections::HashSet, path::Path};

pub fn load_main_instructions(distribution_root: &Path) -> IvyResult<JsString> {
    let text =
        std::fs::read_to_string(distribution_root.join("instructions/chat-main-instructions.json"))
            .map_err(|_| IvyError::internal())?;
    parse(&text, usize::MAX)
        .and_then(|value| value.as_str())
        .map(Into::into)
        .map_err(|_| IvyError::internal())
}

/// Optional object settings retain their fields; absent settings start empty.
pub(crate) fn object_options(value: &Json) -> Json {
    value
        .as_object()
        .map(|fields| Json::Object(fields.to_vec()))
        .unwrap_or_else(|| Json::Object(Vec::new()))
}
pub(crate) fn require_object(value: &Json) -> IvyResult<()> {
    value.as_object().map(|_| ()).ok_or_else(IvyError::internal)
}

pub fn main_input_origin(caller: &JsString, request: &Json) -> IvyResult<JsString> {
    require_object(request)?;
    if request.get("action") != Some(&Json::from("notify")) {
        return Ok("conversation input".into());
    }
    let source = stringify(request.required("source")?, usize::MAX)?;
    Ok(format!(
        "service notification from {}; source {source}",
        caller.to_utf8()?
    )
    .into())
}

/// The restriction belongs to Main's native thread and retains other settings.
pub fn main_thread_options(template: &Json, instructions: &JsString) -> IvyResult<Json> {
    require_object(template)?;
    let mut config = template
        .get("config")
        .map(object_options)
        .unwrap_or_else(|| Json::Object(Vec::new()));
    let mut names = HashSet::new();
    let mut disabled = Vec::new();
    for value in config
        .get("mcp_servers.ivy.disabled_tools")
        .and_then(Json::as_array)
        .unwrap_or(&[])
    {
        let name = value.as_str()?;
        if names.insert(name.clone()) {
            disabled.push(Json::from(name));
        }
    }
    if names.insert("chat_bridge_send".to_owned()) {
        disabled.push("chat_bridge_send".into());
    }
    config.insert("mcp_servers.ivy.disabled_tools", Json::Array(disabled));
    let instructions = instructions.to_utf8()?;
    let developer = template
        .get("developerInstructions")
        .filter(|value| !value.is_null())
        .map(Json::as_str)
        .transpose()?;
    let developer = developer
        .filter(|value| !value.is_empty())
        .map(|value| format!("{value}\n\n{instructions}"))
        .unwrap_or(instructions);
    let mut result = template.clone();
    result.insert("developerInstructions", developer.into());
    result.insert("config", config);
    Ok(result)
}

pub fn main_turn_context(
    template: &Json,
    origin: &JsString,
    instructions: &JsString,
) -> IvyResult<Json> {
    require_object(template)?;
    let mut context = template
        .get("additionalContext")
        .map(object_options)
        .unwrap_or_else(|| Json::Object(Vec::new()));
    context.insert(
        "ivy-main",
        Json::object([
            ("kind", "application".into()),
            (
                "value",
                format!(
                    "{}\n\nInput origin: {}",
                    instructions.to_utf8()?,
                    origin.to_utf8()?
                )
                .into(),
            ),
        ]),
    );
    let mut result = template.clone();
    result.insert("additionalContext", context);
    Ok(result)
}
