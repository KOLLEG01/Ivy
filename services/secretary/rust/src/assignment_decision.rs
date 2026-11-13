//! Decode the retained assignment result without changing its UTF-16 text.
use crate::policy::{Notification, Urgency};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json, json::parse_utf16};
fn whitespace(c: u16) -> bool {
    JsString::trim_ecmascript_units(&[c]).is_empty()
}
pub(crate) fn trimmed(text: &[u16]) -> &[u16] {
    JsString::trim_ecmascript_units(text)
}
#[derive(Debug)]
pub struct AssignmentDecision {
    urgency: Urgency,
    notification: Notification,
    text: JsString,
    reason: JsString,
}
impl AssignmentDecision {
    pub fn from_json(mut value: IvyJson) -> IvyResult<Self> {
        let invalid = |message| IvyError::new("secretary_assignment_result_invalid", message);
        if value.as_object().is_none() {
            return Err(invalid("The assignment decision must be an object."));
        }
        let incomplete = || invalid("The assignment decision is incomplete.");
        let urgency = value
            .get("urgency")
            .and_then(Urgency::read)
            .ok_or_else(incomplete)?;
        let notification = value
            .get("notification")
            .and_then(Notification::read)
            .filter(|n| *n != Notification::NeedsAttention)
            .ok_or_else(incomplete)?;
        let text = value
            .get("text")
            .and_then(IvyJson::as_js_string)
            .ok_or_else(incomplete)?;
        let reason = value
            .get("reason")
            .and_then(IvyJson::as_js_string)
            .ok_or_else(incomplete)?;
        if value.get("schemaVersion") != Some(&IvyJson::Number(1.0))
            || text.0.len() > 8192
            || (notification != Notification::None && trimmed(&text.0).is_empty())
            || reason.0.len() > 1024
            || (notification == Notification::Voice && urgency != Urgency::Critical)
        {
            return Err(incomplete());
        }
        let text = match value.get_mut("text") {
            Some(IvyJson::String(text)) => JsString(std::mem::take(&mut text.0)),
            _ => unreachable!(),
        };
        let reason = match value.get_mut("reason") {
            Some(IvyJson::String(reason)) => JsString(std::mem::take(&mut reason.0)),
            _ => unreachable!(),
        };
        Ok(Self {
            urgency,
            notification,
            text,
            reason,
        })
    }
    pub fn urgency(&self) -> Urgency {
        self.urgency
    }
    pub fn notification(&self) -> Notification {
        self.notification
    }
    pub fn text(&self) -> &JsString {
        &self.text
    }
    pub fn reason(&self) -> &JsString {
        &self.reason
    }
    pub fn json(&self) -> IvyJson {
        IvyJson::object([
            ("schemaVersion", 1.0.into()),
            ("urgency", self.urgency.as_str().into()),
            ("notification", self.notification.as_str().into()),
            ("text", IvyJson::String(self.text.clone())),
            ("reason", IvyJson::String(self.reason.clone())),
        ])
    }
}

/// A call-local parsed decision belongs to one exact saved execution revision.
/// Only the original result remains persisted; retries parse a fresh read once.
pub struct RecordedDecision {
    object_id: JsString,
    revision: u64,
    decision: AssignmentDecision,
    result_hash: String,
}
impl RecordedDecision {
    pub fn read(execution: &IvyJson) -> IvyResult<Self> {
        let original = execution
            .required("value")?
            .required("result")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        Self::record(execution, assignment_decision(original)?)
    }
    pub(crate) fn record(execution: &IvyJson, decision: AssignmentDecision) -> IvyResult<Self> {
        let pin = execution.required("pin")?;
        let object_id = pin
            .required("objectId")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?;
        let revision = pin
            .required("revision")?
            .as_f64()
            .filter(|v| v.fract() == 0.0 && (1.0..=9_007_199_254_740_991.0).contains(v))
            .ok_or_else(IvyError::internal)? as u64;
        Ok(Self {
            object_id,
            revision,
            decision,
            result_hash: hash_json(execution.required("value")?.required("result")?)?,
        })
    }
    pub fn for_execution(&self, execution: &IvyJson) -> IvyResult<&AssignmentDecision> {
        let pin = execution.required("pin")?;
        if pin.get("objectId").and_then(IvyJson::as_js_string) != Some(&self.object_id)
            || pin.get("revision").and_then(IvyJson::as_f64) != Some(self.revision as f64)
        {
            return Err(IvyError::new(
                "secretary_assignment_result_changed",
                "The parsed decision belongs to another execution revision.",
            ));
        }
        Ok(&self.decision)
    }
    pub fn result_hash(&self) -> &str {
        &self.result_hash
    }
}

pub fn assignment_decision(answer: &JsString) -> IvyResult<AssignmentDecision> {
    let mut units = trimmed(&answer.0);
    if units.starts_with(&[96, 96, 96]) && units.ends_with(&[10, 96, 96, 96]) {
        let mut offset = 3;
        if units.get(offset..offset + 4).is_some_and(|s| {
            s.iter()
                .zip([106, 115, 111, 110])
                .all(|(a, b)| *a == b || *a == b - 32)
        }) {
            offset += 4;
        }
        if let Some(newline) = units[offset..].iter().position(|c| *c == 10) {
            let begin = offset + newline;
            if units[offset..begin].iter().all(|c| whitespace(*c)) && begin < units.len() - 4 {
                units = &units[begin + 1..units.len() - 4];
            }
        }
    }
    let invalid = |message| IvyError::new("secretary_assignment_result_invalid", message);
    let value = parse_utf16(units, usize::MAX)
        .map_err(|_| invalid("The assignment did not return its required decision."))?;
    AssignmentDecision::from_json(value)
}
