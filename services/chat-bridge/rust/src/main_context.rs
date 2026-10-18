//! Main's context composition. The caller supplies the canonical instruction
//! bundle; instruction text is not duplicated in the Rust implementation.
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{parse, stringify, try_js_string},
};
use std::path::Path;

/// Read the shared build-time JSON-string asset from the selected distribution.
/// The installed package owns the file's identity. Keep its UTF-16 value intact;
/// missing/invalid package data never falls back to executing a source module.
pub fn load_main_instructions(distribution_root: &Path) -> IvyResult<JsString> {
    let text =
        std::fs::read_to_string(distribution_root.join("instructions/chat-main-instructions.json"))
            .map_err(|_| IvyError::internal())?;
    let value = parse(&text, usize::MAX).map_err(|_| IvyError::internal())?;
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}

pub(crate) fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(value) => *value,
        Json::Number(value) => *value != 0.0 && !value.is_nan(),
        Json::String(value) => !value.0.is_empty(),
        _ => true,
    }
}
// Object spread also enumerates array entries and UTF-16 string indices. These
// inputs can occur in JSON-valued config/preferences before native validation.
pub(crate) fn spread(value: &Json) -> Json {
    match value {
        Json::Object(_) => value.clone(),
        Json::Array(values) => Json::Object(
            values
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string().into(), v.clone()))
                .collect(),
        ),
        Json::String(value) => Json::Object(
            value
                .0
                .iter()
                .enumerate()
                .map(|(i, u)| (i.to_string().into(), Json::String(JsString(vec![*u]))))
                .collect(),
        ),
        _ => Json::Object(vec![]),
    }
}
pub(crate) fn receiver(value: &Json) -> IvyResult<()> {
    if value.is_null() {
        Err(IvyError::internal())
    } else {
        Ok(())
    }
}
fn object(value: Option<&Json>) -> Json {
    value
        .filter(|v| v.as_object().is_some())
        .cloned()
        .unwrap_or_else(|| Json::Object(vec![]))
}
fn concat(parts: &[&JsString]) -> JsString {
    JsString(
        parts
            .iter()
            .flat_map(|part| part.0.iter().copied())
            .collect(),
    )
}

pub fn main_input_origin(caller: &JsString, request: &Json) -> IvyResult<JsString> {
    receiver(request)?;
    if request.get("action") != Some(&Json::from("notify")) {
        return Ok("conversation input".into());
    }
    let source = request
        .get("source")
        .map(|v| stringify(v, usize::MAX))
        .transpose()?
        .unwrap_or_else(|| "undefined".into());
    Ok(concat(&[
        &"service notification from ".into(),
        caller,
        &"; source ".into(),
        &source.into(),
    ]))
}

/// The tool restriction belongs to Main's native thread, rather than its shared
/// AgentManager principal. Existing unrelated settings and order are retained.
pub fn main_thread_options(template: &Json, instructions: &JsString) -> IvyResult<Json> {
    receiver(template)?;
    let mut config = object(template.get("config"));
    let mut disabled = Vec::new();
    for value in config
        .get("mcp_servers.ivy.disabled_tools")
        .and_then(Json::as_array)
        .unwrap_or(&[])
    {
        // JS Set compares JSON objects/arrays by identity. Distinct parsed values
        // remain distinct even when their contents match.
        if value.as_object().is_some()
            || value.as_array().is_some()
            || !disabled.iter().any(|v| v == value)
        {
            disabled.push(value.clone());
        }
    }
    let tool = Json::from("chat_bridge_send");
    if !disabled.contains(&tool) {
        disabled.push(tool);
    }
    config.insert("mcp_servers.ivy.disabled_tools", Json::Array(disabled));
    let developer = match template.get("developerInstructions").filter(|v| truthy(v)) {
        Some(value) => concat(&[&try_js_string(value)?, &"\n\n".into(), instructions]),
        None => instructions.clone(),
    };
    let mut result = spread(template);
    result.insert("developerInstructions", Json::String(developer));
    result.insert("config", config);
    Ok(result)
}

pub fn main_turn_context(
    template: &Json,
    origin: &JsString,
    instructions: &JsString,
) -> IvyResult<Json> {
    receiver(template)?;
    let mut context = object(template.get("additionalContext"));
    context.insert(
        "ivy-main",
        Json::object([
            ("kind", "application".into()),
            (
                "value",
                Json::String(concat(&[
                    instructions,
                    &"\n\nInput origin: ".into(),
                    origin,
                ])),
            ),
        ]),
    );
    let mut result = spread(template);
    result.insert("additionalContext", context);
    Ok(result)
}
