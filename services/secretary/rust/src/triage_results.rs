//! Assessment decoding after the native owner has verified retained turn evidence.
//! This does not establish native provenance or replace completion verification.
use crate::schema::{Schema, validate};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{js_string, parse},
};
use std::collections::HashSet;

fn failure(code: &str) -> IvyError {
    IvyError::new(
        code,
        "Triage requires its original item, owner, native intent and complete evidence.",
    )
}

/// Select the one original turn from an already admitted read observation.
pub fn triage_turn<'a>(
    read: &'a IvyJson,
    turn_id: &JsString,
    full: bool,
) -> IvyResult<&'a IvyJson> {
    let result = read
        .get("reply")
        .and_then(|reply| reply.get("result"))
        .ok_or_else(|| failure("secretary_triage_evidence_invalid"))?;
    let data = result
        .get("data")
        .and_then(IvyJson::as_array)
        .filter(|data| data.len() == 1)
        .ok_or_else(|| failure("secretary_triage_turn_changed"))?;
    let turn = &data[0];
    if turn.get("id").and_then(IvyJson::as_js_string) != Some(turn_id)
        || turn.get("itemsView") != Some(&IvyJson::from(if full { "full" } else { "notLoaded" }))
    {
        return Err(failure("secretary_triage_turn_changed"));
    }
    Ok(turn)
}

pub fn triage_result(read: &IvyJson, turn_id: &JsString) -> IvyResult<IvyJson> {
    let turn = triage_turn(read, turn_id, true)?;
    let items = turn
        .get("items")
        .and_then(IvyJson::as_array)
        .ok_or_else(|| failure("secretary_triage_turn_failed"))?;
    result_items(turn, items)
}

/// Decode the same item list returned by the paginated native verifier.
pub fn result_items(turn: &IvyJson, items: &[IvyJson]) -> IvyResult<IvyJson> {
    if turn.get("status") != Some(&IvyJson::from("completed"))
        || turn.get("error").is_some_and(|error| !error.is_null())
    {
        return Err(failure("secretary_triage_turn_failed"));
    }
    let invalid = || failure("secretary_triage_result_invalid");
    let mut ids = HashSet::new();
    let mut finals = Vec::new();
    for item in items {
        let id = item
            .get("id")
            .and_then(IvyJson::as_js_string)
            .filter(|id| !id.0.is_empty())
            .ok_or_else(invalid)?;
        if !ids.insert(id) {
            return Err(invalid());
        }
        // String(item.type) runs outside the final-JSON error mapping. JSON
        // objects with an own noncallable toString cannot produce a primitive;
        // array joining recursively encounters those objects, but ordinary
        // object properties are not themselves coerced.
        let mut pending: Vec<_> = item.get("type").into_iter().collect();
        while let Some(value) = pending.pop() {
            match value {
                IvyJson::Array(values) => pending.extend(values),
                IvyJson::Object(_) if value.get("toString").is_some() => {
                    return Err(IvyError::internal());
                }
                _ => {}
            }
        }
        let kind = item.get("type").map(js_string);
        if !["userMessage", "agentMessage", "reasoning"]
            .iter()
            .any(|allowed| kind.as_ref() == Some(&JsString::from(*allowed)))
        {
            return Err(failure("secretary_triage_unexpected_action"));
        }
        if item.get("type") == Some(&IvyJson::from("agentMessage")) {
            let text = item
                .get("text")
                .and_then(IvyJson::as_js_string)
                .ok_or_else(invalid)?;
            if item.get("phase") == Some(&IvyJson::from("final_answer")) {
                finals.push(text);
            }
        }
    }
    if finals.len() != 1 {
        return Err(failure("secretary_triage_final_ambiguous"));
    }
    let text = final_json_text(finals[0]).ok_or_else(invalid)?;
    // The original JSON.parse has no separate text-size limit. Its subsequent
    // SchemaValidators admission bounds the decoded canonical value to 1 MiB.
    let result = parse(&text, text.len()).map_err(|_| invalid())?;
    validate(Schema::Assessment, &result).map_err(|_| invalid())?;
    if result.get("task") != Some(&IvyJson::Null)
        || result.get("disposition") == Some(&IvyJson::from("task"))
    {
        return Err(invalid());
    }
    Ok(result)
}

// Native text is an ECMAScript string. Preserve raw lone surrogates inside JSON
// string tokens when passing it to the shared UTF-8 parser; an escaped lone unit
// or one outside a string is invalid JSON, not a replacement character.
pub(crate) fn final_json_text(text: &JsString) -> Option<String> {
    use std::fmt::Write;
    let mut output = String::new();
    let (mut quoted, mut escaped) = (false, false);
    for unit in char::decode_utf16(text.0.iter().copied()) {
        match unit {
            Ok(ch) => {
                output.push(ch);
                if escaped {
                    escaped = false;
                } else if quoted && ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quoted = !quoted;
                }
            }
            Err(error) => {
                if !quoted || escaped {
                    return None;
                }
                write!(output, "\\u{:04x}", error.unpaired_surrogate()).ok()?;
            }
        }
    }
    Some(output)
}
