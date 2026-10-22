use crate::JsString;
use reqwest::header::HeaderValue;

/// Node fetch converts to ByteString, trims HTTP whitespace, then dispatches.
/// Keep this separate from the service/WebSocket header profile.
pub(crate) fn authorization(credential: &JsString) -> Result<Option<HeaderValue>, ()> {
    if credential.0.is_empty() {
        return Ok(None);
    }
    if credential.0.iter().any(|unit| *unit > 255) {
        return Err(());
    }
    let mut bytes = b"Bearer ".to_vec();
    bytes.extend(credential.0.iter().map(|unit| *unit as u8));
    while bytes
        .last()
        .is_some_and(|byte| matches!(*byte, 9 | 10 | 13 | 32))
    {
        bytes.pop();
    }
    HeaderValue::from_bytes(&bytes).map(Some).map_err(|_| ())
}
