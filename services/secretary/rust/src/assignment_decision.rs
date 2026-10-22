//! Decode the retained assignment result without changing its UTF-16 text.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
fn whitespace(c: u16) -> bool {
    matches!(c, 9..=13 | 32 | 160 | 5760 | 8192..=8202 | 8232 | 8233 | 8239 | 8287 | 12288 | 65279)
}
pub(crate) fn trimmed(text: &[u16]) -> &[u16] {
    let start = text
        .iter()
        .position(|c| !whitespace(*c))
        .unwrap_or(text.len());
    let end = text
        .iter()
        .rposition(|c| !whitespace(*c))
        .map(|i| i + 1)
        .unwrap_or(start);
    &text[start..end]
}
pub fn assignment_decision(answer: &JsString) -> IvyResult<IvyJson> {
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
    let value = crate::triage_results::final_json_text(&JsString(units.to_vec()))
        .and_then(|text| parse(&text, text.len()).ok())
        .ok_or_else(|| invalid("The assignment did not return its required decision."))?;
    if value.as_object().is_none() {
        return Err(invalid("The assignment decision must be an object."));
    }
    let string = |key| value.get(key).and_then(IvyJson::as_js_string);
    let notification = value.get("notification");
    let is = |key: &str, choices: &[&str]| {
        choices
            .iter()
            .any(|s| value.get(key) == Some(&IvyJson::from(*s)))
    };
    if value.get("schemaVersion") != Some(&IvyJson::Number(1.0))
        || !is("urgency", &["low", "normal", "high", "critical"])
        || !is("notification", &["none", "main", "voice"])
        || !string("text").is_some_and(|s| {
            s.0.len() <= 8192 && (notification == Some(&"none".into()) || !trimmed(&s.0).is_empty())
        })
        || !string("reason").is_some_and(|s| s.0.len() <= 1024)
        || (notification == Some(&"voice".into())
            && value.get("urgency") != Some(&"critical".into()))
    {
        return Err(invalid("The assignment decision is incomplete."));
    }
    Ok(IvyJson::object(
        ["schemaVersion", "urgency", "notification", "text", "reason"]
            .map(|key| (key, value.get(key).unwrap().clone())),
    ))
}
