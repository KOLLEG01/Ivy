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
        let values = value
            .as_array()
            .filter(|v| v.len() == 3)
            .ok_or_else(IvyError::internal)?;
        let part = |i: usize| {
            values[i]
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)
        };
        Ok(Self {
            parts: [part(0)?, part(1)?, part(2)?],
        })
    }
    pub fn prompt(&self, caller: &JsString, text: &JsString) -> IvyResult<JsString> {
        Ok(concat(&[
            &self.parts[0],
            text,
            &self.parts[1],
            &notice_prefix(caller)?,
            &self.parts[2],
        ]))
    }
}
fn concat(parts: &[&JsString]) -> JsString {
    JsString(parts.iter().flat_map(|p| p.0.iter().copied()).collect())
}
fn delimiter(unit: u16) -> bool {
    matches!(unit, 46 | 58 | 47 | 95 | 45)
}
// These fixed /iu notice markers contain only ASCII letters. ECMAScript simple
// Unicode folding adds long-s and Kelvin sign to the ASCII equivalences; full
// uppercase expansion would change substring/boundary matching.
fn folded(value: &JsString) -> JsString {
    JsString(
        value
            .0
            .iter()
            .map(|u| match u {
                65..=90 => *u + 32,
                0x017f => 115,
                0x212a => 107,
                _ => *u,
            })
            .collect(),
    )
}
fn words(value: &JsString) -> IvyResult<JsString> {
    let folded = folded(value);
    let task = JsString::from("task-board");
    if folded
        .0
        .windows(task.0.len())
        .enumerate()
        .any(|(i, window)| {
            window == task.0
                && (i == 0 || delimiter(folded.0[i - 1]))
                && (i + task.0.len() == folded.0.len() || delimiter(folded.0[i + task.0.len()]))
        })
    {
        return Ok("TaskBoard".into());
    }
    let last = value
        .0
        .split(|u| delimiter(*u))
        .filter(|v| !v.is_empty())
        .next_back()
        .ok_or_else(IvyError::internal)?;
    let mut units = Vec::new();
    for (i, unit) in last.iter().enumerate() {
        units.push(*unit);
        if matches!(*unit, 97..=122 | 48..=57)
            && last.get(i + 1).is_some_and(|u| matches!(*u, 65..=90))
        {
            units.push(32);
        }
    }
    if !matches!(units[0], 10 | 13 | 0x2028 | 0x2029) {
        let first = if (0xd800..=0xdbff).contains(&units[0])
            && units.get(1).is_some_and(|u| (0xdc00..=0xdfff).contains(u))
        {
            2
        } else {
            1
        };
        let mut upper = JsString(units[..first].to_vec()).to_uppercase().0;
        upper.extend(&units[first..]);
        units = upper;
    }
    Ok(JsString(units))
}
pub fn notice_prefix(caller: &JsString) -> IvyResult<JsString> {
    let label = words(caller)?;
    let folded = folded(caller);
    let automation = ["task-board", "automation", "coordinator"]
        .iter()
        .any(|term| folded.contains_ascii(term));
    Ok(concat(&[
        &if automation {
            "[[Automation] ".into()
        } else {
            "[[Service] ".into()
        },
        &label,
        &"]".into(),
    ]))
}
pub fn attributed_notice_reply(caller: &JsString, text: &JsString) -> IvyResult<JsString> {
    let trimmed = text.trim_start();
    let mut existing = false;
    if trimmed.0.first() == Some(&91) {
        for (i, unit) in trimmed.0.iter().enumerate().skip(1) {
            if matches!(*unit, 10 | 13) {
                break;
            }
            if *unit == 93 {
                existing = i > 1;
                break;
            }
        }
    }
    if existing {
        Ok(text.clone())
    } else {
        Ok(concat(&[&notice_prefix(caller)?, &" ".into(), &trimmed]))
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
