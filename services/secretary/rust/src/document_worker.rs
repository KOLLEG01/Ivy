//! Private raw-stdin HTML/DOCX child mode. The Host owner supplies OS limits.
use crate::{
    document::{self, Format},
    media_document::{WORKER_REPLY_BYTES, text_input, validate_document},
    media_input::REQUEST_BYTES,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::{canonical, stringify},
};
use std::{
    ffi::OsString,
    io::{Read, Write},
};

pub fn reply(format: Format, bytes: &[u8], maximum_input_bytes: usize) -> IvyJson {
    let result = (|| -> IvyResult<IvyJson> {
        if format == Format::Pdf || maximum_input_bytes == 0 || maximum_input_bytes > REQUEST_BYTES
        {
            return Err(IvyError::new(
                "media_document_invalid",
                "Invalid private document request.",
            ));
        }
        let value = document::extract(format, bytes)
            .map_err(|code| IvyError::new(code, "Document projection failed."))?;
        let value =
            IvyJson::from_serde(&serde_json::to_value(value).map_err(|_| IvyError::internal())?)?;
        validate_document(&value)?;
        let text = canonical(&value, WORKER_REPLY_BYTES)?;
        if stringify(&text_input(text.clone().into()), maximum_input_bytes).is_err() {
            return Err(IvyError::new(
                "media_native_input_too_large",
                "Document projection exceeds its selected request budget.",
            ));
        }
        Ok(IvyJson::object([("text", text.into())]))
    })();
    result.unwrap_or_else(|error| {
        let code = if error.code.starts_with("media_") {
            error.code_js()
        } else {
            "media_document_invalid".into()
        };
        IvyJson::object([("code", IvyJson::String(code))])
    })
}
pub fn run(arguments: &[OsString]) -> IvyResult<()> {
    let selected = match arguments {
        [format, budget] => match (
            format.to_str(),
            budget
                .to_str()
                .and_then(|value| value.parse::<usize>().ok()),
        ) {
            (Some("html"), Some(budget)) => Some((Format::Html, budget)),
            (Some("docx"), Some(budget)) => Some((Format::Docx, budget)),
            _ => None,
        },
        _ => None,
    };
    let value = if let Some((format, budget)) =
        selected.filter(|(_, budget)| *budget > 0 && *budget <= REQUEST_BYTES)
    {
        let mut bytes = vec![];
        if std::io::stdin()
            .take(document::SOURCE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .is_ok()
        {
            reply(format, &bytes, budget)
        } else {
            IvyJson::object([("code", "media_document_invalid".into())])
        }
    } else {
        IvyJson::object([("code", "media_document_invalid".into())])
    };
    let mut output = canonical(&value, WORKER_REPLY_BYTES)?;
    output.push('\n');
    std::io::stdout()
        .write_all(output.as_bytes())
        .map_err(|_| IvyError::internal())
}
