//! Inert native input from complete, verified original attachment bytes.
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::digest, json::stringify};
use std::{future::Future, pin::Pin};

pub type PrepareFuture<'a> = Pin<Box<dyn Future<Output = IvyResult<Prepared>> + 'a>>;
/// Composition supplies acquired-input preparation, including the disposable
/// document worker and any configured video decoder. There is no parser fallback.
pub trait AcquiredMediaPreparer {
    fn prepare<'a>(
        &'a self,
        source: &'a Source,
        bytes: &'a [u8],
        maximum_input_bytes: f64,
    ) -> PrepareFuture<'a>;
}

pub const REQUEST_BYTES: usize = 6 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: JsString,
    pub media_type: Option<JsString>,
    pub byte_length: usize,
    pub content_hash: String,
}
impl Source {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("name", IvyJson::String(self.name.clone())),
            (
                "mediaType",
                self.media_type
                    .clone()
                    .map(IvyJson::String)
                    .unwrap_or(IvyJson::Null),
            ),
            ("byteLength", IvyJson::Number(self.byte_length as f64)),
            ("contentHash", self.content_hash.clone().into()),
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Image,
    Audio,
}
impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Audio => "audio",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Prepared {
    Input {
        source: Source,
        kind: Kind,
        input: IvyJson,
    },
    Video {
        source: Source,
        inputs: Vec<IvyJson>,
    },
    Unavailable {
        source: Source,
        code: String,
    },
}
impl Prepared {
    pub fn source(&self) -> &Source {
        match self {
            Self::Input { source, .. }
            | Self::Video { source, .. }
            | Self::Unavailable { source, .. } => source,
        }
    }
    pub fn to_json(&self) -> IvyJson {
        match self {
            Self::Input {
                source,
                kind,
                input,
            } => IvyJson::object([
                ("kind", kind.as_str().into()),
                ("source", source.to_json()),
                ("input", input.clone()),
            ]),
            Self::Video { source, inputs } => IvyJson::object([
                ("kind", "video".into()),
                ("source", source.to_json()),
                ("inputs", IvyJson::Array(inputs.clone())),
            ]),
            Self::Unavailable { source, code } => IvyJson::object([
                ("kind", "unavailable".into()),
                ("source", source.to_json()),
                ("code", code.clone().into()),
            ]),
        }
    }
}

// MIME selection uses ECMAScript trim, including BOM but excluding NEL.
fn mime(source: &Source) -> String {
    let units = source
        .media_type
        .as_ref()
        .map(|s| s.0.as_slice())
        .unwrap_or_default();
    let prefix = units
        .split(|unit| *unit == u16::from(b';'))
        .next()
        .unwrap_or_default();
    String::from_utf16_lossy(prefix)
        .trim_matches(|c: char| matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'))
        .to_lowercase()
}

/// The selected native request supplies the budget; this function never truncates.
pub fn prepare(source: &Source, bytes: &[u8], maximum_input_bytes: f64) -> IvyResult<Prepared> {
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
    let unavailable = |code: &str| Prepared::Unavailable {
        source: source.clone(),
        code: code.into(),
    };
    let media_type = mime(source);
    let (kind, input) = if matches!(
        media_type.as_str(),
        "text/plain"
            | "text/markdown"
            | "text/csv"
            | "application/json"
            | "application/xml"
            | "text/xml"
    ) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Ok(unavailable("media_text_encoding_unsupported"));
        };
        // TextDecoder(ignoreBOM: true) retains a leading BOM, as from_utf8 does.
        (
            Kind::Text,
            IvyJson::object([
                ("type", "text".into()),
                ("text", text.into()),
                ("text_elements", IvyJson::Array(vec![])),
            ]),
        )
    } else {
        let (kind, valid) = match media_type.as_str() {
            "image/png" => (
                Kind::Image,
                bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            ),
            "image/jpeg" => (
                Kind::Image,
                bytes.len() >= 4 && bytes.starts_with(b"\xff\xd8\xff"),
            ),
            "image/gif" => (
                Kind::Image,
                bytes.len() >= 13 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")),
            ),
            "image/webp" => (
                Kind::Image,
                bytes.len() >= 20
                    && bytes.starts_with(b"RIFF")
                    && bytes.get(8..12) == Some(b"WEBP"),
            ),
            "audio/ogg" => (Kind::Audio, bytes.len() >= 27 && bytes.starts_with(b"OggS")),
            "audio/wav" => (
                Kind::Audio,
                bytes.len() >= 44
                    && bytes.starts_with(b"RIFF")
                    && bytes.get(8..12) == Some(b"WAVE"),
            ),
            "audio/mpeg" => (
                Kind::Audio,
                bytes.len() >= 4
                    && (bytes.starts_with(b"ID3") || (bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0)),
            ),
            "audio/mp4" => (
                Kind::Audio,
                bytes.len() >= 16 && bytes.get(4..8) == Some(b"ftyp"),
            ),
            "audio/webm" => (
                Kind::Audio,
                bytes.len() >= 8 && bytes.starts_with(b"\x1a\x45\xdf\xa3"),
            ),
            _ => return Ok(unavailable("media_type_unsupported")),
        };
        if !valid {
            return Ok(unavailable("media_signature_mismatch"));
        }
        (
            kind,
            IvyJson::object([
                ("type", kind.as_str().into()),
                (
                    "url",
                    format!("data:{media_type};base64,{}", STANDARD.encode(bytes)).into(),
                ),
            ]),
        )
    };
    match stringify(&input, maximum_input_bytes as usize) {
        Ok(_) => Ok(Prepared::Input {
            source: source.clone(),
            kind,
            input,
        }),
        Err(e) if e.code == "content_too_large" => Ok(unavailable("media_native_input_too_large")),
        Err(e) => Err(e),
    }
}
