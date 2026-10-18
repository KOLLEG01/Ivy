//! Original media contracts and their service-owned semantic admission rules.
use ivy_contracts::{IvyError, IvyJson, IvyResult, builtin_schema::SchemaBundle, json::canonical};
use std::{collections::HashSet, sync::OnceLock};

#[derive(Clone, Copy, Debug)]
pub enum MediaSchema {
    Attachment,
    Materialization,
    DownloadEvidence,
    Admission,
    Intent,
    Manifest,
    ReadPlan,
    ReadCall,
    Coverage,
}
impl MediaSchema {
    pub fn name(self) -> &'static str {
        match self {
            Self::Attachment => "MediaAttachment",
            Self::Materialization => "MediaMaterialization",
            Self::DownloadEvidence => "MediaDownloadEvidence",
            Self::Admission => "MediaAdmission",
            Self::Intent => "MediaIntent",
            Self::Manifest => "MediaManifest",
            Self::ReadPlan => "MediaReadPlan",
            Self::ReadCall => "MediaReadCall",
            Self::Coverage => "MediaCoverage",
        }
    }
}
fn need(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}

// Evidence precision is four-digit-year UTC; native ISO timestamps also admit
// Date.toISOString's signed six-digit years, within ECMAScript's TimeClip range.
fn timestamp_valid(value: &IvyJson, exact_millis: bool) -> bool {
    if matches!(value, IvyJson::Null) {
        return !exact_millis;
    }
    let Ok(text) = value.as_str() else {
        return false;
    };
    let raw = text.as_bytes();
    let extended = exact_millis && raw.len() == 27 && matches!(raw[0], b'+' | b'-');
    let b = if extended { &raw[3..] } else { raw };
    let fraction = match b.len() {
        20 if !exact_millis => 0,
        22..=28 if !exact_millis || b.len() == 24 => b.len() - 21,
        _ => return false,
    };
    if b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[b.len() - 1] != b'Z'
        || (fraction != 0
            && (b[19] != b'.' || !b[20..20 + fraction].iter().all(u8::is_ascii_digit)))
    {
        return false;
    }
    let number = |start: usize, end: usize| -> Option<u32> {
        b[start..end].iter().try_fold(0, |n, c| {
            c.is_ascii_digit().then(|| n * 10 + u32::from(c - b'0'))
        })
    };
    let (Some(mut year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0, 4),
        number(5, 7),
        number(8, 10),
        number(11, 13),
        number(14, 16),
        number(17, 19),
    ) else {
        return false;
    };
    let mut signed_year = year as i32;
    if extended {
        if !raw[1..7].iter().all(u8::is_ascii_digit) {
            return false;
        }
        year = raw[1..7]
            .iter()
            .fold(0, |n, c| n * 10 + u32::from(c - b'0'));
        if (raw[0] == b'+' && year < 10_000) || (raw[0] == b'-' && year == 0) {
            return false;
        }
        signed_year = if raw[0] == b'-' {
            -(year as i32)
        } else {
            year as i32
        };
    }
    if exact_millis {
        let Some(millis) = number(20, 23) else {
            return false;
        };
        let parts = (signed_year, month, day, hour, minute, second, millis);
        if parts < (-271_821, 4, 20, 0, 0, 0, 0) || parts > (275_760, 9, 13, 0, 0, 0, 0) {
            return false;
        }
    }
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return false,
    };
    day >= 1 && day <= days && hour <= 23 && minute <= 59 && second <= 59
}
fn timestamp(value: &IvyJson) -> IvyResult<()> {
    need(
        timestamp_valid(value, false),
        "media_timestamp_invalid",
        "Media evidence needs a real UTC timestamp; its original precision is retained.",
    )
}

pub fn validate_media(name: MediaSchema, value: &IvyJson) -> IvyResult<()> {
    // validateMedia uses SchemaValidators' default 1 MiB, not the main
    // Secretary validator's 32 MiB management-frame limit.
    canonical(value, 1024 * 1024)?;
    static BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
    let bundle = BUNDLE
        .get_or_init(|| {
            SchemaBundle::new(&[include_str!(
                "../../../../specs/schemas/secretary-media.schema.json"
            )])
        })
        .as_ref()
        .map_err(Clone::clone)?;
    need(
        bundle.matches(&format!("#/$defs/{}", name.name()), value)?,
        "invalid_arguments",
        "Content does not satisfy its exact contract.",
    )?;
    match name {
        MediaSchema::Admission | MediaSchema::Intent => return Ok(()),
        MediaSchema::ReadPlan | MediaSchema::ReadCall => {
            let key = if matches!(name, MediaSchema::ReadPlan) {
                "createdAt"
            } else {
                "preparedAt"
            };
            return need(
                timestamp_valid(value.required(key)?, true),
                "media_timestamp_invalid",
                "Native media work needs its original real UTC timestamp.",
            );
        }
        MediaSchema::Manifest => {
            return validate_media(MediaSchema::DownloadEvidence, value.required("evidence")?);
        }
        MediaSchema::Coverage => {
            let attachments = value.required("attachments")?.as_array().unwrap();
            let mut identities = HashSet::new();
            let mut valid = timestamp_valid(value.required("observedAt")?, true);
            for entry in attachments {
                let manifest = !matches!(entry.required("manifest")?, IvyJson::Null);
                let error = !matches!(entry.required("errorCode")?, IvyJson::Null);
                valid &= if manifest {
                    !error && matches!(value.required("gap")?, IvyJson::Null)
                } else {
                    error
                };
                valid &= identities.insert(
                    entry
                        .required("attachment")?
                        .required("attachmentId")?
                        .as_js_string()
                        .unwrap()
                        .clone(),
                );
            }
            need(
                valid,
                "media_coverage_invalid",
                "Coverage retains one complete payload or an explicit error for each original attachment.",
            )?;
            for entry in attachments {
                validate_media(MediaSchema::Attachment, entry.required("attachment")?)?;
            }
            return Ok(());
        }
        _ => (),
    }
    let attachment = if matches!(name, MediaSchema::Attachment) {
        value
    } else {
        value.required("attachment")?
    };
    timestamp(attachment.required("modifiedAt")?)?;
    if matches!(name, MediaSchema::DownloadEvidence) {
        timestamp(value.required("startedAt")?)?;
        timestamp(value.required("observedAt")?)?;
        let length = value.required("byteLength")?.as_f64();
        let relation = if attachment.required("providerSizeBytes")?.as_f64() == length {
            "equal"
        } else {
            "different"
        };
        let http = value.required("http")?;
        let content_length = http.required("contentLength")?;
        let encoding = http.required("contentEncoding")?;
        let identity_encoding = matches!(encoding, IvyJson::Null)
            || encoding.as_js_string().is_some_and(|s| {
                s.0.len() == 8
                    && s.0.iter().zip(b"identity").all(|(&unit, &byte)| {
                        unit == u16::from(byte) || unit == u16::from(byte.to_ascii_uppercase())
                    })
            });
        need(
            value.required("providerSizeRelation")?.as_str()? == relation
                && (matches!(content_length, IvyJson::Null) || content_length.as_f64() == length)
                && identity_encoding,
            "media_evidence_invalid",
            "Media evidence must describe the complete acquired body.",
        )?;
    }
    Ok(())
}
