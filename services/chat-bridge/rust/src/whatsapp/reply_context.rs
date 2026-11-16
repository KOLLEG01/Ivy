//! Bounded quoted-message data over the original account and transport journal.
use super::{
    identity::identity, journal::Inbox, journal_io::JournalIo, journal_owner::Journal,
    media::MediaDependencies, media_content, messages::Translator, sender,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{parse, stringify},
};
use rusqlite::OptionalExtension;

const QUOTE_BYTES: usize = 4096;
const MESSAGE_CHARS: usize = 65536;

fn field(value: &Json, name: &str) -> Option<String> {
    value
        .get(name)?
        .as_str()
        .ok()
        .filter(|s| !s.is_empty() && s.len() <= 256)
}
pub(super) fn phone_jid(jid: &str) -> Option<String> {
    sender::jid_phone(jid).map(|phone| format!("{phone}@s.whatsapp.net"))
}
pub(super) fn lid_user(jid: &str) -> Option<String> {
    if jid.len() > 256 {
        return None;
    }
    let jid = jid.parse::<whatsapp_rust::Jid>().ok()?;
    if !jid.is_lid() || jid.integrator != 0 {
        return None;
    }
    let bare = jid.to_non_ad().to_string();
    let user = bare.strip_suffix("@lid")?;
    (!user.is_empty() && user.bytes().all(|b| b.is_ascii_digit())).then(|| user.to_owned())
}
fn sender_config(config: &Json) -> Option<&Json> {
    let senders = config.get("senders")?.as_array()?;
    (senders.len() == 1).then(|| &senders[0])
}
fn sender_jid(config: &Json) -> Option<String> {
    sender::phone(&field(sender_config(config)?, "phoneNumber")?)
        .map(|p| format!("{p}@s.whatsapp.net"))
}
fn bot_jid(config: &Json) -> Option<String> {
    sender::phone(&field(config, "botPhoneNumber")?).map(|p| format!("{p}@s.whatsapp.net"))
}
fn scope(config: &Json, chat: &str, author: &str, id: &str) -> Option<Json> {
    Some(Json::object([
        ("adapter", "whatsapp".into()),
        ("accountId", field(config, "accountId")?.into()),
        (
            "channelId",
            field(sender_config(config)?, "channelId")?.into(),
        ),
        ("chatJid", chat.into()),
        ("authorJid", author.into()),
        ("messageId", id.into()),
    ]))
}

/// Records actual dispatch scope, never retroactively assigns scope to old sent rows.
pub(super) fn record_output(
    journal: &mut JournalIo,
    config: &Json,
    id: &str,
    jid: &str,
) -> IvyResult<()> {
    let Some(chat) = phone_jid(jid).filter(|chat| Some(chat) == sender_jid(config).as_ref()) else {
        return Ok(());
    };
    let Some(author) = bot_jid(config) else {
        return Ok(());
    };
    let Some(scope) = scope(config, &chat, &author, id) else {
        return Ok(());
    };
    let key = format!("out-scope:{id}");
    let prior = journal.get(&key)?;
    if !prior.is_null() && prior != scope {
        return Err(IvyError::new(
            "whatsui_identity_conflict",
            "An outgoing message cannot change its original account or channel.",
        ));
    }
    journal.set(&key, &scope)
}

fn context(content: &Json) -> Option<&Json> {
    [
        "extendedTextMessage",
        "imageMessage",
        "audioMessage",
        "documentMessage",
    ]
    .iter()
    .find_map(|key| {
        let ctx = content.get(key)?.get("contextInfo")?;
        // Mentions and forwarding metadata alone do not identify a reply.
        (ctx.get("stanzaId").is_some_and(|v| !v.is_null())
            || ctx.get("quotedMessage").is_some_and(|v| !v.is_null()))
        .then_some(ctx)
    })
}
fn expiring(context: &Json) -> bool {
    context
        .get("expiration")
        .and_then(Json::as_f64)
        .is_some_and(|n| n > 0.0)
}
fn restricted(message: &Json) -> bool {
    let mut value = message;
    for _ in 0..5 {
        if [
            "ephemeralMessage",
            "viewOnceMessage",
            "viewOnceMessageV2",
            "viewOnceMessageV2Extension",
        ]
        .iter()
        .any(|key| value.get(key).is_some_and(|v| !v.is_null()))
        {
            return true;
        }
        if [
            "extendedTextMessage",
            "imageMessage",
            "audioMessage",
            "documentMessage",
            "videoMessage",
            "stickerMessage",
        ]
        .iter()
        .filter_map(|key| value.get(key)?.get("contextInfo"))
        .any(expiring)
        {
            return true;
        }
        let Some(inner) = [
            "documentWithCaptionMessage",
            "editedMessage",
            "associatedChildMessage",
            "groupStatusMessage",
            "groupStatusMessageV2",
        ]
        .iter()
        .find_map(|key| value.get(key)?.get("message")) else {
            return false;
        };
        value = inner;
    }
    true
}
fn quote_content(message: &Json) -> IvyResult<Json> {
    media_content::content(&stringify(
        &Json::object([("message", message.clone())]),
        usize::MAX,
    )?)
}
fn kind(content: &Json) -> &'static str {
    for (field, kind) in [
        ("imageMessage", "image"),
        ("audioMessage", "audio"),
        ("documentMessage", "document"),
        ("videoMessage", "video"),
        ("stickerMessage", "sticker"),
    ] {
        if content.get(field).is_some_and(|v| !v.is_null()) {
            return kind;
        }
    }
    "text"
}
fn quote_text(content: &Json) -> String {
    media_content::text(content, false)
        .ok()
        .and_then(|v| v.to_utf8().ok())
        .unwrap_or_default()
}
fn chars(value: &JsString) -> usize {
    char::decode_utf16(value.0.iter().copied()).count()
}
fn bounded(text: &str, maximum: usize) -> &str {
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

async fn original(
    journal: &Journal,
    config: &Json,
    expected: &Json,
    deps: &dyn MediaDependencies,
) -> IvyResult<Option<Json>> {
    let id = field(expected, "messageId").ok_or_else(IvyError::internal)?;
    if field(expected, "authorJid") == bot_jid(config) {
        return journal.access(|j| {
            if j.get(&format!("out-scope:{id}"))? != *expected { return Ok(None); }
            let text: Option<String> = j.connection().query_row(
                "SELECT text FROM outgoing WHERE id=? AND state='sent' AND text<>'' AND id NOT IN (SELECT id FROM retired_outgoing)",
                [&id], |r| r.get(0),
            ).optional().map_err(|_| IvyError::internal())?;
            let Some(text) = text else { return Ok(None); };
            let file = j.get(&format!("out-file:{id}"))?;
            let saved = if file.is_null() { Json::Null } else { j.get(&file.as_str()?)? };
            let kind = saved.get("delivery").and_then(|v| v.get("kind")).and_then(Json::as_js_string);
            Ok(Some(if kind.is_some_and(|v| v.eq_str("image")) {
                Json::object([("imageMessage", Json::object([("caption", text.into())]))])
            } else if kind.is_some_and(|v| v.eq_str("document")) {
                Json::object([("documentMessage", Json::object([("caption", text.into())]))])
            } else {
                Json::object([("conversation", text.into())])
            }))
        });
    }
    if field(expected, "authorJid") != sender_jid(config) {
        return Ok(None);
    }
    let inbox_id = identity(&[
        expected.required("accountId")?.clone(),
        expected.required("channelId")?.clone(),
        id.into(),
    ])?;
    let principal = field(
        sender_config(config).ok_or_else(IvyError::internal)?,
        "principalId",
    )
    .ok_or_else(IvyError::internal)?;
    let saved: Option<(String, String)> = journal.access(|j| {
        j.connection().query_row(
            "SELECT jid,message FROM inbox WHERE id=? AND sender=? AND message<>'' AND length(CAST(message AS BLOB))<=1048576",
            (&inbox_id, &principal), |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional().map_err(|_| IvyError::internal())
    })?;
    let Some((jid, raw)) = saved else {
        return Ok(None);
    };
    if deps.canonical_jid(&jid).await? != field(expected, "chatJid") {
        return Ok(None);
    }
    let raw = parse(&raw, usize::MAX)?;
    let message = raw.required("message")?;
    Ok(Some(message.clone()))
}

pub(super) async fn append(
    journal: &Journal,
    config: &Json,
    row: &Inbox,
    content: &Json,
    text: JsString,
    deps: &dyn MediaDependencies,
    translator: Translator,
) -> IvyResult<JsString> {
    let Some(ctx) = context(content) else {
        return Ok(text);
    };
    let mut notes = Vec::new();
    let mut note = |key| -> IvyResult<()> {
        notes.push(Json::String(translator.translate(key, None)?));
        Ok(())
    };
    let id = field(ctx, "stanzaId");
    let author = field(ctx, "participant");
    let remote = field(ctx, "remoteJid");
    let current = deps.canonical_jid(&row.jid).await?;
    let chat = match &remote {
        Some(remote) => deps.canonical_jid(remote).await?,
        None if ctx.get("remoteJid").is_none_or(Json::is_null) => current.clone(),
        _ => None,
    };
    let author_jid = match &author {
        Some(author) => deps.canonical_jid(author).await?,
        None => None,
    };
    let expected = match (&id, &chat, &author_jid) {
        (Some(id), Some(chat), Some(author))
            if field(sender_config(config).unwrap_or(&Json::Null), "principalId").as_deref()
                == Some(row.sender.as_str())
                && Some(chat) == current.as_ref()
                && Some(chat) == sender_jid(config).as_ref()
                && (Some(author) == bot_jid(config).as_ref()
                    || Some(author) == sender_jid(config).as_ref()) =>
        {
            scope(config, chat, author, id)
        }
        _ => None,
    };
    if expected.is_none() {
        note("quote.identity_unresolved")?;
    }
    let snapshot = ctx.get("quotedMessage").filter(|v| v.as_object().is_some());
    let deleted = snapshot
        .and_then(|v| v.get("protocolMessage"))
        .is_some_and(|v| {
            v.get("type") == Some(&Json::from("REVOKE"))
                || v.get("type") == Some(&Json::Number(0.0))
        });
    let private = snapshot.is_some_and(restricted) || expiring(ctx);
    let mut source = "quote.unavailable";
    let mut selected = None;
    if deleted {
        source = "quote.deleted";
    } else if private {
        source = "quote.restricted";
    } else {
        if let Some(snapshot) = snapshot {
            selected = Some(quote_content(snapshot)?);
            source = "quote.embedded";
        }
        if selected.as_ref().is_none_or(|v| quote_text(v).is_empty()) {
            if let Some(expected) = &expected {
                if let Some(message) = original(journal, config, expected, deps).await? {
                    if restricted(&message) {
                        selected = None;
                        source = "quote.restricted";
                    } else {
                        selected = Some(quote_content(&message)?);
                        source = "quote.original";
                    }
                }
            }
        }
    }
    let raw_text = selected.as_ref().map(quote_text).unwrap_or_default();
    let selected_kind = selected.as_ref().map(kind).unwrap_or("unknown");
    if selected.as_ref().is_some_and(|v| context(v).is_some()) {
        note("quote.nested")?;
    }
    if raw_text.is_empty() && selected.is_some() {
        note("quote.no_text")?;
    }
    let mut quote = Json::object([
        ("messageId", id.map_or(Json::Null, Json::from)),
        ("participant", author.map_or(Json::Null, Json::from)),
        ("remoteJid", remote.map_or(Json::Null, Json::from)),
        ("source", Json::String(translator.translate(source, None)?)),
        ("kind", selected_kind.into()),
        ("notes", Json::Array(notes)),
        ("truncated", (raw_text.len() > QUOTE_BYTES).into()),
        ("text", bounded(&raw_text, QUOTE_BYTES).into()),
    ]);
    let header = translator.translate("quote.header", None)?;
    let mut prefix = text.0;
    prefix.extend("\n\n".encode_utf16());
    prefix.extend(header.0);
    prefix.extend("\n".encode_utf16());
    let prefix = JsString(prefix);
    let serialize =
        |quote: &Json| -> IvyResult<JsString> { Ok(stringify(quote, usize::MAX)?.into()) };
    let mut encoded = serialize(&quote)?;
    if chars(&prefix) + chars(&encoded) > MESSAGE_CHARS {
        quote.insert("text", "".into());
        quote.insert("truncated", true.into());
        encoded = serialize(&quote)?;
        if chars(&prefix) + chars(&encoded) > MESSAGE_CHARS {
            return Err(IvyError::new_js(
                "whatsapp_media_limit".into(),
                translator.translate("media.message_limit", None)?,
            ));
        }
        let candidates: Vec<char> = bounded(&raw_text, QUOTE_BYTES).chars().collect();
        let (mut low, mut high) = (0, candidates.len());
        while low < high {
            let mid = (low + high).div_ceil(2);
            quote.insert("text", candidates[..mid].iter().collect::<String>().into());
            if chars(&prefix) + chars(&serialize(&quote)?) <= MESSAGE_CHARS {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        quote.insert("text", candidates[..low].iter().collect::<String>().into());
        encoded = serialize(&quote)?;
    }
    let mut result = prefix.0;
    result.extend(encoded.0);
    Ok(JsString(result))
}
