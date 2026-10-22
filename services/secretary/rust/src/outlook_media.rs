//! Metadata and signed materialization admission, separate from byte acquisition.
use crate::media_schema::{MediaSchema, validate_media};
use ivy_contracts::{IvyError, IvyJson, IvyResult, hash::hash_json, json::canonical};
use std::collections::HashSet;

pub const MAXIMUM_BYTES: usize = 32 * 1024 * 1024;
fn need(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn check(value: bool, code: &str) -> IvyResult<()> {
    need(
        value,
        code,
        "Media acquisition requires complete original account-bound attachment evidence.",
    )
}
fn record(value: Option<&IvyJson>) -> IvyResult<&IvyJson> {
    value.filter(|v| v.as_object().is_some()).ok_or_else(|| {
        IvyError::new(
            "media_response_invalid",
            "Media evidence must be an object.",
        )
    })
}
fn text(value: Option<&IvyJson>, maximum: usize) -> IvyResult<IvyJson> {
    let message = "Media metadata must be bounded lossless text.";
    let value = value.and_then(IvyJson::as_js_string);
    need(
        value.is_some_and(|v| !v.0.is_empty() && v.0.len() <= maximum),
        "media_response_invalid",
        message,
    )?;
    let value = value.unwrap();
    let decoded = value
        .to_utf8()
        .map_err(|_| IvyError::new("media_response_invalid", message))?;
    need(
        !decoded
            .chars()
            .any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}' | '\u{fffe}' | '\u{ffff}')),
        "media_response_invalid",
        message,
    )?;
    Ok(IvyJson::String(value.clone()))
}
fn nullable_text(value: Option<&IvyJson>, maximum: usize) -> IvyResult<IvyJson> {
    if value == Some(&IvyJson::Null) {
        Ok(IvyJson::Null)
    } else {
        text(value, maximum)
    }
}
fn integer(value: Option<&IvyJson>) -> IvyResult<IvyJson> {
    need(
        value.and_then(IvyJson::as_f64).is_some_and(|n| {
            n.is_finite() && n.fract() == 0.0 && (0.0..=2147483647.0).contains(&n)
        }),
        "media_response_invalid",
        "Attachment size must be a nonnegative provider integer.",
    )?;
    Ok(value.unwrap().clone())
}
pub fn outlook_attachments(value: &IvyJson, message_id: &IvyJson) -> IvyResult<Vec<IvyJson>> {
    text(Some(message_id), 4096)?;
    canonical(value, 1024 * 1024)?;
    let page = record(Some(value))?;
    check(
        page.get("message_id") == Some(message_id)
            && page.get("next_link") == Some(&IvyJson::Null)
            && page
                .get("errors")
                .and_then(IvyJson::as_array)
                .is_some_and(|v| v.is_empty()),
        "media_metadata_incomplete",
    )?;
    let entries = page.get("attachments").and_then(IvyJson::as_array);
    need(
        entries.is_some_and(|v| v.len() <= 32),
        "media_metadata_incomplete",
        "Attachment metadata needs a complete page of at most 32 entries.",
    )?;
    let mut seen = HashSet::new();
    let mut result = vec![];
    for entry in entries.unwrap() {
        let raw = record(Some(entry))?;
        let missing = IvyJson::Null;
        let kind = raw.get("attachment_type").unwrap_or(&missing);
        let kind_string = kind.as_str()?;
        check(
            matches!(
                kind_string.as_str(),
                "fileAttachment" | "itemAttachment" | "referenceAttachment" | "unknown"
            ) && (kind == &"unknown".into()
                || raw.get("odata_type")
                    == Some(&format!("#microsoft.graph.{kind_string}").into())),
            "media_response_invalid",
        )?;
        check(
            matches!(raw.get("is_inline"), Some(IvyJson::Bool(_)))
                && matches!(raw.get("payload_fetch_supported"), Some(IvyJson::Bool(_)))
                && raw.get("payload_status") == Some(&"not_requested".into())
                && raw.get("payload_error") == Some(&IvyJson::Null)
                && raw.get("file_uri") == Some(&IvyJson::Null),
            "media_response_invalid",
        )?;
        let attachment = IvyJson::object([
            ("messageId", message_id.clone()),
            ("attachmentId", text(raw.get("id"), 4096)?),
            ("attachmentType", kind.clone()),
            ("name", text(raw.get("name"), 1024)?),
            ("mediaType", nullable_text(raw.get("content_type"), 256)?),
            ("providerSizeBytes", integer(raw.get("size_bytes"))?),
            ("inline", raw.required("is_inline")?.clone()),
            ("contentId", nullable_text(raw.get("content_id"), 4096)?),
            (
                "modifiedAt",
                nullable_text(raw.get("last_modified_date_time"), 40)?,
            ),
            (
                "payloadFetchSupported",
                raw.required("payload_fetch_supported")?.clone(),
            ),
        ]);
        check(
            seen.insert(
                attachment
                    .required("attachmentId")?
                    .as_js_string()
                    .unwrap()
                    .clone(),
            ),
            "media_duplicate_attachment",
        )?;
        validate_media(MediaSchema::Attachment, &attachment)?;
        result.push(attachment);
    }
    Ok(result)
}
pub fn materialization_url(value: &str) -> IvyResult<url::Url> {
    let url = url::Url::parse(value).map_err(|_| {
        IvyError::new(
            "media_download_url_invalid",
            "The native materialization URL is invalid.",
        )
    })?;
    check(
        url.scheme() == "https"
            && url.host_str().is_some_and(|v| {
                v.ends_with(".oaiusercontent.com") && v.len() > ".oaiusercontent.com".len()
            })
            && url.username().is_empty()
            && url.password().is_none_or(str::is_empty)
            && url.port().is_none()
            && url.fragment().is_none_or(str::is_empty),
        "media_download_url_invalid",
    )?;
    Ok(url)
}
pub fn outlook_materialization(value: &IvyJson, attachment: &IvyJson) -> IvyResult<IvyJson> {
    validate_media(MediaSchema::Attachment, attachment)?;
    canonical(value, 1024 * 1024)?;
    let raw = record(Some(value))?;
    check(
        attachment.get("attachmentType") == Some(&"fileAttachment".into())
            && attachment.get("payloadFetchSupported") == Some(&IvyJson::Bool(true)),
        "media_attachment_unsupported",
    )?;
    check(
        raw.get("payload_status") == Some(&"materialized".into())
            && [
                ("message_id", "messageId"),
                ("attachment_id", "attachmentId"),
                ("attachment_type", "attachmentType"),
                ("filename", "name"),
                ("size_bytes", "providerSizeBytes"),
                ("mime_type", "mediaType"),
                ("is_inline", "inline"),
                ("content_id", "contentId"),
            ]
            .iter()
            .all(|(key, expected)| raw.get(key) == attachment.get(expected)),
        "media_materialization_mismatch",
    )?;
    let uri = record(raw.get("file_uri"))?;
    check(
        uri.get("file_name") == attachment.get("name")
            && uri.get("mime_type") == attachment.get("mediaType"),
        "media_materialization_mismatch",
    )?;
    let result = IvyJson::object([
        ("attachment", attachment.clone()),
        ("fileId", text(uri.get("file_id"), 1024)?),
        ("downloadUrl", text(uri.get("download_url"), 16384)?),
        ("nativeResponseHash", hash_json(value)?.into()),
    ]);
    materialization_url(&result.required("downloadUrl")?.as_str()?)?;
    validate_media(MediaSchema::Materialization, &result)?;
    Ok(result)
}
