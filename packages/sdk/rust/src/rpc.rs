use crate::{IvyError, IvyJson, IvyResult, Outcome, admission::RequestQueue};
use futures_util::StreamExt;
use ivy_contracts::{
    json::{parse, stringify},
    limits::*,
    validation::{equal, exact_keys, safe_integer, text},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub type RequestFuture<'a> = Pin<Box<dyn Future<Output = IvyResult<IvyJson>> + Send + 'a>>;

/// Project a validated query row's bundled document, requiring its exact revision.
/// The returned document is borrowed unchanged, including unknown fields and UTF-16 strings.
pub fn query_document(item: &IvyJson) -> IvyResult<&IvyJson> {
    let invalid = || IvyError::new("invalid_frame", "Hive query returned no matching document.");
    let read = item.get("document").ok_or_else(invalid)?;
    let object_id = item.get("objectId").and_then(IvyJson::as_js_string);
    let revision = item.get("revision").and_then(IvyJson::as_f64);
    let version = item.get("contractVersion").and_then(IvyJson::as_js_string);
    let metadata = read.get("revision");
    if object_id.is_some()
        && revision.is_some()
        && version.is_some()
        && read
            .get("object")
            .and_then(|v| v.get("id"))
            .and_then(IvyJson::as_js_string)
            == object_id
        && metadata
            .and_then(|v| v.get("objectId"))
            .and_then(IvyJson::as_js_string)
            == object_id
        && metadata
            .and_then(|v| v.get("revision"))
            .and_then(IvyJson::as_f64)
            == revision
        && metadata
            .and_then(|v| v.get("contractVersion"))
            .and_then(IvyJson::as_js_string)
            == version
    {
        Ok(read)
    } else {
        Err(invalid())
    }
}

/// An RPC request may omit its params member without replacing it with JSON null.
pub trait OptionalRpcClient: RpcClient {
    fn request_optional<'a>(
        &'a self,
        method: &'a str,
        params: Option<IvyJson>,
        options: RequestOptions,
    ) -> RequestFuture<'a>;
}

pub trait RpcClient: Send + Sync {
    /// Only an actual provider generation owns a bound-owner cache.
    fn service_connection(&self) -> Option<&crate::service::ServiceConnection> {
        None
    }
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a>;
}
#[derive(Clone)]
pub struct BoundRead(pub(crate) bool);
#[derive(Clone)]
pub struct RequestOptions {
    pub cancellation: CancellationToken,
    pub timeout: Duration,
    pub cancel_error: IvyError,
    /// Constructed only by a pinned tool binding.
    pub bound_read: Option<BoundRead>,
}
impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            timeout: Duration::from_secs(35),
            cancel_error: IvyError::new("cancelled", "Request cancelled before sending."),
            bound_read: None,
        }
    }
}
pub struct ClientOptions<C = String> {
    pub credential: Option<C>,
    pub request_calls: Option<usize>,
    pub provider_calls: Option<usize>,
    pub queued_requests: usize,
    pub batch_reads: bool,
}
impl<C> Default for ClientOptions<C> {
    fn default() -> Self {
        Self {
            credential: None,
            request_calls: None,
            provider_calls: None,
            queued_requests: 128,
            batch_reads: false,
        }
    }
}

pub fn base_url(value: &str) -> IvyResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(value).map_err(|_| IvyError::internal())?;
    if !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some_and(|value| !value.is_empty())
        || url.fragment().is_some_and(|value| !value.is_empty())
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Expected the canonical Hive base URL.",
        ));
    }
    if url.scheme() != "https"
        && !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Remote Hive connections require HTTPS.",
        ));
    }
    let path = format!("{}/", url.path().strip_suffix('/').unwrap_or(url.path()));
    url.set_path(&path);
    Ok(url)
}
fn unknown() -> IvyError {
    IvyError::new(
        "outcome_unknown",
        "The request connection failed or timed out. Reconcile its original operation identity.",
    )
    .with_outcome(Outcome::Unknown)
}
fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_frame", message).with_outcome(Outcome::Unknown)
}

pub fn response_value(frame: &IvyJson, id: &str) -> IvyResult<IvyJson> {
    if frame.as_object().is_none() {
        return Err(invalid("Hive returned an invalid response."));
    }
    let result = frame.get("result").is_some();
    if !equal(frame.get("jsonrpc"), "2.0") || result == frame.get("error").is_some() {
        return Err(invalid("Hive response envelope is invalid."));
    }
    if !exact_keys(
        frame,
        if result {
            &["jsonrpc", "id", "result"]
        } else {
            &["jsonrpc", "id", "error"]
        },
    ) {
        return Err(invalid(
            "Hive response envelope contains unexpected fields.",
        ));
    }
    if result {
        if !equal(frame.get("id"), id) {
            return Err(invalid("Hive response identity is invalid."));
        }
        return Ok(frame.get("result").unwrap().clone());
    }
    let error = frame.get("error").unwrap();
    let data = error.get("data");
    if !exact_keys(error, &["code", "message", "data"])
        || !safe_integer(error.get("code"), -9_007_199_254_740_991.0)
        || !text(error.get("message"), 2048)
        || data.is_none()
    {
        return Err(invalid("Hive returned an invalid error."));
    }
    let data = data.unwrap();
    if !exact_keys(data, &["code", "outcome", "details"])
        || !text(data.get("code"), 128)
        || !["not_executed", "unknown", "completed"]
            .iter()
            .any(|v| equal(data.get("outcome"), v))
    {
        return Err(invalid("Hive returned an invalid error."));
    }
    let outcome = match data.get("outcome").unwrap().as_str()?.as_str() {
        "not_executed" => Outcome::NotExecuted,
        "completed" => Outcome::Completed,
        _ => Outcome::Unknown,
    };
    if !equal(frame.get("id"), id)
        && !(frame.get("id").is_some_and(IvyJson::is_null) && outcome == Outcome::NotExecuted)
    {
        return Err(invalid("Hive error identity is invalid."));
    }
    let mut failure = IvyError::new_js(
        data.get("code").unwrap().as_js_string().unwrap().clone(),
        error
            .get("message")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
    )
    .with_outcome(outcome);
    failure.details = data.get("details").cloned();
    Err(failure)
}

struct Inner {
    base: reqwest::Url,
    http: reqwest::Client,
    credential: Option<String>,
    queue: Option<Arc<RequestQueue>>,
    active: Arc<Semaphore>,
}
#[derive(Clone)]
pub struct HiveClient {
    inner: Arc<Inner>,
    reads: Option<Arc<crate::batching::ReadBatcher>>,
}
impl HiveClient {
    pub fn new(value: &str, options: ClientOptions) -> IvyResult<Self> {
        let base = base_url(value)?;
        let maximum = options
            .request_calls
            .unwrap_or(CONNECTION_IN_FLIGHT_REQUESTS);
        if maximum == 0 || maximum > CONNECTION_IN_FLIGHT_REQUESTS {
            return Err(IvyError::new(
                "invalid_arguments",
                "Invalid client concurrency limit.",
            ));
        }
        let queue = if options.batch_reads
            || options.request_calls.is_some()
            || options.provider_calls.is_some()
        {
            Some(RequestQueue::new(
                maximum,
                options.provider_calls.unwrap_or(maximum),
                options.queued_requests,
            )?)
        } else {
            None
        };
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| IvyError::internal())?;
        let inner = Arc::new(Inner {
            base,
            http,
            credential: options.credential,
            queue,
            active: Arc::new(Semaphore::new(CONNECTION_IN_FLIGHT_REQUESTS)),
        });
        let reads = if options.batch_reads {
            let transport = Self {
                inner: inner.clone(),
                reads: None,
            };
            Some(crate::batching::ReadBatcher::new(
                inner.queue.as_ref().unwrap().clone(),
                Arc::new(move |body, cancellation| {
                    let transport = transport.clone();
                    Box::pin(async move {
                        tokio::select! {biased;_=cancellation.cancelled()=>Err(unknown()),result=transport.exchange(body)=>result}
                    })
                }),
            ))
        } else {
            None
        };
        Ok(Self { inner, reads })
    }
    /// Accept UTF-16 configuration through the same native HTTP client.
    pub fn new_js(
        value: &crate::JsString,
        options: ClientOptions<crate::JsString>,
    ) -> IvyResult<Self> {
        Self::new(
            &String::from_utf16_lossy(&value.0),
            ClientOptions {
                credential: options
                    .credential
                    .map(|value| value.to_utf8())
                    .transpose()?,
                request_calls: options.request_calls,
                provider_calls: options.provider_calls,
                queued_requests: options.queued_requests,
                batch_reads: options.batch_reads,
            },
        )
    }
    pub fn base(&self) -> &reqwest::Url {
        &self.inner.base
    }
    async fn exchange(&self, body: String) -> IvyResult<IvyJson> {
        let _permit =
            self.inner.active.clone().try_acquire_owned().map_err(|_| {
                IvyError::new("limit_exceeded", "Hive client request limit reached.")
            })?;
        let mut request = self
            .inner
            .http
            .post(
                self.inner
                    .base
                    .join("api/v1/rpc")
                    .map_err(|_| IvyError::internal())?,
            )
            .header("Content-Type", "application/json")
            .body(body);
        if let Some(value) = &self.inner.credential {
            if !value.is_empty() {
                request = request.bearer_auth(value);
            }
        }
        let response = request.send().await.map_err(|_| unknown())?;
        if response.status().is_redirection() {
            return Err(unknown());
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| unknown())?;
            if bytes.len().saturating_add(chunk.len()) > MANAGEMENT_FRAME_BYTES {
                return Err(IvyError::new(
                    "result_too_large",
                    "Hive response exceeds its frame limit.",
                )
                .with_outcome(Outcome::Unknown));
            }
            bytes.extend_from_slice(&chunk);
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|_| invalid("Hive returned malformed JSON."))?;
        parse(
            text.strip_prefix('\u{feff}').unwrap_or(text),
            MANAGEMENT_FRAME_BYTES,
        )
        .map_err(|_| invalid("Hive returned malformed JSON."))
    }
}
impl RpcClient for HiveClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let sent = Arc::new(AtomicBool::new(false));
            let read = if method == "tools.call" {
                options
                    .bound_read
                    .as_ref()
                    .map(|value| value.0)
                    .unwrap_or_else(|| {
                        ivy_contracts::read_request::is_read_request(method, &params)
                    })
            } else {
                ivy_contracts::read_request::is_read_request(method, &params)
            };
            struct Invalidate(Option<Arc<crate::batching::ReadBatcher>>);
            impl Drop for Invalidate {
                fn drop(&mut self) {
                    if let Some(reads) = &self.0 {
                        reads.invalidate();
                    }
                }
            }
            let _invalidate = Invalidate(if !read { self.reads.clone() } else { None });
            let work = async {
                if options.cancellation.is_cancelled() {
                    return Err(options.cancel_error.clone());
                }
                if read {
                    if let Some(reads) = &self.reads {
                        return reads.request(method, params, sent.clone()).await;
                    }
                } else if let Some(reads) = &self.reads {
                    reads.invalidate();
                }
                let _permit = match &self.inner.queue {
                    Some(queue) => Some(
                        queue
                            .acquire(
                                matches!(method, "tools.call" | "discovery.call"),
                                &options.cancellation,
                            )
                            .await?,
                    ),
                    None => None,
                };
                if options.cancellation.is_cancelled() {
                    return Err(options.cancel_error.clone());
                }
                sent.store(true, Ordering::Release);
                let id = uuid::Uuid::new_v4().to_string();
                let body = stringify(
                    &IvyJson::object([
                        ("jsonrpc", "2.0".into()),
                        ("id", id.clone().into()),
                        ("method", method.into()),
                        ("params", params),
                    ]),
                    MANAGEMENT_FRAME_BYTES,
                )?;
                response_value(&self.exchange(body).await?, &id)
            };
            tokio::select! {biased;
                _=options.cancellation.cancelled()=>Err(if sent.load(Ordering::Acquire){unknown()}else{options.cancel_error.clone()}),
                result=tokio::time::timeout(options.timeout,work)=>match result{Ok(value)=>value,Err(_)=>Err(if sent.load(Ordering::Acquire){unknown()}else{IvyError::new("deadline_exceeded","Hive request expired before sending.")})}
            }
        })
    }
}
