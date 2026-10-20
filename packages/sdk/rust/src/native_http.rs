//! Owned HTTP/1 connections. RPC framing and profile-specific body decoding stay with callers.
#![allow(dead_code)]

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Request, StatusCode, Uri, body::Incoming, client::conn::http1};
use hyper_rustls::{DefaultServerNameResolver, HttpsConnector, MaybeHttpsStream};
use hyper_util::{client::legacy::connect::HttpConnector, rt::TokioIo};
use std::{
    collections::HashMap,
    error::Error,
    fmt,
    future::{Future, Ready, ready},
    io,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, Weak},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{net::TcpStream, sync::Notify, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;
use tower_service::Service;

pub(crate) type RequestBody = Full<Bytes>;
pub(crate) type ConfigFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Arc<rustls::ClientConfig>, TransportError>> + Send + 'a>>;

pub(crate) trait TlsConfigSource: Send + Sync {
    fn for_origin<'a>(&'a self, origin: &'a Uri) -> ConfigFuture<'a>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HeadAdmission {
    Continue,
    RejectAndRetire,
}

#[derive(Clone, Copy)]
pub(crate) struct HeadPolicy {
    pub on_informational: fn(StatusCode) -> HeadAdmission,
    pub on_final_head: fn(StatusCode) -> HeadAdmission,
}

pub(crate) struct ConnectionSettings {
    pub tls: Arc<dyn TlsConfigSource>,
    pub connect_timeout: Duration,
    pub headers_timeout: Duration,
    /// Maximum wait for a body frame while the consumer is reading.
    pub body_idle_timeout: Duration,
    pub idle_timeout: Duration,
    pub maximum_idle_timeout: Duration,
    pub idle_timeout_threshold: Duration,
    pub tcp_keepalive_delay: Duration,
    pub happy_eyeballs_delay: Duration,
    /// Hyper counts the entire wire head, including its status line and syntax.
    pub maximum_head_bytes: usize,
    pub maximum_head_fields: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransportErrorKind {
    Cancelled,
    RejectedHead(StatusCode),
    InvalidRequest,
    Connect,
    Tls,
    Timeout,
    Protocol,
    Body,
}

#[derive(Debug)]
pub(crate) struct TransportError {
    pub kind: TransportErrorKind,
    source: Option<Box<dyn Error + Send + Sync>>,
}
impl TransportError {
    fn new(kind: TransportErrorKind) -> Self {
        Self { kind, source: None }
    }
    fn caused(kind: TransportErrorKind, source: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self {
            kind,
            source: Some(source.into()),
        }
    }
}
impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP transport {:?}", self.kind)
    }
}
impl Error for TransportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

type TcpIo = TokioIo<TcpStream>;
type NativeIo = MaybeHttpsStream<TcpIo>;
type Socket = Physical<NativeIo>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PoolKey {
    origin: String,
    tls_identity: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Active,
    Idle,
    Retired,
}

struct Wire<T> {
    io: Option<T>,
    generation: u64,
    phase: Phase,
    terminal: Option<TransportErrorKind>,
    driver_done: bool,
    driver: Option<JoinHandle<()>>,
    idle_timer: Option<JoinHandle<()>>,
}

struct Physical<T> {
    wire: Mutex<Wire<T>>,
    changed: Notify,
    // Retain this exact policy identity for as long as any pool entry can refer to it.
    tls: Option<Arc<rustls::ClientConfig>>,
}

impl<T> Physical<T> {
    fn new(io: T, tls: Option<Arc<rustls::ClientConfig>>) -> Arc<Self> {
        Arc::new(Self {
            wire: Mutex::new(Wire {
                io: Some(io),
                generation: 1,
                phase: Phase::Active,
                terminal: None,
                driver_done: false,
                driver: None,
                idle_timer: None,
            }),
            changed: Notify::new(),
            tls,
        })
    }

    fn terminal(&self, generation: u64) -> Option<TransportErrorKind> {
        let wire = lock(&self.wire);
        if wire.generation != generation {
            Some(TransportErrorKind::Cancelled)
        } else {
            wire.terminal
        }
    }

    /// The epoch test and the actual I/O take share the same authority as pool transfer.
    fn retire(&self, generation: u64, kind: TransportErrorKind) -> bool {
        let retired = {
            let mut wire = lock(&self.wire);
            if wire.generation != generation || wire.phase != Phase::Active {
                return false;
            }
            wire.phase = Phase::Retired;
            wire.terminal.get_or_insert(kind);
            (wire.io.take(), wire.driver.take(), wire.idle_timer.take())
        };
        // Close the concrete TCP/TLS stream synchronously, before waking or aborting tasks.
        drop(retired.0);
        if let Some(driver) = retired.1 {
            driver.abort();
        }
        if let Some(timer) = retired.2 {
            timer.abort();
        }
        self.changed.notify_waiters();
        true
    }

    fn shutdown(&self) {
        let retired = {
            let mut wire = lock(&self.wire);
            if wire.phase == Phase::Active {
                wire.terminal.get_or_insert(TransportErrorKind::Cancelled);
            }
            wire.phase = Phase::Retired;
            (wire.io.take(), wire.driver.take(), wire.idle_timer.take())
        };
        drop(retired.0);
        if let Some(driver) = retired.1 {
            driver.abort();
        }
        if let Some(timer) = retired.2 {
            timer.abort();
        }
        self.changed.notify_waiters();
    }

    fn driver_finished(&self) {
        let io = {
            let mut wire = lock(&self.wire);
            wire.driver_done = true;
            wire.io.take()
        };
        drop(io);
        // A closed driver can still have delivered a complete buffered body. Incoming
        // supplies any actual body failure; driver completion alone is not a terminal latch.
    }

    async fn wait_terminal(&self, generation: u64) -> TransportErrorKind {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            // Register both the exchange reader and watcher before testing the latch.
            notified.as_mut().enable();
            if let Some(kind) = self.terminal(generation) {
                return kind;
            }
            notified.await;
        }
    }
}

impl<T> Drop for Physical<T> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The driver holds only a weak reference, so its handle and I/O never form an Arc cycle.
struct ManagedIo<T> {
    socket: Weak<Physical<T>>,
}

impl<T: hyper::rt::Read + Unpin> hyper::rt::Read for ManagedIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let Some(socket) = self.socket.upgrade() else {
            return Poll::Ready(Err(closed()));
        };
        let mut wire = lock(&socket.wire);
        match wire.io.as_mut() {
            Some(io) => Pin::new(io).poll_read(cx, buffer),
            None => Poll::Ready(Err(closed())),
        }
    }
}
impl<T: hyper::rt::Write + Unpin> hyper::rt::Write for ManagedIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let Some(socket) = self.socket.upgrade() else {
            return Poll::Ready(Err(closed()));
        };
        let mut wire = lock(&socket.wire);
        match wire.io.as_mut() {
            Some(io) => Pin::new(io).poll_write(cx, bytes),
            None => Poll::Ready(Err(closed())),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Some(socket) = self.socket.upgrade() else {
            return Poll::Ready(Err(closed()));
        };
        let mut wire = lock(&socket.wire);
        match wire.io.as_mut() {
            Some(io) => Pin::new(io).poll_flush(cx),
            None => Poll::Ready(Err(closed())),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Some(socket) = self.socket.upgrade() else {
            return Poll::Ready(Err(closed()));
        };
        let mut wire = lock(&socket.wire);
        match wire.io.as_mut() {
            Some(io) => Pin::new(io).poll_shutdown(cx),
            None => Poll::Ready(Err(closed())),
        }
    }
}
fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "HTTP connection retired")
}

struct Idle {
    socket: Arc<Socket>,
    sender: http1::SendRequest<RequestBody>,
    expires: Instant,
}
#[derive(Default)]
struct Pool {
    idle: HashMap<PoolKey, Vec<Idle>>,
    sockets: Vec<Weak<Socket>>,
}
struct Owner {
    state: Arc<ClientState>,
}
struct ClientState {
    settings: ConnectionSettings,
    pool: Mutex<Pool>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        // Only client clones own this token. Even a worker temporarily upgrading
        // ClientState cannot postpone the last logical client's synchronous shutdown.
        let mut pool = lock(&self.state.pool);
        for socket in pool.sockets.iter().filter_map(Weak::upgrade) {
            socket.shutdown();
        }
        pool.idle.clear();
        pool.sockets.clear();
    }
}

#[derive(Clone)]
pub(crate) struct Http1Client {
    owner: Arc<Owner>,
}

struct Lease {
    socket: Arc<Socket>,
    sender: Option<http1::SendRequest<RequestBody>>,
    generation: u64,
    state: Weak<ClientState>,
    key: PoolKey,
    watcher: Option<JoinHandle<()>>,
    idle_timeout: Option<Duration>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.socket
            .retire(self.generation, TransportErrorKind::Cancelled);
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
        }
    }
}

impl Lease {
    fn watch(&mut self, cancellation: CancellationToken) {
        let socket = self.socket.clone();
        let generation = self.generation;
        self.watcher = Some(tokio::spawn(async move {
            tokio::select! {
                _ = cancellation.cancelled() => { socket.retire(generation, TransportErrorKind::Cancelled); }
                _ = socket.wait_terminal(generation) => {}
            }
        }));
    }

    fn complete(mut self) -> Result<(), TransportError> {
        let Some(state) = self.state.upgrade() else {
            return Err(TransportError::new(TransportErrorKind::Cancelled));
        };
        // Pool -> wire is also the checkout/expiry order. No I/O poll takes pool.
        let mut pool = lock(&state.pool);
        let mut wire = lock(&self.socket.wire);
        if let Some(kind) = wire.terminal {
            return Err(TransportError::new(kind));
        }
        if wire.generation != self.generation || wire.phase != Phase::Active {
            return Err(TransportError::new(TransportErrorKind::Cancelled));
        }
        let expires = self
            .idle_timeout
            .and_then(|timeout| Instant::now().checked_add(timeout));
        if let Some(expires) = expires.filter(|_| wire.io.is_some() && !wire.driver_done) {
            // Disarm this generation before insertion, under the I/O retirement lock.
            wire.phase = Phase::Idle;
            if let Some(watcher) = self.watcher.take() {
                watcher.abort();
            }
            let socket = Arc::downgrade(&self.socket);
            let pool_state = Arc::downgrade(&state);
            let key = self.key.clone();
            let generation = self.generation;
            wire.idle_timer = Some(tokio::spawn(async move {
                tokio::time::sleep_until(expires).await;
                if let (Some(state), Some(socket)) = (pool_state.upgrade(), socket.upgrade()) {
                    let mut pool = lock(&state.pool);
                    let mut wire = lock(&socket.wire);
                    if wire.phase != Phase::Idle || wire.generation != generation {
                        return;
                    }
                    wire.phase = Phase::Retired;
                    let io = wire.io.take();
                    let driver = wire.driver.take();
                    if let Some(entries) = pool.idle.get_mut(&key) {
                        entries.retain(|entry| !Arc::ptr_eq(&entry.socket, &socket));
                        if entries.is_empty() {
                            pool.idle.remove(&key);
                        }
                    }
                    drop(wire);
                    drop(pool);
                    drop(io);
                    if let Some(driver) = driver {
                        driver.abort();
                    }
                }
            }));
            pool.idle.entry(self.key.clone()).or_default().push(Idle {
                socket: self.socket.clone(),
                sender: self.sender.take().unwrap(),
                expires,
            });
        } else {
            wire.phase = Phase::Retired;
            let io = wire.io.take();
            let driver = wire.driver.take();
            drop(wire);
            drop(pool);
            drop(io);
            if let Some(driver) = driver {
                driver.abort();
            }
        }
        Ok(())
    }
}

/// Adapts an already connected TCP stream to the public HTTPS connector, separating
/// TCP/DNS errors from TLS errors without opening a second connection or retrying.
struct Connected(Option<TcpIo>);
impl Service<Uri> for Connected {
    type Response = TcpIo;
    type Error = io::Error;
    type Future = Ready<Result<TcpIo, io::Error>>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, _: Uri) -> Self::Future {
        ready(self.0.take().ok_or_else(closed))
    }
}

impl Http1Client {
    pub(crate) fn new(settings: ConnectionSettings) -> Self {
        Self {
            owner: Arc::new(Owner {
                state: Arc::new(ClientState {
                    settings,
                    pool: Mutex::new(Pool::default()),
                }),
            }),
        }
    }

    fn checkout(&self, key: &PoolKey) -> Option<Lease> {
        let mut pool = lock(&self.owner.state.pool);
        let entries = pool.idle.get_mut(key)?;
        while let Some(idle) = entries.pop() {
            let mut wire = lock(&idle.socket.wire);
            if wire.phase != Phase::Idle
                || wire.driver_done
                || wire.io.is_none()
                || idle.expires <= Instant::now()
            {
                drop(wire);
                idle.socket.shutdown();
                continue;
            }
            let Some(generation) = wire.generation.checked_add(1) else {
                drop(wire);
                idle.socket.shutdown();
                continue;
            };
            if let Some(timer) = wire.idle_timer.take() {
                timer.abort();
            }
            wire.generation = generation;
            wire.phase = Phase::Active;
            wire.terminal = None;
            drop(wire);
            let lease = Lease {
                socket: idle.socket,
                sender: Some(idle.sender),
                generation,
                state: Arc::downgrade(&self.owner.state),
                key: key.clone(),
                watcher: None,
                idle_timeout: None,
            };
            if entries.is_empty() {
                pool.idle.remove(key);
            }
            return Some(lease);
        }
        pool.idle.remove(key);
        None
    }

    pub(crate) async fn send(
        &self,
        mut request: Request<RequestBody>,
        policy: HeadPolicy,
        cancellation: CancellationToken,
    ) -> Result<Response, TransportError> {
        if cancellation.is_cancelled() {
            return Err(TransportError::new(TransportErrorKind::Cancelled));
        }
        let destination = request.uri().clone();
        let scheme = destination
            .scheme_str()
            .filter(|s| matches!(*s, "http" | "https"));
        let authority = destination
            .authority()
            .filter(|a| !a.as_str().contains('@') && !a.host().is_empty());
        let (Some(scheme), Some(authority)) = (scheme, authority) else {
            return Err(TransportError::new(TransportErrorKind::InvalidRequest));
        };
        if !matches!(
            request.version(),
            hyper::Version::HTTP_10 | hyper::Version::HTTP_11
        ) || (authority.port().is_some() && authority.port_u16().is_none())
        {
            return Err(TransportError::new(TransportErrorKind::InvalidRequest));
        }
        let host = authority.host().to_ascii_lowercase();
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        let origin: Uri = format!(
            "{scheme}://{host}:{}/",
            authority
                .port_u16()
                .unwrap_or(if scheme == "https" { 443 } else { 80 })
        )
        .parse()
        .map_err(|error| TransportError::caused(TransportErrorKind::InvalidRequest, error))?;
        if !request.headers().contains_key(hyper::header::HOST) {
            request.headers_mut().insert(
                hyper::header::HOST,
                authority.as_str().parse().map_err(|error| {
                    TransportError::caused(TransportErrorKind::InvalidRequest, error)
                })?,
            );
        }
        let request_close = has_token(request.headers(), "connection", "close");
        let connect = async {
            let tls = if scheme == "https" {
                let tls = self.owner.state.settings.tls.for_origin(&origin).await?;
                if tls.alpn_protocols != [b"http/1.1".to_vec()] {
                    return Err(TransportError::new(TransportErrorKind::Tls));
                }
                Some(tls)
            } else {
                None
            };
            let key = PoolKey {
                origin: origin.to_string(),
                tls_identity: tls.as_ref().map_or(0, |tls| Arc::as_ptr(tls) as usize),
            };
            if let Some(lease) = self.checkout(&key) {
                return Ok(lease);
            }
            let mut http = HttpConnector::new();
            http.enforce_http(false);
            http.set_nodelay(true);
            http.set_keepalive(Some(self.owner.state.settings.tcp_keepalive_delay));
            http.set_connect_timeout(Some(self.owner.state.settings.connect_timeout));
            http.set_happy_eyeballs_timeout(Some(self.owner.state.settings.happy_eyeballs_delay));
            let tcp = http
                .call(origin.clone())
                .await
                .map_err(|error| TransportError::caused(TransportErrorKind::Connect, error))?;
            let io = if let Some(tls) = &tls {
                HttpsConnector::new(
                    Connected(Some(tcp)),
                    tls.clone(),
                    false,
                    Arc::new(DefaultServerNameResolver::default()),
                )
                .call(origin.clone())
                .await
                .map_err(|error| TransportError::caused(TransportErrorKind::Tls, error))?
            } else {
                MaybeHttpsStream::Http(tcp)
            };
            let socket = Physical::new(io, tls);
            {
                let mut pool = lock(&self.owner.state.pool);
                pool.sockets.retain(|socket| socket.strong_count() > 0);
                pool.sockets.push(Arc::downgrade(&socket));
            }
            let mut lease = Lease {
                socket,
                sender: None,
                generation: 1,
                state: Arc::downgrade(&self.owner.state),
                key,
                watcher: None,
                idle_timeout: None,
            };
            let mut builder = http1::Builder::new();
            builder
                .max_header_size(self.owner.state.settings.maximum_head_bytes)
                .max_headers(self.owner.state.settings.maximum_head_fields);
            let (sender, connection) = builder
                .handshake(ManagedIo {
                    socket: Arc::downgrade(&lease.socket),
                })
                .await
                .map_err(|error| TransportError::caused(TransportErrorKind::Protocol, error))?;
            lease.sender = Some(sender);
            let weak = Arc::downgrade(&lease.socket);
            let driver = tokio::spawn(async move {
                let _ = connection.await;
                if let Some(socket) = weak.upgrade() {
                    socket.driver_finished();
                }
            });
            lock(&lease.socket.wire).driver = Some(driver);
            Ok(lease)
        };
        let mut lease = tokio::select! { biased;
            _ = cancellation.cancelled() => return Err(TransportError::new(TransportErrorKind::Cancelled)),
            connected = tokio::time::timeout(self.owner.state.settings.connect_timeout, connect) => {
                connected.map_err(|error| TransportError::caused(TransportErrorKind::Timeout, error))??
            }
        };
        if cancellation.is_cancelled() {
            return Err(TransportError::new(TransportErrorKind::Cancelled));
        }
        lease.watch(cancellation.clone());
        let socket = lease.socket.clone();
        let generation = lease.generation;
        let observer = Arc::downgrade(&socket);
        hyper::ext::on_informational(&mut request, move |response| {
            if (policy.on_informational)(response.status()) == HeadAdmission::RejectAndRetire {
                if let Some(socket) = observer.upgrade() {
                    #[cfg(test)]
                    assert!(
                        socket.wire.try_lock().is_ok(),
                        "informational callback must run outside the I/O poll lock"
                    );
                    socket.retire(
                        generation,
                        TransportErrorKind::RejectedHead(response.status()),
                    );
                    #[cfg(test)]
                    assert!(
                        lock(&socket.wire).io.is_none(),
                        "concrete I/O must be taken before the callback returns"
                    );
                }
            }
        });
        *request.uri_mut() = destination
            .path_and_query()
            .map(|path| path.as_str())
            .unwrap_or("/")
            .parse()
            .map_err(|error| TransportError::caused(TransportErrorKind::InvalidRequest, error))?;
        let head = async {
            let sender = lease.sender.as_mut().unwrap();
            sender.ready().await?;
            sender.send_request(request).await
        };
        let received = tokio::select! { biased;
            _ = cancellation.cancelled() => {
                socket.retire(generation, TransportErrorKind::Cancelled);
                Err(TransportError::new(TransportErrorKind::Cancelled))
            }
            kind = socket.wait_terminal(generation) => Err(TransportError::new(kind)),
            result = tokio::time::timeout(self.owner.state.settings.headers_timeout, head) => match result {
                Ok(result) => result.map_err(|error| TransportError::caused(TransportErrorKind::Protocol, error)),
                Err(error) => Err(TransportError::caused(TransportErrorKind::Timeout, error)),
            }
        };
        // Hyper may parse a buffered200 after invoking its void informational callback.
        if let Some(kind) = socket.terminal(generation) {
            return Err(TransportError::new(kind));
        }
        if cancellation.is_cancelled() {
            socket.retire(generation, TransportErrorKind::Cancelled);
            return Err(TransportError::new(TransportErrorKind::Cancelled));
        }
        let response = match received {
            Ok(response) => response,
            Err(error) => {
                socket.retire(generation, error.kind);
                return Err(error);
            }
        };
        if (policy.on_final_head)(response.status()) == HeadAdmission::RejectAndRetire {
            let kind = TransportErrorKind::RejectedHead(response.status());
            socket.retire(generation, kind);
            return Err(TransportError::new(kind));
        }
        lease.idle_timeout = idle_timeout(
            response.status(),
            response.version(),
            response.headers(),
            request_close,
            &self.owner.state.settings,
        );
        let (head, body) = response.into_parts();
        let mut admitted = Response {
            status: head.status,
            headers: head.headers,
            body,
            lease: Some(lease),
            body_idle_timeout: self.owner.state.settings.body_idle_timeout,
            cancellation,
            finished: false,
            failure: None,
        };
        if hyper::body::Body::is_end_stream(&admitted.body) {
            admitted.complete()?;
        }
        Ok(admitted)
    }
}

fn has_token(headers: &HeaderMap, name: &str, token: &str) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value
                .split(',')
                .any(|value| value.trim().eq_ignore_ascii_case(token))
        })
}

fn idle_timeout(
    status: StatusCode,
    version: hyper::Version,
    headers: &HeaderMap,
    request_close: bool,
    settings: &ConnectionSettings,
) -> Option<Duration> {
    if request_close
        || status == StatusCode::SWITCHING_PROTOCOLS
        || has_token(headers, "connection", "close")
        || (version == hyper::Version::HTTP_10 && !has_token(headers, "connection", "keep-alive"))
    {
        return None;
    }
    let advertised = headers
        .get_all("keep-alive")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|value| {
            let after = value.split_once("timeout=")?.1;
            let digits = after
                .bytes()
                .take_while(u8::is_ascii_digit)
                .collect::<Vec<_>>();
            if digits.is_empty() {
                return None;
            }
            Some(digits.into_iter().fold(0u64, |n, digit| {
                n.saturating_mul(10).saturating_add(u64::from(digit - b'0'))
            }))
        });
    let timeout = advertised
        .map_or(settings.idle_timeout, |seconds| {
            Duration::from_secs(seconds).saturating_sub(settings.idle_timeout_threshold)
        })
        .min(settings.maximum_idle_timeout);
    (!timeout.is_zero()).then_some(timeout)
}

pub(crate) struct Response {
    status: StatusCode,
    headers: HeaderMap,
    body: Incoming,
    lease: Option<Lease>,
    body_idle_timeout: Duration,
    cancellation: CancellationToken,
    finished: bool,
    failure: Option<TransportErrorKind>,
}
impl Response {
    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }
    pub(crate) fn headers(&self) -> &HeaderMap {
        &self.headers
    }
    fn complete(&mut self) -> Result<(), TransportError> {
        self.finished = true;
        if let Some(lease) = self.lease.take() {
            if let Err(error) = lease.complete() {
                self.failure = Some(error.kind);
                return Err(error);
            }
        }
        Ok(())
    }
    pub(crate) async fn next_chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        if let Some(kind) = self.failure {
            return Err(TransportError::new(kind));
        }
        if self.finished {
            return Ok(None);
        }
        let lease = self.lease.as_ref().unwrap();
        let socket = lease.socket.clone();
        let generation = lease.generation;
        loop {
            if self.cancellation.is_cancelled() {
                socket.retire(generation, TransportErrorKind::Cancelled);
            }
            if let Some(kind) = socket.terminal(generation) {
                return Err(TransportError::new(kind));
            }
            let next = tokio::select! { biased;
                _ = self.cancellation.cancelled() => {
                    socket.retire(generation, TransportErrorKind::Cancelled);
                    Err(TransportError::new(TransportErrorKind::Cancelled))
                }
                kind = socket.wait_terminal(generation) => Err(TransportError::new(kind)),
                frame = tokio::time::timeout(self.body_idle_timeout, self.body.frame()) => match frame {
                    Ok(Some(Ok(frame))) => Ok(Some(frame)),
                    Ok(None) => Ok(None),
                    Ok(Some(Err(error))) => Err(TransportError::caused(TransportErrorKind::Body, error)),
                    Err(error) => Err(TransportError::caused(TransportErrorKind::Timeout, error)),
                }
            };
            if self.cancellation.is_cancelled() {
                socket.retire(generation, TransportErrorKind::Cancelled);
            }
            if let Some(kind) = socket.terminal(generation) {
                return Err(TransportError::new(kind));
            }
            match next {
                Ok(Some(frame)) => {
                    if let Ok(bytes) = frame.into_data() {
                        return Ok(Some(bytes));
                    }
                }
                Ok(None) => {
                    self.complete()?;
                    return Ok(None);
                }
                Err(error) => {
                    socket.retire(generation, error.kind);
                    return Err(error);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/sdk-native-http.rs"]
mod tests;
