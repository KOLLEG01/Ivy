//! Protected WhatsApp account configuration and exact sender/channel policy.
use super::sender::phone;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, json::parse};
use ivy_sdk::clock::parse_date;
use std::path::Path;

fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}
fn validate(value: &Json, definition: &Json) -> IvyResult<()> {
    let valid = value.as_object().is_some()
        && value
            .get("accountId")
            .is_some_and(|value| value.as_str().is_ok())
        && value
            .get("botPhoneNumber")
            .and_then(|value| value.as_str().ok())
            .is_some_and(|number| phone(&number).is_some())
        && value
            .get("since")
            .and_then(Json::as_js_string)
            .is_some_and(|since| parse_date(since).is_finite())
        && value.get("heartbeatMs").is_none_or(|value| {
            value.as_f64().is_some_and(|number| {
                number.fract() == 0.0 && (10_000.0..=120_000.0).contains(&number)
            })
        });
    let senders = value
        .get("senders")
        .and_then(Json::as_array)
        .filter(|values| values.len() == 1);
    if !valid || senders.is_none() {
        return Err(invalid(
            "Invalid WhatsApp account, sender policy or heartbeat.",
        ));
    }
    let sender = &senders.unwrap()[0];
    if sender
        .get("phoneNumber")
        .and_then(|value| value.as_str().ok())
        .is_none_or(|number| phone(&number).is_none())
        || sender
            .get("principalId")
            .is_none_or(|value| value.as_str().is_err())
        || sender
            .get("channelId")
            .is_none_or(|value| value.as_str().is_err())
    {
        return Err(invalid(
            "WhatsApp requires unique exact phone senders, principals and channels.",
        ));
    }
    let channels = definition
        .required("channels")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    if channels
        .iter()
        .filter_map(|row| row.get("channel"))
        .any(|channel| {
            channel.get("adapter") == Some(&Json::from("whatsapp"))
                && channel.get("accountId") == value.get("accountId")
                && channel.get("channelId") == sender.get("channelId")
        })
    {
        return Ok(());
    }
    Err(IvyError::new(
        "chat_sender_refused",
        "Every WhatsApp sender requires its exact configured channel.",
    ))
}

pub async fn read_whatsapp_config(path: &JsString, definition: &Json) -> IvyResult<Json> {
    let path = path.to_utf8()?;
    let path = Path::new(&path);
    if !path.is_absolute() {
        return Err(invalid(
            "WhatsApp configuration must use an absolute protected path.",
        ));
    }
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| IvyError::internal())?;
    let value = parse(&text, usize::MAX).map_err(|_| IvyError::internal())?;
    validate(&value, definition)?;
    Ok(value)
}
