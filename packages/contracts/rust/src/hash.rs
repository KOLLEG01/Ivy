use crate::{IvyJson, IvyResult, json::{canonical, canonical_object}, limits::MANAGEMENT_FRAME_BYTES};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

fn keyed_sha256(key: &[u8], message: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac
}

/// Sign exact bytes. Text secrets must be passed as their UTF-8 bytes, even when
/// they look like hexadecimal; this function performs no decoding or normalization.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    keyed_sha256(key, message).finalize().into_bytes().into()
}

/// Verify a complete SHA-256 tag with constant-time equality for equal lengths.
/// Wrong lengths return false; truncated tags are never accepted.
pub fn verify_hmac_sha256(key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    keyed_sha256(key, message).verify_slice(signature).is_ok()
}

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
    Ok(digest(canonical_object(
        pairs.iter().filter(|(key, _)| !key.eq_str("discovery"))
            .map(|(key, value)| (key, value)),
        MANAGEMENT_FRAME_BYTES,
    )?))
}
pub fn hash_json(value: &IvyJson) -> IvyResult<String> {
    Ok(digest(canonical(value, MANAGEMENT_FRAME_BYTES)?))
}
