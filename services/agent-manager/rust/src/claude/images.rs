//! Multimodal projection from the pinned adapter. File failures retain its
//! visible unavailable-image text; malformed URI escapes remain errors.
use super::{
    error::{AdapterError, Result},
    value::*,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyJson as J, JsString};
use ivy_host_runtime::node_path;
pub fn extract(input: &J) -> Result<(JsString, Vec<J>)> {
    let mut texts = vec![];
    let mut images = vec![];
    for item in input.as_array().unwrap_or(&[]) {
        match field(item, "type").as_str().ok().as_deref() {
            Some("text") => {
                if let Some(s) = field(item, "text").as_js_string() {
                    texts.push(s.clone())
                }
            }
            Some("mention") => texts.push(concat(&[
                &"@".into(),
                &string(&coalesce(
                    field(item, "path"),
                    &coalesce(field(item, "name"), &"".into()),
                )),
            ])),
            Some("skill") => texts.push(concat(&[
                &"/".into(),
                &string(&coalesce(field(item, "name"), &"skill".into())),
            ])),
            Some("localImage") => {
                if let Some(path) = field(item, "path").as_js_string() {
                    let image = (|| {
                        let native = node_path::filesystem_path(path);
                        if std::fs::metadata(&native).ok()?.len() > 10 * 1024 * 1024 {
                            return None;
                        }
                        let bytes = std::fs::read(native).ok()?;
                        let lower = path.to_english_lowercase();
                        let lower = lower.to_utf8().ok()?;
                        let media = if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
                            "image/jpeg"
                        } else if lower.ends_with(".webp") {
                            "image/webp"
                        } else if lower.ends_with(".gif") {
                            "image/gif"
                        } else if lower.ends_with(".bmp") {
                            "image/bmp"
                        } else {
                            "image/png"
                        };
                        Some(J::object([
                            ("kind", "base64".into()),
                            ("mediaType", media.into()),
                            ("data", STANDARD.encode(bytes).into()),
                            ("displayPath", path.clone().into_json()),
                        ]))
                    })();
                    if let Some(image) = image {
                        images.push(image)
                    } else {
                        texts.push(concat(&[
                            &"[local image unavailable: ".into(),
                            path,
                            &"]".into(),
                        ]))
                    }
                }
            }
            Some("image") => {
                if let Some(url) = field(item, "url").as_js_string() {
                    if let Some(image) = parse_url(url)? {
                        images.push(image)
                    } else {
                        texts.push("[image]".into())
                    }
                }
            }
            _ => {}
        }
    }
    texts.retain(|s| !s.0.is_empty());
    Ok((join(&texts, "\n"), images))
}
fn parse_url(url: &JsString) -> Result<Option<J>> {
    if url.0.starts_with(&JsString::from("data:").0) {
        // Original regexp excludes line terminators from its final dot group.
        if url.0.iter().any(|u| [10, 13, 0x2028, 0x2029].contains(u)) {
            return Ok(None);
        }
        let Some(comma) = url.0.iter().position(|u| *u == 44) else {
            return Ok(None);
        };
        let metadata = &url.0[5..comma];
        let semi = metadata.iter().position(|u| *u == 59);
        let media = &metadata[..semi.unwrap_or(metadata.len())];
        if media.is_empty() {
            return Ok(None);
        }
        let attrs = semi.map(|n| JsString(metadata[n + 1..].to_vec()));
        if attrs.as_ref().is_some_and(|s| s.0.is_empty()) {
            return Ok(None);
        }
        let payload = JsString(url.0[comma + 1..].to_vec());
        let data = if attrs.is_some_and(|s| s.contains_ascii("base64")) {
            payload.into_json()
        } else {
            STANDARD
                .encode(String::from_utf16_lossy(&decode_uri(&payload)?.0).as_bytes())
                .into()
        };
        let suffix = if url.0.len() > 64 { "..." } else { "" };
        return Ok(Some(J::object([
            ("kind", "base64".into()),
            ("mediaType", JsString(media.to_vec()).into_json()),
            ("data", data),
            (
                "displayPath",
                concat(&[&slice(url, 0, 64), &suffix.into()]).into_json(),
            ),
        ])));
    }
    if ["http://", "https://", "file://"]
        .iter()
        .any(|prefix| url.0.starts_with(&JsString::from(*prefix).0))
    {
        return Ok(Some(J::object([
            ("kind", "url".into()),
            ("mediaType", "image/*".into()),
            ("data", url.clone().into_json()),
            ("displayPath", url.clone().into_json()),
        ])));
    }
    Ok(None)
}
fn decode_uri(value: &JsString) -> Result<JsString> {
    let mut out = vec![];
    let mut i = 0;
    while i < value.0.len() {
        if value.0[i] != 37 {
            out.push(value.0[i]);
            i += 1;
            continue;
        }
        let mut bytes = vec![];
        while i < value.0.len() && value.0[i] == 37 {
            if i + 2 >= value.0.len() {
                return Err(AdapterError::typed("URIError", "URI malformed"));
            }
            let hex = |u: u16| char::from_u32(u as u32).and_then(|c| c.to_digit(16));
            let a = hex(value.0[i + 1])
                .ok_or_else(|| AdapterError::typed("URIError", "URI malformed"))?;
            let b = hex(value.0[i + 2])
                .ok_or_else(|| AdapterError::typed("URIError", "URI malformed"))?;
            bytes.push((a * 16 + b) as u8);
            i += 3;
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| AdapterError::typed("URIError", "URI malformed"))?;
        out.extend(text.encode_utf16());
    }
    Ok(JsString(out))
}
