//! Account-bound original Outlook message projection. HTML is parsed as inert data.
use crate::{
    microsoft_schema::validate_microsoft,
    schema::{Schema, validate},
};
use html5ever::{parse_document, tendril::TendrilSink};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, hash::hash_json, json::canonical,
};
use ivy_sdk::native_evidence::native_instant;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::collections::HashSet;

pub(crate) fn check(value: bool, code: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(
            code,
            "Microsoft source requires complete account-bound original evidence.",
        ))
    }
}
pub(crate) fn record(value: Option<&IvyJson>) -> IvyResult<&IvyJson> {
    value.filter(|v| v.as_object().is_some()).ok_or_else(|| {
        IvyError::new(
            "microsoft_response_invalid",
            "Microsoft source requires complete account-bound original evidence.",
        )
    })
}
pub(crate) fn whitespace(c: char) -> bool {
    matches!(c, '\u{9}'..='\u{d}' | '\u{20}' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
pub(crate) fn text(value: Option<&IvyJson>, maximum: usize, empty: bool) -> IvyResult<String> {
    let value = value.and_then(IvyJson::as_js_string);
    check(
        value.is_some_and(|v| v.0.len() <= maximum),
        "microsoft_response_invalid",
    )?;
    let value = value.unwrap().to_utf8().map_err(|_| {
        IvyError::new(
            "microsoft_response_invalid",
            "Microsoft source requires complete account-bound original evidence.",
        )
    })?;
    check((empty || !value.trim_matches(whitespace).is_empty()) && !value.chars().any(|c| matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}')), "microsoft_response_invalid")?;
    Ok(value)
}
fn email(value: Option<&IvyJson>) -> IvyResult<String> {
    let value = text(value, 256, false)?;
    let mut parts = value.split('@');
    check(
        parts.next().is_some_and(|v| !v.is_empty())
            && parts.next().is_some_and(|v| !v.is_empty())
            && parts.next().is_none()
            && !value
                .chars()
                .any(|c| whitespace(c) || matches!(c, '<' | '>')),
        "microsoft_response_invalid",
    )?;
    Ok(value.to_lowercase())
}
pub(crate) fn utc(value: Option<&IvyJson>) -> IvyResult<String> {
    let raw = text(value, 40, false)?;
    let bytes = raw.as_bytes();
    let valid = (bytes.len() == 20 || (22..=28).contains(&bytes.len()))
        && [4, 7, 10, 13, 16]
            .into_iter()
            .zip(b"--T::")
            .all(|(n, c)| bytes.get(n) == Some(c))
        && bytes.last() == Some(&b'Z')
        && bytes
            .iter()
            .take(19)
            .enumerate()
            .all(|(i, c)| [4, 7, 10, 13, 16].contains(&i) || c.is_ascii_digit())
        && (bytes.len() == 20
            || bytes[19] == b'.' && bytes[20..bytes.len() - 1].iter().all(u8::is_ascii_digit));
    check(valid, "microsoft_time_invalid")?;
    let fraction = if bytes.len() == 20 {
        ""
    } else {
        &raw[20..raw.len() - 1]
    };
    check(
        fraction.len() <= 3 || fraction.as_bytes()[3..].iter().all(|c| *c == b'0'),
        "microsoft_time_invalid",
    )?;
    let mut millis = fraction.chars().take(3).collect::<String>();
    while millis.len() < 3 {
        millis.push('0');
    }
    let normalized = format!("{}.{millis}Z", &raw[..19]);
    check(
        native_instant(&normalized.as_str().into(), None).is_ok(),
        "microsoft_time_invalid",
    )?;
    Ok(normalized)
}
pub fn microsoft_profile(value: &IvyJson) -> IvyResult<IvyJson> {
    let source = record(Some(value))?;
    let profile = IvyJson::object([
        ("id", text(source.get("id"), 256, false)?.into()),
        ("email", text(source.get("email"), 256, false)?.into()),
    ]);
    email(profile.get("email"))?;
    validate_microsoft("MicrosoftProfile", &profile)?;
    Ok(profile)
}
pub fn microsoft_mcp(value: &IvyJson) -> IvyResult<IvyJson> {
    let result = record(Some(value))?;
    check(
        result.get("content").and_then(IvyJson::as_array).is_some(),
        "microsoft_response_invalid",
    )?;
    if result.get("isError") == Some(&IvyJson::Bool(true)) {
        let failure = record(result.get("structuredContent"))?;
        if failure.get("error_code") == Some(&"RATE_LIMITED".into()) {
            let seconds = failure.get("retry_after_seconds").and_then(IvyJson::as_f64);
            check(
                seconds.is_some_and(|v| {
                    v.is_finite() && v.fract() == 0.0 && (1.0..=86400.0).contains(&v)
                }),
                "microsoft_retry_invalid",
            )?;
            return Err(IvyError::new(
                "microsoft_rate_limited",
                "Microsoft source must wait before a later new attempt.",
            )
            .with_outcome(Outcome::Completed)
            .with_details(IvyJson::object([(
                "retryAfterSeconds",
                IvyJson::Number(seconds.unwrap()),
            )])));
        }
        return Err(IvyError::new(
            "microsoft_provider_failed",
            "Microsoft rejected the original read.",
        )
        .with_outcome(Outcome::Completed));
    }
    check(
        matches!(
            result.get("isError"),
            None | Some(IvyJson::Null) | Some(IvyJson::Bool(false))
        ),
        "microsoft_response_invalid",
    )?;
    Ok(record(result.get("structuredContent"))?.clone())
}
fn selection(value: &IvyJson) -> IvyResult<()> {
    validate_microsoft("OutlookSelection", value)?;
    check(
        IvyJson::from(utc(value.get("since"))?) == *value.required("since")?
            && IvyJson::from(utc(value.get("until"))?) == *value.required("until")?
            && value.required("since")?.as_str()? < value.required("until")?.as_str()?,
        "microsoft_selection_invalid",
    )
}
pub fn outlook_arguments(value: &IvyJson) -> IvyResult<IvyJson> {
    selection(value)?;
    Ok(IvyJson::object([
        ("folder_id", "inbox".into()),
        ("top", value.required("pageSize")?.clone()),
        ("skip", value.required("skip")?.clone()),
        (
            "filter",
            format!(
                "receivedDateTime ge {} and receivedDateTime lt {} and isRead eq false",
                value.required("since")?.as_str()?,
                value.required("until")?.as_str()?
            )
            .into(),
        ),
        ("order_by", "receivedDateTime desc".into()),
    ]))
}
fn block(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "div"
            | "dl"
            | "dt"
            | "dd"
            | "footer"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "tr"
            | "ul"
    )
}
fn inert(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "template" | "head")
}
fn attributes(attrs: &[html5ever::Attribute]) -> IvyJson {
    IvyJson::Array(
        attrs
            .iter()
            .map(|a| {
                let mut value = IvyJson::object([
                    ("name", a.name.local.to_string().into()),
                    ("value", a.value.to_string().into()),
                ]);
                if !a.name.ns.as_ref().is_empty() {
                    value.insert(
                        "prefix",
                        a.name.prefix.as_ref().map_or("", |p| p.as_ref()).into(),
                    );
                    value.insert("namespace", a.name.ns.to_string().into());
                }
                value
            })
            .collect(),
    )
}
pub fn outlook_inline_references(value: &IvyJson) -> IvyResult<Vec<String>> {
    let body = record(Some(value))?;
    if body.get("contentType") != Some(&"html".into()) {
        return Ok(vec![]);
    }
    let dom = parse_document(RcDom::default(), Default::default()).one(text(
        body.get("content"),
        1024 * 1024,
        true,
    )?);
    let mut pending = vec![dom.document.clone()];
    let mut references = vec![];
    let mut visited = 0;
    while let Some(node) = pending.pop() {
        visited += 1;
        check(visited <= 100000, "microsoft_body_too_large")?;
        if let NodeData::Element { name, attrs, .. } = &node.data {
            let tag = name.local.as_ref();
            if inert(tag) {
                continue;
            }
            if matches!(tag, "img" | "image" | "object" | "video" | "source") {
                references.push(hash_json(&IvyJson::object([
                    ("tag", tag.into()),
                    ("attrs", attributes(&attrs.borrow())),
                ]))?);
                check(references.len() <= 1024, "microsoft_body_too_large")?;
            }
        }
        pending.extend(node.children.borrow().iter().cloned());
    }
    references.sort();
    Ok(references)
}
pub fn outlook_has_media(raw: &IvyJson) -> IvyResult<bool> {
    if raw.get("has_attachments") == Some(&IvyJson::Bool(true)) {
        return Ok(true);
    }
    Ok(!outlook_inline_references(raw.get("body").ok_or_else(|| {
        IvyError::new(
            "microsoft_response_invalid",
            "Microsoft source requires complete account-bound original evidence.",
        )
    })?)?
    .is_empty())
}
fn no_credentials(url: &url::Url) -> bool {
    url.username().is_empty() && url.password().is_none_or(str::is_empty)
}
pub fn outlook_body(value: &IvyJson) -> IvyResult<String> {
    let body = record(Some(value))?;
    let content = text(body.get("content"), 1024 * 1024, true)?;
    check(
        matches!(
            body.get("contentType")
                .and_then(|v| v.as_str().ok())
                .as_deref(),
            Some("text" | "html")
        ),
        "microsoft_body_invalid",
    )?;
    if body.get("contentType") == Some(&"text".into()) {
        return text(Some(&content.into()), 32768, true);
    }
    let dom = parse_document(RcDom::default(), Default::default()).one(content);
    enum Pending {
        Node(Handle),
        Text(String),
    }
    let mut pending = vec![Pending::Node(dom.document.clone())];
    let mut parts = String::new();
    let mut nodes = 0;
    while let Some(part) = pending.pop() {
        let node = match part {
            Pending::Text(s) => {
                parts.push_str(&s);
                continue;
            }
            Pending::Node(n) => n,
        };
        nodes += 1;
        check(nodes <= 100000, "microsoft_body_too_large")?;
        match &node.data {
            NodeData::Text { contents } => {
                parts.push_str(&contents.borrow());
                continue;
            }
            NodeData::Element { name, attrs, .. } => {
                let tag = name.local.as_ref();
                if inert(tag) {
                    continue;
                }
                let attrs = attrs.borrow();
                let attr = |key: &str| {
                    attrs
                        .iter()
                        .find(|a| a.name.local.as_ref() == key)
                        .map(|a| a.value.as_ref())
                };
                if tag == "br" || block(tag) {
                    parts.push('\n');
                }
                if block(tag) {
                    pending.push(Pending::Text("\n".into()));
                }
                if matches!(tag, "td" | "th") {
                    parts.push(' ');
                    pending.push(Pending::Text(" ".into()));
                }
                if tag == "a" {
                    if let Some(href) = attr("href").filter(|v| !v.is_empty()) {
                        if let Ok(url) = url::Url::parse(href) {
                            if matches!(url.scheme(), "https" | "http" | "mailto")
                                && no_credentials(&url)
                            {
                                pending.push(Pending::Text(format!(
                                    " ({})",
                                    text(Some(&url.as_str().into()), 4096, false)?
                                )));
                            }
                        }
                    }
                }
                if tag == "img" {
                    if let Some(alt) = attr("alt").filter(|v| !v.is_empty()) {
                        parts.push_str(&format!(" [{alt}] "));
                    }
                }
            }
            _ => (),
        }
        pending.extend(
            node.children
                .borrow()
                .iter()
                .rev()
                .cloned()
                .map(Pending::Node),
        );
    }
    // Original ASCII space folding, followed by ECMAScript trim.
    let mut folded = String::new();
    let mut space = false;
    for c in parts.chars() {
        if matches!(c, '\t' | '\r' | ' ') {
            if !space {
                folded.push(' ');
            }
            space = true;
        } else {
            space = false;
            folded.push(c);
        }
    }
    let lines = folded
        .split('\n')
        .map(|s| s.trim_matches(' '))
        .collect::<Vec<_>>()
        .join("\n");
    let mut output = String::new();
    let mut newlines = 0;
    for c in lines.chars() {
        if c == '\n' {
            newlines += 1;
            if newlines <= 2 {
                output.push(c);
            }
        } else {
            newlines = 0;
            output.push(c);
        }
    }
    text(Some(&output.trim_matches(whitespace).into()), 32768, true)
}
pub fn outlook_page(
    value: &IvyJson,
    profile: &IvyJson,
    source: &IvyJson,
    selected: &IvyJson,
    observed_at: &IvyJson,
) -> IvyResult<IvyJson> {
    validate_microsoft("MicrosoftProfile", profile)?;
    validate(Schema::SourceDefinition, source)?;
    selection(selected)?;
    check(
        source.get("kind") == Some(&"email".into()) && source.get("accountId") == profile.get("id"),
        "microsoft_account_mismatch",
    )?;
    let senders = |key: &str| -> IvyResult<Vec<JsString>> {
        source
            .required(key)?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
            .collect()
    };
    let own_senders = senders("ownSenderIds")?;
    let allowed = if source.required("allowedSenderIds")?.is_null() {
        None
    } else {
        Some(senders("allowedSenderIds")?)
    };
    for id in own_senders.iter().chain(allowed.iter().flatten()) {
        check(
            JsString::from(email(Some(&IvyJson::String(id.clone())))?) == *id,
            "microsoft_sender_config_invalid",
        )?;
    }
    let normalized_observed = utc(Some(observed_at))?;
    let observed = observed_at.as_str()?;
    check(
        normalized_observed == observed && observed >= selected.required("until")?.as_str()?,
        "microsoft_time_invalid",
    )?;
    canonical(value, 8 * 1024 * 1024)?;
    let page = record(Some(value))?;
    let entries = page.get("value").and_then(IvyJson::as_array);
    check(
        entries.is_some_and(|v| {
            v.len() as f64 <= selected.get("pageSize").and_then(IvyJson::as_f64).unwrap()
        }) && matches!(page.get("has_more"), Some(IvyJson::Bool(_))),
        "microsoft_page_incomplete",
    )?;
    let entries = entries.unwrap();
    let more = page.get("has_more") == Some(&IvyJson::Bool(true));
    if more {
        let next = page.get("next_from_index").and_then(IvyJson::as_f64);
        check(
            !entries.is_empty()
                && next.is_some_and(|n| {
                    n.is_finite()
                        && n.fract() == 0.0
                        && n > selected.required("skip").unwrap().as_f64().unwrap()
                        && n <= 1000000.0
                })
                && page
                    .get("next_link")
                    .and_then(IvyJson::as_js_string)
                    .is_some(),
            "microsoft_page_incomplete",
        )?;
        let url = page.required("next_link")?.as_js_string().unwrap();
        let url = url::Url::parse(&String::from_utf16_lossy(&url.0));
        check(
            url.is_ok_and(|u| {
                u.scheme() == "https"
                    && u.host_str() == Some("graph.microsoft.com")
                    && no_credentials(&u)
            }),
            "microsoft_page_incomplete",
        )?;
    } else {
        check(
            page.get("next_from_index") == Some(&IvyJson::Null)
                && page.get("next_link") == Some(&IvyJson::Null),
            "microsoft_page_incomplete",
        )?;
    }
    let mut seen = HashSet::new();
    let mut own = own_senders.into_iter().collect::<HashSet<_>>();
    own.insert(email(profile.get("email"))?.into());
    let mut previous = selected.required("until")?.as_str()?;
    let mut messages = vec![];
    let (mut own_count, mut disallowed_count, mut media_count) = (0, 0, 0);
    for entry in entries {
        let raw = record(Some(entry))?;
        let sender = email(record(record(raw.get("sender"))?.get("emailAddress"))?.get("address"))?;
        if own.contains(&sender.as_str().into()) {
            own_count += 1;
            continue;
        }
        if allowed
            .as_ref()
            .is_some_and(|v| !v.contains(&sender.as_str().into()))
        {
            disallowed_count += 1;
            continue;
        }
        let provider = text(raw.get("id"), 4096, false)?;
        check(seen.insert(provider.clone()), "microsoft_duplicate_id")?;
        let occurred = utc(raw.get("receivedDateTime"))?;
        check(
            occurred >= selected.required("since")?.as_str()?
                && occurred < selected.required("until")?.as_str()?
                && occurred <= previous
                && raw.get("isRead") == Some(&IvyJson::Bool(false)),
            "microsoft_selection_mismatch",
        )?;
        previous = occurred.clone();
        check(
            matches!(raw.get("has_attachments"), Some(IvyJson::Bool(_))),
            "microsoft_response_invalid",
        )?;
        let references = outlook_inline_references(raw.get("body").unwrap_or(&IvyJson::Null))?;
        let has_media =
            raw.get("has_attachments") == Some(&IvyJson::Bool(true)) || !references.is_empty();
        if has_media {
            media_count += 1;
        }
        let title = text(raw.get("subject"), 1024, true)?;
        let body = outlook_body(raw.get("body").unwrap_or(&IvyJson::Null))?;
        let link = text(raw.get("web_link"), 4096, false)?;
        check(
            url::Url::parse(&link)
                .is_ok_and(|u| u.scheme() == "https" && no_credentials(&u) && u.as_str() == link),
            "microsoft_url_invalid",
        )?;
        let message_id = hash_json(&IvyJson::Array(vec![
            "outlook-message".into(),
            provider.clone().into(),
        ]))?;
        let revision = hash_json(&IvyJson::object([
            ("projectionVersion", IvyJson::Number(2.0)),
            ("providerId", provider.into()),
            ("sender", sender.clone().into()),
            ("occurredAt", occurred.clone().into()),
            ("title", title.clone().into()),
            ("text", body.clone().into()),
            ("hasAttachments", raw.required("has_attachments")?.clone()),
            (
                "inlineReferences",
                IvyJson::Array(references.iter().cloned().map(IvyJson::from).collect()),
            ),
        ]))?;
        let message = IvyJson::object([
            ("accountId", profile.required("id")?.clone()),
            ("conversationId", message_id.clone().into()),
            ("messageId", message_id.into()),
            ("senderId", sender.into()),
            ("outgoing", false.into()),
            (
                "attachments",
                if has_media { "expected" } else { "none" }.into(),
            ),
            ("occurredAt", occurred.into()),
            ("observedAt", observed_at.clone()),
            ("title", title.into()),
            ("text", body.into()),
            ("url", link.into()),
            (
                "nativeRevision",
                format!(
                    "{}{}",
                    if references.is_empty() {
                        "outlook-content:"
                    } else {
                        "outlook-inline-content:"
                    },
                    &revision[7..]
                )
                .into(),
            ),
        ]);
        validate(Schema::Message, &message)?;
        messages.push(message);
    }
    let result = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("sourceId", source.required("sourceId")?.clone()),
        ("accountId", profile.required("id")?.clone()),
        ("observedAt", observed_at.clone()),
        ("selection", selected.clone()),
        (
            "nextIndex",
            if more {
                page.required("next_from_index")?.clone()
            } else {
                IvyJson::Null
            },
        ),
        ("complete", (!more).into()),
        ("messages", IvyJson::Array(messages)),
        (
            "excluded",
            IvyJson::object([
                ("ownSender", IvyJson::Number(own_count as f64)),
                ("senderNotAllowed", IvyJson::Number(disallowed_count as f64)),
            ]),
        ),
        (
            "messagesWithAttachments",
            IvyJson::Number(media_count as f64),
        ),
    ]);
    canonical(&result, 192 * 1024)?;
    validate_microsoft("OutlookPage", &result)?;
    Ok(result)
}
