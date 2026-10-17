//! Native output helpers. Opaque cursors and text retain JavaScript UTF-16 units.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};

pub fn page_cursor(value: Option<&Json>) -> IvyResult<Option<JsString>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(value)) if !value.0.is_empty() && value.0.len() <= 8192 => {
            Ok(Some(value.clone()))
        }
        _ => Err(IvyError::new(
            "task_board_native_cursor_unsupported",
            "Native cursor must fit its explicit nonempty bounded opaque representation.",
        )),
    }
}

pub fn turns_params(thread_id: &JsString, cursor: Option<&JsString>) -> Json {
    Json::object([
        ("threadId", Json::String(thread_id.clone())),
        (
            "cursor",
            cursor.cloned().map(Json::String).unwrap_or(Json::Null),
        ),
        ("limit", Json::Number(100.0)),
        ("itemsView", "notLoaded".into()),
        ("sortDirection", "desc".into()),
    ])
}

pub fn output_params(thread_id: &JsString, turn_id: &JsString, cursor: Option<&JsString>) -> Json {
    Json::object([
        ("threadId", Json::String(thread_id.clone())),
        ("turnId", Json::String(turn_id.clone())),
        (
            "cursor",
            cursor.cloned().map(Json::String).unwrap_or(Json::Null),
        ),
        ("limit", Json::Number(1.0)),
        ("sortDirection", "desc".into()),
    ])
}

pub fn final_output_text(item: &Json) -> JsString {
    if item.get("type").and_then(Json::as_js_string) == Some(&"agentMessage".into())
        && item.get("phase").and_then(Json::as_js_string) != Some(&"commentary".into())
    {
        return item
            .get("text")
            .and_then(Json::as_js_string)
            .cloned()
            .unwrap_or_default();
    }
    JsString::default()
}

pub fn bounded_output_summary(text: &JsString) -> JsString {
    // Buffer.byteLength replaces each unpaired surrogate with U+FFFD. Count
    // without allocating a UTF-8 copy, stopping as soon as the limit is crossed.
    let mut bytes = 0;
    for scalar in char::decode_utf16(text.0.iter().copied()) {
        bytes += scalar.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8();
        if bytes > 262144 {
            let mut shortened = text.0[..65536].to_vec();
            if shortened
                .last()
                .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
            {
                shortened.pop();
            }
            shortened.extend(
                "\n\nThe complete native output remains in the original Codex conversation."
                    .encode_utf16(),
            );
            return JsString(shortened);
        }
    }
    text.clone()
}
