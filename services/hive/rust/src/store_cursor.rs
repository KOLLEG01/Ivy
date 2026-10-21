use super::*;
use base64::{
    Engine,
    alphabet::URL_SAFE,
    engine::{
        DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose::URL_SAFE_NO_PAD,
    },
};
use ivy_contracts::hash::{hash_json, hmac_sha256, verify_hmac_sha256};

fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_cursor", message)
}

// Buffer.from(base64url) ignores an incomplete final sextet and unused bits.
// The cursor syntax check, unlike Buffer, first excludes padding/other alphabets.
fn decode(value: &str) -> IvyResult<Vec<u8>> {
    let value = if value.len() % 4 == 1 {
        &value[..value.len() - 1]
    } else {
        value
    };
    GeneralPurpose::new(
        &URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    )
    .decode(value)
    .map_err(|_| invalid("Invalid cursor."))
}

impl Store {
    pub fn cursor(&self, identity: &IvyJson, boundary: &IvyJson) -> IvyResult<String> {
        let data = IvyJson::object([
            ("query", hash_json(identity)?.into()),
            ("boundary", boundary.clone()),
        ]);
        let body = URL_SAFE_NO_PAD.encode(canonical(&data, MANAGEMENT_FRAME_BYTES)?);
        let tag = hmac_sha256(self.cursor_secret.as_bytes(), body.as_bytes());
        Ok(format!("{body}.{}", URL_SAFE_NO_PAD.encode(tag)))
    }

    /// None retains JSON.parse's missing boundary (JavaScript undefined).
    /// Query/catalog callers apply their original boundary/coercion rules.
    pub fn read_cursor(&self, identity: &IvyJson, cursor: &JsString) -> IvyResult<Option<IvyJson>> {
        let cursor = cursor.to_utf8().map_err(|_| invalid("Invalid cursor."))?;
        let mut parts = cursor.split('.');
        let body = parts.next().unwrap_or("");
        let signature = parts.next().unwrap_or("");
        let extra = parts.next().unwrap_or("");
        let alphabet = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        };
        require(
            alphabet(body) && alphabet(signature) && extra.is_empty(),
            "invalid_cursor",
            "Invalid cursor.",
        )?;
        let tag = decode(signature)?;
        require(
            verify_hmac_sha256(self.cursor_secret.as_bytes(), body.as_bytes(), &tag),
            "invalid_cursor",
            "Invalid cursor authentication.",
        )?;
        let decoded = decode(body)?;
        let text = String::from_utf8_lossy(&decoded);
        let data = parse(&text, usize::MAX).map_err(|_| invalid("Invalid cursor."))?;
        require(!data.is_null(), "invalid_cursor", "Invalid cursor.")?;
        let hash = hash_json(identity)?;
        require(
            data.get("query") == Some(&IvyJson::from(hash)),
            "invalid_cursor",
            "Cursor belongs to a different query.",
        )?;
        Ok(data.get("boundary").cloned())
    }
}
