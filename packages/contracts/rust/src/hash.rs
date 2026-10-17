use crate::{IvyJson, IvyResult, json::canonical, limits::MANAGEMENT_FRAME_BYTES};
use sha2::{Digest, Sha256};
pub fn digest(bytes: impl AsRef<[u8]>) -> String {
    let mut result = String::from("sha256:");
    for byte in Sha256::digest(bytes.as_ref()) {
        use std::fmt::Write;
        write!(result, "{byte:02x}").unwrap();
    }
    result
}
pub fn tool_definition_hash(definition: &IvyJson) -> IvyResult<String> {
    let IvyJson::Object(pairs) = definition else {
        return Err(crate::IvyError::new(
            "invalid_arguments",
            "Expected a tool definition.",
        ));
    };
    hash_json(&IvyJson::object(
        pairs
            .iter()
            .filter(|(k, _)| k != &crate::JsString::from("discovery"))
            .cloned(),
    ))
}
pub fn hash_json(value: &IvyJson) -> IvyResult<String> {
    Ok(digest(canonical(value, MANAGEMENT_FRAME_BYTES)?))
}
