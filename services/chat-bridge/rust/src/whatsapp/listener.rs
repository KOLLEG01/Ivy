//! Independent Main history and exact transport delivery checkpoints.
pub use super::listener_main::ListenerMain;
use super::{
    identity::identity, journal_owner::Journal, messages::Translator, native::WhatsAppNative,
    sender::phone,
};
use crate::{generation::ChatGenerationLease, main_context::truthy, store::mutation};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{stringify, try_js_string},
};
use ivy_sdk::{
    clock::{Clock, SystemClock},
    rpc::RequestFuture,
};
use std::{
    collections::VecDeque,
    future::poll_fn,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
};

pub trait ListenerNative: Send + Sync {
    fn bound(&self) -> RequestFuture<'_>;
    fn turn_page<'a>(&'a self, thread: &'a JsString, cursor: Json) -> RequestFuture<'a>;
    fn source_name(&self, thread: JsString) -> RequestFuture<'_>;
}
impl ListenerNative for WhatsAppNative {
    fn bound(&self) -> RequestFuture<'_> {
        Box::pin(WhatsAppNative::bound(self))
    }
    fn turn_page<'a>(&'a self, thread: &'a JsString, cursor: Json) -> RequestFuture<'a> {
        Box::pin(WhatsAppNative::turn_page(self, thread, cursor))
    }
    fn source_name(&self, thread: JsString) -> RequestFuture<'_> {
        Box::pin(async move {
            self.owner()
                .observation(
                    "thread/read",
                    Json::object([
                        ("threadId", Json::String(thread)),
                        ("includeTurns", false.into()),
                    ]),
                )
                .await
        })
    }
}
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
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
fn text(value: &Json, key: &str) -> IvyResult<JsString> {
    value
        .get(key)
        .and_then(Json::as_js_string)
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn contains(value: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() {
        Some(0)
    } else {
        value.windows(needle.len()).position(|v| v == needle)
    }
}
fn units(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}
fn concat(parts: &[&JsString]) -> JsString {
    JsString(parts.iter().flat_map(|v| v.0.iter().copied()).collect())
}
pub fn whatsapp_text(value: &JsString, translator: &Translator) -> IvyResult<JsString> {
    let marker = units("::created-thread{");
    let mut from = 0;
    let mut clean = Vec::new();
    while let Some(index) = contains(&value.0[from..], &marker) {
        let start = from + index;
        let Some(end) = value.0[start + marker.len()..]
            .iter()
            .position(|unit| *unit == 125)
        else {
            break;
        };
        clean.extend_from_slice(&value.0[from..start]);
        from = start + marker.len() + end + 1;
    }
    clean.extend_from_slice(&value.0[from..]);
    let clean = JsString(clean).trim();
    if clean.0.is_empty() && contains(&value.0, &marker).is_some() {
        translator.translate("listener.created", None)
    } else {
        Ok(clean)
    }
}
fn auth_required(value: &JsString) -> bool {
    let lower: Vec<_> = value
        .0
        .iter()
        .map(|u| if (65..=90).contains(u) { u + 32 } else { *u })
        .collect();
    if [
        "workspace routing discovery unauthorized",
        "access token could not be refreshed",
        "not authenticated",
        "not logged in",
    ]
    .iter()
    .any(|needle| contains(&lower, &units(needle)).is_some())
    {
        return true;
    }
    lower
        .split(|u| [10, 13, 0x2028, 0x2029].contains(u))
        .any(|line| {
            let Some(start) = contains(line, &units("refresh token")) else {
                return false;
            };
            let rest = &line[start + "refresh token".len()..];
            contains(rest, &units("expired")).is_some()
                || contains(rest, &units("revoked")).is_some()
                || contains(rest, &units("already")).is_some_and(|at| {
                    contains(&rest[at + "already".len()..], &units("used")).is_some()
                })
        })
}
pub fn terminal_text(turn: &Json, translator: &Translator) -> IvyResult<JsString> {
    let error = property(turn, "error")?;
    let reason = error
        .filter(|v| v.as_object().is_some())
        .and_then(|v| v.get("message"))
        .and_then(Json::as_js_string)
        .map(JsString::trim)
        .unwrap_or_default();
    if turn.get("status") == Some(&Json::from("failed")) && !reason.0.is_empty() {
        if auth_required(&reason) {
            return translator.translate("listener.auth_required", None);
        }
        return translator.translate(
            "listener.failed",
            Some(&Json::object([(
                "reason",
                Json::String(JsString(reason.0.into_iter().take(600).collect())),
            )])),
        );
    }
    translator.translate(
        "listener.terminal",
        Some(&Json::object([(
            "status",
            Json::String(string(turn.get("status"))?),
        )])),
    )
}
pub fn delegation_source(item: &Json) -> IvyResult<Option<JsString>> {
    let kind = string(property(item, "type")?)?;
    if !["userMessage", "steeringUserMessage"]
        .iter()
        .any(|value| kind == JsString::from(*value))
    {
        return Ok(None);
    }
    let content = item.get("content").and_then(Json::as_array).unwrap_or(&[]);
    for entry in content {
        if entry.as_object().is_none() {
            continue;
        }
        if let Some(metadata) = entry
            .get("codexDelegation")
            .filter(|v| v.as_object().is_some())
        {
            if let Some(id) = metadata.get("sourceThreadId").and_then(Json::as_js_string) {
                return Ok(Some(id.clone()));
            }
        }
    }
    for value in std::iter::once(item.get("text")).chain(content.iter().map(|v| {
        if v.as_object().is_some() {
            v.get("text")
        } else {
            None
        }
    })) {
        let Some(text) = value.and_then(Json::as_js_string) else {
            continue;
        };
        let trim = text.trim();
        if !trim.0.starts_with(&units("<codex_delegation>"))
            || !trim.0.ends_with(&units("</codex_delegation>"))
        {
            continue;
        }
        let open = units("<source_thread_id>");
        let mut from = 0;
        while let Some(index) = contains(&text.0[from..], &open) {
            let start = from + index + open.len();
            let Some(end) = text.0[start..].iter().position(|u| *u == 60) else {
                break;
            };
            if end > 0 && text.0[start + end..].starts_with(&units("</source_thread_id>")) {
                return Ok(Some(JsString(text.0[start..start + end].to_vec()).trim()));
            }
            from = start;
        }
    }
    Ok(None)
}

fn key(parts: &[&JsString]) -> JsString {
    concat(parts)
}
fn sql_key(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn ids(value: &Json) -> IvyResult<Vec<String>> {
    value
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(Json::as_str)
        .collect()
}
impl WhatsAppListener {
    pub async fn outbox(&self) -> IvyResult<()> {
        for sender in self.senders()? {
            let caller = text(sender, "principalId")?;
            let channel = Json::object([
                ("adapter", "whatsapp".into()),
                ("accountId", f(&self.0.config, "accountId").clone()),
                ("channelId", f(sender, "channelId").clone()),
            ]);
            let expected = self.0.main.expected(&caller);
            let partition = concat(&[
                &self.0.main.definition_hash().into(),
                &":".into(),
                &string(sender.get("channelId"))?,
            ]);
            let offer_key = key(&[&"offer:".into(), &partition]);
            let reply_cursor = key(&[&"reply-cursor:".into(), &partition]);
            let mut pending = self.0.journal.get(&sql_key(&offer_key))?;
            if truthy(f(&pending, "offer"))
                && !truthy(
                    &self
                        .0
                        .main
                        .operation(&caller, f(f(&pending, "request"), "operationId"))
                        .await?,
                )
            {
                self.0.journal.set(&sql_key(&offer_key), &Json::Null)?;
                continue;
            }
            if !truthy(&pending) {
                let mut after = self.0.journal.get(&sql_key(&reply_cursor))?;
                if after.is_null() {
                    after = Json::Number(0.0);
                }
                let request = Json::object([
                    ("expectedBridge", expected.clone()),
                    ("channel", channel.clone()),
                    ("afterSequence", after.clone()),
                    ("limit", Json::Number(8.0)),
                ]);
                let history = self.0.main.history(&caller, &request).await?;
                let entries = f(&history, "entries")
                    .as_array()
                    .ok_or_else(IvyError::internal)?;
                let mut origin = false;
                for entry in entries {
                    let data = property(entry, "data")?.ok_or_else(IvyError::internal)?;
                    // JS's `in` throws for primitives, even when no origin is present.
                    if data.as_object().is_none() && data.as_array().is_none() {
                        return Err(IvyError::internal());
                    }
                    origin |= data.get("origin").is_some();
                    if origin {
                        break;
                    }
                }
                if !origin {
                    if js_number(f(&history, "throughSequence"))? > js_number(&after)? {
                        self.0
                            .journal
                            .set(&sql_key(&reply_cursor), f(&history, "throughSequence"))?;
                    }
                    continue;
                }
                let operation = self
                    .0
                    .main
                    .operation_id(Json::Array(vec![
                        "whatsapp-receive".into(),
                        Json::String(partition.clone()),
                        after.clone(),
                    ]))
                    .await?;
                pending = Json::object([(
                    "request",
                    Json::object([
                        ("action", "receive".into()),
                        ("operationId", operation),
                        ("expectedBridge", expected.clone()),
                        ("channel", channel.clone()),
                        ("afterSequence", after),
                        ("limit", Json::Number(8.0)),
                    ]),
                )]);
                self.0.journal.set(&sql_key(&offer_key), &pending)?;
            }
            if !truthy(f(&pending, "offer")) {
                let result = self.0.main.receive(&caller, f(&pending, "request")).await?;
                if result.get("phase") != Some(&Json::from("succeeded"))
                    || f(&result, "outcome").get("action") != Some(&Json::from("receive"))
                {
                    continue;
                }
                for field in ["offer", "replies"] {
                    if let Some(value) = f(&result, "outcome").get(field) {
                        pending.insert(field, value.clone());
                    } else {
                        pending.remove(field);
                    }
                }
                self.0.journal.set(&sql_key(&offer_key), &pending)?;
            }
            let replies = f(&pending, "replies")
                .as_array()
                .ok_or_else(IvyError::internal)?
                .to_vec();
            let mut complete = true;
            for reply in &replies {
                let data = reply.required("data")?;
                let origin = data.required("origin")?;
                let native = origin.get("kind") == Some(&Json::from("native"));
                let canonical = native
                    && truthy(
                        &self
                            .0
                            .main
                            .is_notice_input(
                                &mutation(
                                    &text(origin, "inputId")?.to_utf8()?,
                                    "native-user-message",
                                )?
                                .into(),
                            )
                            .await?,
                    );
                if canonical {
                    let saved = self
                        .0
                        .main
                        .reply(f(f(reply, "object"), "objectId"), f(data, "channel"))
                        .await?;
                    if f(&saved, "value").get("state") != Some(&Json::from("confirmed")) {
                        complete = false;
                    }
                } else if native {
                    let result = self
                        .0
                        .main
                        .result(f(origin, "result"), f(origin, "inputId"))
                        .await?;
                    let delivery_key = key(&[
                        &"turn-delivered:".into(),
                        &string(f(origin, "binding").get("objectId"))?,
                        &":".into(),
                        &string(f(&result, "value").get("turnId"))?,
                    ]);
                    let delivered = self.0.journal.get(&sql_key(&delivery_key))?;
                    if delivered.as_array().is_some()
                        && self.0.journal.access(|j| j.sent(&ids(&delivered)?))?
                    {
                        self.0.journal.set(&sql_key(&delivery_key), &true.into())?;
                    } else if delivered != Json::Bool(true) {
                        complete = false;
                    }
                } else {
                    let notice_key = key(&[
                        &"notice:".into(),
                        &string(f(reply, "object").get("objectId"))?,
                    ]);
                    let ids = self.0.journal.access(|j| {
                        j.enqueue(
                            &notice_key,
                            &jid(sender)?,
                            &text(data, "text")?,
                            true,
                            self.0.clock.utc_millis(),
                        )
                    })?;
                    if !self.0.journal.access(|j| j.sent(&ids))? {
                        complete = false;
                    }
                }
            }
            if !complete {
                continue;
            }
            if !replies.is_empty() {
                if pending.get("ack").is_none_or(Json::is_null) {
                    let offer = f(f(&pending, "offer"), "object")
                        .get("objectId")
                        .cloned()
                        .unwrap_or(Json::Null);
                    let operation = self
                        .0
                        .main
                        .operation_id(Json::Array(vec![
                            "whatsapp-acknowledge".into(),
                            offer.clone(),
                        ]))
                        .await?;
                    let ack = Json::object([
                        ("action", "acknowledge".into()),
                        ("operationId", operation),
                        ("expectedBridge", expected.clone()),
                        ("offerId", offer),
                        (
                            "replyIds",
                            Json::Array(
                                replies
                                    .iter()
                                    .map(|r| f(f(r, "object"), "objectId").clone())
                                    .collect(),
                            ),
                        ),
                    ]);
                    pending.insert("ack", ack);
                }
                self.0.journal.set(&sql_key(&offer_key), &pending)?;
                let result = self.0.main.acknowledge(&caller, f(&pending, "ack")).await?;
                if result.get("phase") != Some(&Json::from("succeeded")) {
                    continue;
                }
            }
            self.0.journal.access(|j| {
                j.try_transaction(|j| -> IvyResult<()> {
                    j.set(
                        &sql_key(&reply_cursor),
                        f(f(f(&pending, "offer"), "data"), "throughSequence"),
                    )?;
                    j.set(&sql_key(&offer_key), &Json::Null)
                })
            })?;
        }
        Ok(())
    }
    pub async fn notices(&self) -> IvyResult<()> {
        let cursor = self.0.state.lock().await.notice_cursor.clone();
        let page = self.0.main.pending_notices(cursor).await?;
        let sender = self.senders()?.first().ok_or_else(IvyError::internal)?;
        let replies = f(&page, "replies")
            .as_array()
            .ok_or_else(IvyError::internal)?;
        for reply in replies {
            let parts = self.0.main.notice_parts(reply).await?;
            let parts = parts.as_array().ok_or_else(IvyError::internal)?;
            let mut all = Vec::new();
            for part in parts {
                let id = string(f(part, "pin").get("objectId"))?;
                let prior = key(&[
                    &"mirrored-notice:".into(),
                    &string(sender.get("channelId"))?,
                    &":".into(),
                    &id,
                ]);
                let selected = if self.0.journal.access(|j| j.has_output(&prior))? {
                    prior
                } else {
                    key(&[&"notice:".into(), &id])
                };
                all.extend(self.0.journal.access(|j| {
                    j.enqueue(
                        &selected,
                        &jid(sender)?,
                        &text(f(part, "value"), "text")?,
                        true,
                        self.0.clock.utc_millis(),
                    )
                })?);
            }
            if self.0.journal.access(|j| j.sent(&all))? {
                self.0.main.confirm_notice(parts).await?;
            }
        }
        self.0.state.lock().await.notice_cursor = page
            .get("nextCursor")
            .filter(|v| !v.is_null())
            .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
            .transpose()?;
        Ok(())
    }
}
fn js_number(value: &Json) -> IvyResult<f64> {
    use ivy_contracts::json::number_from_string;
    Ok(match value {
        Json::Number(n) => *n,
        Json::Null => 0.0,
        Json::Bool(v) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        _ => number_from_string(&try_js_string(value)?),
    })
}
fn hash(value: &Json) -> IvyResult<Json> {
    Ok(identity(&[value.clone()])?.into())
}
fn existing_hash(value: &Json) -> IvyResult<Json> {
    value
        .get("hash")
        .filter(|v| !v.is_null())
        .cloned()
        .map_or_else(|| hash(f(value, "text")), Ok)
}
fn jid(sender: &Json) -> IvyResult<String> {
    let value = string(sender.get("phoneNumber"))?;
    Ok(format!(
        "{}@s.whatsapp.net",
        String::from_utf16(&value.0)
            .ok()
            .and_then(|v| phone(&v).map(str::to_owned))
            .unwrap_or_else(|| "null".into())
    ))
}
impl WhatsAppListener {
    pub async fn notification(&self, event: &Json, commentary: bool) -> IvyResult<Json> {
        let method = property(event, "method")?;
        if !["turn/started", "turn/completed", "item/completed"]
            .iter()
            .any(|v| method == Some(&Json::from(*v)))
        {
            return Ok(Json::Null);
        }
        let params = event.required("params").map_err(|_| IvyError::internal())?;
        let item = property(params, "item")?;
        if method == Some(&Json::from("item/completed")) {
            let Some(item) = item.filter(|v| v.as_object().is_some()) else {
                return Ok(Json::Null);
            };
            let kind = string(item.get("type"))?;
            if !["agentMessage", "userMessage", "steeringUserMessage"]
                .iter()
                .any(|v| kind == JsString::from(*v))
            {
                return Ok(Json::Null);
            }
        }
        let bound = self.0.native.bound().await?;
        let binding = bound.required("binding")?;
        let bid = text(f(binding, "pin"), "objectId")?;
        if params.get("threadId") != f(f(binding, "value"), "primary").get("nativeId") {
            return Ok(Json::Null);
        }
        if method == Some(&Json::from("turn/started")) {
            return Ok(Json::object([("busy", true.into())]));
        }
        if method == Some(&Json::from("turn/completed")) {
            return Ok(Json::object([("busy", false.into())]));
        }
        let item = item.ok_or_else(IvyError::internal)?;
        let mut state = self.0.state.lock().await;
        if let Some(turn) = params.get("turnId").and_then(Json::as_js_string) {
            self.source(&mut state, &bid, turn, item).await?;
        }
        if item.get("type") != Some(&Json::from("agentMessage")) {
            return Ok(Json::Null);
        }
        require(
            params.get("turnId").and_then(Json::as_js_string).is_some()
                && item.get("id").and_then(Json::as_js_string).is_some()
                && item.get("text").and_then(Json::as_js_string).is_some(),
            "native_read_failed",
            "Completed native message identity missing.",
        )?;
        let turn = text(params, "turnId")?;
        let notice_key = key(&[&"notice-turn:".into(), &bid, &":".into(), &turn]);
        if !state.sources.iter().any(|(id, _)| id == &turn)
            || truthy(&self.0.journal.get(&sql_key(&notice_key))?)
        {
            return Ok(Json::object([("busy", Json::Null)]));
        }
        let body = text(item, "text")?;
        let id = text(item, "id")?;
        for sender in self.senders()? {
            let message_key = key(&[
                &"message:".into(),
                &bid,
                &":".into(),
                &turn,
                &":".into(),
                &id,
                &":".into(),
                &string(sender.get("channelId"))?,
            ]);
            let existing = self.0.journal.get(&sql_key(&message_key))?;
            if truthy(&existing) {
                require(
                    existing_hash(&existing)? == hash(&Json::String(body.clone()))?,
                    "whatsapp_native_message_changed",
                    "A completed native message changed.",
                )?;
                continue;
            }
            let suppressed = item.get("phase") == Some(&Json::from("commentary")) && !commentary;
            self.0.journal.access(|j| {
                j.try_transaction(|j| -> IvyResult<()> {
                    let ids = if suppressed {
                        vec![]
                    } else {
                        j.enqueue(
                            &message_key,
                            &jid(sender)?,
                            &self.message_text(&state, &body, &turn)?,
                            false,
                            self.0.clock.utc_millis(),
                        )?
                    };
                    j.set(
                        &sql_key(&message_key),
                        &Json::object([
                            ("hash", hash(&Json::String(body.clone()))?),
                            (
                                "ids",
                                Json::Array(ids.into_iter().map(Json::from).collect()),
                            ),
                            ("suppressed", suppressed.into()),
                            ("at", Json::Number(self.0.clock.utc_millis() as f64)),
                        ]),
                    )
                })
            })?;
        }
        Ok(Json::object([("busy", Json::Null)]))
    }
    pub async fn tick(&self, commentary: bool) -> IvyResult<bool> {
        let bound = self.0.native.bound().await?;
        let binding = bound.required("binding")?;
        let bid = text(f(binding, "pin"), "objectId")?;
        let cursor_key = key(&[&"listener:".into(), &bid]);
        let mut cursor = self.0.journal.get(&sql_key(&cursor_key))?;
        if !truthy(&cursor) {
            cursor = Json::object([
                ("cursor", Json::Null),
                (
                    "baseline",
                    (!truthy(
                        &self
                            .0
                            .journal
                            .get(&sql_key(&key(&[&"listener-new:".into(), &bid])))?,
                    ))
                    .into(),
                ),
                ("started", false.into()),
            ]);
            self.0.journal.set(&sql_key(&cursor_key), &cursor)?;
        }
        let recovery = {
            let mut state = self.0.state.lock().await;
            if state
                .recovery
                .as_ref()
                .is_none_or(|r| r.lock().unwrap().bid != bid)
            {
                state.recovery = Some(Arc::new(Mutex::new(Recovery {
                    bid: bid.clone(),
                    cursor,
                    next: Json::Null,
                    head: None,
                    turns: vec![],
                    bytes: 0,
                    busy: false,
                })));
            }
            state.recovery.as_ref().unwrap().clone()
        };
        self.0.needs_recovery.store(false, Ordering::Relaxed);
        let mut done = false;
        let began = self.0.clock.utc_millis();
        let thread = text(f(f(binding, "value"), "primary"), "nativeId")?;
        for _ in 0..64 {
            let next = recovery.lock().unwrap().next.clone();
            let page = self.0.native.turn_page(&thread, next.clone()).await?;
            let mut turns = page
                .get("data")
                .and_then(Json::as_array)
                .map(<[Json]>::to_vec)
                .ok_or_else(|| IvyError::new("native_read_failed", "Native turn page missing."))?;
            for turn in &mut turns {
                property(turn, "items")?;
                if let Some(items) = turn.get("items").and_then(Json::as_array) {
                    let mut projected = Vec::new();
                    for item in items {
                        let kind = string(property(item, "type")?)?;
                        projected.push(
                            if ["userMessage", "steeringUserMessage", "agentMessage"]
                                .iter()
                                .any(|v| kind == JsString::from(*v))
                            {
                                item.clone()
                            } else {
                                Json::object(["id", "type"].into_iter().filter_map(|field| {
                                    item.get(field).map(|value| (field, value.clone()))
                                }))
                            },
                        );
                    }
                    turn.insert("items", Json::Array(projected));
                }
            }
            let bytes = stringify(&Json::Array(turns.clone()), usize::MAX)?.len();
            let over_limit = {
                let mut r = recovery.lock().unwrap();
                r.bytes += bytes;
                r.bytes > 32 * 1024 * 1024
            };
            if over_limit {
                self.0.state.lock().await.recovery = None;
                return Err(IvyError::new(
                    "native_history_gap",
                    "History recovery exceeds the bounded buffer; live interactions remain available.",
                ));
            }
            {
                let mut r = recovery.lock().unwrap();
                if r.head.is_none() {
                    r.head = turns.first().and_then(|t| t.get("id")).cloned();
                }
                for turn in turns {
                    r.busy |= turn.get("status") == Some(&Json::from("inProgress"));
                    let last = turn.get("id") == r.cursor.get("lastTurn");
                    r.turns.push(turn);
                    if last {
                        done = true;
                        break;
                    }
                }
                let value = page.get("nextCursor");
                require(
                    value == Some(&Json::Null)
                        || value.and_then(Json::as_js_string).is_some() && value != Some(&r.next),
                    "native_read_failed",
                    "Native cursor did not progress.",
                )?;
                r.next = value.unwrap().clone();
                done |= truthy(f(&r.cursor, "baseline")) || r.next.is_null();
            }
            if done || self.0.clock.utc_millis() - began >= 12000 {
                break;
            }
        }
        if !done {
            self.0.needs_recovery.store(true, Ordering::Relaxed);
            return Ok(recovery.lock().unwrap().busy);
        }
        let (cursor, busy, head, length) = {
            let r = recovery.lock().unwrap();
            (r.cursor.clone(), r.busy, r.head.clone(), r.turns.len())
        };
        // Local marker reads may use the blocking SQLite owner. Keep this
        // delivery segment together; no native/Hive request holds this guard.
        let mut state = self.0.state.lock().await;
        for index in (0..length).rev() {
            let turn = recovery.lock().unwrap().turns[index].clone();
            let turn_id = string(turn.get("id"))?;
            let turn_busy = turn.get("status") == Some(&Json::from("inProgress"));
            require(
                turn.get("itemsView") == Some(&Json::from("full"))
                    && turn.get("items").and_then(Json::as_array).is_some(),
                "native_read_failed",
                "A complete native turn is required.",
            )?;
            let items = turn.get("items").unwrap().as_array().unwrap();
            let mut all = Vec::new();
            let mut finals = 0;
            for (at, item) in items.iter().enumerate() {
                self.source(&mut state, &bid, &turn_id, item).await?;
                if item.get("type") != Some(&Json::from("agentMessage")) {
                    continue;
                }
                if truthy(&self.0.journal.get(&sql_key(&key(&[
                    &"notice-turn:".into(),
                    &bid,
                    &":".into(),
                    &turn_id,
                ])))?) {
                    finals += 1;
                    continue;
                }
                let is_commentary = item.get("phase") == Some(&Json::from("commentary"));
                if !is_commentary {
                    finals += 1;
                }
                if turn_busy && at == items.len() - 1 {
                    continue;
                }
                require(
                    item.get("id").and_then(Json::as_js_string).is_some()
                        && item.get("text").and_then(Json::as_js_string).is_some(),
                    "native_read_failed",
                    "Native message identity missing.",
                )?;
                let id = text(item, "id")?;
                let body = text(item, "text")?;
                for sender in self.senders()? {
                    let message_key = key(&[
                        &"message:".into(),
                        &bid,
                        &":".into(),
                        &turn_id,
                        &":".into(),
                        &id,
                        &":".into(),
                        &string(sender.get("channelId"))?,
                    ]);
                    let existing = self.0.journal.get(&sql_key(&message_key))?;
                    if truthy(&existing) {
                        require(
                            existing_hash(&existing)? == hash(&Json::String(body.clone()))?,
                            "whatsapp_native_message_changed",
                            "A delivered native message changed.",
                        )?;
                        all.extend(ids(f(&existing, "ids"))?);
                        continue;
                    }
                    let suppressed = truthy(f(&cursor, "baseline")) || is_commentary && !commentary;
                    self.0.journal.access(|j| {
                        j.try_transaction(|j| -> IvyResult<()> {
                            let ids = if suppressed {
                                vec![]
                            } else {
                                j.enqueue(
                                    &message_key,
                                    &jid(sender)?,
                                    &self.message_text(&state, &body, &turn_id)?,
                                    false,
                                    self.0.clock.utc_millis(),
                                )?
                            };
                            all.extend(ids.clone());
                            j.set(
                                &sql_key(&message_key),
                                &Json::object([
                                    ("hash", hash(&Json::String(body.clone()))?),
                                    (
                                        "ids",
                                        Json::Array(ids.into_iter().map(Json::from).collect()),
                                    ),
                                    ("suppressed", suppressed.into()),
                                    ("at", Json::Number(self.0.clock.utc_millis() as f64)),
                                ]),
                            )
                        })
                    })?;
                }
            }
            if !turn_busy && finals == 0 && !truthy(f(&cursor, "baseline")) {
                for sender in self.senders()? {
                    let terminal_key = key(&[
                        &"terminal:".into(),
                        &bid,
                        &":".into(),
                        &turn_id,
                        &":".into(),
                        &string(sender.get("channelId"))?,
                    ]);
                    let saved = self.0.journal.access(|j| j.notice_text(&terminal_key))?;
                    let body = match saved {
                        Some(s) => s.into(),
                        None => terminal_text(&turn, &self.0.translator)?,
                    };
                    all.extend(self.0.journal.access(|j| {
                        j.enqueue(
                            &terminal_key,
                            &jid(sender)?,
                            &body,
                            false,
                            self.0.clock.utc_millis(),
                        )
                    })?);
                }
            }
            let delivered = if turn_busy {
                false.into()
            } else if self.0.journal.access(|j| j.sent(&all))? {
                true.into()
            } else {
                Json::Array(all.into_iter().map(Json::from).collect())
            };
            self.0.journal.set(
                &sql_key(&key(&[
                    &"turn-delivered:".into(),
                    &bid,
                    &":".into(),
                    &turn_id,
                ])),
                &delivered,
            )?;
        }
        let mut watermark = Json::object([
            ("cursor", Json::Null),
            ("baseline", false.into()),
            ("started", true.into()),
        ]);
        if let Some(head) = head {
            watermark.insert("lastTurn", head.clone());
            watermark.insert("head", head);
        }
        self.0.journal.set(&sql_key(&cursor_key), &watermark)?;
        state.recovery = None;
        Ok(busy)
    }
}
struct Recovery {
    bid: JsString,
    cursor: Json,
    next: Json,
    head: Option<Json>,
    turns: Vec<Json>,
    bytes: usize,
    busy: bool,
}
#[derive(Default)]
struct State {
    recovery: Option<Arc<Mutex<Recovery>>>,
    sources: VecDeque<(JsString, Option<JsString>)>,
    names: VecDeque<(JsString, Json)>,
    notice_cursor: Option<JsString>,
}
struct Inner {
    main: Arc<dyn ListenerMain>,
    native: Arc<dyn ListenerNative>,
    journal: Journal,
    config: Json,
    translator: Translator,
    clock: Arc<dyn Clock>,
    state: tokio::sync::Mutex<State>,
    needs_recovery: AtomicBool,
}
#[derive(Clone)]
pub struct WhatsAppListener(Arc<Inner>);
fn map_set<T>(map: &mut VecDeque<(JsString, T)>, key: JsString, value: T) {
    if let Some((_, prior)) = map.iter_mut().find(|(id, _)| id == &key) {
        *prior = value;
    } else {
        map.push_back((key, value));
    }
}
impl WhatsAppListener {
    pub fn new(
        main: Arc<ChatGenerationLease>,
        journal: Journal,
        native: WhatsAppNative,
        config: Json,
        translator: Translator,
    ) -> Self {
        Self::with_dependencies(
            main,
            journal,
            Arc::new(native),
            config,
            translator,
            Arc::new(SystemClock),
        )
    }
    pub fn with_dependencies(
        main: Arc<dyn ListenerMain>,
        journal: Journal,
        native: Arc<dyn ListenerNative>,
        config: Json,
        translator: Translator,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self(Arc::new(Inner {
            main,
            journal,
            native,
            config,
            translator,
            clock,
            state: Default::default(),
            needs_recovery: AtomicBool::new(false),
        }))
    }
    pub fn needs_recovery(&self) -> bool {
        self.0.needs_recovery.load(Ordering::Relaxed)
    }
    fn senders(&self) -> IvyResult<&[Json]> {
        self.0
            .config
            .required("senders")?
            .as_array()
            .ok_or_else(IvyError::internal)
    }
    async fn source(
        &self,
        state: &mut State,
        bid: &JsString,
        turn: &JsString,
        item: &Json,
    ) -> IvyResult<()> {
        let kind = string(property(item, "type")?)?;
        if !["userMessage", "steeringUserMessage"]
            .iter()
            .any(|v| kind == JsString::from(*v))
        {
            return Ok(());
        }
        let id = delegation_source(item)?;
        map_set(&mut state.sources, turn.clone(), id.clone());
        if let Some(client) = item.get("clientId").and_then(Json::as_js_string) {
            if truthy(&self.0.main.is_notice_input(client).await?) {
                self.0.journal.set(
                    &sql_key(&key(&[&"notice-turn:".into(), bid, &":".into(), turn])),
                    &true.into(),
                )?;
            }
        }
        if state.sources.len() > 256 {
            state.sources.pop_front();
        }
        if let Some(id) =
            id.filter(|id| !id.0.is_empty() && !state.names.iter().any(|(key, _)| key == id))
        {
            let fallback = self.0.translator.translate(
                "listener.task",
                Some(&Json::object([(
                    "id",
                    Json::String(JsString(id.0.iter().copied().take(8).collect())),
                )])),
            )?;
            map_set(&mut state.names, id.clone(), Json::String(fallback));
            if state.names.len() > 256 {
                state.names.pop_front();
            }
            // Start the read now, as the original async call does, but publish
            // cosmetic names after this synchronous delivery segment releases
            // state. Naming cannot alter the text halfway through one turn.
            let native = self.0.native.clone();
            let selected = id.clone();
            let mut work = Box::pin(async move { native.source_name(selected).await });
            let first = poll_fn(|cx| Poll::Ready(work.as_mut().poll(cx))).await;
            let owner = self.clone();
            tokio::spawn(async move {
                let result = match first {
                    Poll::Ready(v) => v,
                    Poll::Pending => work.await,
                };
                if let Ok(value) = result {
                    if f(&value, "reply").get("result").is_some() {
                        if let Some(name) = f(f(f(&value, "reply"), "result"), "thread")
                            .get("name")
                            .filter(|v| truthy(v))
                        {
                            map_set(&mut owner.0.state.lock().await.names, id, name.clone());
                        }
                    }
                }
            });
        }
        Ok(())
    }
    fn message_text(
        &self,
        state: &State,
        value: &JsString,
        turn: &JsString,
    ) -> IvyResult<JsString> {
        let value = whatsapp_text(value, &self.0.translator)?;
        if let Some(source) = state
            .sources
            .iter()
            .find(|(id, _)| id == turn)
            .and_then(|(_, source)| source.as_ref())
            .filter(|s| !s.0.is_empty() && !value.0.is_empty())
        {
            let name = state
                .names
                .iter()
                .find(|(id, _)| id == source)
                .map(|(_, v)| v)
                .filter(|v| !v.is_null())
                .cloned();
            let name = match name {
                Some(v) => try_js_string(&v)?,
                None => self.0.translator.translate(
                    "listener.task",
                    Some(&Json::object([(
                        "id",
                        Json::String(JsString(source.0.iter().copied().take(8).collect())),
                    )])),
                )?,
            };
            Ok(concat(&[&"[".into(), &name, &"]\n".into(), &value]))
        } else {
            Ok(value)
        }
    }
}
