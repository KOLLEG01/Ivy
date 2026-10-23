//! Native downloads and transcription behind the existing media policy/cache.
use super::{
    audio_worker::AudioWorker,
    media::{
        MediaDependencies, MediaFuture, MediaStream, TranscriptionRequest, TranscriptionResponse,
    },
    media_content,
    messages::Translator,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, json::parse};
use ivy_sdk::rpc::RequestFuture;
use std::{
    io::{self, Cursor, Seek, SeekFrom, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use whatsapp_rust::{
    TokioRuntime,
    download::{DEFAULT_MEDIA_HOSTS, DownloadWriter, Downloadable, MediaDownloader, MediaType},
    http::{HttpClient, UreqHttpClient},
};

fn invalid() -> IvyError {
    IvyError::new(
        "whatsapp_media_invalid",
        "WhatsApp media could not be downloaded.",
    )
}
fn transcription_failed() -> IvyError {
    IvyError::new(
        "whatsapp_transcription_failed",
        "Voice transcription failed.",
    )
}
struct Original {
    url: String,
    key: Vec<u8>,
    kind: MediaType,
}
impl Original {
    fn read(media: &Json, kind: &'static str) -> IvyResult<Self> {
        let kind = match kind {
            "image" => MediaType::Image,
            "audio" => MediaType::Audio,
            "document" => MediaType::Document,
            _ => return Err(invalid()),
        };
        let text = |name| {
            media
                .get(name)
                .filter(|v| !v.is_null())
                .map(Json::as_str)
                .transpose()
        };
        let url = match text("url")? {
            Some(url) if url.starts_with("https://") => url,
            _ => {
                let direct = text("directPath")?.ok_or_else(invalid)?;
                if !direct.starts_with('/') || direct.starts_with("//") {
                    return Err(invalid());
                }
                // Preserve the original signed query and select its original,
                // never thumbnailDirectPath or a newly derived token URL.
                format!("https://{}{direct}", DEFAULT_MEDIA_HOSTS[0])
            }
        };
        if url::Url::parse(&url).is_err() {
            return Err(invalid());
        }
        let key = media_content::bytes(media.required("mediaKey")?).map_err(|_| invalid())?;
        if key.len() != 32 {
            return Err(invalid());
        }
        Ok(Self { url, key, kind })
    }
}
impl Downloadable for Original {
    fn direct_path(&self) -> Option<&str> {
        None
    }
    fn static_url(&self) -> Option<&str> {
        Some(&self.url)
    }
    fn media_key(&self) -> Option<&[u8]> {
        Some(&self.key)
    }
    fn file_enc_sha256(&self) -> Option<&[u8]> {
        None
    }
    fn file_sha256(&self) -> Option<&[u8]> {
        None
    }
    fn file_length(&self) -> Option<u64> {
        None
    }
    fn app_info(&self) -> MediaType {
        self.kind
    }
}

struct BoundedWriter {
    bytes: Cursor<Vec<u8>>,
    maximum: usize,
    deadline: Instant,
    overflow: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if self.bytes.position().saturating_add(bytes.len() as u64) > self.maximum as u64 {
            self.overflow.store(true, Ordering::Release);
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.bytes.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Seek for BoundedWriter {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.bytes.seek(position)
    }
}
impl DownloadWriter for BoundedWriter {
    fn truncate(&mut self, length: u64) -> io::Result<()> {
        self.bytes
            .get_mut()
            .truncate(usize::try_from(length).unwrap_or(usize::MAX));
        Ok(())
    }
}
struct CancelDownload(Arc<AtomicBool>);
impl Drop for CancelDownload {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
struct VerifiedBytes(Option<Vec<u8>>);
impl MediaStream for VerifiedBytes {
    fn next(&mut self) -> MediaFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.0.take()) })
    }
    fn destroy(&mut self) {
        self.0.take();
    }
}

pub struct NativeMediaDependencies {
    downloads: MediaDownloader,
    audio: AudioWorker,
    http: reqwest::Client,
}
impl NativeMediaDependencies {
    pub fn new(audio: AudioWorker) -> IvyResult<Self> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into();
        Self::with_http(
            audio,
            Arc::new(UreqHttpClient::with_agent(agent).with_max_body_bytes(32 * 1024 * 1024 + 32)),
            reqwest::Client::builder(),
        )
    }
    /// Hosts may supply network/CA configuration; redirect refusal is retained.
    pub fn with_http(
        audio: AudioWorker,
        downloads: Arc<dyn HttpClient>,
        http: reqwest::ClientBuilder,
    ) -> IvyResult<Self> {
        Ok(Self {
            downloads: MediaDownloader::with_default_hosts(downloads, Arc::new(TokioRuntime)),
            audio,
            http: http
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| transcription_failed())?,
        })
    }
}
impl MediaDependencies for NativeMediaDependencies {
    fn download<'a>(
        &'a self,
        media: &'a Json,
        kind: &'static str,
        timeout_ms: u64,
    ) -> MediaFuture<'a, Box<dyn MediaStream>> {
        Box::pin(async move {
            let original = Original::read(media, kind)?;
            let maximum = if kind == "image" {
                4 * 1024 * 1024
            } else {
                32 * 1024 * 1024
            };
            let duration = Duration::from_millis(timeout_ms);
            let overflow = Arc::new(AtomicBool::new(false));
            let cancelled = Arc::new(AtomicBool::new(false));
            let _cancel = CancelDownload(cancelled.clone());
            let writer = BoundedWriter {
                bytes: Cursor::new(Vec::new()),
                maximum,
                deadline: Instant::now() + duration,
                overflow: overflow.clone(),
                cancelled,
            };
            let result = tokio::time::timeout(
                duration,
                self.downloads.download_to_writer(&original, writer),
            )
            .await;
            let writer = match result {
                Ok(Ok(writer)) => writer,
                _ if overflow.load(Ordering::Acquire) => {
                    return Err(IvyError::new(
                        "whatsapp_media_limit",
                        "WhatsApp media exceeds its size limit.",
                    ));
                }
                _ => return Err(invalid()),
            };
            Ok(Box::new(VerifiedBytes(Some(writer.bytes.into_inner()))) as Box<dyn MediaStream>)
        })
    }
    fn voice_wav<'a>(
        &'a self,
        bytes: &'a [u8],
        translator: Translator,
    ) -> MediaFuture<'a, Vec<u8>> {
        Box::pin(async move { self.audio.decode(bytes, translator).await })
    }
    fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> MediaFuture<'_, Box<dyn TranscriptionResponse>> {
        Box::pin(async move {
            let url = url::Url::parse(&request.url).map_err(|_| transcription_failed())?;
            if url.scheme() != "https" || !request.redirect_error {
                return Err(transcription_failed());
            }
            let file = reqwest::multipart::Part::bytes(request.wav)
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .map_err(|_| transcription_failed())?;
            let mut form = reqwest::multipart::Form::new()
                .part("file", file)
                .text("model", request.model.to_utf8()?)
                .text("response_format", "json");
            if let Some(language) = request.language {
                form = form.text("language", language.to_utf8()?);
            }
            let response = self
                .http
                .post(url)
                .header(
                    reqwest::header::AUTHORIZATION,
                    request.authorization.to_utf8()?,
                )
                .multipart(form)
                .timeout(Duration::from_millis(request.timeout_ms))
                .send()
                .await
                .map_err(|_| transcription_failed())?;
            Ok(Box::new(HttpTranscriptionResponse(Some(response)))
                as Box<dyn TranscriptionResponse>)
        })
    }
}
struct HttpTranscriptionResponse(Option<reqwest::Response>);
impl TranscriptionResponse for HttpTranscriptionResponse {
    fn ok(&self) -> bool {
        self.0.as_ref().is_some_and(|r| r.status().is_success())
    }
    fn json(&mut self) -> RequestFuture<'_> {
        Box::pin(async move {
            let mut response = self.0.take().ok_or_else(transcription_failed)?;
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| transcription_failed())? {
                if body.len() + chunk.len() > 1024 * 1024 {
                    return Err(transcription_failed());
                }
                body.extend_from_slice(&chunk);
            }
            parse(
                std::str::from_utf8(&body).map_err(|_| transcription_failed())?,
                usize::MAX,
            )
            .map_err(|_| transcription_failed())
        })
    }
}
