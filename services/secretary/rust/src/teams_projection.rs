//! Original account-bound Teams chat/message projection. Preview text is never input.
use crate::{
    microsoft_schema::validate_microsoft,
    outlook_projection,
    schema::{Schema, validate},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json, json::canonical};
use std::collections::HashSet;

fn check(value: bool, code: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(
            code,
            "Teams requires original account-bound complete chat and message evidence.",
        ))
    }
}
fn text(value: Option<&IvyJson>, maximum: usize, empty: bool) -> IvyResult<String> {
    outlook_projection::text(value, maximum, empty).map_err(|_| {
        IvyError::new(
            "teams_response_invalid",
            "Teams requires original account-bound complete chat and message evidence.",
        )
    })
}
fn record(value: &IvyJson) -> IvyResult<&IvyJson> {
    check(value.as_object().is_some(), "teams_response_invalid")?;
    Ok(value)
}
pub fn teams_time(value: &IvyJson) -> IvyResult<String> {
    outlook_projection::utc(Some(value)).map_err(|error| {
        IvyError::new(
            if error.code == "microsoft_time_invalid" {
                "teams_time_invalid"
            } else {
                "teams_response_invalid"
            },
            "Teams requires original account-bound complete chat and message evidence.",
        )
    })
}
pub fn teams_arguments(configuration: &IvyJson, selection: &IvyJson) -> IvyResult<IvyJson> {
    validate_microsoft("TeamsCollector", configuration)?;
    validate_microsoft("TeamsSelection", selection)?;
    let since = selection.required("since")?;
    let until = selection.required("until")?;
    check(
        teams_time(since)? == since.as_str()?
            && teams_time(until)? == until.as_str()?
            && since.as_js_string() < until.as_js_string()
            && Some(since) == configuration.get("since"),
        "teams_selection_invalid",
    )?;
    let chat = selection.required("chat")?;
    Ok(if !chat.is_null() {
        IvyJson::object([
            ("chat_id", chat.required("id")?.clone()),
            ("sent_after", since.clone()),
            (
                "top",
                IvyJson::Number(configuration.required("maximumMessages")?.as_f64().unwrap() + 1.0),
            ),
        ])
    } else {
        IvyJson::object([
            ("unread_only", true.into()),
            (
                "top",
                IvyJson::Number(configuration.required("maximumChats")?.as_f64().unwrap() + 1.0),
            ),
        ])
    })
}
fn sender(value: &JsString) -> bool {
    let start = if value.0.starts_with(&[117, 115, 101, 114, 58]) {
        5
    } else if value
        .0
        .starts_with(&[97, 112, 112, 108, 105, 99, 97, 116, 105, 111, 110, 58])
    {
        12
    } else {
        return false;
    };
    value.0.len() > start
        && value.0[start..]
            .iter()
            .all(|c| *c != 58 && !char::from_u32(*c as u32).is_some_and(char::is_whitespace))
}
pub fn teams_source(configuration: &IvyJson, source: &IvyJson) -> IvyResult<()> {
    check(
        source.get("kind") == Some(&"teams".into())
            && source.get("accountId") == configuration.required("profile")?.get("id"),
        "microsoft_account_mismatch",
    )?;
    for key in ["ownSenderIds", "allowedSenderIds"] {
        let ids = source.required(key)?;
        if ids.is_null() {
            continue;
        }
        check(
            ids.as_array()
                .unwrap()
                .iter()
                .all(|id| id.as_js_string().is_some_and(sender)),
            "teams_sender_config_invalid",
        )?;
    }
    Ok(())
}
pub fn teams_page(
    value: &IvyJson,
    configuration: &IvyJson,
    source: &IvyJson,
    selection: &IvyJson,
    observed_at: &IvyJson,
) -> IvyResult<IvyJson> {
    teams_arguments(configuration, selection)?;
    validate(Schema::SourceDefinition, source)?;
    teams_source(configuration, source)?;
    check(
        teams_time(observed_at)? == observed_at.as_str()?
            && observed_at.as_js_string() >= selection.required("until")?.as_js_string(),
        "teams_time_invalid",
    )?;
    canonical(value, 8 * 1024 * 1024)?;
    let chat = selection.required("chat")?;
    let messages = !chat.is_null();
    let page = record(value)?;
    let entries = page
        .get(if messages { "messages" } else { "chats" })
        .and_then(IvyJson::as_array);
    let maximum = configuration
        .required(if messages {
            "maximumMessages"
        } else {
            "maximumChats"
        })?
        .as_f64()
        .unwrap() as usize;
    check(
        entries.is_some_and(|v| v.len() <= maximum + 1),
        "teams_page_incomplete",
    )?;
    let entries = entries.unwrap();
    check(entries.len() <= maximum, "teams_scan_limit")?;
    check(
        ["next_link", "nextLink", "@odata.nextLink"]
            .iter()
            .all(|key| {
                page.get(key)
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|text| text.0.is_empty())
            })
            && page.get("has_more") != Some(&IvyJson::Bool(true))
            && page
                .get("next_from_index")
                .and_then(IvyJson::as_f64)
                .is_none_or(|index| index == 0.0),
        "teams_page_incomplete",
    )?;
    let mut excluded = IvyJson::object(
        [
            "hiddenChat",
            "configuredChat",
            "ownSender",
            "senderNotAllowed",
            "deleted",
            "system",
            "outsideWindow",
        ]
        .map(|key| (key, IvyJson::Number(0.0))),
    );
    let mut exclude = |key: &str| {
        let n = excluded.get(key).unwrap().as_f64().unwrap();
        excluded.insert(key, IvyJson::Number(n + 1.0));
    };
    let mut seen = HashSet::new();
    let mut self_id = JsString::from("user:");
    self_id.0.extend_from_slice(
        &configuration
            .required("profile")?
            .required("id")?
            .as_js_string()
            .unwrap()
            .0,
    );
    let mut own = HashSet::from([self_id]);
    for id in source.required("ownSenderIds")?.as_array().unwrap() {
        own.insert(id.as_js_string().unwrap().clone());
    }
    let allowed = source.required("allowedSenderIds")?;
    let mut chats = vec![];
    let mut projected = vec![];
    let mut attachments = 0;
    for entry in entries {
        let raw = record(entry)?;
        let id = text(
            raw.get(if messages { "message_id" } else { "id" }),
            4096,
            false,
        )?;
        check(seen.insert(id.clone()), "teams_duplicate_id")?;
        if !messages {
            check(
                raw.get("is_unread") == Some(&true.into())
                    && raw.get("is_hidden").and_then(IvyJson::as_bool).is_some(),
                "teams_selection_mismatch",
            )?;
            if configuration
                .required("excludedChatIds")?
                .as_array()
                .unwrap()
                .contains(&id.clone().into())
            {
                exclude("configuredChat");
                continue;
            }
            if raw.get("is_hidden") == Some(&true.into()) {
                exclude("hiddenChat");
                continue;
            }
            let title = if raw.get("topic") == Some(&IvyJson::Null) {
                String::new()
            } else {
                text(raw.get("topic"), 1024, true)?
            };
            chats.push(IvyJson::object([
                ("id", id.into()),
                ("title", title.into()),
            ]));
            continue;
        }
        let chat_id = chat.required("id")?;
        let mut path = JsString::from("/chats/");
        path.0.extend_from_slice(&chat_id.as_js_string().unwrap().0);
        path.0
            .extend_from_slice(&JsString::from(format!("/messages/{id}")).0);
        check(
            raw.get("chat_id") == Some(chat_id)
                && raw.get("container_type") == Some(&"chat".into())
                && ["channel_id", "team_id", "parent_message_id"]
                    .iter()
                    .all(|key| raw.get(key) == Some(&IvyJson::Null))
                && raw.get("path") == Some(&IvyJson::String(path)),
            "teams_message_identity_invalid",
        )?;
        if raw.get("deleted_at") != Some(&IvyJson::Null) {
            teams_time(raw.get("deleted_at").unwrap_or(&IvyJson::Null))?;
            exclude("deleted");
            continue;
        }
        if raw.get("message_type") != Some(&"message".into()) {
            check(
                raw.get("message_type") == Some(&"systemEventMessage".into()),
                "teams_message_type_invalid",
            )?;
            exclude("system");
            continue;
        }
        let user = raw.get("author_user_id");
        let app = raw.get("author_application_id");
        check(
            (user == Some(&IvyJson::Null)) != (app == Some(&IvyJson::Null)),
            "teams_sender_invalid",
        )?;
        let sender_id = format!(
            "{}:{}",
            if user == Some(&IvyJson::Null) {
                "application"
            } else {
                "user"
            },
            text(
                if user == Some(&IvyJson::Null) {
                    app
                } else {
                    user
                },
                256,
                false
            )?
        );
        check(sender(&sender_id.clone().into()), "teams_sender_invalid")?;
        if own.contains(&JsString::from(sender_id.clone())) {
            exclude("ownSender");
            continue;
        }
        if !allowed.is_null()
            && !allowed
                .as_array()
                .unwrap()
                .contains(&sender_id.clone().into())
        {
            exclude("senderNotAllowed");
            continue;
        }
        let occurred = teams_time(raw.get("created_at").unwrap_or(&IvyJson::Null))?;
        check(
            occurred.as_str() >= selection.required("since")?.as_str()?.as_str(),
            "teams_selection_mismatch",
        )?;
        if occurred.as_str() >= selection.required("until")?.as_str()?.as_str() {
            exclude("outsideWindow");
            continue;
        }
        let body = text(raw.get("content"), 32768, true)?;
        let title = text(raw.get("title"), 1024, true)?;
        let link = text(raw.get("web_link"), 4096, false)?;
        let url = url::Url::parse(&link).ok();
        check(
            url.as_ref().is_some_and(|u| {
                u.scheme() == "https"
                    && u.username().is_empty()
                    && u.password().is_none_or(str::is_empty)
            }),
            "teams_url_invalid",
        )?;
        let has = raw.get("has_attachments").and_then(IvyJson::as_bool);
        let mentions = raw.get("mentions").and_then(IvyJson::as_array);
        check(
            has.is_some() && mentions.is_some_and(|v| v.len() <= 100),
            "teams_body_invalid",
        )?;
        if has == Some(true) {
            attachments += 1;
        }
        let revision = hash_json(&IvyJson::object([
            ("chatId", chat_id.clone()),
            ("id", id.clone().into()),
            ("senderId", sender_id.clone().into()),
            ("occurredAt", occurred.clone().into()),
            ("title", title.clone().into()),
            ("text", body.clone().into()),
            ("hasAttachments", has.unwrap().into()),
            ("mentions", raw.required("mentions")?.clone()),
            ("mediaVerificationRequired", true.into()),
        ]))?;
        let message = IvyJson::object([
            ("accountId", source.required("accountId")?.clone()),
            (
                "conversationId",
                hash_json(&IvyJson::Array(vec!["teams-chat".into(), chat_id.clone()]))?.into(),
            ),
            (
                "messageId",
                hash_json(&IvyJson::Array(vec![
                    "teams-message".into(),
                    chat_id.clone(),
                    id.into(),
                ]))?
                .into(),
            ),
            ("senderId", sender_id.into()),
            ("outgoing", false.into()),
            ("attachments", "expected".into()),
            ("occurredAt", occurred.into()),
            ("observedAt", observed_at.clone()),
            ("title", title.into()),
            ("text", body.into()),
            ("url", url.unwrap().as_str().into()),
            (
                "nativeRevision",
                format!("teams-content:{}", &revision[7..]).into(),
            ),
        ]);
        validate(Schema::Message, &message)?;
        projected.push(message);
    }
    let result = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("sourceId", source.required("sourceId")?.clone()),
        ("accountId", source.required("accountId")?.clone()),
        ("observedAt", observed_at.clone()),
        ("selection", selection.clone()),
        ("chats", IvyJson::Array(chats)),
        ("messages", IvyJson::Array(projected)),
        ("excluded", excluded),
        (
            "messagesWithAttachments",
            IvyJson::Number(attachments as f64),
        ),
    ]);
    canonical(&result, 192 * 1024)?;
    validate_microsoft("TeamsPage", &result)?;
    Ok(result)
}
