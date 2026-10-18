//! Retained WhatsApp identities use ordinary JSON.stringify, not Ivy canonical
//! JSON. Changing the serializer would change message IDs across a restart.
use super::output_chunks::output_chunks;
use ivy_contracts::{IvyJson, IvyResult, JsString, hash::digest, json::stringify};

pub fn identity(parts: &[IvyJson]) -> IvyResult<String> {
    let encoded = stringify(&IvyJson::Array(parts.to_vec()), usize::MAX)?;
    Ok(digest(encoded)["sha256:".len()..].to_owned())
}

pub fn output_id(key: &JsString, offset: usize) -> IvyResult<String> {
    let hash = identity(&[
        IvyJson::from("out"),
        IvyJson::String(key.clone()),
        IvyJson::Number(offset as f64),
    ])?;
    Ok(hash[..32].to_ascii_uppercase())
}

pub struct OutputPart {
    pub id: String,
    pub text: JsString,
}

pub fn output_parts(key: &JsString, text: &JsString) -> IvyResult<Vec<OutputPart>> {
    output_chunks(&text.0)
        .map(|chunk| {
            Ok(OutputPart {
                id: output_id(key, chunk.offset)?,
                text: JsString(chunk.text.to_vec()),
            })
        })
        .collect()
}
