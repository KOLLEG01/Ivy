//! UI publication policy shared with private asset storage.
use ivy_contracts::{IvyError, IvyResult, JsString};

pub fn asset_path(path: &JsString) -> IvyResult<&JsString> {
    let valid = !path.0.is_empty()
        && path.0.len() <= 1024
        && char::decode_utf16(path.0.iter().copied()).all(|c| c.is_ok())
        && *path == path.normalize_nfc()
        && !path.0.iter().any(|c| {
            *c <= 31
                || *c == 127
                || [b'\\', b'%', b'?', b'#', b':'].contains(&(*c as u8)) && *c < 128
        })
        && path.0.split(|c| *c == b'/' as u16).all(|part| {
            !part.is_empty() && part != [b'.' as u16] && part != [b'.' as u16, b'.' as u16]
        });
    if valid {
        Ok(path)
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Asset path must be a normalized relative path.",
        ))
    }
}
