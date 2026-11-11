//! Canonical binary wire content without full UTF-8 or re-encoded validation copies.
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{JsString, hash::digest_string};
use sha2::{Digest, Sha256};

pub const CHUNK_CHARS: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Invalid,
    TooLarge,
}

fn decoded_length(value: &JsString) -> Result<usize, DecodeError> {
    let length = value.0.len();
    let padding = value.0.iter().rev().take_while(|unit| **unit == 61).count();
    if length % 4 != 0 || padding > 2 {
        return Err(DecodeError::Invalid);
    }
    (length / 4 * 3)
        .checked_sub(padding)
        .ok_or(DecodeError::Invalid)
}

/// Visits bounded decoded slices. The visitor must not publish effects before this returns Ok.
/// Oversized input is still validated so malformed data retains its original error precedence.
pub fn visit(
    value: &JsString,
    maximum: usize,
    mut visitor: impl FnMut(&[u8]),
) -> Result<usize, DecodeError> {
    let length = decoded_length(value)?;
    let mut encoded = [0u8; CHUNK_CHARS];
    let mut decoded = [0u8; CHUNK_CHARS / 4 * 3];
    let mut written = 0;
    for (index, chunk) in value.0.chunks(CHUNK_CHARS).enumerate() {
        for (target, unit) in encoded.iter_mut().zip(chunk) {
            *target = u8::try_from(*unit).map_err(|_| DecodeError::Invalid)?;
        }
        let input = &encoded[..chunk.len()];
        if (index + 1) * CHUNK_CHARS < value.0.len() && input.contains(&b'=') {
            return Err(DecodeError::Invalid);
        }
        let count = STANDARD
            .decode_slice(input, &mut decoded)
            .map_err(|_| DecodeError::Invalid)?;
        written += count;
        if length <= maximum {
            visitor(&decoded[..count]);
        }
    }
    if written != length {
        return Err(DecodeError::Invalid);
    }
    if length > maximum {
        return Err(DecodeError::TooLarge);
    }
    Ok(length)
}

/// Materializes only the required decoded buffer. Validation never allocates a full text copy.
pub fn decode(value: &JsString, maximum: usize) -> Result<Vec<u8>, DecodeError> {
    let length = decoded_length(value)?;
    if length > maximum {
        visit(value, maximum, |_| {})?;
        return Err(DecodeError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(length);
    visit(value, maximum, |chunk| bytes.extend_from_slice(chunk))?;
    Ok(bytes)
}

/// An immutable admitted wire value and its compact byte identity; no decoded body is retained.
pub struct Base64Content<'a> {
    value: &'a JsString,
    byte_length: usize,
    content_hash: String,
}
impl<'a> Base64Content<'a> {
    pub fn inspect(value: &'a JsString, maximum: usize) -> Result<Self, DecodeError> {
        let mut hash = Sha256::new();
        let byte_length = visit(value, maximum, |chunk| hash.update(chunk))?;
        Ok(Self {
            value,
            byte_length,
            content_hash: digest_string(hash.finalize().into()),
        })
    }
    pub fn encoded(&self) -> &JsString {
        self.value
    }
    pub fn byte_length(&self) -> usize {
        self.byte_length
    }
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }
}
