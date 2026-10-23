//! Service notice attribution and private Main context. Prompt text is loaded
//! from the canonical build asset rather than duplicated in this implementation.
use crate::admission::{ChatAdmission, expected_bridge};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, chat_schema::validate_chat, hash::hash_json,
    json::parse,
};
use std::path::Path;

pub struct NoticeInstructions {
    parts: [JsString; 3],
}
impl NoticeInstructions {
    pub fn load(distribution_root: &Path) -> IvyResult<Self> {
        let text =
            std::fs::read_to_string(distribution_root.join("instructions/chat-notice-prompt.json"))
                .map_err(|_| IvyError::internal())?;
        Self::from_json(&parse(&text, usize::MAX).map_err(|_| IvyError::internal())?)
    }
    pub fn from_json(value: &Json) -> IvyResult<Self> {
        let parts = value
            .as_array()
            .filter(|values| values.len() == 3)
            .ok_or_else(IvyError::internal)?;
        let part = |index: usize| {
            parts[index]
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)
        };
        Ok(Self {
            parts: [part(0)?, part(1)?, part(2)?],
        })
    }
    pub fn prompt(&self, caller: &JsString, text: &JsString) -> IvyResult<JsString> {
        Ok(joined_text(&[
            &self.parts[0],
            text,
            &self.parts[1],
            &notice_prefix(caller)?,
            &self.parts[2],
        ]))
    }
}
// Prompt text remains opaque JSON wire data; it is never coerced from other values.
fn joined_text(parts: &[&JsString]) -> JsString {
    JsString(
        parts
            .iter()
            .flat_map(|part| part.0.iter().copied())
            .collect(),
    )
}
fn delimiter(value: char) -> bool {
    matches!(value, '.' | ':' | '/' | '_' | '-')
}
fn words(value: &str) -> IvyResult<String> {
    let lower = value.to_lowercase();
    if lower.match_indices("task-board").any(|(offset, matched)| {
        (offset == 0 || lower[..offset].chars().next_back().is_some_and(delimiter))
            && (offset + matched.len() == lower.len()
                || lower[offset + matched.len()..]
                    .chars()
                    .next()
                    .is_some_and(delimiter))
    }) {
        return Ok("TaskBoard".to_owned());
    }
    let last = value
        .split(delimiter)
        .filter(|value| !value.is_empty())
        .next_back()
        .ok_or_else(IvyError::internal)?;
    let mut result = String::new();
    let mut previous = None;
    for current in last.chars() {
        if previous.is_some_and(|value: char| value.is_ascii_lowercase() || value.is_ascii_digit())
            && current.is_ascii_uppercase()
        {
            result.push(' ');
        }
        if previous.is_none() {
            result.extend(current.to_uppercase());
        } else {
            result.push(current);
        }
        previous = Some(current);
    }
    Ok(result)
}
pub fn notice_prefix(caller: &JsString) -> IvyResult<JsString> {
    let caller = caller.to_utf8()?;
    let lower = caller.to_lowercase();
    let kind = if ["task-board", "automation", "coordinator"]
        .iter()
        .any(|term| lower.contains(term))
    {
        "Automation"
    } else {
        "Service"
    };
    Ok(format!("[[{kind}] {}]", words(&caller)?).into())
}
pub fn attributed_notice_reply(caller: &JsString, text: &JsString) -> IvyResult<JsString> {
    let offset: usize = char::decode_utf16(text.0.iter().copied())
        .take_while(|value| value.as_ref().is_ok_and(|value| value.is_whitespace()))
        .map(|value| value.unwrap().len_utf16())
        .sum();
    let trimmed = &text.0[offset..];
    let end = trimmed
        .iter()
        .skip(1)
        .position(|unit| matches!(*unit, 10 | 13 | 93))
        .map(|offset| offset + 1);
    if trimmed.first() == Some(&91) && end.is_some_and(|offset| offset > 1 && trimmed[offset] == 93)
    {
        Ok(text.clone())
    } else {
        Ok(joined_text(&[
            &notice_prefix(caller)?,
            &" ".into(),
            &JsString(trimmed.to_vec()),
        ]))
    }
}

pub struct AdmittedNotice {
    pub identity: Json,
    pub payload: Json,
    pub request_hash: String,
}
pub fn admitted_notice_input(
    admission: &ChatAdmission,
    caller: &JsString,
    request: &Json,
    binding: &Json,
    instructions: &NoticeInstructions,
) -> IvyResult<AdmittedNotice> {
    admission.authorize(caller, expected_bridge(request)?, None)?;
    validate_chat("AdmittedNotifyRequest", request)?;
    let identity = Json::object([
        ("channel", request.required("channel")?.clone()),
        ("senderPrincipalId", Json::String(caller.clone())),
        ("messageId", request.required("operationId")?.clone()),
    ]);
    let payload = Json::object([
        (
            "text",
            Json::String(
                instructions.prompt(
                    caller,
                    request
                        .required("text")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?,
                )?,
            ),
        ),
        ("images", Json::Array(vec![])),
    ]);
    let request_hash = hash_json(&Json::object([
        (
            "workspaceId",
            admission.definition().required("workspaceId")?.clone(),
        ),
        (
            "definitionHash",
            request
                .required("expectedBridge")?
                .required("definitionHash")?
                .clone(),
        ),
        ("identity", identity.clone()),
        ("binding", binding.clone()),
        ("payload", payload.clone()),
    ]))?;
    Ok(AdmittedNotice {
        identity,
        payload,
        request_hash,
    })
}
