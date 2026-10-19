//! Protected WhatsApp configuration, retaining the original JSON and policy.
use super::sender::phone;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{parse, try_js_string},
};
use ivy_sdk::clock::parse_date;
use std::path::PathBuf;

fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}

// Node path.isAbsolute accepts a rooted Windows path without a drive. Rust's
// Path::is_absolute requires the drive too, so it cannot express this policy.
fn absolute(path: &JsString) -> bool {
    let units = &path.0;
    if cfg!(windows) {
        let separator = |unit| unit == 47 || unit == 92;
        units.first().is_some_and(|unit| separator(*unit))
            || (units.len() > 2
                && ((65..=90).contains(&units[0]) || (97..=122).contains(&units[0]))
                && units[1] == 58
                && separator(units[2]))
    } else {
        units.first() == Some(&47)
    }
}

// JSON primitives have boxed property reads in JS; only null throws. Missing
// fields stay distinct from null for undefined, strict equality and coercion.
fn property<'a>(value: &'a Json, key: &str) -> IvyResult<Option<&'a Json>> {
    if value.is_null() {
        Err(IvyError::internal())
    } else {
        Ok(value.get(key))
    }
}

fn string(value: Option<&Json>) -> IvyResult<JsString> {
    value.map_or_else(|| Ok("undefined".into()), try_js_string)
}

fn is_phone(value: Option<&Json>) -> IvyResult<bool> {
    let value = string(value)?;
    // The grammar is ASCII, so unpaired UTF-16 units cannot be phone digits.
    Ok(String::from_utf16(&value.0)
        .ok()
        .is_some_and(|value| phone(&value).is_some()))
}

fn strict_equal(left: Option<&Json>, right: Option<&Json>) -> bool {
    match (left, right) {
        (None, None) | (Some(Json::Null), Some(Json::Null)) => true,
        (Some(Json::String(a)), Some(Json::String(b))) => a == b,
        (Some(Json::Number(a)), Some(Json::Number(b))) => a == b,
        (Some(Json::Bool(a)), Some(Json::Bool(b))) => a == b,
        // The file and definition are independently parsed objects. Structural
        // equality would admit a channel that the original === refuses.
        _ => false,
    }
}

fn validate(value: &Json, definition: &Json) -> IvyResult<()> {
    let valid = (|| -> IvyResult<bool> {
        if !matches!(value.get("accountId"), Some(Json::String(_)))
            || !is_phone(value.get("botPhoneNumber"))?
        {
            return Ok(false);
        }
        let Some(Json::Array(senders)) = value.get("senders") else {
            return Ok(false);
        };
        if senders.len() != 1 || !parse_date(&string(value.get("since"))?).is_finite() {
            return Ok(false);
        }
        Ok(match value.get("heartbeatMs") {
            None => true,
            Some(Json::Number(number)) => {
                number.fract() == 0.0 && (10000.0..=120000.0).contains(number)
            }
            _ => false,
        })
    })()?;
    if !valid {
        return Err(invalid(
            "Invalid WhatsApp account, sender policy or heartbeat.",
        ));
    }
    let Json::Array(senders) = value.get("senders").unwrap() else {
        unreachable!()
    };
    // The preceding exact-one-sender policy makes the original three uniqueness
    // sets vacuous. Preserve its phone check and do not add field type checks.
    let sender = &senders[0];
    if !is_phone(property(sender, "phoneNumber")?)? {
        return Err(invalid(
            "WhatsApp requires unique exact phone senders, principals and channels.",
        ));
    }
    let Some(Json::Array(channels)) = property(definition, "channels")? else {
        return Err(IvyError::internal());
    };
    for row in channels {
        let channel = property(row, "channel")?.ok_or_else(IvyError::internal)?;
        if property(channel, "adapter")? == Some(&Json::from("whatsapp"))
            && strict_equal(property(channel, "accountId")?, value.get("accountId"))
            && strict_equal(
                property(channel, "channelId")?,
                property(sender, "channelId")?,
            )
        {
            return Ok(());
        }
    }
    Err(IvyError::new(
        "chat_sender_refused",
        "Every WhatsApp sender requires its exact configured channel.",
    ))
}

/// Read the same protected absolute path used by the original adapter. This
/// does not normalize the configuration, apply defaults or validate media keys.
pub async fn read_whatsapp_config(path: &JsString, definition: &Json) -> IvyResult<Json> {
    if !absolute(path) {
        return Err(invalid(
            "WhatsApp configuration must use an absolute protected path.",
        ));
    }
    // Node converts string filesystem paths through UTF-8, replacing unpaired
    // UTF-16 units. readFile('utf8') likewise replaces malformed byte sequences.
    let path = PathBuf::from(String::from_utf16_lossy(&path.0));
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| IvyError::internal())?;
    let value =
        parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| IvyError::internal())?;
    validate(&value, definition)?;
    Ok(value)
}
