//! Browser installation assets and authenticated console-file responses.
use crate::{
    http_response::{self, Response},
    store::sql_error,
    uis::asset_path,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, json::stringify, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::{native_path, paths::realpath};
use std::path::PathBuf;

pub trait AssetResponse: Response {
    fn end_bytes(&mut self, bytes: &[u8]) -> IvyResult<()>;
}
pub const WORKER: &[u8] = include_bytes!("../assets/browser-app-worker.js");
const MCP_ICON: &[u8] = include_bytes!("../assets/mcp-icon.svg");
const MCP_DEV_ICON: &[u8] = include_bytes!("../assets/mcp-dev-icon.svg");
fn joined(root: &JsString, suffix: &str) -> JsString {
    let mut value = root.clone();
    value.0.extend(suffix.encode_utf16());
    value
}
pub fn manifest(base_path: &JsString) -> IvyJson {
    let root = joined(base_path, "/");
    IvyJson::object([
        ("id", IvyJson::String(root.clone())),
        ("name", "Ivy".into()),
        ("short_name", "Ivy".into()),
        ("description", "Your Ivy workspace".into()),
        ("start_url", IvyJson::String(root.clone())),
        ("scope", IvyJson::String(root.clone())),
        ("display", "standalone".into()),
        ("background_color", "#ffffff".into()),
        ("theme_color", "#ffffff".into()),
        (
            "icons",
            IvyJson::Array(
                [192, 512]
                    .into_iter()
                    .map(|size| {
                        IvyJson::object([
                            (
                                "src",
                                IvyJson::String(joined(&root, &format!("app/icon-{size}.png"))),
                            ),
                            ("sizes", format!("{size}x{size}").into()),
                            ("type", "image/png".into()),
                            ("purpose", "any maskable".into()),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
pub struct Assets {
    pub console_root: Option<PathBuf>,
}
impl Assets {
    /// The caller already performed common request/path admission. These routes
    /// occur before maintenance and general credential authentication.
    pub async fn public(
        &self,
        method: Option<&JsString>,
        path: &JsString,
        base_path: &JsString,
        response: &mut impl AssetResponse,
    ) -> IvyResult<bool> {
        if method != Some(&JsString::from("GET")) {
            return Ok(false);
        }
        if path == &JsString::from("/app/manifest.webmanifest") {
            response.write_head(
                200,
                IvyJson::object([("Content-Type", "application/manifest+json".into())]),
            )?;
            response.end(stringify(&manifest(base_path), usize::MAX)?)?;
            return Ok(true);
        }
        if path == &JsString::from("/app-worker.js") {
            response.write_head(
                200,
                IvyJson::object([
                    ("Content-Type", "text/javascript; charset=utf-8".into()),
                    (
                        "Service-Worker-Allowed",
                        IvyJson::String(joined(base_path, "/")),
                    ),
                ]),
            )?;
            response.end_bytes(WORKER)?;
            return Ok(true);
        }
        if [180, 192, 512]
            .into_iter()
            .any(|size| path == &JsString::from(format!("/app/icon-{size}.png")))
        {
            let root = self
                .console_root
                .as_ref()
                .filter(|root| !root.as_os_str().is_empty())
                .ok_or_else(|| IvyError::new("not_found", "App icons are not installed."))?;
            let file = root
                .join("icons")
                .join(String::from_utf16_lossy(&path.0[5..]));
            let bytes = tokio::fs::read(file).await.map_err(sql_error)?;
            response.write_head(
                200,
                IvyJson::object([
                    ("Content-Type", "image/png".into()),
                    ("Cache-Control", "public, max-age=86400".into()),
                ]),
            )?;
            response.end_bytes(&bytes)?;
            return Ok(true);
        }
        let icon = if path == &JsString::from("/mcp-icon.svg") {
            Some(MCP_ICON)
        } else if path == &JsString::from("/mcp-dev-icon.svg") {
            Some(MCP_DEV_ICON)
        } else {
            None
        };
        if let Some(icon) = icon {
            response.set_header("Cache-Control", &"public, max-age=86400".into())?;
            response.write_head(
                200,
                IvyJson::object([("Content-Type", "image/svg+xml; charset=utf-8".into())]),
            )?;
            response.end_bytes(icon)?;
            return Ok(true);
        }
        Ok(false)
    }
    /// Caller reached this final static route after the source's maintenance,
    /// authentication and operation routing. This does not grant anonymous access.
    pub async fn console(
        &self,
        method: Option<&JsString>,
        pathname: &JsString,
        path: &JsString,
        base_path: &JsString,
        response: &mut impl AssetResponse,
    ) -> IvyResult<bool> {
        if method != Some(&JsString::from("GET"))
            || !(path == &JsString::from("/") || path.0.starts_with(&JsString::from("/console/").0))
        {
            return Ok(false);
        }
        let root = self
            .console_root
            .as_ref()
            .filter(|root| !root.as_os_str().is_empty())
            .ok_or_else(|| {
                IvyError::new(
                    "service_unavailable",
                    "Hive Console artifact has not been built.",
                )
            })?;
        if (path == &JsString::from("/") && pathname != &joined(base_path, "/"))
            || path == &JsString::from("/console/index.html")
        {
            http_response::redirect(response, &joined(base_path, "/"))?;
            return Ok(true);
        }
        let relative = if path == &JsString::from("/") {
            JsString::from("index.html")
        } else {
            decode_relative(&JsString(path.0[9..].to_vec()))?
        };
        asset_path(&relative)?;
        let root = realpath(root).map_err(sql_error)?;
        let file =
            realpath(&root.join(native_path::to_native(&relative))).map_err(|_| not_found())?;
        let mut prefix = root.as_os_str().to_os_string();
        prefix.push(std::path::MAIN_SEPARATOR_STR);
        if !file
            .as_os_str()
            .as_encoded_bytes()
            .starts_with(prefix.as_encoded_bytes())
        {
            return Err(not_found());
        }
        let info = tokio::fs::metadata(&file).await.map_err(sql_error)?;
        if !info.is_file() || info.len() > MANAGEMENT_FRAME_BYTES as u64 {
            return Err(not_found());
        }
        let path = file.as_os_str().as_encoded_bytes();
        let mime = [
            (".html", "text/html"),
            (".js", "text/javascript"),
            (".css", "text/css"),
            (".woff2", "font/woff2"),
        ]
        .into_iter()
        .find(|(suffix, _)| path.ends_with(suffix.as_bytes()))
        .map_or("application/octet-stream", |(_, mime)| mime);
        response.write_head(200, IvyJson::object([("Content-Type", mime.into())]))?;
        response.end_bytes(&tokio::fs::read(&file).await.map_err(sql_error)?)?;
        Ok(true)
    }
}
fn not_found() -> IvyError {
    IvyError::new("not_found", "Console asset not found.")
}
/// Decode a percent-serialized pathname component strictly before asset-path
/// validation. A literal '+' stays literal; malformed UTF-8 is rejected.
pub fn decode_relative(value: &JsString) -> IvyResult<JsString> {
    let source = String::from_utf16_lossy(&value.0);
    let input = source.as_bytes();
    let mut bytes = Vec::with_capacity(input.len());
    let mut at = 0;
    while at < input.len() {
        if input[at] == b'%' {
            let digit = |b: u8| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                b'A'..=b'F' => Some(b - b'A' + 10),
                _ => None,
            };
            let a = input
                .get(at + 1)
                .copied()
                .and_then(digit)
                .ok_or_else(IvyError::internal)?;
            let b = input
                .get(at + 2)
                .copied()
                .and_then(digit)
                .ok_or_else(IvyError::internal)?;
            bytes.push(a * 16 + b);
            at += 3;
        } else {
            bytes.push(input[at]);
            at += 1;
        }
    }
    String::from_utf8(bytes)
        .map(Into::into)
        .map_err(|_| IvyError::internal())
}
