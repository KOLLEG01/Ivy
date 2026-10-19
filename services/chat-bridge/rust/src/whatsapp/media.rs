//! Original inbound media policy, immutable publication and transport caches.
//! Account download, killable audio decoding and HTTP remain required owners.
use super::{journal::Inbox, journal_owner::Journal, messages::Translator, runtime::RuntimeMedia};
use super::{media_content as content, media_storage::MediaStorage};
use crate::{chat_main::ChatMain, generation::ChatGenerationLease, main_context::truthy};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::digest, whatwg_url::parse_url,
};
use ivy_sdk::{
    clock::{Clock, SystemClock},
    operation_ids::scoped_operation_id,
    rpc::{RequestFuture, RequestOptions},
    service::ServiceFuture,
};
use rusqlite::OptionalExtension;
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

pub type MediaFuture<'a, T> = Pin<Box<dyn Future<Output = IvyResult<T>> + Send + 'a>>;
pub trait MediaStream: Send {
    fn next(&mut self) -> MediaFuture<'_, Option<Vec<u8>>>;
    fn destroy(&mut self);
}
struct Download(Box<dyn MediaStream>);
impl Drop for Download {
    fn drop(&mut self) {
        self.0.destroy();
    }
}

pub struct TranscriptionRequest {
    pub url: String,
    pub authorization: JsString,
    pub wav: Vec<u8>,
    pub model: JsString,
    pub language: Option<JsString>,
    pub timeout_ms: u64,
    pub redirect_error: bool,
}
pub trait TranscriptionResponse: Send {
    fn ok(&self) -> bool;
    fn json(&mut self) -> RequestFuture<'_>;
}
/// Adapters must preserve the selected original, deadlines and HTTP form fields:
/// file=audio.wav (audio/wav), model, response_format=json, optional language.
pub trait MediaDependencies: Send + Sync {
    /// Host-owned Node filesystem conversion. Keep logical UTF-16 paths separate
    /// from native I/O paths, including Windows verbatim spelling and replacement.
    fn filesystem_path(&self, logical: &JsString) -> IvyResult<PathBuf>;
    fn download<'a>(
        &'a self,
        media: &'a Json,
        kind: &'static str,
        timeout_ms: u64,
    ) -> MediaFuture<'a, Box<dyn MediaStream>>;
    fn voice_wav<'a>(&'a self, bytes: &'a [u8], translator: Translator)
    -> MediaFuture<'a, Vec<u8>>;
    fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> MediaFuture<'_, Box<dyn TranscriptionResponse>>;
}
/// The same Main lookup, operation allocator and SDK write used by the service.
pub trait MediaMain: Send + Sync {
    fn main(&self) -> RequestFuture<'_>;
    fn operation_id(&self, scope: Json) -> RequestFuture<'_>;
    fn root(&self) -> Json;
    fn write(&self, request: Json) -> RequestFuture<'_>;
}
struct GenerationMain<'a>(&'a ChatGenerationLease);
impl MediaMain for GenerationMain<'_> {
    fn main(&self) -> RequestFuture<'_> {
        Box::pin(async {
            let native = self.0.native()?;
            let operations = self.0.operations()?;
            let main = ChatMain::new(&operations, &native, &self.0.config().notice_instructions)?;
            Ok(main.queue().main().await?.value)
        })
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            Ok(Json::String(
                scoped_operation_id(self.0.store().client.clone(), &scope).await?,
            ))
        })
    }
    fn root(&self) -> Json {
        self.0
            .store()
            .root_object_id
            .as_deref()
            .map_or(Json::Null, Json::from)
    }
    fn write(&self, request: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            self.0
                .store()
                .client
                .request("objects.write", request, RequestOptions::default())
                .await
        })
    }
}

#[derive(Clone)]
pub struct WhatsAppMedia {
    journal: Journal,
    config: Json,
    files: MediaStorage,
    dependencies: Arc<dyn MediaDependencies>,
    translator: Translator,
    clock: Arc<dyn Clock>,
}
impl WhatsAppMedia {
    pub fn new(
        journal: Journal,
        config: Json,
        data_root: PathBuf,
        dependencies: Arc<dyn MediaDependencies>,
        translator: Translator,
    ) -> Self {
        Self::with_clock(
            journal,
            config,
            data_root,
            dependencies,
            translator,
            Arc::new(SystemClock),
        )
    }
    pub fn with_clock(
        journal: Journal,
        config: Json,
        data_root: PathBuf,
        dependencies: Arc<dyn MediaDependencies>,
        translator: Translator,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            journal,
            config,
            files: MediaStorage::new(data_root, dependencies.clone()),
            dependencies,
            translator,
            clock,
        }
    }
    fn require(
        &self,
        condition: bool,
        code: &str,
        key: &str,
        params: Option<&Json>,
    ) -> IvyResult<()> {
        if condition {
            Ok(())
        } else {
            Err(IvyError::new_js(
                code.into(),
                self.translator.translate(key, params)?,
            ))
        }
    }
    pub fn command_text(&self, row: &Inbox) -> IvyResult<JsString> {
        content::text(&content::content(&row.message)?, true)
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)
    }
    pub fn message_text(&self, row: &Inbox) -> IvyResult<JsString> {
        content::text(&content::content(&row.message)?, false)
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)
    }
    pub fn content(&self, row: &Inbox) -> IvyResult<Json> {
        content::content(&row.message)
    }
    pub async fn payload(
        &self,
        row: &Inbox,
        main: &dyn MediaMain,
        same_host: bool,
    ) -> IvyResult<Json> {
        let saved = self.journal.get(&format!("payload:{}", row.id))?;
        if truthy(&saved) {
            return Ok(saved);
        }
        let content = content::content(&row.message)?;
        let mut text = content::text(&content, false);
        let mut images = Vec::new();
        let image = content.get("imageMessage").filter(|v| !v.is_null());
        let audio_message = content.get("audioMessage").filter(|v| !v.is_null());
        let media = image
            .or(audio_message)
            .or_else(|| content.get("documentMessage").filter(|v| !v.is_null()));
        if let Some(media) = media.filter(|v| truthy(v)) {
            let maximum: usize = if image.is_some_and(truthy) {
                if same_host { 4 * 1024 * 1024 } else { 2097152 }
            } else {
                32 * 1024 * 1024
            };
            let audio = audio_message.is_some_and(truthy);
            let declared = content::number(media.get("fileLength"))?;
            let checksum = media
                .get("fileSha256")
                .filter(|v| truthy(v))
                .map(|v| {
                    content::bytes(v).map(|b| {
                        format!(
                            "sha256:{}",
                            b.iter().map(|v| format!("{v:02x}")).collect::<String>()
                        )
                    })
                })
                .transpose()?;
            self.require(
                (audio
                    || (declared.is_finite()
                        && declared.fract() == 0.0
                        && declared > 0.0
                        && declared <= 9007199254740991.0))
                    && (!declared.is_finite() || declared <= maximum as f64),
                "whatsapp_media_limit",
                "media.declared_limit",
                Some(&Json::object([(
                    "mib",
                    Json::Number(maximum as f64 / 1024.0 / 1024.0),
                )])),
            )?;
            self.files.create(&row.id).await?;
            let bytes = match self.files.original(&row.id).await? {
                Some(bytes) => bytes,
                None => {
                    let kind = if image.is_some_and(truthy) {
                        "image"
                    } else if audio {
                        "audio"
                    } else {
                        "document"
                    };
                    let mut stream =
                        Download(self.dependencies.download(media, kind, 60000).await?);
                    let mut bytes = Vec::new();
                    while let Some(part) = stream.0.next().await? {
                        self.require(
                            part.len() <= maximum.saturating_sub(bytes.len()),
                            "whatsapp_media_limit",
                            "media.too_large",
                            None,
                        )?;
                        bytes.extend(part);
                    }
                    drop(stream);
                    self.require(
                        !bytes.is_empty(),
                        "whatsapp_media_invalid",
                        "media.empty",
                        None,
                    )?;
                    if !audio {
                        self.require(
                            bytes.len() as f64 == declared
                                && checksum.as_ref().is_none_or(|v| *v == digest(&bytes)),
                            "whatsapp_media_invalid",
                            "media.checksum",
                            None,
                        )?;
                    }
                    self.files.save_original(&row.id, bytes.clone()).await?;
                    bytes
                }
            };
            self.require(
                !bytes.is_empty() && bytes.len() <= maximum,
                "whatsapp_media_limit",
                "media.limit",
                None,
            )?;
            if !audio {
                self.require(
                    bytes.len() as f64 == declared
                        && checksum.as_ref().is_none_or(|v| *v == digest(&bytes)),
                    "whatsapp_media_invalid",
                    "media.checksum",
                    None,
                )?;
            }
            if image.is_some_and(truthy) {
                let media_type = image
                    .and_then(|i| i.get("mimetype"))
                    .cloned()
                    .unwrap_or(Json::Null);
                self.require(
                    ["image/png", "image/jpeg", "image/webp"]
                        .iter()
                        .any(|s| media_type == Json::from(*s)),
                    "whatsapp_media_unsupported",
                    "media.image_types",
                    None,
                )?;
                let kind = media_type.as_str()?;
                if same_host {
                    self.journal.set(
                        &format!("local-input:{}", row.id),
                        &Json::Array(vec![Json::object([
                            ("type", "image".into()),
                            (
                                "url",
                                format!("data:{kind};base64,{}", STANDARD.encode(&bytes)).into(),
                            ),
                        ])]),
                    )?;
                    if !truthy(&text) {
                        text =
                            Json::String(self.translator.translate("media.image_caption", None)?);
                    }
                } else {
                    let current = main.main().await?;
                    let operation = main
                        .operation_id(Json::Array(vec![
                            "whatsapp-image".into(),
                            row.id.as_str().into(),
                        ]))
                        .await?;
                    let mut create = Json::object([(
                        "contractKey",
                        format!(
                            "chat-bridge/image-{}",
                            if kind == "image/jpeg" {
                                "jpeg"
                            } else if kind == "image/png" {
                                "png"
                            } else {
                                "webp"
                            }
                        )
                        .into(),
                    )]);
                    let root = main.root();
                    if !root.is_null() {
                        create.insert("parentId", root);
                    }
                    create.insert(
                        "ownerObjectId",
                        current
                            .required("conversation")?
                            .required("objectId")?
                            .clone(),
                    );
                    create.insert("name", format!("WhatsApp image {}", row.id).into());
                    let result = main
                        .write(Json::object([
                            ("mutationId", operation),
                            ("contractVersion", "1.0.0".into()),
                            ("references", Json::object([] as [(&str, Json); 0])),
                            ("create", create),
                            (
                                "content",
                                Json::object([
                                    ("encoding", "base64".into()),
                                    ("value", STANDARD.encode(&bytes).into()),
                                ]),
                            ),
                        ]))
                        .await?;
                    images.push(Json::object([
                        (
                            "object",
                            Json::object([
                                (
                                    "objectId",
                                    result.required("object")?.required("id")?.clone(),
                                ),
                                (
                                    "revision",
                                    result.required("revision")?.required("revision")?.clone(),
                                ),
                            ]),
                        ),
                        ("contentHash", digest(&bytes).into()),
                        ("byteLength", Json::Number(bytes.len() as f64)),
                        ("mediaType", media_type),
                        ("label", "WhatsApp image".into()),
                    ]));
                }
            } else if audio {
                let settings = self.config.get("transcription").filter(|v| truthy(v));
                self.require(
                    settings.is_some(),
                    "whatsapp_transcription_unconfigured",
                    "media.transcription_missing",
                    None,
                )?;
                let cached = self.journal.get(&format!("transcript:{}", row.id))?;
                let transcript = if !cached.is_null() {
                    cached
                } else {
                    let settings = settings.unwrap();
                    let seconds = content::number(audio_message.and_then(|v| v.get("seconds")))?;
                    self.require(
                        !seconds.is_finite() || seconds <= 600.0,
                        "whatsapp_media_limit",
                        "media.voice_declared_limit",
                        None,
                    )?;
                    let wav = self.dependencies.voice_wav(&bytes, self.translator).await?;
                    self.require(
                        wav.len() <= 600 * 32000 + 4096,
                        "whatsapp_media_limit",
                        "media.voice_actual_limit",
                        None,
                    )?;
                    let setting = |key| {
                        settings.get(key).map_or_else(
                            || Ok("undefined".into()),
                            ivy_contracts::json::try_js_string,
                        )
                    };
                    let model = setting("model")?;
                    let language = settings
                        .get("language")
                        .filter(|v| truthy(v))
                        .map(ivy_contracts::json::try_js_string)
                        .transpose()?;
                    let mut base = settings
                        .get("baseUrl")
                        .and_then(Json::as_js_string)
                        .cloned()
                        .ok_or_else(IvyError::internal)?;
                    if base.0.last() == Some(&47) {
                        base.0.pop();
                    }
                    base.0.extend("/audio/transcriptions".encode_utf16());
                    let url = parse_url(&base)?;
                    self.require(
                        url.protocol == "https:",
                        "invalid_arguments",
                        "media.transcription_https",
                        None,
                    )?;
                    let mut authorization = JsString::from("Bearer ");
                    authorization.0.extend(setting("apiKey")?.0);
                    let mut response = self
                        .dependencies
                        .transcribe(TranscriptionRequest {
                            url: url.href,
                            authorization,
                            wav,
                            model,
                            language,
                            timeout_ms: 180000,
                            redirect_error: true,
                        })
                        .await?;
                    self.require(
                        response.ok(),
                        "whatsapp_transcription_failed",
                        "media.transcription_failed",
                        None,
                    )?;
                    let body = response.json().await?;
                    if body.is_null() {
                        return Err(IvyError::internal());
                    }
                    let transcript = body
                        .get("text")
                        .and_then(Json::as_js_string)
                        .map(JsString::trim)
                        .unwrap_or_default();
                    self.require(
                        !transcript.0.is_empty() && transcript.code_point_len() <= 65000,
                        "whatsapp_transcription_failed",
                        "media.transcription_invalid",
                        None,
                    )?;
                    let transcript = Json::String(transcript);
                    self.journal
                        .set(&format!("transcript:{}", row.id), &transcript)?;
                    transcript
                };
                text = Json::String(self.translator.translate(
                    "media.transcription_prefix",
                    Some(&Json::object([("transcript", transcript)])),
                )?);
            } else {
                self.require(
                    same_host,
                    "whatsapp_attachment_owner_mismatch",
                    "media.document_host",
                    None,
                )?;
                let name = content::filename(
                    content
                        .get("documentMessage")
                        .and_then(|v| v.get("fileName")),
                )?;
                let (path, same) = self.files.document(&row.id, &name, bytes).await?;
                self.require(
                    same,
                    "whatsapp_media_invalid",
                    "media.original_changed",
                    None,
                )?;
                self.journal.set(
                    &format!("local-input:{}", row.id),
                    &Json::Array(vec![Json::object([
                        ("type", "mention".into()),
                        ("name", Json::String(name.clone())),
                        ("path", path),
                    ])]),
                )?;
                if !truthy(&text) {
                    text = Json::String(self.translator.translate(
                        "media.attachment",
                        Some(&Json::object([("name", Json::String(name))])),
                    )?);
                }
            }
        } else {
            self.require(
                !content.get("videoMessage").is_some_and(truthy)
                    && !content.get("stickerMessage").is_some_and(truthy)
                    && truthy(&text),
                "whatsapp_media_unsupported",
                "media.supported",
                None,
            )?;
        }
        let length = match &text {
            Json::String(s) => s.code_point_len(),
            Json::Array(a) => a.len(),
            _ => return Err(IvyError::internal()),
        };
        self.require(
            length <= 65536,
            "whatsapp_media_limit",
            "media.message_limit",
            None,
        )?;
        let payload = Json::object([("text", text), ("images", Json::Array(images))]);
        self.journal.set(&format!("payload:{}", row.id), &payload)?;
        Ok(payload)
    }
    pub async fn prune(&self) -> IvyResult<()> {
        for id in self.files.entries().await? {
            if self.clock.utc_millis() as f64 - self.files.modified_ms(&id).await?
                < 7.0 * 86400000.0
            {
                continue;
            }
            let pending = self.journal.access(|j| {
                j.connection()
                    .query_row(
                        "SELECT 1 FROM inbox WHERE id=? AND state='pending'",
                        [&id],
                        |_| Ok(()),
                    )
                    .optional()
                    .map(|v| v.is_some())
                    .map_err(|_| super::journal_io::StorageUnavailable.into())
            })?;
            if pending {
                continue;
            }
            self.files.remove(&id).await?;
            self.journal
                .set(&format!("local-input:{id}"), &Json::Null)?;
        }
        Ok(())
    }
}
impl RuntimeMedia for WhatsAppMedia {
    fn command_text(&self, row: &Inbox) -> IvyResult<JsString> {
        self.command_text(row)
    }
    fn content(&self, row: &Inbox) -> IvyResult<Json> {
        self.content(row)
    }
    fn payload<'a>(
        &'a self,
        row: &'a Inbox,
        lease: &'a ChatGenerationLease,
        same_host: bool,
    ) -> RequestFuture<'a> {
        Box::pin(async move { self.payload(row, &GenerationMain(lease), same_host).await })
    }
    fn prune(&self) -> ServiceFuture<()> {
        let media = self.clone();
        Box::pin(async move { media.prune().await })
    }
}
