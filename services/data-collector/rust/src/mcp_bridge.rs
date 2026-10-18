//! Run-local loopback transport. Only a temporary bearer token reaches the worker.
use crate::{
    http::{self, BodyError, Reply, Server},
    policy::MCP_BYTES,
    runner::McpBridge,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hyper::{Request, body::Incoming};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::stringify};
use ivy_sdk::service::ServiceFuture;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub trait TaskMcp: Send + Sync + 'static {
    fn call(&self, tool: JsString, args: IvyJson) -> ServiceFuture<IvyJson>;
    fn close(&self) -> ServiceFuture<()>;
}
struct Inner {
    server: Server,
    session: Arc<dyn TaskMcp>,
    context: IvyJson,
    closing: AtomicBool,
    finished: watch::Sender<Option<IvyResult<()>>>,
}
#[derive(Clone)]
pub struct LocalBridge(Arc<Inner>);
impl LocalBridge {
    pub async fn open(session: Arc<dyn TaskMcp>, signal: CancellationToken) -> IvyResult<Self> {
        let opened = async {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|_| IvyError::internal())?;
            let token = URL_SAFE_NO_PAD.encode(bytes);
            let supplied_token = token.clone();
            let supplied_session = session.clone();
            let server = Server::bind(
                "127.0.0.1",
                0,
                Arc::new(move |request| {
                    let token = supplied_token.clone();
                    let session = supplied_session.clone();
                    let signal = signal.clone();
                    Box::pin(async move {
                        match handle(request, &token, session, signal).await {
                            Ok(reply) => Ok(reply),
                            Err(error) => http::json(
                                400,
                                IvyJson::object([(
                                    "error",
                                    IvyJson::object([
                                        ("code", IvyJson::String(error.code_js())),
                                        ("outcome", error.outcome.as_str().into()),
                                    ]),
                                )]),
                            ),
                        }
                    })
                }),
            )
            .await?;
            let context = IvyJson::object([
                (
                    "url",
                    format!("http://127.0.0.1:{}/call", server.address().port()).into(),
                ),
                ("token", token.into()),
            ]);
            let (finished, _) = watch::channel(None);
            Ok(Self(Arc::new(Inner {
                server,
                session: session.clone(),
                context,
                closing: AtomicBool::new(false),
                finished,
            })))
        }
        .await;
        if opened.is_err() {
            let _ = session.close().await;
        }
        opened
    }
}
impl McpBridge for LocalBridge {
    fn context(&self) -> IvyJson {
        self.0.context.clone()
    }
    fn close(&self) -> ServiceFuture<()> {
        let this = self.0.clone();
        Box::pin(async move {
            let mut done = this.finished.subscribe();
            if !this.closing.swap(true, Ordering::AcqRel) {
                let closing = this.clone();
                // Once started, cleanup survives cancellation of the close caller.
                // DC03: session cleanup is unconditional after socket retirement.
                tokio::spawn(async move {
                    closing.server.close().await;
                    let result = closing.session.close().await;
                    closing.finished.send_replace(Some(result));
                });
            }
            loop {
                if let Some(result) = done.borrow_and_update().clone() {
                    return result;
                }
                if done.changed().await.is_err() {
                    return Err(IvyError::internal());
                }
            }
        })
    }
}
async fn handle(
    request: Request<Incoming>,
    token: &str,
    session: Arc<dyn TaskMcp>,
    signal: CancellationToken,
) -> IvyResult<Reply> {
    if request.method() != "POST"
        || request.uri().to_string() != "/call"
        || !http::equal(http::bearer(request.headers()).as_bytes(), token.as_bytes())
    {
        http::drain(request.into_body());
        return Ok(http::empty(401));
    }
    if signal.is_cancelled() {
        http::drain(request.into_body());
        return Err(IvyError::internal());
    }
    let value = match http::body(request.into_body(), MCP_BYTES).await {
        Ok(value) => value,
        Err(BodyError::TooLarge) => {
            return http::json(
                400,
                IvyJson::object([(
                    "error",
                    IvyJson::object([
                        ("code", "configuration_invalid".into()),
                        ("outcome", "not_executed".into()),
                    ]),
                )]),
            )
            .map(http::close_response);
        }
        Err(BodyError::Failed) => return Err(IvyError::internal()),
    };
    if value.is_null() {
        return Err(IvyError::internal());
    }
    let tool = value
        .get("tool")
        .and_then(|v| v.as_js_string())
        .ok_or_else(|| IvyError::new("configuration_invalid", "MCP tool name is required."))?
        .clone();
    let args = value
        .get("arguments")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| IvyJson::object(Vec::<(&str, IvyJson)>::new()));
    let result = session.call(tool, args).await?;
    let encoded = stringify(&IvyJson::object([("result", result)]), usize::MAX)?;
    if encoded.len() > MCP_BYTES {
        return Err(IvyError::new(
            "provider_unavailable",
            "MCP result exceeds the task budget.",
        ));
    }
    Ok(http::encoded(200, encoded))
}
