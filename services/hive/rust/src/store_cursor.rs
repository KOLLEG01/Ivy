use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_contracts::hash::{hash_json, hmac_sha256, verify_hmac_sha256};

fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_cursor", message)
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

    /// Authenticate a cursor and its query identity. Each operation validates
    /// the expected shape of its decoded boundary.
    pub fn read_cursor(&self, identity: &IvyJson, cursor: &JsString) -> IvyResult<Option<IvyJson>> {
        let cursor = cursor.to_utf8().map_err(|_| invalid("Invalid cursor."))?;
        let (body, signature) = cursor
            .split_once('.')
            .ok_or_else(|| invalid("Invalid cursor."))?;
        require(
            !body.is_empty() && !signature.is_empty(),
            "invalid_cursor",
            "Invalid cursor.",
        )?;
        let tag = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| invalid("Invalid cursor."))?;
        require(
            verify_hmac_sha256(self.cursor_secret.as_bytes(), body.as_bytes(), &tag),
            "invalid_cursor",
            "Invalid cursor authentication.",
        )?;
        let decoded = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| invalid("Invalid cursor."))?;
        let text = std::str::from_utf8(&decoded).map_err(|_| invalid("Invalid cursor."))?;
        let data = parse(text, usize::MAX).map_err(|_| invalid("Invalid cursor."))?;
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
