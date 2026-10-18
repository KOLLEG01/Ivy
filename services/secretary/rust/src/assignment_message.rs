//! Producer-independent message-channel admission. The injected date parser must
//! implement the shared Date.parse boundary, including invalid-date NaN results.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};

pub const MESSAGE_WINDOW_KEY: &str = "secretary/channel-window";
pub const MESSAGE_BATCH_KIND: &str = "secretary.channel.batch";
pub const MESSAGE_WINDOW_MS: i64 = 5 * 60_000;

pub fn message_channel(
    payload: &IvyJson,
    parse_date: &impl Fn(&JsString) -> f64,
) -> Option<IvyJson> {
    let value = payload.get("messageChannel")?;
    let key = value.get("key")?.as_js_string()?;
    let occurred_at = value.get("occurredAt")?.as_js_string()?;
    if key.0.is_empty() || !parse_date(occurred_at).is_finite() {
        return None;
    }
    let reference = value.get("reference")?;
    reference.as_object()?;
    let absent = IvyJson::Null;
    let item_id = value.get("itemId").unwrap_or(&absent);
    if !item_id.is_null() && item_id.as_js_string().is_none() {
        return None;
    }
    Some(IvyJson::object([
        ("key", IvyJson::String(key.clone())),
        ("reference", reference.clone()),
        ("occurredAt", IvyJson::String(occurred_at.clone())),
        ("itemId", item_id.clone()),
    ]))
}

pub fn message_batch(payload: &IvyJson, parse_date: &impl Fn(&JsString) -> f64) -> Option<IvyJson> {
    let channel = payload.get("channel")?;
    message_channel(
        &IvyJson::object([("messageChannel", channel.clone())]),
        parse_date,
    )?;
    if payload.get("kind")?.as_js_string()? != &JsString::from(MESSAGE_BATCH_KIND)
        || payload.get("dueAt")?.as_js_string().is_none()
        || payload.get("serviceNodeId")?.as_js_string().is_none()
    {
        return None;
    }
    for name in ["itemIds", "eventIds"] {
        if !payload
            .get(name)?
            .as_array()?
            .iter()
            .all(|id| id.as_js_string().is_some())
        {
            return None;
        }
    }
    // Validation uses a normalized channel, but the original returns the supplied
    // batch unchanged, including extra fields and omitted channel.itemId.
    Some(payload.clone())
}

pub fn message_unread_snapshot(
    observation: &IvyJson,
    due_at: &JsString,
    parse_date: &impl Fn(&JsString) -> f64,
) -> IvyResult<bool> {
    let pending = || {
        IvyError::new(
            "secretary_message_unread_pending",
            "The channel unread state is incomplete or stale.",
        )
    };
    if observation.get("complete").and_then(IvyJson::as_bool) != Some(true) {
        return Err(pending());
    }
    let unread = observation
        .get("unread")
        .and_then(IvyJson::as_bool)
        .ok_or_else(pending)?;
    let observed_at = observation
        .get("observedAt")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(pending)?;
    if !parse_date(observed_at).is_finite() || parse_date(observed_at) < parse_date(due_at) {
        return Err(pending());
    }
    Ok(unread)
}
