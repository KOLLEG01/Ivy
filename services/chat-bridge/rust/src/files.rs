//! Explicit attachments share the durable WhatsApp output order and replay fences.
use crate::{
    bridge::ChatBridge,
    whatsapp::{
        identity::identity, journal::OutputState, journal_io::JournalIo, journal_owner::Journal,
        sender::phone,
    },
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::{digest, hash_json},
    json::canonical,
};
use ivy_sdk::clock::{Clock, SystemClock, date_to_iso, parse_date};
use rusqlite::OptionalExtension;
use std::sync::Arc;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const RETENTION_MS: i64 = 7 * 86_400_000;
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn storage() -> IvyError {
    IvyError::new(
        "storage_unavailable",
        "The file delivery lost its original output identity.",
    )
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, usize::MAX)? == canonical(b, usize::MAX)?)
}
fn key(caller: &JsString, operation: &JsString) -> IvyResult<String> {
    Ok(format!(
        "file:{}",
        identity(&[
            Json::String(caller.clone()),
            Json::String(operation.clone())
        ])?
    ))
}
fn message_id(caller: &JsString, operation: &JsString) -> IvyResult<String> {
    Ok(identity(&[
        "file".into(),
        Json::String(caller.clone()),
        Json::String(operation.clone()),
    ])?[..32]
        .to_ascii_uppercase())
}
fn string<'a>(value: &'a Json, field: &str) -> IvyResult<&'a JsString> {
    value.required(field)?.as_js_string().ok_or_else(storage)
}

pub struct FilePayload {
    pub bytes: Vec<u8>,
    pub image: bool,
    pub file: Json,
    pub caption: Option<String>,
}
/// MIME admission inspects content, never a filename or a filesystem path.
pub fn outgoing_file_bytes(file: &Json) -> IvyResult<(Vec<u8>, bool)> {
    let encoded = file.required("bytesBase64")?.as_str()?;
    let bytes = STANDARD
        .decode(&encoded)
        .map_err(|_| IvyError::new("chat_file_integrity", "The file requires canonical base64."))?;
    require(
        !bytes.is_empty()
            && bytes.len() <= MAX_BYTES
            && STANDARD.encode(&bytes) == encoded
            && file.get("byteLength").and_then(Json::as_f64) == Some(bytes.len() as f64)
            && file.get("contentHash") == Some(&Json::from(digest(&bytes))),
        "chat_file_integrity",
        "Canonical base64, byte length and SHA-256 must match the selected file (at most 8 MiB).",
    )?;
    let mime = file.required("mediaType")?.as_str()?;
    // Text formats follow the public UTF-8 policy rather than a detector's guesses.
    let detected = infer::get(&bytes).filter(|kind| {
        kind.matcher_type() != infer::MatcherType::Text || kind.mime_type() == "application/rtf"
    });
    if let Some(kind) = detected {
        require(
            mime == kind.mime_type(),
            "chat_file_type",
            "Declared MIME does not match the file signature.",
        )?;
    } else {
        let readable = std::str::from_utf8(&bytes)
            .ok()
            .map(|text| text.strip_prefix('\u{feff}').unwrap_or(text))
            .filter(|text| {
                !text.chars().any(
                    |c| matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{c}' | '\u{e}'..='\u{1f}'),
                )
            });
        let mut valid = mime == "application/octet-stream";
        if let Some(text) = readable {
            valid |= mime.starts_with("text/");
            if mime == "application/json" {
                valid |= serde_json::from_str::<Box<serde_json::value::RawValue>>(text).is_ok();
            }
            let xml = text
                .trim_start()
                .strip_prefix("<?xml")
                .and_then(|s| s.split_once("?>").map(|(_, tail)| tail.trim_start()))
                .unwrap_or(text.trim_start());
            if mime == "image/svg+xml" {
                valid |= xml.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("<svg"))
                    && xml
                        .chars()
                        .nth(4)
                        .is_some_and(|c| c == '>' || c.is_whitespace());
            }
            if mime == "application/xml" {
                valid |= text.trim_start().starts_with('<');
            }
        }
        require(
            valid,
            "chat_file_type",
            "Unknown binary formats require application/octet-stream; text must be valid UTF-8.",
        )?;
    }
    let image = matches!(mime.as_str(), "image/png" | "image/jpeg");
    require(
        !image || bytes.len() <= 4 * 1024 * 1024,
        "limit_exceeded",
        "Native PNG/JPEG chat images are limited to 4 MiB.",
    )?;
    Ok((bytes, image))
}

#[derive(Clone)]
pub struct ChatFiles {
    pub journal: Journal,
    config: Json,
    clock: Arc<dyn Clock>,
}
impl ChatFiles {
    pub fn new(journal: Journal, config: Json) -> Self {
        Self::with_clock(journal, config, Arc::new(SystemClock))
    }
    pub fn with_clock(journal: Journal, config: Json, clock: Arc<dyn Clock>) -> Self {
        Self {
            journal,
            config,
            clock,
        }
    }
    fn transport_hash(&self) -> IvyResult<String> {
        hash_json(&Json::object([
            ("account", self.config.required("accountId")?.clone()),
            ("bot", self.config.required("botPhoneNumber")?.clone()),
            ("sender", self.sender()?.clone()),
        ]))
    }
    fn sender(&self) -> IvyResult<&Json> {
        self.config
            .required("senders")?
            .as_array()
            .and_then(|v| v.first())
            .ok_or_else(storage)
    }
    pub fn target(&self, workspace: &Json) -> IvyResult<Json> {
        let binding = workspace.required("binding")?;
        let channels = workspace
            .required("channels")?
            .as_array()
            .ok_or_else(storage)?;
        require(
            !binding.is_null()
                && channels.len() == 1
                && channels[0].required("channel")?.get("adapter") == Some(&"whatsapp".into()),
            "chat_main_missing",
            "File delivery requires the current Main and its single WhatsApp channel.",
        )?;
        let channel = channels[0].required("channel")?;
        let sender = self.sender()?;
        require(
            self.config.get("accountId") == channel.get("accountId")
                && sender.get("channelId") == channel.get("channelId")
                && phone(&sender.required("phoneNumber")?.as_str()?).is_some(),
            "chat_file_target_changed",
            "The configured WhatsApp transport does not match Main.",
        )?;
        Ok(Json::object([
            (
                "expectedBridge",
                workspace.required("expectedBridge")?.clone(),
            ),
            ("expectedBinding", binding.required("object")?.clone()),
            ("channel", channel.clone()),
        ]))
    }
    pub fn has(&self, caller: &JsString, operation: &JsString) -> IvyResult<bool> {
        let id = message_id(caller, operation)?;
        self.journal.access(|j| {
            Ok(!j.get(&key(caller, operation)?)?.is_null()
                || j.connection()
                    .query_row("SELECT 1 FROM retired_outgoing WHERE id=?", [&id], |_| {
                        Ok(())
                    })
                    .optional()
                    .map_err(|_| storage())?
                    .is_some())
        })
    }
    pub fn is_file(&self, id: &str) -> IvyResult<bool> {
        Ok(!self.journal.get(&format!("out-file:{id}"))?.is_null())
    }
    fn saved(&self, id: &str) -> IvyResult<Json> {
        let key = self.journal.get(&format!("out-file:{id}"))?;
        if key.is_null() {
            return Ok(Json::Null);
        }
        self.journal.get(&key.as_str()?)
    }
    fn view(j: &mut JournalIo, saved: &Json) -> IvyResult<Json> {
        let mut delivery = saved.required("delivery")?.clone();
        let id = delivery.required("messageId")?.as_str()?;
        let state: String = j
            .connection()
            .query_row("SELECT state FROM outgoing WHERE id=?", [&id], |r| r.get(0))
            .map_err(|_| storage())?;
        delivery.insert(
            "state",
            match state.as_str() {
                "pending" => "queued",
                "sending" => "sending",
                "sent" => "sent",
                "unknown" => "outcome_unknown",
                "failed" => "failed",
                _ => return Err(storage()),
            }
            .into(),
        );
        if let Some(fields) = j.get(&format!("file-state:{id}"))?.as_object() {
            for (key, value) in fields {
                delivery.insert(key.clone(), value.clone());
            }
        }
        Ok(delivery)
    }
    pub async fn read(
        &self,
        bridge: &ChatBridge<'_>,
        caller: &JsString,
        query: &Json,
    ) -> IvyResult<Json> {
        if query.get("fileTarget") == Some(&true.into()) {
            return self.target(&bridge.workspace(caller).await?);
        }
        self.read_operation(caller, string(query, "fileOperationId")?)
            .await
    }
    pub async fn read_operation(&self, caller: &JsString, operation: &JsString) -> IvyResult<Json> {
        self.prune()?;
        let result = self.journal.access(|j| {
            let saved = j.get(&key(caller, operation)?)?;
            require(
                saved.get("caller") == Some(&Json::String(caller.clone())),
                "not_found",
                "This caller has no retained file delivery with that operationId.",
            )?;
            Self::view(j, &saved)
        })?;
        self.journal.flush().await?;
        Ok(result)
    }
    pub async fn send(
        &self,
        bridge: &ChatBridge<'_>,
        caller: &JsString,
        request: &Json,
    ) -> IvyResult<Json> {
        bridge
            .main
            .admission()
            .authorize(caller, request.required("expectedBridge")?, None)?;
        require(
            request.required("expectedBridge")?.get("definitionHash")
                == Some(&Json::from(bridge.main.admission().definition_hash())),
            "chat_file_target_changed",
            "Read the current Main file target before sending.",
        )?;
        let operation = string(request, "operationId")?;
        let key = key(caller, operation)?;
        let request_hash = hash_json(request)?;
        let prior = self.journal.get(&key)?;
        if !prior.is_null() {
            return self.replay(&prior, &request_hash).await;
        }
        require(
            !self.has(caller, operation)?,
            "operation_expired",
            "This file operation has expired; its transport identity cannot be reused.",
        )?;
        let target = self.target(&bridge.workspace(caller).await?)?;
        require(
            same(
                request.required("expectedBinding")?,
                target.required("expectedBinding")?,
            )?,
            "chat_file_target_changed",
            "Main changed; a file cannot silently follow another conversation.",
        )?;
        let (_, image) = outgoing_file_bytes(request.required("file")?)?;
        require(
            same(&target, &self.target(&bridge.workspace(caller).await?)?)?,
            "chat_file_target_changed",
            "Main changed during file admission.",
        )?;
        self.enqueue(caller, request, &target, &request_hash, image)
            .await
    }
    async fn replay(&self, saved: &Json, request_hash: &str) -> IvyResult<Json> {
        require(
            saved.get("requestHash") == Some(&request_hash.into()),
            "chat_identity_conflict",
            "An existing file operation cannot change its bytes, name, caption or target.",
        )?;
        self.journal.flush().await?;
        self.journal.access(|j| Self::view(j, saved))
    }
    async fn enqueue(
        &self,
        caller: &JsString,
        request: &Json,
        target: &Json,
        request_hash: &str,
        image: bool,
    ) -> IvyResult<Json> {
        let operation = string(request, "operationId")?;
        let key = key(caller, operation)?;
        let id = message_id(caller, operation)?;
        let at = date_to_iso(self.clock.utc_millis() as f64)?;
        let mut file = request.required("file")?.clone();
        let bytes = file.remove("bytesBase64").ok_or_else(storage)?;
        let saved = Json::object([
            ("caller", Json::String(caller.clone())),
            ("expectedBridge", target.required("expectedBridge")?.clone()),
            ("requestHash", request_hash.into()),
            ("transportHash", self.transport_hash()?.into()),
            ("bytesBase64", bytes),
            ("caption", request.required("caption")?.clone()),
            (
                "delivery",
                Json::object([
                    ("operationId", Json::String(operation.clone())),
                    ("binding", target.required("expectedBinding")?.clone()),
                    ("channel", target.required("channel")?.clone()),
                    ("file", file),
                    ("kind", if image { "image" } else { "document" }.into()),
                    ("messageId", id.as_str().into()),
                    ("state", "queued".into()),
                    ("errorCode", Json::Null),
                    ("createdAt", Json::String(at.clone())),
                    ("updatedAt", Json::String(at)),
                ]),
            ),
        ]);
        self.prune()?;
        let result = self.journal.access(|j| {
            let prior = j.get(&key)?;
            if !prior.is_null() {
                require(prior.get("requestHash") == Some(&request_hash.into()), "chat_identity_conflict", "A concurrent file operation changed its content.")?;
                return Self::view(j, &prior);
            }
            let rows = j.kv_rows("SELECT key,value FROM kv WHERE key LIKE 'file:%'")?;
            let mut pending = 0;
            let mut retained = 0.0;
            for (_,row) in rows {
                if matches!(Self::view(j, &row)?.required("state")?.as_str()?.as_str(), "queued" | "sending" | "outcome_unknown") { pending += 1; }
                if !row.required("bytesBase64")?.is_null() { retained += row.required("delivery")?.required("file")?.required("byteLength")?.as_f64().ok_or_else(storage)?; }
            }
            require(pending < 32 && retained + request.required("file")?.required("byteLength")?.as_f64().ok_or_else(storage)? <= (64*1024*1024) as f64, "limit_exceeded", "The file queue is full (32 unresolved deliveries or 64 MiB). Reconcile original deliveries first.")?;
            let number = self.sender()?.required("phoneNumber")?.as_str()?;
            let jid = format!("{}@s.whatsapp.net", phone(&number).ok_or_else(storage)?);
            j.try_transaction(|j| -> IvyResult<()> {
                j.enqueue_attachment(&id, &jid, &request.required("caption")?.as_str().unwrap_or_default(), self.clock.utc_millis())?;
                j.set(&key, &saved)?;
                j.set(&format!("out-file:{id}"), &key.as_str().into())
            })?;
            Self::view(j, &saved)
        })?;
        self.journal.flush().await?;
        Ok(result)
    }
    pub fn admitted(&self, id: &str, target: &Json) -> IvyResult<()> {
        let saved = self.saved(id)?;
        require(
            !saved.is_null(),
            "chat_file_integrity",
            "The queued file receipt is missing.",
        )?;
        require(
            saved.get("transportHash") == Some(&self.transport_hash()?.into())
                && same(
                    saved.required("expectedBridge")?,
                    target.required("expectedBridge")?,
                )?
                && same(
                    saved.required("delivery")?.required("binding")?,
                    target.required("expectedBinding")?,
                )?
                && same(
                    saved.required("delivery")?.required("channel")?,
                    target.required("channel")?,
                )?,
            "chat_file_target_changed",
            "The original file recipient or Main binding changed.",
        )
    }
    pub fn caller(&self, id: &str) -> IvyResult<JsString> {
        Ok(string(&self.saved(id)?, "caller")?.clone())
    }
    pub fn payload(&self, id: &str) -> IvyResult<FilePayload> {
        let saved = self.saved(id)?;
        require(
            saved.get("bytesBase64").is_some_and(|v| !v.is_null()),
            "chat_file_integrity",
            "The queued file bytes are unavailable.",
        )?;
        let mut file = saved.required("delivery")?.required("file")?.clone();
        file.insert("bytesBase64", saved.required("bytesBase64")?.clone());
        let (bytes, image) = outgoing_file_bytes(&file)?;
        require(
            saved.required("delivery")?.get("kind")
                == Some(&Json::from(if image { "image" } else { "document" })),
            "chat_file_integrity",
            "The queued file kind changed.",
        )?;
        Ok(FilePayload {
            bytes,
            image,
            file,
            caption: saved.required("caption")?.as_str().ok(),
        })
    }
    pub fn mark(&self, id: &str, state: OutputState, code: Option<&str>) -> IvyResult<()> {
        self.journal
            .access(|j| mark(j, id, state, code, self.clock.utc_millis()))
    }
    pub fn prune(&self) -> IvyResult<()> {
        self.journal.access(|j| {
            let mut rows = j.kv_rows("SELECT key,value FROM kv WHERE key LIKE 'file:%'")?;
            rows.sort_by(|a, b| {
                b.1.get("delivery")
                    .and_then(|v| v.get("createdAt"))
                    .and_then(Json::as_js_string)
                    .cmp(
                        &a.1.get("delivery")
                            .and_then(|v| v.get("createdAt"))
                            .and_then(Json::as_js_string),
                    )
            });
            let mut completed = 0;
            for (key, row) in rows {
                let view = Self::view(j, &row)?;
                let state = view.required("state")?.as_str()?;
                let id = view.required("messageId")?.as_str()?;
                let terminal = matches!(state.as_str(), "sent" | "failed");
                if terminal {
                    completed += 1;
                }
                let expired = self.clock.utc_millis() as f64
                    - parse_date(string(&view, "createdAt")?)
                    >= RETENTION_MS as f64;
                if expired || terminal && completed > 256 {
                    j.try_transaction(|j| -> IvyResult<()> {
                        j.retire_attachment(&id, &row.required("requestHash")?.as_str()?)?;
                        for key in [
                            key,
                            format!("out-file:{id}"),
                            format!("out-file-message:{id}"),
                            format!("file-submitted:{id}"),
                            format!("file-state:{id}"),
                        ] {
                            j.set(&key, &Json::Null)?;
                        }
                        Ok(())
                    })?;
                }
            }
            Ok(())
        })
    }
}
pub(crate) fn mark(
    j: &mut JournalIo,
    id: &str,
    state: OutputState,
    code: Option<&str>,
    now: i64,
) -> IvyResult<()> {
    let key = j.get(&format!("out-file:{id}"))?;
    if key.is_null() {
        return j.mark(id, state);
    }
    let key = key.as_str()?;
    let mut saved = j.get(&key)?;
    if ChatFiles::view(j, &saved)?.get("state") == Some(&"sent".into())
        && !matches!(state, OutputState::Sent)
    {
        return Ok(());
    }
    j.try_transaction(|j| -> IvyResult<()> {
        j.mark(id, state)?;
        j.set(
            &format!("file-state:{id}"),
            &Json::object([
                ("updatedAt", Json::String(date_to_iso(now as f64)?)),
                ("errorCode", code.map(Json::from).unwrap_or(Json::Null)),
            ]),
        )?;
        if matches!(state, OutputState::Sent | OutputState::Failed) {
            saved.insert("bytesBase64", Json::Null);
            j.set(&key, &saved)?;
        }
        Ok(())
    })
}

#[cfg(test)]
#[path = "../../../../tests/rust/chat-files.rs"]
mod tests;
