use crate::{IvyJson, IvyResult, JsString, json::{canonical_into, canonical_object}, limits::MANAGEMENT_FRAME_BYTES};
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
    digest_string(Sha256::digest(bytes.as_ref()).into())
}
fn digest_string(bytes: [u8; 32]) -> String {
    let mut result = String::from("sha256:");
    for byte in bytes {
        use std::fmt::Write;
        write!(result, "{byte:02x}").unwrap();
    }
    result
}

/// The encoder emits individual characters. Buffer them before updating SHA-256
/// so hashing needs neither a complete JSON string nor a digest call per byte.
struct HashWriter {
    digest: Sha256,
    buffer: [u8; 8192],
    used: usize,
}
impl HashWriter {
    fn new() -> Self {
        Self { digest: Sha256::new(), buffer: [0; 8192], used: 0 }
    }
    fn flush(&mut self) {
        self.digest.update(&self.buffer[..self.used]);
        self.used = 0;
    }
    fn finish(mut self) -> String {
        self.flush();
        digest_string(self.digest.finalize().into())
    }
}
impl std::fmt::Write for HashWriter {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if text.len() >= self.buffer.len() {
            self.flush();
            self.digest.update(text.as_bytes());
        } else {
            if self.used + text.len() > self.buffer.len() { self.flush(); }
            self.buffer[self.used..self.used + text.len()].copy_from_slice(text.as_bytes());
            self.used += text.len();
        }
        Ok(())
    }
}
pub fn tool_definition_hash(definition: &IvyJson) -> IvyResult<String> {
    let IvyJson::Object(pairs) = definition else {
        return Err(crate::IvyError::new(
            "invalid_arguments",
            "Expected a tool definition.",
        ));
    };
    hash_object(
        pairs.iter().filter(|(key, _)| !key.eq_str("discovery"))
            .map(|(key, value)| (key, value)),
    )
}
/// Hash a borrowed object projection with IvyJson::object's last-value rule.
pub fn hash_object<'a>(
    pairs: impl IntoIterator<Item = (&'a JsString, &'a IvyJson)>,
) -> IvyResult<String> {
    let mut writer = HashWriter::new();
    canonical_object(pairs, MANAGEMENT_FRAME_BYTES, &mut writer)?;
    Ok(writer.finish())
}
pub fn hash_json(value: &IvyJson) -> IvyResult<String> {
    let mut writer = HashWriter::new();
    canonical_into(value, MANAGEMENT_FRAME_BYTES, &mut writer)?;
    Ok(writer.finish())
}
/// Hash a generated array with one canonical budget and no retained record tree.
pub fn hash_array(records: impl IntoIterator<Item = IvyResult<IvyJson>>) -> IvyResult<String> {
    let mut writer = HashWriter::new();
    crate::json::canonical_array(records, MANAGEMENT_FRAME_BYTES, &mut writer)?;
    Ok(writer.finish())
}
/// Hash a fallible catalog stream without retaining every materialized record.
pub fn hash_object_with_array<'a>(
    pairs: impl IntoIterator<Item = (&'a JsString, &'a IvyJson)>,
    array_key: &'a JsString,
    records: impl IntoIterator<Item = IvyResult<IvyJson>>,
) -> IvyResult<String> {
    let mut writer = HashWriter::new();
    crate::json::canonical_object_with_array(pairs, array_key, records, MANAGEMENT_FRAME_BYTES, &mut writer)?;
    Ok(writer.finish())
}
