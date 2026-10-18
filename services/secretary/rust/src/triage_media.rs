//! Ordered attachment evidence and native frame admission for retained triage.
use crate::{
    journal::Pin,
    media_input::{Prepared, REQUEST_BYTES},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::canonical};
use std::future::Future;

const MEDIA_BYTES: usize = REQUEST_BYTES - 512 * 1024;
const MISMATCH: &str = "secretary_triage_media_mismatch";

#[derive(Debug, Clone)]
pub struct Attachment {
    pub name: Option<JsString>,
    pub media_type: Option<JsString>,
    pub manifest: Option<Pin>,
    pub error_code: Option<JsString>,
    pub representation: Representation,
}
#[derive(Debug, Clone, Copy)]
pub enum Representation {
    OriginalBytes,
    RenderedPreview,
}
impl Representation {
    fn as_str(self) -> &'static str {
        match self {
            Self::OriginalBytes => "original_bytes",
            Self::RenderedPreview => "rendered_preview",
        }
    }
}
#[derive(Debug, Clone)]
pub struct Context {
    pub coverage: Pin,
    pub coverage_contract: String,
    pub coverage_hash: String,
    pub item: Pin,
    pub source_id: JsString,
    pub gap: Option<JsString>,
    pub attachments: Vec<Attachment>,
}
#[derive(Debug)]
pub struct Inputs {
    pub media: IvyJson,
    pub inputs: Vec<IvyJson>,
}
fn mismatch() -> IvyError {
    IvyError::new(
        MISMATCH,
        "Triage requires its original item, owner, native intent and complete evidence.",
    )
}
fn pin(value: &Pin) -> IvyJson {
    IvyJson::object([
        ("objectId", value.object_id.clone().into()),
        ("revision", IvyJson::Number(value.revision as f64)),
    ])
}
fn optional(value: &Option<JsString>) -> IvyJson {
    value.clone().map(IvyJson::String).unwrap_or(IvyJson::Null)
}

/// The resolver must be the internal verified original-coverage reader.
/// Source verification still runs when the frame is exhausted; gaps stay explicit.
pub async fn assemble<F, Fut>(
    context: &Context,
    item: &Pin,
    source_id: &JsString,
    mut input: F,
) -> IvyResult<Inputs>
where
    F: FnMut(usize, usize) -> Fut,
    Fut: Future<Output = IvyResult<Option<Prepared>>>,
{
    let original = context.clone();
    if &original.item != item || &original.source_id != source_id {
        return Err(mismatch());
    }
    let mut attachments = Vec::new();
    let mut inputs = Vec::new();
    let mut remaining = MEDIA_BYTES;
    for (index, attachment) in original.attachments.iter().enumerate() {
        let mut entry = IvyJson::object([
            ("name", optional(&attachment.name)),
            ("mediaType", optional(&attachment.media_type)),
            (
                "manifest",
                attachment
                    .manifest
                    .as_ref()
                    .map(pin)
                    .unwrap_or(IvyJson::Null),
            ),
            ("byteLength", IvyJson::Null),
            ("contentHash", IvyJson::Null),
            ("representation", attachment.representation.as_str().into()),
            ("kind", "unavailable".into()),
            ("inputIndex", IvyJson::Null),
            ("errorCode", optional(&attachment.error_code)),
        ]);
        if attachment.manifest.is_none() {
            if !attachment
                .error_code
                .as_ref()
                .is_some_and(|s| !s.0.is_empty())
            {
                return Err(mismatch());
            }
            attachments.push(entry);
            continue;
        }
        let label = IvyJson::object([
            ("type", "text".into()),
            (
                "text",
                canonical(
                    &IvyJson::object([
                        ("attachment", IvyJson::Number((index + 1) as f64)),
                        ("name", optional(&attachment.name)),
                        ("mediaType", optional(&attachment.media_type)),
                        ("representation", attachment.representation.as_str().into()),
                    ]),
                    usize::MAX,
                )?
                .into(),
            ),
            ("text_elements", IvyJson::Array(vec![])),
        ]);
        let label_bytes = canonical(&label, usize::MAX)?.len() + 2;
        let prepared = input(index, remaining.saturating_sub(label_bytes).max(1))
            .await?
            .ok_or_else(mismatch)?;
        if attachment.name.as_ref() != Some(&prepared.source().name)
            || attachment.media_type != prepared.source().media_type
        {
            return Err(mismatch());
        }
        entry.insert(
            "byteLength",
            IvyJson::Number(prepared.source().byte_length as f64),
        );
        entry.insert("contentHash", prepared.source().content_hash.clone().into());
        match prepared {
            Prepared::Unavailable { code, .. } => entry.insert("errorCode", code.into()),
            prepared => {
                let (kind, parts) = match prepared {
                    Prepared::Input { kind, input, .. } => (kind.as_str(), vec![input]),
                    Prepared::Video { inputs, .. } => ("video", inputs),
                    Prepared::Unavailable { .. } => unreachable!(),
                };
                let used =
                    label_bytes + canonical(&IvyJson::Array(parts.clone()), usize::MAX)?.len();
                if used > remaining {
                    entry.insert("errorCode", "media_native_input_too_large".into());
                } else {
                    entry.insert("kind", kind.into());
                    // Native input index includes the leading triage prompt outside this list.
                    entry.insert("inputIndex", IvyJson::Number((inputs.len() + 2) as f64));
                    entry.insert("errorCode", IvyJson::Null);
                    inputs.push(label);
                    inputs.extend(parts);
                    remaining -= used;
                }
            }
        }
        attachments.push(entry);
    }
    let gap = original.gap.or_else(|| {
        original
            .attachments
            .is_empty()
            .then(|| "media_expected_but_empty".into())
    });
    Ok(Inputs {
        media: IvyJson::object([
            ("coverage", pin(&original.coverage)),
            ("coverageContract", original.coverage_contract.into()),
            ("coverageHash", original.coverage_hash.into()),
            ("gap", optional(&gap)),
            ("attachments", IvyJson::Array(attachments)),
        ]),
        inputs,
    })
}
