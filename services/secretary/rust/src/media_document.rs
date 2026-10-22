//! Acquired-input admission and the original bounded document projection cache.
use crate::{
    document::Format,
    media_input::{
        self, AcquiredMediaPreparer, Kind, PrepareFuture, Prepared, REQUEST_BYTES, Source,
    },
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    builtin_schema::SchemaBundle,
    hash::digest,
    json::{canonical, parse, stringify},
};
use std::{cell::RefCell, collections::VecDeque, future::Future, pin::Pin, sync::OnceLock};

pub const CACHE_BYTES: usize = 16 * 1024 * 1024;
pub const CACHE_ENTRIES: usize = 16;
pub const WORKER_REPLY_BYTES: usize = 8 * 1024 * 1024;
pub type DocumentFuture<'a> = Pin<Box<dyn Future<Output = IvyResult<IvyJson>> + 'a>>;

/// The process adapter must enforce the caller's resource policy and the 30s
/// deadline, drain private diagnostics, and return only after owned retirement.
/// A lost retirement proof is an Unknown error, never a successful projection.
/// Rust HTML/DOCX needs the qualified Host memory-limit seam; PDF uses the
/// separately approved PDF.js/V8 adapter. No unbounded/default launcher exists.
pub trait DocumentWorker {
    fn extract<'a>(
        &'a self,
        format: Format,
        bytes: &'a [u8],
        maximum_input_bytes: f64,
    ) -> DocumentFuture<'a>;
}
fn invalid() -> IvyError {
    IvyError::new(
        "media_document_invalid",
        "The document worker needs a complete bounded projection.",
    )
}
fn unavailable(source: &Source, code: &str) -> Prepared {
    Prepared::Unavailable {
        source: source.clone(),
        code: code.into(),
    }
}
fn valid_code(value: &IvyJson) -> bool {
    let Some(text) = value
        .get("code")
        .and_then(IvyJson::as_js_string)
        .and_then(|value| value.to_utf8().ok())
    else {
        return false;
    };
    text.starts_with("media_")
        && text.len() > 6
        && text[6..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_')
}
pub fn format_name(format: Format) -> &'static str {
    match format {
        Format::Pdf => "pdf",
        Format::Docx => "docx",
        Format::Html => "html",
    }
}
pub fn document_format(media_type: &str) -> Option<Format> {
    match media_type {
        "application/pdf" => Some(Format::Pdf),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(Format::Docx)
        }
        "text/html" => Some(Format::Html),
        _ => None,
    }
}
pub fn validate_document(value: &IvyJson) -> IvyResult<()> {
    canonical(value, 1024 * 1024)?;
    static BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
    let bundle = BUNDLE
        .get_or_init(|| {
            SchemaBundle::new(&[include_str!(
                "../../../../specs/schemas/secretary-document.schema.json"
            )])
        })
        .as_ref()
        .map_err(Clone::clone)?;
    if bundle.matches("#/$defs/DocumentText", value)? {
        Ok(())
    } else {
        Err(invalid())
    }
}
pub fn text_input(text: JsString) -> IvyJson {
    IvyJson::object([
        ("type", "text".into()),
        ("text", IvyJson::String(text)),
        ("text_elements", IvyJson::Array(vec![])),
    ])
}
fn projection(format: Format, reply: &IvyJson) -> IvyResult<JsString> {
    if reply.as_object().is_none() {
        return Err(invalid());
    }
    let text = if let Some(text) = reply.get("text").and_then(IvyJson::as_js_string) {
        text.clone()
    } else if format == Format::Pdf {
        // This is the approved external-library adapter's existing private
        // value envelope. The Rust owner canonicalizes it before admission.
        canonical(reply.get("value").ok_or_else(invalid)?, WORKER_REPLY_BYTES)?.into()
    } else {
        return Err(invalid());
    };
    let bytes = String::from_utf16_lossy(&text.0);
    if bytes.len() > REQUEST_BYTES {
        return Err(invalid());
    }
    let value = parse(&bytes, WORKER_REPLY_BYTES).map_err(|_| invalid())?;
    validate_document(&value)?;
    if value.get("format") != Some(&format_name(format).into()) {
        return Err(invalid());
    }
    Ok(text)
}
struct Cached {
    key: String,
    text: JsString,
    bytes: usize,
}
#[derive(Default)]
struct Cache {
    entries: VecDeque<Cached>,
    bytes: usize,
}
impl Cache {
    fn get(&mut self, key: &str) -> Option<JsString> {
        let index = self.entries.iter().position(|entry| entry.key == key)?;
        let entry = self.entries.remove(index)?;
        let text = entry.text.clone();
        self.entries.push_back(entry);
        Some(text)
    }
    fn insert(&mut self, key: String, text: JsString) {
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key) {
            self.bytes -= self.entries.remove(index).unwrap().bytes;
        }
        let bytes = String::from_utf16_lossy(&text.0).len();
        while !self.entries.is_empty()
            && (self.bytes + bytes > CACHE_BYTES || self.entries.len() >= CACHE_ENTRIES)
        {
            self.bytes -= self.entries.pop_front().unwrap().bytes;
        }
        self.bytes += bytes;
        self.entries.push_back(Cached { key, text, bytes });
    }
}
pub struct AcquiredInput<'a> {
    worker: &'a dyn DocumentWorker,
    video: Option<&'a dyn AcquiredMediaPreparer>,
    cache: RefCell<Cache>,
}
impl<'a> AcquiredInput<'a> {
    pub fn new(
        worker: &'a dyn DocumentWorker,
        video: Option<&'a dyn AcquiredMediaPreparer>,
    ) -> Self {
        Self {
            worker,
            video,
            cache: RefCell::new(Cache::default()),
        }
    }
    pub async fn prepare(
        &self,
        source: &Source,
        bytes: &[u8],
        maximum_input_bytes: f64,
    ) -> IvyResult<Prepared> {
        let media_type = media_input::mime(source);
        if matches!(
            media_type.as_str(),
            "video/mp4" | "video/quicktime" | "video/webm" | "video/x-matroska" | "video/x-msvideo"
        ) {
            if let Some(video) = self.video {
                return video.prepare(source, bytes, maximum_input_bytes).await;
            }
            media_input::prepare(source, bytes, maximum_input_bytes)?;
            return Ok(unavailable(source, "media_video_decoder_unconfigured"));
        }
        let Some(format) = document_format(&media_type) else {
            return media_input::prepare(source, bytes, maximum_input_bytes);
        };
        if !maximum_input_bytes.is_finite()
            || maximum_input_bytes.fract() != 0.0
            || maximum_input_bytes <= 0.0
            || maximum_input_bytes > REQUEST_BYTES as f64
        {
            return Err(IvyError::new(
                "media_input_limit_invalid",
                "Media input needs its bounded selected native request budget.",
            ));
        }
        if bytes.len() != source.byte_length || digest(bytes) != source.content_hash {
            return Err(IvyError::new(
                "media_input_integrity_invalid",
                "Semantic input must retain the complete verified original attachment bytes.",
            ));
        }
        if bytes.len() > crate::document::SOURCE_LIMIT {
            return Ok(unavailable(source, "media_document_too_large"));
        }
        let key = format!(
            "{}\0{media_type}\0{}",
            source.content_hash, maximum_input_bytes as usize
        );
        let cached = self.cache.borrow_mut().get(&key);
        if let Some(text) = cached {
            return Ok(Prepared::Input {
                source: source.clone(),
                kind: Kind::Text,
                input: text_input(text),
            });
        }
        let reply = match self
            .worker
            .extract(format, bytes, maximum_input_bytes)
            .await
        {
            Ok(reply) => reply,
            Err(error) if error.outcome == Outcome::Unknown => return Err(error),
            Err(_) => return Ok(unavailable(source, "media_document_parser_failed")),
        };
        if valid_code(&reply) {
            return Ok(unavailable(source, &reply.required("code")?.as_str()?));
        }
        let text = match projection(format, &reply) {
            Ok(text) => text,
            Err(_) => return Ok(unavailable(source, "media_document_invalid")),
        };
        let input = text_input(text.clone());
        if stringify(&input, maximum_input_bytes as usize).is_err() {
            return Ok(unavailable(source, "media_native_input_too_large"));
        }
        self.cache.borrow_mut().insert(key, text);
        Ok(Prepared::Input {
            source: source.clone(),
            kind: Kind::Text,
            input,
        })
    }
}
impl AcquiredMediaPreparer for AcquiredInput<'_> {
    fn prepare<'a>(
        &'a self,
        source: &'a Source,
        bytes: &'a [u8],
        budget: f64,
    ) -> PrepareFuture<'a> {
        Box::pin(AcquiredInput::prepare(self, source, bytes, budget))
    }
}
