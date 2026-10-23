//! Original inbound media policy, immutable publication and transport caches.
//! Account download, killable audio decoding and HTTP remain required owners.
use super::{journal::Inbox, journal_owner::Journal, messages::Translator, runtime::RuntimeMedia};
use super::{media_content as content, media_storage::MediaStorage};
use crate::{chat_main::ChatMain, generation::ChatGenerationLease};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, chat_schema::validate_chat, hash::digest,
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
            files: MediaStorage::new(data_root),
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
    }
    pub fn message_text(&self, row: &Inbox) -> IvyResult<JsString> {
        content::text(&content::content(&row.message)?, false)
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
        if !saved.is_null() {
            validate_chat("Payload", &saved).map_err(|_| IvyError::internal())?;
            return Ok(saved);
        }
        let content = content::content(&row.message)?;
        let selected_text = content::text(&content, false);
        self.require(
            selected_text.is_ok(),
            "whatsapp_media_unsupported",
            "media.supported",
            None,
        )?;
        let mut text = selected_text?;
        let mut images = Vec::new();
        let image = content.get("imageMessage").filter(|v| !v.is_null());
        let audio_message = content.get("audioMessage").filter(|v| !v.is_null());
        let media = image
            .or(audio_message)
            .or_else(|| content.get("documentMessage").filter(|v| !v.is_null()));
        if let Some(media) = media.filter(|v| !(v).is_null()) {
            let maximum: usize = if image.is_some_and(|value| !value.is_null()) {
                if same_host { 4 * 1024 * 1024 } else { 2097152 }
            } else {
                32 * 1024 * 1024
            };
            let audio = audio_message.is_some_and(|value| !value.is_null());
            let declared = content::number(media.get("fileLength"));
            let checksum = media
                .get("fileSha256")
                .filter(|v| !(v).is_null())
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
                declared.as_ref().is_ok_and(|declared| {
                    (audio
                        || (declared.is_finite()
                            && declared.fract() == 0.0
                            && *declared > 0.0
                            && *declared <= 9007199254740991.0))
                        && (!declared.is_finite() || *declared <= maximum as f64)
                }),
                "whatsapp_media_limit",
                "media.declared_limit",
                Some(&Json::object([(
                    "mib",
                    Json::Number(maximum as f64 / 1024.0 / 1024.0),
                )])),
            )?;
            let declared = declared?;
            self.files.create(&row.id).await?;
            let bytes = match self.files.original(&row.id).await? {
                Some(bytes) => bytes,
                None => {
                    let kind = if image.is_some_and(|value| !value.is_null()) {
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
            if image.is_some_and(|value| !value.is_null()) {
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
                    if text.0.is_empty() {
                        text = self.translator.translate("media.image_caption", None)?;
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
                    create.insert("parentId", main.root());
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
                let settings = self.config.get("transcription").filter(|v| !(v).is_null());
                self.require(
                    settings.is_some(),
                    "whatsapp_transcription_unconfigured",
                    "media.transcription_missing",
                    None,
                )?;
                let cached = self.journal.get(&format!("transcript:{}", row.id))?;
                let transcript = if !cached.is_null() {
                    cached.as_js_string().ok_or_else(IvyError::internal)?;
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
                        settings
                            .get(key)
                            .and_then(|value| value.as_str().ok())
                            .ok_or_else(IvyError::internal)
                    };
                    let model = setting("model")?;
                    let language = settings
                        .get("language")
                        .filter(|v| !(v).is_null())
                        .map(Json::as_str)
                        .transpose()?
                        .filter(|value| !value.is_empty())
                        .map(Into::into);
                    let base = setting("baseUrl")?;
                    let url = url::Url::parse(&format!(
                        "{}/audio/transcriptions",
                        base.strip_suffix('/').unwrap_or(&base)
                    ))
                    .map_err(|_| IvyError::internal())?;
                    self.require(
                        url.scheme() == "https",
                        "invalid_arguments",
                        "media.transcription_https",
                        None,
                    )?;
                    let authorization = format!("Bearer {}", setting("apiKey")?).into();
                    let mut response = self
                        .dependencies
                        .transcribe(TranscriptionRequest {
                            url: url.into(),
                            authorization,
                            wav,
                            model: model.into(),
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
                        .and_then(|value| value.as_str().ok())
                        .map(|value| {
                            value
                                .trim_matches(|value: char| {
                                    value.is_whitespace() || value == '\u{feff}'
                                })
                                .to_owned()
                        })
                        .unwrap_or_default();
                    self.require(
                        !transcript.is_empty() && transcript.chars().count() <= 65000,
                        "whatsapp_transcription_failed",
                        "media.transcription_invalid",
                        None,
                    )?;
                    let transcript = Json::from(transcript);
                    self.journal
                        .set(&format!("transcript:{}", row.id), &transcript)?;
                    transcript
                };
                text = self.translator.translate(
                    "media.transcription_prefix",
                    Some(&Json::object([("transcript", transcript)])),
                )?;
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
                if text.0.is_empty() {
                    text = self.translator.translate(
                        "media.attachment",
                        Some(&Json::object([("name", Json::String(name))])),
                    )?;
                }
            }
        } else {
            self.require(
                !content
                    .get("videoMessage")
                    .is_some_and(|value| !value.is_null())
                    && !content
                        .get("stickerMessage")
                        .is_some_and(|value| !value.is_null())
                    && !text.0.is_empty(),
                "whatsapp_media_unsupported",
                "media.supported",
                None,
            )?;
        }
        let length = char::decode_utf16(text.0.iter().copied()).count();
        self.require(
            length <= 65536,
            "whatsapp_media_limit",
            "media.message_limit",
            None,
        )?;
        let payload = Json::object([
            ("text", Json::String(text)),
            ("images", Json::Array(images)),
        ]);
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
