use crate::JsString;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub(crate) fn authorization(credential: &JsString) -> Result<HeaderValue, ()> {
    // Node's HTTP header check operates on UTF-16 code units, then writes
    // accepted strings as Latin-1. DEL, other controls and units >255 fail.
    if credential
        .0
        .iter()
        .any(|unit| !matches!(*unit, 9 | 32..=126 | 128..=255))
    {
        return Err(());
    }
    let mut bytes = b"Bearer ".to_vec();
    bytes.extend(credential.0.iter().map(|unit| *unit as u8));
    HeaderValue::from_bytes(&bytes).map_err(|_| ())
}
