//! The public image route permits only a provider's token-checked dashboard PNG.
use crate::http_static::AssetResponse;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as J, IvyResult, JsString,
    hash::digest,
    json::{encode_uri_component, stringify},
};
use std::{collections::BTreeMap, future::Future};

fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}

fn query(value: &JsString) -> IvyResult<(JsString, J)> {
    let mut params = BTreeMap::new();
    for (name, value) in ivy_contracts::whatwg_url::parse_form(value) {
        require(
            ["node", "id", "token", "width", "height", "colorMode"]
                .iter()
                .any(|allowed| name == JsString::from(*allowed))
                && params.insert(name, value).is_none(),
            "invalid_arguments",
            "Invalid image query parameters.",
        )?;
    }
    let get = |key: &str| params.get(&JsString::from(key));
    let node = get("node");
    let id = get("id");
    require(
        [node, id]
            .into_iter()
            .all(|text| text.is_some_and(|s| !s.0.is_empty() && s.0.len() <= 200)),
        "invalid_arguments",
        "Select a dashboard and its service node.",
    )?;
    let token = get("token");
    require(
        token.is_some_and(|s| !s.0.is_empty() && s.0.len() <= 128),
        "unauthenticated",
        "A dashboard image token is required.",
    )?;
    let mut args = J::object([
        ("id", J::String(id.unwrap().clone())),
        ("token", J::String(token.unwrap().clone())),
    ]);
    for key in ["width", "height"] {
        if let Some(value) = get(key) {
            let dimension = value
                .to_utf8()
                .ok()
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse::<u32>().ok())
                .ok_or_else(|| IvyError::new("invalid_arguments", "Invalid image dimensions."))?;
            args.insert(key, J::Number(f64::from(dimension)));
        }
    }
    if let Some(mode) = get("colorMode") {
        args.insert("colorMode", J::String(mode.clone()));
    }
    Ok((node.unwrap().clone(), args))
}

fn image(value: &J) -> IvyResult<(Vec<u8>, String, u32)> {
    let invalid = || {
        IvyError::new(
            "provider_contract_error",
            "Invalid dashboard image response.",
        )
    };
    let data = value
        .get("data")
        .and_then(J::as_js_string)
        .ok_or_else(invalid)?;
    let metadata = value
        .get("metadata")
        .filter(|v| v.as_object().is_some())
        .ok_or_else(invalid)?;
    require(
        value.get("mimeType") == Some(&"image/png".into())
            && value.get("encoding") == Some(&"base64".into())
            && data.0.len() <= 12 * 1024 * 1024,
        "provider_contract_error",
        "Invalid dashboard image response.",
    )?;
    let seconds = metadata
        .get("refreshSeconds")
        .and_then(J::as_f64)
        .filter(|v| v.fract() == 0.0 && (*v == 0.0 || (5.0..=86400.0).contains(v)))
        .ok_or_else(invalid)? as u32;
    let metadata = encode_uri_component(&JsString::from(stringify(metadata, 1024 * 1024)?))?;
    require(
        metadata.len() <= 8192,
        "provider_contract_error",
        "Dashboard metadata is too large.",
    )?;
    let png = STANDARD
        .decode(data.to_utf8().map_err(|_| invalid())?)
        .map_err(|_| invalid())?;
    require(
        (33..=8 * 1024 * 1024).contains(&png.len()) && png.starts_with(b"\x89PNG\r\n\x1a\n"),
        "provider_contract_error",
        "Invalid dashboard PNG.",
    )?;
    Ok((png, metadata, seconds))
}

pub async fn handle<F, Work>(
    method: &str,
    search: &JsString,
    if_none_match: Option<&str>,
    invoke: F,
    response: &mut impl AssetResponse,
) -> IvyResult<()>
where
    F: FnOnce(JsString, J) -> Work,
    Work: Future<Output = IvyResult<J>>,
{
    for (name, value) in [
        ("Cache-Control", "private, no-cache"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Content-Type-Options", "nosniff"),
        ("Access-Control-Allow-Origin", "*"),
        (
            "Access-Control-Expose-Headers",
            "ETag, X-Dashboard-Metadata, X-Dashboard-Refresh-Seconds",
        ),
    ] {
        response.set_header(name, &value.into())?;
    }
    if !["GET", "HEAD"].contains(&method) {
        response.write_head(405, J::object([("Allow", "GET, HEAD".into())]))?;
        return response.end_empty();
    }
    let result = async {
        let (node, args) = query(search)?;
        let (png, metadata, seconds) = image(&invoke(node, args).await?)?;
        let etag = format!("\"{}\"", &digest(&png)[7..]);
        response.set_header("X-Dashboard-Metadata", &metadata.into())?;
        response.set_header("X-Dashboard-Refresh-Seconds", &seconds.to_string().into())?;
        response.set_header("ETag", &etag.as_str().into())?;
        if if_none_match.is_some_and(|values| values.split(',').any(|value| value.trim() == etag)) {
            response.write_head(304, J::object([] as [(&str, J); 0]))?;
            return response.end_empty();
        }
        response.write_head(
            200,
            J::object([
                ("Content-Type", "image/png".into()),
                ("Content-Length", png.len().to_string().into()),
            ]),
        )?;
        if method == "HEAD" {
            response.end_empty()
        } else {
            response.end_bytes(&png)
        }
    }
    .await;
    if let Err(error) = result {
        if response.destroyed() {
            return Ok(());
        }
        let status = match error.code.as_str() {
            "unauthenticated" | "forbidden" => 401,
            "not_found" | "target_conflict" => 404,
            "invalid_arguments" => 400,
            _ => 503,
        };
        response.set_header("Cache-Control", &"no-store".into())?;
        if status == 503 {
            response.set_header("Retry-After", &"5".into())?;
        }
        response.write_head(
            status,
            J::object([("Content-Type", "application/json".into())]),
        )?;
        if method == "HEAD" {
            response.end_empty()
        } else {
            response.end(stringify(
                &J::object([(
                    "error",
                    if status == 503 {
                        "image_unavailable".into()
                    } else {
                        error.code.into()
                    },
                )]),
                1024,
            )?)
        }
    } else {
        Ok(())
    }
}
