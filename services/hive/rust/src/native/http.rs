mod assets;
use super::{App, failure};
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request as HttpRequest, Response as HttpResponse,
    body::{Bytes, Frame, Incoming},
    header::{HeaderMap, HeaderName, HeaderValue},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_hive::{
    http_body::BodyStream,
    http_response::{self, Response},
    http_static::AssetResponse,
    mcp_dispatch, mcp_request,
    native_runtime::NativeRuntime,
    server_http, server_http_rpc, server_mcp,
};
use std::{
    convert::Infallible,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{net::TcpStream, sync::watch};

#[derive(Default)]
struct Output {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
    stream: Option<UnsyncBoxBody<Bytes, io::Error>>,
    ended: bool,
    destroyed: bool,
}
struct Outgoing {
    inner: UnsyncBoxBody<Bytes, io::Error>,
    _lease: Option<ivy_hive::http_common::Request>,
}
impl hyper::body::Body for Outgoing {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}
impl Response for Output {
    fn writable_ended(&self) -> bool {
        self.ended
    }
    fn destroyed(&self) -> bool {
        self.destroyed
    }
    fn headers_sent(&self) -> bool {
        self.status != 0
    }
    fn set_header(&mut self, name: &str, value: &JsString) -> IvyResult<()> {
        self.headers.insert(
            HeaderName::from_bytes(name.as_bytes()).map_err(failure)?,
            HeaderValue::from_str(&String::from_utf16_lossy(&value.0)).map_err(failure)?,
        );
        Ok(())
    }
    fn write_head(&mut self, status: u16, headers: IvyJson) -> IvyResult<()> {
        self.status = status;
        if let IvyJson::Object(entries) = &headers {
            for (name, value) in entries {
                self.set_header(
                    &String::from_utf16_lossy(&name.0),
                    value.as_js_string().ok_or_else(IvyError::internal)?,
                )?;
            }
        }
        Ok(())
    }
    fn end(&mut self, text: String) -> IvyResult<()> {
        self.body.extend_from_slice(text.as_bytes());
        self.ended = true;
        Ok(())
    }
    fn end_empty(&mut self) -> IvyResult<()> {
        self.ended = true;
        Ok(())
    }
    fn destroy(&mut self) -> IvyResult<()> {
        self.destroyed = true;
        Ok(())
    }
}
impl AssetResponse for Output {
    fn end_bytes(&mut self, bytes: &[u8]) -> IvyResult<()> {
        self.body.extend_from_slice(bytes);
        self.ended = true;
        Ok(())
    }
}
impl Output {
    fn finish(self, lease: Option<ivy_hive::http_common::Request>) -> HttpResponse<Outgoing> {
        let inner = if self.destroyed {
            StreamBody::new(futures_util::stream::once(async {
                Err::<Frame<Bytes>, _>(io::Error::other("HTTP response was terminated."))
            }))
            .boxed_unsync()
        } else {
            self.stream.unwrap_or_else(|| {
                Full::new(Bytes::from(self.body))
                    .map_err(|never| match never {})
                    .boxed_unsync()
            })
        };
        let mut response = HttpResponse::new(Outgoing {
            inner,
            _lease: lease,
        });
        *response.status_mut() =
            hyper::StatusCode::from_u16(if self.status == 0 { 200 } else { self.status })
                .unwrap_or(hyper::StatusCode::INTERNAL_SERVER_ERROR);
        *response.headers_mut() = self.headers;
        response
    }
}
struct Body(Option<Incoming>);
struct ConnectionGuard(server_http::Connection);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}
struct PendingUpgrade(Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>);
impl Drop for PendingUpgrade {
    fn drop(&mut self) {
        if let Some(task) = self.0.lock().unwrap().take() {
            task.abort();
        }
    }
}
struct UpgradeTask(tokio::task::JoinHandle<()>);
impl Drop for UpgradeTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl BodyStream for Body {
    async fn next_chunk(&mut self) -> IvyResult<Option<Vec<u8>>> {
        let Some(body) = self.0.as_mut() else {
            return Ok(None);
        };
        while let Some(frame) = body.frame().await {
            if let Ok(bytes) = frame.map_err(failure)?.into_data() {
                return Ok(Some(bytes.to_vec()));
            }
        }
        Ok(None)
    }
    async fn cancel(&mut self) -> IvyResult<()> {
        self.0.take();
        Ok(())
    }
}
fn header(headers: &HeaderMap, name: &str) -> Option<JsString> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(Into::into)
}
fn authentication(headers: &HeaderMap) -> ivy_hive::http_auth::Headers {
    ivy_hive::http_auth::Headers {
        authorization: header(headers, "authorization"),
        cookie: header(headers, "cookie"),
        origin: header(headers, "origin").map(IvyJson::String),
        forwarded_client_ip: header(headers, "x-forwarded-for").map(IvyJson::String),
    }
}
pub(super) async fn connection(
    app: Arc<App>,
    stream: TcpStream,
    remote: SocketAddr,
    mut stopping: watch::Receiver<bool>,
) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let connection = app.server.http.connection(
        NEXT.fetch_add(1, Ordering::Relaxed),
        Some(remote.ip().to_string().into()),
    );
    let _connection_guard = ConnectionGuard(connection.clone());
    let socket = connection.clone();
    let shutdown = stopping.clone();
    let upgrade = Arc::new(Mutex::new(None));
    let _upgrade_guard = PendingUpgrade(upgrade.clone());
    let active_upgrade = upgrade.clone();
    let service = hyper::service::service_fn(move |request| {
        let app = app.clone();
        let socket = socket.clone();
        let shutdown = shutdown.clone();
        let upgrade = active_upgrade.clone();
        async move { Ok::<_, Infallible>(handle(app, socket, request, shutdown, upgrade).await) }
    });
    let mut serving = hyper::server::conn::http1::Builder::new()
        .max_headers(2000)
        .max_buf_size(16 * 1024)
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Duration::from_secs(60))
        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
        .with_upgrades();
    tokio::select! {
        result = &mut serving => { if let Err(error) = result { eprintln!("Hive HTTP: {error}"); } },
        _ = stopping.changed() => { Pin::new(&mut serving).graceful_shutdown(); let _ = tokio::time::timeout(Duration::from_secs(10), &mut serving).await; },
    }
    if let Some(parts) = serving.into_parts() {
        // After flushing and shutting down writes, discard trailing request data.
        // Closing TCP with unread upload bytes can reset an already sent 4xx.
        // A fixed deadline bounds clients that keep sending after rejection.
        let mut stream = parts.io.into_inner();
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            tokio::io::copy(&mut stream, &mut tokio::io::sink()),
        )
        .await;
    }
    // An upgraded socket remains owned by this connection through its actual
    // completion, including the listener's graceful shutdown.
    let upgraded = upgrade.lock().unwrap().take();
    if let Some(upgraded) = upgraded {
        let mut task = UpgradeTask(upgraded);
        let _ = (&mut task.0).await;
    }
}
async fn handle(
    app: Arc<App>,
    connection: server_http::Connection,
    mut incoming: HttpRequest<Incoming>,
    stopping: watch::Receiver<bool>,
    upgrade: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
) -> HttpResponse<Outgoing> {
    let method: JsString = incoming.method().as_str().into();
    let uri: JsString = incoming.uri().to_string().into();
    let headers = incoming.headers().clone();
    let auth = authentication(&headers);
    let content_type = header(&headers, "content-type").map(IvyJson::String);
    let content_length = header(&headers, "content-length").map(IvyJson::String);
    let mut output = Output::default();
    let mut rpc_id = IvyJson::Null;
    let mut path = JsString::default();
    let mut request = match connection.begin(&mut output) {
        Ok(request) => request,
        Err(error) => {
            let _ = http_common_error(&app, &path, rpc_id, &error, &mut output);
            return output.finish(None);
        }
    };
    let result = async {
        request.admit(&auth)?;
        let route = request.route(Some(&method), Some(&uri))?;
        path = route.path.clone();
        let buffers = app.body_buffers.clone();
        if path == JsString::from("/api/v1/dashboards/image.png") {
            request.admit_public_route(Some(&method), &path)?;
            return ivy_hive::dashboard_image::handle(
                incoming.method().as_str(),
                &incoming_search(&uri).into(),
                headers
                    .get("if-none-match")
                    .and_then(|value| value.to_str().ok()),
                |node, args| {
                    let peers = app.server.network.peers.clone();
                    async move {
                        let context = IvyJson::object([
                            ("transport", "http".into()),
                            ("principalId", "hive.dashboard-image".into()),
                            ("credentialDigest", "dashboard-image".into()),
                        ]);
                        peers.dashboard_image(&context, &node, args)?.await
                    }
                },
                &mut output,
            )
            .await;
        }
        if method == JsString::from("GET")
            && path == JsString::from("/ws")
            && headers
                .get("upgrade")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        {
            request.admit_public_route(Some(&method), &path)?;
            let context = request
                .authenticate(ivy_hive::http_auth::Request {
                    method: Some(method.clone()),
                    headers: auth.clone(),
                    transport: "ws".into(),
                    resource: None,
                })
                .await?;
            let mut handshake = HttpRequest::builder()
                .method(incoming.method())
                .version(incoming.version())
                .uri(incoming.uri())
                .body(())
                .map_err(failure)?;
            *handshake.headers_mut() = headers.clone();
            let handshake =
                tokio_tungstenite::tungstenite::handshake::server::create_response(&handshake)
                    .map_err(|_| {
                        IvyError::new("invalid_arguments", "Invalid WebSocket handshake.")
                    })?;
            output.status = handshake.status().as_u16();
            for (name, value) in handshake.headers() {
                output.headers.insert(name.clone(), value.clone());
            }
            let upgraded = hyper::upgrade::on(&mut incoming);
            *upgrade.lock().unwrap() = Some(tokio::task::spawn_local(super::websocket::run(
                app.clone(),
                upgraded,
                context,
                stopping,
            )));
            return Ok(());
        }
        let mut body = Body(Some(incoming.into_body()));
        let oauth = ivy_hive::oauth_http::Handler {
            connection: connection.authentication(),
            oauth: &app.server.oauth,
            buffers: &buffers,
        };
        if oauth
            .try_handle(
                Some(&method),
                &route.pathname,
                ivy_hive::oauth_http::Headers {
                    authentication: &auth,
                    content_type: content_type.as_ref(),
                    content_length: content_length.as_ref(),
                },
                &mut body,
                None,
                &mut output,
            )
            .await?
        {
            return Ok(());
        }
        if method == JsString::from("GET") && path == JsString::from("/health/live") {
            return http_response::json(
                &mut output,
                200,
                &IvyJson::object([("live", true.into())]),
            );
        }
        if method == JsString::from("GET") && path == JsString::from("/health/ready") {
            let ready = !app.reset_bootstrap && app.storage_ready(1000).await;
            return http_response::json(
                &mut output,
                if ready { 200 } else { 503 },
                &IvyJson::object([("ready", ready.into())]),
            );
        }
        if assets::upload(&app, &method, &path, &headers, &mut body, &mut output).await? {
            return Ok(());
        }
        if app
            .assets
            .public(
                Some(&method),
                &path,
                &app.server.base.base_path,
                &mut output,
            )
            .await?
        {
            return Ok(());
        }
        request.admit_public_route(Some(&method), &path)?;
        let search: JsString = incoming_search(&uri).into();
        if method == JsString::from("GET") && path == JsString::from("/login") {
            return ivy_hive::auth_pages::login_query_response(
                connection.authentication(),
                &search,
                &mut output,
            );
        }
        if method == JsString::from("GET") && path == JsString::from("/login/theme.js") {
            return ivy_hive::auth_pages::theme_response(&mut output);
        }
        if method == JsString::from("GET") && path == JsString::from("/oauth/authorize") {
            return oauth.authorize_query(&auth, &search, &mut output);
        }
        if method == JsString::from("POST") && path == JsString::from("/login") {
            return ivy_hive::browser_session::Session {
                connection: connection.authentication(),
                worker: app.server.worker.as_ref(),
            }
            .login_post(
                ivy_hive::browser_session::LoginHeaders {
                    authentication: &auth,
                    content_type: content_type.as_ref(),
                    content_length: content_length.as_ref(),
                },
                &buffers,
                &mut body,
                None,
                &mut ivy_hive::auth_pages::SessionResponse {
                    connection: connection.authentication(),
                    response: &mut output,
                },
            )
            .await;
        }
        if server_mcp::handle(
            &mut request,
            &app.server.mcp,
            Some(&method),
            &path,
            &auth,
            server_http_rpc::Headers {
                content_type: content_type.as_ref(),
                content_length: content_length.as_ref(),
            },
            &mut rpc_id,
            &mut body,
            &mut output,
            &mut mcp_dispatch::Factory {
                headers: mcp_dispatch::Headers {
                    protocol_version: header(&headers, "mcp-protocol-version"),
                    method: header(&headers, "mcp-method"),
                    name: header(&headers, "mcp-name"),
                    content_type: header(&headers, "content-type"),
                    accept: header(&headers, "accept"),
                },
                runtime: &NativeRuntime,
            },
        )
        .await
        .map_err(|error| match error {
            mcp_request::Error::Ivy(e) => e,
            mcp_request::Error::Protocol(e) => failure(format!("{e:?}")),
        })? {
            return Ok(());
        }
        request.admit_public_route(Some(&method), &path)?;
        let short_ui = assets::resolve_slug(&app, &method, &path).await?;
        let authenticated = if method == JsString::from("POST")
            && (path == JsString::from("/api/browser-push") || path == JsString::from("/logout"))
        {
            request
                .authenticate(ivy_hive::http_auth::Request {
                    method: Some(method.clone()),
                    headers: auth.clone(),
                    transport: "http".into(),
                    resource: None,
                })
                .await
        } else {
            request
                .authenticate_http(Some(method.clone()), &auth, &mut output)
                .await
        };
        let context = match authenticated {
            Ok(context) => context,
            Err(error)
                if error.code == "unauthenticated"
                    && method == JsString::from("GET")
                    && (path == JsString::from("/")
                        || ((short_ui.is_some()
                            || path.0.starts_with(&JsString::from("/ui/").0)
                            || path.0.starts_with(&JsString::from("/console/").0))
                            && header(&headers, "sec-fetch-mode") == Some("navigate".into())
                            && header(&headers, "sec-fetch-dest") == Some("document".into()))) =>
            {
                let target = format!(
                    "{}/login?returnTo={}&resume=1",
                    String::from_utf16_lossy(&app.server.base.base_path.0),
                    query_component(&uri)
                );
                return http_response::redirect(&mut output, &target.into());
            }
            Err(error) => return Err(error),
        };
        if method == JsString::from("POST") && path == JsString::from("/logout") {
            return ivy_hive::browser_session::Session {
                connection: connection.authentication(),
                worker: app.server.worker.as_ref(),
            }
            .logout(
                &auth,
                &context,
                &app.server.network.peers,
                &mut ivy_hive::auth_pages::SessionResponse {
                    connection: connection.authentication(),
                    response: &mut output,
                },
            )
            .await;
        }
        if method == JsString::from("POST") && path == JsString::from("/api/browser-push") {
            return ivy_hive::browser_push_http::Handler {
                canonical_origin: &app.server.base.origin,
                buffers: &buffers,
                worker: app.server.worker.as_ref(),
                push: &app.server.browser_push,
            }
            .handle(
                &context,
                ivy_hive::browser_push_http::Headers {
                    origin: auth.origin.as_ref(),
                    content_length: content_length.as_ref(),
                },
                &mut body,
                None,
                &mut output,
            )
            .await;
        }
        if server_http_rpc::handle(
            &mut request,
            Some(&method),
            &path,
            server_http_rpc::Headers {
                content_type: content_type.as_ref(),
                content_length: content_length.as_ref(),
            },
            &context,
            &mut rpc_id,
            &mut body,
            &mut output,
        )
        .await?
        {
            return Ok(());
        }
        if assets::serve(
            &app,
            &method,
            &path,
            &search,
            short_ui.as_ref(),
            &headers,
            &context,
            &mut output,
        )
        .await?
        {
            return Ok(());
        }
        if app
            .assets
            .console(
                Some(&method),
                &route.pathname,
                &path,
                &app.server.base.base_path,
                &mut output,
            )
            .await?
        {
            return Ok(());
        }
        Err(IvyError::new("not_found", "Route not found."))
    }
    .await;
    if let Err(error) = result {
        let _ = http_common_error(&app, &path, rpc_id, &error, &mut output);
    }
    output.finish(Some(request.into_response_lease()))
}
fn incoming_search(uri: &JsString) -> String {
    let text = String::from_utf16_lossy(&uri.0);
    text.find('?')
        .map_or(String::new(), |index| text[index..].to_string())
}
fn query_component(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}
fn http_common_error(
    app: &App,
    path: &JsString,
    id: IvyJson,
    error: &IvyError,
    output: &mut Output,
) -> IvyResult<()> {
    ivy_hive::http_common::error(
        &app.server.base.origin,
        &app.server.base.base_path,
        path,
        id,
        Some(error),
        output,
    )
}
