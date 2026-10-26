//! Bounded HTTP/WS authentication. The transport owns normalized headers and
//! socket lifetime; OAuth validation and the storage worker are required owners.
use crate::{
    admission::{AuthenticationRate, RequestBudget, RequestBudgetOptions, RequestLease},
    browser_push::{RequestFuture, Worker},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::digest, json::stringify};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr},
    sync::{Arc, Mutex},
};
use tokio::sync::{oneshot, watch};

pub trait OAuth: Send + Sync {
    fn authenticate(
        &self,
        token: &JsString,
        resource: Option<&JsString>,
    ) -> IvyResult<Option<IvyJson>>;
}
pub struct Options {
    pub canonical_origin: JsString,
    pub base_path: JsString,
    pub trusted_proxy_addresses: Vec<JsString>,
}
#[derive(Clone, Default)]
pub struct Headers {
    pub authorization: Option<JsString>,
    pub cookie: Option<JsString>,
    pub origin: Option<IvyJson>,
    pub forwarded_client_ip: Option<IvyJson>,
}
#[derive(Clone)]
pub struct Request {
    pub method: Option<JsString>,
    pub headers: Headers,
    pub transport: JsString,
    pub resource: Option<JsString>,
}
type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
type Pending = watch::Receiver<Option<IvyResult<IvyJson>>>;
type Cache = Arc<Mutex<HashMap<String, Pending>>>;
struct State {
    epoch: Arc<()>,
    arrivals: AuthenticationRate,
}
struct Inner {
    options: Options,
    worker: Arc<dyn Worker>,
    oauth: Arc<dyn OAuth>,
    clock: Clock,
    authentication: RequestBudget,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct Authentication(Arc<Inner>);
impl Authentication {
    pub fn canonical_origin(&self) -> &JsString {
        &self.0.options.canonical_origin
    }
    pub fn base_path(&self) -> &JsString {
        &self.0.options.base_path
    }
    /// Canonical origin/base path come from the server's validated public URL.
    /// No anonymous OAuth, default worker or wall-clock fallback is supplied.
    pub fn new(
        options: Options,
        worker: Arc<dyn Worker>,
        oauth: Arc<dyn OAuth>,
        clock: Clock,
    ) -> IvyResult<Self> {
        validate_trusted_proxies(&options.trusted_proxy_addresses)?;
        Ok(Self(Arc::new(Inner {
            options,
            worker,
            oauth,
            clock,
            authentication: RequestBudget::new(RequestBudgetOptions {
                maximum: 512,
                maximum_bytes: 512 * 1024,
                per_consumer: 64,
                per_consumer_bytes: 64 * 1024,
                name: "authentication".into(),
            }),
            state: Mutex::new(State {
                epoch: Arc::new(()),
                arrivals: AuthenticationRate::default(),
            }),
        })))
    }
    pub fn connection(&self, remote_address: Option<JsString>) -> Connection {
        Connection {
            owner: self.0.clone(),
            remote_address,
            state: Mutex::new(ConnectionState::default()),
        }
    }
    /// Replace the current WeakMap-equivalent epoch. Already running requests
    /// keep their original cache and completion; reset does not cancel them.
    pub fn reset(&self) {
        self.0.state.lock().unwrap().epoch = Arc::new(());
    }
    pub fn cookie(&self, value: &JsString, clear: bool) -> JsString {
        let mut units = JsString::from("ivy_session=").0;
        units.extend_from_slice(&value.0);
        units.extend("; Path=".encode_utf16());
        if self.0.options.base_path.0.is_empty() {
            units.push(b'/' as u16);
        } else {
            units.extend_from_slice(&self.0.options.base_path.0);
        }
        units.extend(
            format!(
                "; HttpOnly; Secure; SameSite=Lax; Max-Age={}",
                if clear { 0 } else { 365 * 24 * 60 * 60 }
            )
            .encode_utf16(),
        );
        JsString(units)
    }
}
#[derive(Default)]
struct ConnectionState {
    current: Option<(Arc<()>, Cache)>,
    // Each original cache installs its own once(close) listener. Retain replaced
    // maps until that connection closes, including maps captured by pending work.
    close_handlers: Vec<Cache>,
    closed: bool,
}
pub struct Connection {
    owner: Arc<Inner>,
    remote_address: Option<JsString>,
    state: Mutex<ConnectionState>,
}
impl Connection {
    pub fn authentication(&self) -> Authentication {
        Authentication(self.owner.clone())
    }
    /// Login/OAuth routes share the same arrival counter and retained slots as
    /// bounded bearer/session authentication on this connection's owner.
    pub fn admit_authentication(&self, headers: &Headers) -> IvyResult<RequestLease> {
        self.owner
            .admit_authentication(&self.client_address(headers)?)
    }
    pub fn client_address(&self, headers: &Headers) -> IvyResult<JsString> {
        let peer = normalize(
            self.remote_address
                .as_ref()
                .unwrap_or(&JsString::from("local")),
        );
        if !self
            .owner
            .options
            .trusted_proxy_addresses
            .iter()
            .any(|value| normalize(value) == peer)
        {
            return Ok(peer);
        }
        let Some(forwarded) = &headers.forwarded_client_ip else {
            return Ok(peer);
        };
        let value = forwarded.as_js_string().filter(|value| is_ip(value) != 0);
        require(
            value.is_some(),
            "unauthenticated",
            "The trusted proxy must overwrite X-Ivy-Client-IP with the original client address.",
        )?;
        Ok(normalize(value.unwrap()))
    }
    /// Start immediately, even if the returned observation future is dropped.
    /// A connection serializes identity lookup/admission/posting before the first
    /// await. Required providers must not reenter that connection synchronously.
    pub fn authenticate(&self, request: Request) -> RequestFuture {
        self.admit(request)
            .unwrap_or_else(|error| Box::pin(async move { Err(error) }))
    }
    fn admit(&self, request: Request) -> IvyResult<RequestFuture> {
        if request.headers.authorization.is_none() && needs_origin(&request) {
            self.owner.same_origin(&request.headers)?;
        }
        let epoch = self.owner.state.lock().unwrap().epoch.clone();
        let cache = {
            let mut state = self.state.lock().unwrap();
            if state
                .current
                .as_ref()
                .is_none_or(|(current, _)| !Arc::ptr_eq(current, &epoch))
            {
                let cache: Cache = Arc::default();
                state.close_handlers.push(cache.clone());
                state.current = Some((epoch, cache));
            }
            state.current.as_ref().unwrap().1.clone()
        };
        let identity = digest(stringify(
            &IvyJson::Array(vec![
                request
                    .headers
                    .authorization
                    .as_ref()
                    .map(|v| IvyJson::String(v.clone()))
                    .unwrap_or(IvyJson::Null),
                request
                    .headers
                    .cookie
                    .as_ref()
                    .map(|v| IvyJson::String(v.clone()))
                    .unwrap_or(IvyJson::Null),
                request
                    .resource
                    .as_ref()
                    .map(|v| IvyJson::String(v.clone()))
                    .unwrap_or(IvyJson::Null),
            ]),
            usize::MAX,
        )?);
        let mut entries = cache.lock().unwrap();
        let owner = self.owner.clone();
        let (reply, observe) = oneshot::channel();
        if let Some(pending) = entries.get(&identity) {
            let pending = pending.clone();
            drop(entries);
            tokio::spawn(async move {
                let result = wait(pending).await.and_then(|value| {
                    let value = with_transport(&value, &request.transport);
                    owner.require_fresh(&value)?;
                    Ok(value)
                });
                // Cached freshness failures do not evict this identity.
                let _ = reply.send(result);
            });
        } else {
            require(
                entries.len() < 16,
                "limit_exceeded",
                "Connection identity capacity reached.",
            )?;
            let key = self.client_address(&request.headers)?;
            let mut lease = owner.admit_authentication(&key)?;
            let work = owner.authenticate(request);
            let (completed, pending) = watch::channel(None);
            entries.insert(identity.clone(), pending);
            drop(entries);
            tokio::spawn(async move {
                let result = work.await;
                lease.release();
                completed.send_replace(Some(result.clone()));
                let result = result.and_then(|value| {
                    owner.require_fresh(&value)?;
                    Ok(value)
                });
                if result.is_err() {
                    cache.lock().unwrap().remove(&identity);
                }
                let _ = reply.send(result);
            });
        }
        Ok(Box::pin(async move {
            observe.await.map_err(|_| IvyError::internal())?
        }))
    }
    /// Bind this to the owning socket close and invoke it before discarding the
    /// connection. In-flight work survives clearing its cache, as in the source.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        state.closed = true;
        for cache in state.close_handlers.drain(..) {
            cache.lock().unwrap().clear();
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}
impl Inner {
    fn admit_authentication(&self, key: &JsString) -> IvyResult<RequestLease> {
        let now = (self.clock)();
        self.state.lock().unwrap().arrivals.admit_at_ms(key, now)?;
        self.authentication.acquire(key, None)
    }
    fn same_origin(&self, headers: &Headers) -> IvyResult<()> {
        same_origin(&self.options.canonical_origin, headers.origin.as_ref())
    }
    fn require_fresh(&self, context: &IvyJson) -> IvyResult<()> {
        crate::server_rpc::require_fresh(context, self.clock.as_ref())
    }
    fn authenticate(self: &Arc<Self>, request: Request) -> RequestFuture {
        let initial = (|| {
            if let Some(authorization) = &request.headers.authorization {
                let prefix = JsString::from("Bearer ");
                let token = authorization.0.strip_prefix(prefix.0.as_slice());
                require(
                    token.is_some_and(|token| {
                        !token.is_empty()
                            && !token.iter().any(|unit| {
                                // Reuse the canonical ECMAScript whitespace set. Printable
                                // ASCII needs no temporary string/Unicode conversion.
                                !matches!(unit, 0x21..=0x7e)
                                    && JsString(vec![*unit]).trim_ecmascript().0.is_empty()
                            })
                    }),
                    "unauthenticated",
                    "Expected a bearer credential.",
                )?;
                let token = JsString(token.unwrap().to_vec());
                if let Some(value) = self
                    .oauth
                    .authenticate(&token, request.resource.as_ref())?
                    .filter(|context| context.as_object().is_some())
                {
                    return Ok(Prepared::Ready(with_transport(&value, &request.transport)));
                }
                let context = IvyJson::object([
                    (
                        "credentialDigest",
                        digest(String::from_utf16_lossy(&token.0)).into(),
                    ),
                    ("transport", IvyJson::String(request.transport.clone())),
                ]);
                let posted = self.worker.request(IvyJson::object([
                    ("action", "authenticate".into()),
                    ("context", context.clone()),
                ]));
                return Ok(Prepared::Bearer(context, posted));
            }
            let token = session_token(request.headers.cookie.as_ref());
            require(token.is_some(), "unauthenticated", "Sign in to Hive.")?;
            let posted = self.worker.request(IvyJson::object([
                ("action", "session.resolve".into()),
                (
                    "sessionDigest",
                    digest(String::from_utf16_lossy(&token.unwrap().0)).into(),
                ),
            ]));
            Ok(Prepared::Session(posted))
        })();
        let owner = self.clone();
        Box::pin(async move {
            match initial? {
                Prepared::Ready(value) => Ok(value),
                Prepared::Bearer(mut context, posted) => {
                    let principal = posted.await?;
                    if principal.is_null() {
                        return Err(IvyError::internal());
                    }
                    if let Some(id) = principal.get("principalId") {
                        context.insert("principalId", id.clone());
                    }
                    Ok(context)
                }
                Prepared::Session(posted) => {
                    let value = posted.await?;
                    if needs_origin(&request) {
                        owner.same_origin(&request.headers)?;
                    }
                    Ok(with_transport(&value, &request.transport))
                }
            }
        })
    }
}
enum Prepared {
    Ready(IvyJson),
    Bearer(IvyJson, RequestFuture),
    Session(RequestFuture),
}
async fn wait(mut pending: Pending) -> IvyResult<IvyJson> {
    loop {
        if let Some(result) = pending.borrow_and_update().clone() {
            return result;
        }
        pending.changed().await.map_err(|_| IvyError::internal())?;
    }
}
fn with_transport(value: &IvyJson, transport: &JsString) -> IvyJson {
    let mut context = match value {
        IvyJson::Object(_) => value.clone(),
        IvyJson::Array(values) => IvyJson::object(
            values
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v.clone())),
        ),
        IvyJson::String(value) => IvyJson::object(
            value
                .0
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), IvyJson::String(JsString(vec![*v])))),
        ),
        _ => IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
    };
    context.insert("transport", IvyJson::String(transport.clone()));
    context
}
fn needs_origin(request: &Request) -> bool {
    request.method.as_ref() != Some(&JsString::from("GET"))
        || request.transport == JsString::from("ws")
}
pub fn same_origin(canonical_origin: &JsString, origin: Option<&IvyJson>) -> IvyResult<()> {
    require(
        origin.and_then(IvyJson::as_js_string) == Some(canonical_origin),
        "unauthenticated",
        "Browser action requires the exact Hive origin.",
    )
}
pub fn mcp_origin(canonical_origin: &JsString, origin: Option<&IvyJson>) -> IvyResult<()> {
    let Some(origin) = origin else { return Ok(()) };
    let allowed = (|| {
        let text = origin.as_js_string()?;
        if *text == JsString::from("null") {
            return None;
        }
        let parsed = ivy_contracts::whatwg_url::parse_url(text).ok()?;
        let components = ivy_contracts::whatwg_url::url_components(text).ok()?;
        let base = canonical_origin.to_utf8().ok()?;
        Some(components.origin == base && parsed.href == base + "/")
    })()
    .unwrap_or(false);
    require(allowed, "forbidden", "MCP Origin is not allowed.")
}
pub fn session_token(cookie: Option<&JsString>) -> Option<JsString> {
    let cookie = cookie?;
    let prefix = JsString::from("ivy_session=");
    let mut token = None;
    for part in cookie.0.split(|unit| *unit == b';' as u16) {
        let part = JsString(part.to_vec()).trim_ecmascript();
        if let Some(value) = part.0.strip_prefix(prefix.0.as_slice()) {
            if token.is_some() {
                return None;
            }
            token = Some(JsString(value.to_vec()));
        }
    }
    token
}
fn normalize(value: &JsString) -> JsString {
    let prefix = JsString::from("::ffff:");
    if let Some(tail) = value.0.strip_prefix(prefix.0.as_slice()) {
        let tail = JsString(tail.to_vec());
        if is_ip(&tail) == 4 {
            return tail;
        }
    }
    value.clone()
}
pub(crate) fn validate_trusted_proxies(addresses: &[JsString]) -> IvyResult<()> {
    require(
        addresses.iter().all(|value| is_ip(value) != 0),
        "invalid_arguments",
        "Trusted proxies must be exact IP addresses.",
    )
}
fn is_ip(value: &JsString) -> u8 {
    let Ok(value) = value.to_utf8() else { return 0 };
    if value.parse::<Ipv4Addr>().is_ok() {
        return 4;
    }
    let address = if let Some((address, zone)) = value.split_once('%') {
        if zone.is_empty()
            || !zone
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'-' | b'.' | b':'))
        {
            return 0;
        }
        address
    } else {
        value.as_str()
    };
    if address.parse::<Ipv6Addr>().is_ok() {
        6
    } else {
        0
    }
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
