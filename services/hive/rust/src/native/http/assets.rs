//! Package streams and published UI routes over the existing authoritative owners.
use super::{App, Body, Output, header};
use futures_util::TryStreamExt;
use http_body_util::{BodyExt, StreamBody};
use hyper::{body::Frame, header::HeaderMap};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, json::encode_uri_component, ui_route::RESERVED_UI_SLUGS,
};
use ivy_hive::{
    http_response::{self, Response},
    http_static::{AssetResponse, decode_relative},
    oauth::Parameters,
    server_rpc::Storage,
    worker_client::RequestOptions,
};
use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    time::{Instant, Sleep},
};
use tokio_util::io::{ReaderStream, StreamReader};

const UPLOAD_IDLE: Duration = Duration::from_secs(5 * 60);
struct IdleReader<R> {
    reader: R,
    deadline: Pin<Box<Sleep>>,
}
impl<R: AsyncRead + Unpin> AsyncRead for IdleReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buffer.filled().len();
        match Pin::new(&mut this.reader).poll_read(cx, buffer) {
            Poll::Ready(result) => {
                if buffer.filled().len() > before {
                    this.deadline.as_mut().reset(Instant::now() + UPLOAD_IDLE);
                }
                Poll::Ready(result)
            }
            Poll::Pending if this.deadline.as_mut().poll(cx).is_ready() => Poll::Ready(Err(
                io::Error::new(io::ErrorKind::TimedOut, "Package upload timed out."),
            )),
            Poll::Pending => Poll::Pending,
        }
    }
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn not_found() -> IvyError {
    IvyError::new("not_found", "UI asset path not found.")
}
fn package_registry(app: &App) -> IvyResult<&ivy_hive::packages::PackageRegistry> {
    app.server.packages.as_ref().ok_or_else(|| {
        IvyError::new(
            "service_unavailable",
            "Hive package storage is not configured.",
        )
    })
}
pub(super) async fn upload(
    app: &App,
    method: &JsString,
    path: &JsString,
    headers: &HeaderMap,
    body: &mut Body,
    output: &mut Output,
) -> IvyResult<bool> {
    let path = text(path);
    let Some(id) = path.strip_prefix("/api/v1/packages/uploads/").filter(|id| {
        id.len() == 36
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
    }) else {
        return Ok(false);
    };
    if method != &JsString::from("PUT") {
        return Ok(false);
    }
    let registry = package_registry(app)?;
    let headers = IvyJson::object(headers.iter().filter_map(|(key, value)| {
        value
            .to_str()
            .ok()
            .map(|value| (key.as_str(), value.into()))
    }));
    // Constructing the reader reads no bytes. The registry admits the one-time
    // grant before polling the stream, and owns publication and cleanup.
    let incoming = body.0.take().ok_or_else(IvyError::internal)?;
    let reader = StreamReader::new(incoming.into_data_stream().map_err(io::Error::other));
    let reader = IdleReader {
        reader,
        deadline: Box::pin(tokio::time::sleep(UPLOAD_IDLE)),
    };
    let result = registry.upload(&id.into(), &headers, reader).await?;
    http_response::json(output, 201, &result)?;
    Ok(true)
}
pub(super) async fn resolve_slug(
    app: &App,
    method: &JsString,
    path: &JsString,
) -> IvyResult<Option<JsString>> {
    if method != &JsString::from("GET") {
        return Ok(None);
    }
    let path = text(path);
    let slug = path.split('/').nth(1).unwrap_or_default();
    if slug.is_empty()
        || slug.len() > 64
        || !slug.as_bytes()[0].is_ascii_lowercase()
        || !slug
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || RESERVED_UI_SLUGS.contains(&slug)
    {
        return Ok(None);
    }
    let value = Storage::request(
        app.server.worker.as_ref(),
        IvyJson::object([("action", "ui.resolveSlug".into()), ("slug", slug.into())]),
        RequestOptions::default(),
    )
    .await?;
    if value.is_null() {
        Ok(None)
    } else {
        Ok(Some(
            value
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?,
        ))
    }
}
pub(super) async fn serve(
    app: &App,
    method: &JsString,
    path: &JsString,
    search: &JsString,
    short_ui: Option<&JsString>,
    headers: &HeaderMap,
    context: &IvyJson,
    output: &mut Output,
) -> IvyResult<bool> {
    if method != &JsString::from("GET") {
        return Ok(false);
    }
    let path = text(path);
    let query = Parameters::parse_form(search);
    if path == "/api/v1/packages/catalog" {
        let after = match query.get("after").filter(|v| !v.0.is_empty()) {
            Some(value) => value
                .to_utf8()
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v <= 9_007_199_254_740_991)
                .ok_or_else(|| {
                    IvyError::new(
                        "invalid_arguments",
                        "Package catalog revision must be a nonnegative integer.",
                    )
                })?,
            None => 0,
        };
        let snapshot = package_registry(app)?
            .catalog_snapshot(
                &IvyJson::Number(after as f64),
                query.get("include") == Some(&"all".into()),
            )
            .await?;
        let catalog = snapshot.required("catalog")?;
        let revision = catalog
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)?;
        output.set_header(
            "X-Ivy-Object-Id",
            snapshot
                .required("objectId")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
        )?;
        output.set_header(
            "X-Ivy-Object-Revision",
            &snapshot
                .required("objectRevision")?
                .as_f64()
                .ok_or_else(IvyError::internal)?
                .to_string()
                .into(),
        )?;
        output.set_header("ETag", &format!("\"{revision}\"").into())?;
        if after as f64 >= revision {
            output.write_head(204, IvyJson::object([] as [(&str, IvyJson); 0]))?;
            output.end_empty()?;
        } else {
            http_response::json(output, 200, catalog)?;
        }
        return Ok(true);
    }
    if let Some(tail) = path.strip_prefix("/api/v1/packages/") {
        let parts: Vec<_> = tail.split('/').collect();
        if parts.len() == 3 && parts[2] == "artifact" && component(parts[0]) && version(parts[1]) {
            let (file, entry) = package_registry(app)?
                .artifact(&parts[0].into(), &parts[1].into())
                .await?;
            let file = tokio::fs::File::open(file).await.map_err(super::failure)?;
            output.write_head(
                200,
                IvyJson::object([
                    ("Content-Type", "application/gzip".into()),
                    (
                        "Content-Length",
                        entry
                            .required("bytes")?
                            .as_f64()
                            .ok_or_else(IvyError::internal)?
                            .to_string()
                            .into(),
                    ),
                    (
                        "ETag",
                        format!("\"{}\"", entry.required("archiveHash")?.as_str()?).into(),
                    ),
                    (
                        "Cache-Control",
                        "private, max-age=31536000, immutable".into(),
                    ),
                ]),
            )?;
            output.stream =
                Some(StreamBody::new(ReaderStream::new(file).map_ok(Frame::data)).boxed_unsync());
            output.ended = true;
            return Ok(true);
        }
    }
    if short_ui.is_none() && !path.starts_with("/ui/") {
        return Ok(false);
    }
    let slug = path.split('/').nth(1).unwrap_or_default();
    if short_ui.is_some() && path == format!("/{slug}") {
        http_response::redirect(
            output,
            &format!(
                "{}/{slug}/{}",
                text(&app.server.base.base_path),
                text(search)
            )
            .into(),
        )?;
        return Ok(true);
    }
    let mut parts: Vec<JsString> = path
        .split('/')
        .skip(2)
        .map(|part| decode_relative(&part.into()))
        .collect::<IvyResult<_>>()?;
    if let Some(ui) = short_ui {
        parts.insert(0, ui.clone());
    }
    let ui = parts.first().ok_or_else(not_found)?;
    let historical = parts.len() >= 4 && parts[1] == JsString::from("releases");
    let current_entry = parts.len() == 2 && parts[1].0.is_empty();
    let current_asset =
        parts.len() >= 2 && !parts[1].0.is_empty() && parts[1] != JsString::from("releases");
    let joined = |start: usize| {
        parts[start..]
            .iter()
            .map(text)
            .collect::<Vec<_>>()
            .join("/")
    };
    if historical
        && joined(3) == "index.html"
        && header(headers, "sec-fetch-mode") == Some("navigate".into())
    {
        let mount = if short_ui.is_some() {
            slug.to_owned()
        } else {
            format!("ui/{}", encode_uri_component(ui).map_err(super::failure)?)
        };
        http_response::redirect(
            output,
            &format!(
                "{}/{mount}/{}",
                text(&app.server.base.base_path),
                text(search)
            )
            .into(),
        )?;
        return Ok(true);
    }
    if !(historical || current_entry || current_asset) {
        return Err(not_found());
    }
    let mut selection = IvyJson::object([
        ("context", context.clone()),
        ("uiId", IvyJson::String(ui.clone())),
    ]);
    selection.insert(
        "action",
        if historical {
            "asset"
        } else {
            "ui.currentAsset"
        }
        .into(),
    );
    if historical {
        selection.insert("releaseId", IvyJson::String(parts[2].clone()));
        selection.insert("path", joined(3).into());
    } else if !parts[1].0.is_empty() {
        selection.insert("path", joined(1).into());
    }
    let selected = Storage::request(
        app.server.worker.as_ref(),
        selection,
        RequestOptions::default(),
    )
    .await?;
    let asset = selected.required("asset")?;
    let asset_path = asset
        .required("path")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    let bytes = app
        .server
        .ui_files
        .read(
            ui,
            selected
                .required("releaseId")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
            asset,
        )
        .await?;
    output.set_header(
        "Content-Security-Policy",
        &ivy_hive::ui_security::content_policy(asset_path).into(),
    )?;
    if text(asset_path).ends_with(".sandbox.html") {
        output.headers.remove("x-frame-options");
    }
    let etag = format!("\"{}\"", asset.required("contentHash")?.as_str()?);
    output.set_header("ETag", &etag.clone().into())?;
    output.set_header("Vary", &"Cookie, Authorization".into())?;
    output.set_header(
        "Cache-Control",
        &(if historical {
            "private, max-age=31536000, immutable"
        } else {
            "private, no-cache"
        })
        .into(),
    )?;
    if header(headers, "if-none-match") == Some(etag.into()) {
        output.write_head(304, IvyJson::object([] as [(&str, IvyJson); 0]))?;
        output.end_empty()?;
    } else {
        output.write_head(
            200,
            IvyJson::object([
                ("Content-Type", asset.required("mediaType")?.clone()),
                ("Content-Length", bytes.len().to_string().into()),
            ]),
        )?;
        output.end_bytes(&bytes)?;
    }
    Ok(true)
}
fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
fn version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()))
}
