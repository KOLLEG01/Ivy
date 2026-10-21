//! HTTP/1 delivery for browser push, with an explicitly supplied TLS profile.
//! No ambient roots, proxy, redirect, decompression or request retry is selected.
use crate::browser_push_native::{Https, PostFuture, Request, SOCKET_TIMEOUT};
use http_body_util::{BodyExt, Full};
use hyper::{Uri, body::Bytes, client::conn::http1, header};
use hyper_util::rt::TokioIo;
use ivy_contracts::{IvyError, IvyResult};
use std::{
    collections::HashMap,
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpStream, lookup_host},
    runtime::Handle,
    sync::watch,
    task::JoinHandle,
    time::{Instant, sleep_until, timeout},
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, pki_types::ServerName},
};

const FREE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FREE_PER_ORIGIN: usize = 256;

pub type LookupFuture = Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>>;
/// The service supplies the resolver belonging to its qualified runtime profile.
pub trait Lookup: Send + Sync {
    fn addresses(&self, host: String, port: u16) -> LookupFuture;
}
pub struct SystemLookup;
impl Lookup for SystemLookup {
    fn addresses(&self, host: String, port: u16) -> LookupFuture {
        Box::pin(async move {
            lookup_host((host.as_str(), port))
                .await
                .map(Iterator::collect)
        })
    }
}
#[derive(Clone, Copy)]
pub enum AddressOrder {
    Verbatim,
    Ipv4First,
    Ipv6First,
}
#[derive(Clone, Copy)]
/// Base network options. Production callers must provide their effective runtime
/// options and resolver explicitly; this type does not discover process inputs.
pub struct NetworkProfile {
    pub auto_select_family: bool,
    pub attempt_timeout: Duration,
    pub address_order: AddressOrder,
}
impl Default for NetworkProfile {
    fn default() -> Self {
        Self {
            auto_select_family: true,
            attempt_timeout: Duration::from_millis(250),
            address_order: AddressOrder::Verbatim,
        }
    }
}
fn ordered_addresses(mut addresses: Vec<SocketAddr>, profile: NetworkProfile) -> Vec<SocketAddr> {
    match profile.address_order {
        AddressOrder::Verbatim => (),
        AddressOrder::Ipv4First => addresses.sort_by_key(SocketAddr::is_ipv6),
        AddressOrder::Ipv6First => addresses.sort_by_key(SocketAddr::is_ipv4),
    }
    if !profile.auto_select_family {
        addresses.truncate(1);
        return addresses;
    }
    let Some(first) = addresses.first() else {
        return addresses;
    };
    let preferred_v6 = first.is_ipv6();
    let mut preferred = Vec::new();
    let mut other = Vec::new();
    for address in addresses {
        let group = if address.is_ipv6() == preferred_v6 {
            &mut preferred
        } else {
            &mut other
        };
        if !group.contains(&address) {
            group.push(address);
        }
    }
    let mut ordered = Vec::new();
    for i in 0..preferred.len().max(other.len()) {
        if let Some(address) = preferred.get(i) {
            ordered.push(*address);
        }
        if let Some(address) = other.get(i) {
            ordered.push(*address);
        }
    }
    ordered
}
async fn connect_tcp(
    host: &str,
    port: u16,
    lookup: &dyn Lookup,
    profile: NetworkProfile,
) -> IvyResult<TcpStream> {
    let addresses = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        timeout(SOCKET_TIMEOUT, lookup.addresses(host.to_string(), port))
            .await
            .map_err(|_| IvyError::internal())?
            .map_err(|_| IvyError::internal())?
    };
    let addresses = ordered_addresses(addresses, profile);
    // Node refreshes its socket timer after lookup. Each nonfinal family attempt
    // is canceled at the configured interval; the final one uses the remaining
    // socket timeout. It does not leave parallel attempts alive.
    let deadline = Instant::now() + SOCKET_TIMEOUT;
    for (index, address) in addresses.iter().enumerate() {
        let attempt_deadline = if profile.auto_select_family && index + 1 < addresses.len() {
            (Instant::now() + profile.attempt_timeout.max(Duration::from_millis(10))).min(deadline)
        } else {
            deadline
        };
        if let Ok(Ok(stream)) =
            tokio::time::timeout_at(attempt_deadline, TcpStream::connect(address)).await
        {
            return Ok(stream);
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    Err(IvyError::internal())
}

#[derive(Clone, Copy)]
struct Timer {
    deadline: Instant,
    duration: Duration,
}
impl Timer {
    fn new(duration: Duration) -> Self {
        Self {
            deadline: Instant::now() + duration,
            duration,
        }
    }
}

/// Progress below TLS also refreshes the timeout during a partial TLS record or
/// HTTP header. A response deadline or a per-body-chunk timeout is insufficient.
struct Socket {
    stream: TcpStream,
    timer: watch::Sender<Timer>,
}
impl Socket {
    fn progress(&self) {
        self.timer
            .send_modify(|timer| timer.deadline = Instant::now() + timer.duration);
    }
}
impl AsyncRead for Socket {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.stream).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.progress();
        }
        result
    }
}
impl AsyncWrite for Socket {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.stream).poll_write(cx, bytes);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.progress();
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

async fn while_active<F: Future>(
    future: F,
    mut timer: watch::Receiver<Timer>,
) -> IvyResult<F::Output> {
    tokio::pin!(future);
    loop {
        let deadline = timer.borrow_and_update().deadline;
        tokio::select! {
            result = &mut future => return Ok(result),
            changed = timer.changed() => changed.map_err(|_| IvyError::internal())?,
            _ = sleep_until(deadline) => {
                // I/O may have refreshed the watch in the same scheduler turn.
                if timer.borrow().deadline <= Instant::now() {
                    return Err(IvyError::internal());
                }
            }
        }
    }
}

struct Connection {
    sender: http1::SendRequest<Full<Bytes>>,
    driver: JoinHandle<()>,
    timer: watch::Sender<Timer>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.driver.abort();
    }
}
impl Connection {
    fn set_timeout(&self, duration: Duration) {
        self.timer.send_replace(Timer::new(duration));
    }
    fn available(&self) -> bool {
        !self.driver.is_finished()
            && !self.sender.is_closed()
            && self.timer.borrow().deadline > Instant::now()
    }
}

struct Inner {
    runtime: Handle,
    tls: Arc<ClientConfig>,
    lookup: Arc<dyn Lookup>,
    network: NetworkProfile,
    free: Mutex<HashMap<String, Vec<Connection>>>,
}

/// Hive's sender owns this pool. The service must supply its qualified TLS root
/// and verifier policy; choosing operating-system or library defaults here would
/// silently change the original https.request trust boundary.
#[derive(Clone)]
pub struct NativeHttps(Arc<Inner>);
impl NativeHttps {
    pub fn new(runtime: Handle, tls: Arc<ClientConfig>) -> IvyResult<Self> {
        Self::with_network_profile(
            runtime,
            tls,
            Arc::new(SystemLookup),
            NetworkProfile::default(),
        )
    }
    pub fn with_network_profile(
        runtime: Handle,
        tls: Arc<ClientConfig>,
        lookup: Arc<dyn Lookup>,
        network: NetworkProfile,
    ) -> IvyResult<Self> {
        // The original https.request does not negotiate HTTP/2.
        if !tls.alpn_protocols.is_empty() {
            return Err(IvyError::internal());
        }
        Ok(Self(Arc::new(Inner {
            runtime,
            tls,
            lookup,
            network,
            free: Mutex::new(HashMap::new()),
        })))
    }

    async fn connect(&self, host: &str, port: u16) -> IvyResult<Connection> {
        let stream = connect_tcp(host, port, self.0.lookup.as_ref(), self.0.network).await?;
        stream.set_nodelay(true).map_err(|_| IvyError::internal())?;
        let (timer, watched) = watch::channel(Timer::new(SOCKET_TIMEOUT));
        let socket = Socket {
            stream,
            timer: timer.clone(),
        };
        let name = ServerName::try_from(host.to_string()).map_err(|_| IvyError::internal())?;
        let tls = while_active(
            TlsConnector::from(self.0.tls.clone()).connect(name, socket),
            watched.clone(),
        )
        .await?
        .map_err(|_| IvyError::internal())?;
        let (sender, connection) = http1::Builder::new()
            .max_headers(2000)
            .max_buf_size(16 * 1024)
            .handshake(TokioIo::new(tls))
            .await
            .map_err(|_| IvyError::internal())?;
        let driver = self.0.runtime.spawn(async move {
            // Dropping the driver closes the socket on inactivity or protocol error.
            let _ = while_active(connection, watched).await;
        });
        Ok(Connection {
            sender,
            driver,
            timer,
        })
    }

    async fn send(&self, request: Request) -> IvyResult<u16> {
        let uri: Uri = request.endpoint.parse().map_err(|_| IvyError::internal())?;
        if uri.scheme_str() != Some("https") || uri.authority().is_none() {
            return Err(IvyError::internal());
        }
        let authority = uri.authority().unwrap();
        let host = uri
            .host()
            .ok_or_else(IvyError::internal)?
            .trim_start_matches('[')
            .trim_end_matches(']');
        let key = authority.as_str().to_string();
        let mut connection = {
            let mut pool = self.0.free.lock().unwrap();
            let connections = pool.entry(key.clone()).or_default();
            // Node's global agent selects free sockets last-in-first-out.
            let found = loop {
                match connections.pop() {
                    Some(connection) if connection.available() => break Some(connection),
                    Some(_) => continue,
                    None => break None,
                }
            };
            if connections.is_empty() {
                pool.remove(&key);
            }
            found
        };
        if connection.is_none() {
            connection = Some(self.connect(host, uri.port_u16().unwrap_or(443)).await?);
        }
        let mut connection = connection.unwrap();
        connection.set_timeout(SOCKET_TIMEOUT);
        let mut outgoing = hyper::Request::builder()
            .method("POST")
            .uri(uri.path_and_query().map_or("/", |v| v.as_str()));
        for (name, value) in request.headers {
            outgoing = outgoing.header(name, value);
        }
        let outgoing = outgoing
            .header(header::HOST, authority.as_str())
            .header(header::CONNECTION, "keep-alive")
            .body(Full::new(Bytes::from(request.body)))
            .map_err(|_| IvyError::internal())?;
        let response = connection
            .sender
            .send_request(outgoing)
            .await
            .map_err(|_| IvyError::internal())?;
        let status = response.status().as_u16();
        let close = response
            .headers()
            .get_all(header::CONNECTION)
            .iter()
            .any(|v| {
                v.to_str()
                    .is_ok_and(|v| v.split(',').any(|s| s.trim().eq_ignore_ascii_case("close")))
            });
        let idle = response
            .headers()
            .get("keep-alive")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("timeout="))
            .and_then(|v| {
                let end = v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len());
                v[..end].parse::<u64>().ok()
            })
            .map(|seconds| {
                Duration::from_millis(seconds.saturating_mul(1000).saturating_sub(1000))
                    .min(FREE_TIMEOUT)
            })
            .unwrap_or(FREE_TIMEOUT);
        let mut body = response.into_body();
        while let Some(frame) = body.frame().await {
            frame.map_err(|_| IvyError::internal())?;
        }
        // A status is observable only after response end. Failed POSTs are never
        // retried here, including a stale pooled socket or a partial response.
        if !close && !idle.is_zero() && connection.available() {
            connection.set_timeout(idle);
            let mut pool = self.0.free.lock().unwrap();
            let connections = pool.entry(key).or_default();
            connections.retain(Connection::available);
            if connections.len() < MAX_FREE_PER_ORIGIN {
                connections.push(connection);
            }
        }
        Ok(status)
    }
}
impl Https for NativeHttps {
    fn post(&self, request: Request) -> PostFuture {
        let owner = self.clone();
        // Starting the operation now preserves Promise behavior: abandoning the
        // returned observer must not cancel an already issued notification.
        let sent = self
            .0
            .runtime
            .spawn(async move { owner.send(request).await });
        Box::pin(async move { sent.await.map_err(|_| IvyError::internal())? })
    }
}
