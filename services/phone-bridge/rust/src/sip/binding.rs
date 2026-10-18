//! Native SIP sockets stay bound from preparation through confirmed shutdown.
//! The host supplies already parsed literal endpoints and owns call admission.
use super::{PreparedInvite, SipEndpoint, SipIoError};
use rsipstack::{
    sip::{Request, Transport},
    transaction::endpoint::EndpointOption,
    transport::{SipAddr, SipConnection, TransportLayer, tcp::TcpConnection, udp::UdpConnection},
};
use std::{
    any::Any,
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{
    net::TcpSocket,
    sync::{Mutex, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const TCP_CONNECTIONS: usize = 1000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindError {
    InvalidDestination,
    BindFailed,
    ConnectFailed,
    ConnectTimedOut,
    Capacity,
    Stopped,
    Endpoint(SipIoError),
}
impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for BindError {}

type Connections = Arc<Mutex<HashMap<SocketAddr, SipConnection>>>;
type SocketOwners = Arc<Mutex<Vec<Weak<dyn Any + Send + Sync>>>>;
fn observed_owner<T: Any + Send + Sync>(owner: Arc<T>) -> Weak<dyn Any + Send + Sync> {
    let owner: Arc<dyn Any + Send + Sync> = owner;
    Arc::downgrade(&owner)
}
fn live(connection: &SipConnection) -> bool {
    connection
        .cancel_token()
        .is_none_or(|token| !token.is_cancelled())
}
// Zero linger preserves the original abortive close, without a positive wait.
#[allow(deprecated)]
fn tcp_socket(address: SocketAddr) -> Result<TcpSocket, BindError> {
    let socket = if address.is_ipv4() {
        TcpSocket::new_v4()
    } else {
        TcpSocket::new_v6()
    }
    .map_err(|_| BindError::BindFailed)?;
    // The original TCP channel and its outbound sockets share this endpoint.
    socket
        .set_reuseaddr(true)
        .map_err(|_| BindError::BindFailed)?;
    socket
        .set_linger(Some(Duration::ZERO))
        .map_err(|_| BindError::BindFailed)?;
    socket.bind(address).map_err(|_| BindError::BindFailed)?;
    Ok(socket)
}
fn sip_address(address: SocketAddr, transport: Transport) -> SipAddr {
    SipAddr::new(transport, address.into())
}

pub struct BoundSipTransport {
    endpoint: Option<Arc<SipEndpoint>>,
    local: SocketAddr,
    protocol: Transport,
    datagram: Option<SipConnection>,
    connections: Connections,
    sockets: SocketOwners,
    accepting: Option<JoinHandle<()>>,
    listener_failure: watch::Receiver<Option<BindError>>,
    stop: CancellationToken,
    closed: bool,
}
impl BoundSipTransport {
    pub async fn udp(address: SocketAddr, options: EndpointOption) -> Result<Self, BindError> {
        let stop = CancellationToken::new();
        let transport = TransportLayer::new(stop.child_token());
        let socket = Arc::new(
            tokio::net::UdpSocket::bind(address)
                .await
                .map_err(|_| BindError::BindFailed)?,
        );
        let local = socket.local_addr().map_err(|_| BindError::BindFailed)?;
        let released = observed_owner(socket.clone());
        let connection = UdpConnection::from_socket(socket, None, Some(stop.child_token()))
            .await
            .map_err(|_| BindError::BindFailed)?;
        let connection = SipConnection::Udp(connection);
        transport.add_transport(connection.clone());
        let (_, listener_failure) = watch::channel(None);
        Ok(Self {
            endpoint: Some(Arc::new(SipEndpoint::start(
                transport,
                stop.clone(),
                options,
            ))),
            local,
            protocol: Transport::Udp,
            datagram: Some(connection),
            connections: Arc::new(Mutex::new(HashMap::new())),
            accepting: None,
            sockets: Arc::new(Mutex::new(vec![released])),
            listener_failure,
            stop,
            closed: false,
        })
    }

    pub fn tcp(address: SocketAddr, options: EndpointOption) -> Result<Self, BindError> {
        let socket = tcp_socket(address)?;
        let listener = socket
            .listen(TCP_CONNECTIONS as u32)
            .map_err(|_| BindError::BindFailed)?;
        let local = listener.local_addr().map_err(|_| BindError::BindFailed)?;
        let stop = CancellationToken::new();
        let transport = TransportLayer::new(stop.child_token());
        let endpoint = Arc::new(SipEndpoint::start(transport, stop.clone(), options));
        let connections: Connections = Arc::new(Mutex::new(HashMap::new()));
        let owned_connections = connections.clone();
        let sockets: SocketOwners = Arc::new(Mutex::new(Vec::new()));
        let owned_sockets = sockets.clone();
        let inner = endpoint.inner().clone();
        let cancelled = stop.clone();
        let (failure, listener_failure) = watch::channel(None);
        let accepting = tokio::spawn(async move {
            loop {
                let (stream, remote) = tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => break,
                    accepted = listener.accept() => match accepted {
                        Ok(value) => value,
                        Err(_) => { failure.send_replace(Some(BindError::BindFailed)); break; }
                    }
                };
                let mut cache = owned_connections.lock().await;
                if cancelled.is_cancelled() {
                    break;
                }
                cache.retain(|_, connection| live(connection));
                if cache.len() >= TCP_CONNECTIONS && !cache.contains_key(&remote) {
                    continue;
                }
                let local_connection = match stream.local_addr() {
                    Ok(local) => local,
                    Err(_) => continue,
                };
                let connection = match TcpConnection::from_stream(
                    stream,
                    sip_address(local_connection, Transport::Tcp),
                    Some(cancelled.child_token()),
                ) {
                    Ok(connection) => connection,
                    Err(_) => continue,
                };
                let mut sockets = owned_sockets.lock().await;
                sockets.retain(|owner| owner.strong_count() > 0);
                sockets.push(observed_owner(connection.inner.clone()));
                let connection = SipConnection::Tcp(connection);
                cache.insert(remote, connection.clone());
                inner.transport_layer.add_connection(connection);
            }
        });
        Ok(Self {
            endpoint: Some(endpoint),
            local,
            protocol: Transport::Tcp,
            datagram: None,
            connections,
            sockets,
            accepting: Some(accepting),
            listener_failure,
            stop,
            closed: false,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }
    pub fn endpoint(&self) -> Result<Arc<SipEndpoint>, BindError> {
        self.endpoint.clone().ok_or(BindError::Stopped)
    }
    pub fn listener_failure(&self) -> Option<BindError> {
        (*self.listener_failure.borrow()).or_else(|| {
            (!self.stop.is_cancelled()
                && (self
                    .accepting
                    .as_ref()
                    .is_some_and(|task| task.is_finished())
                    || self
                        .endpoint
                        .as_ref()
                        .is_some_and(|endpoint| !endpoint.is_running())))
            .then_some(BindError::Endpoint(SipIoError::DriverFailed))
        })
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Resolution precedes the INVITE attempt. The request URI stays intact for
    /// identity/digest; the resolved socket is carried separately on one flow.
    pub async fn prepare_invite(
        &self,
        request: Request,
        destination: SipAddr,
        proxy: Option<SipAddr>,
    ) -> Result<PreparedInvite, BindError> {
        if self.closed || self.stop.is_cancelled() {
            return Err(BindError::Stopped);
        }
        if let Some(error) = self.listener_failure() {
            return Err(error);
        }
        let remote = destination
            .get_socketaddr()
            .map_err(|_| BindError::InvalidDestination)?;
        if remote.port() == 0
            || remote.is_ipv4() != self.local.is_ipv4()
            || destination.r#type.unwrap_or_default() != self.protocol
            || proxy.as_ref().is_some_and(|value| {
                value.r#type.unwrap_or_default() != self.protocol
                    || value.get_socketaddr().ok() != Some(remote)
            })
        {
            return Err(BindError::InvalidDestination);
        }
        // Validate the INVITE before opening even a local TCP connection.
        let mut prepared = PreparedInvite::new(request, destination.clone(), proxy)
            .map_err(BindError::Endpoint)?;
        let connection = if let Some(connection) = &self.datagram {
            connection.clone()
        } else {
            let mut cache = self.connections.lock().await;
            if self.stop.is_cancelled() {
                return Err(BindError::Stopped);
            }
            cache.retain(|_, connection| live(connection));
            if let Some(connection) = cache.get(&remote) {
                connection.clone()
            } else {
                if cache.len() >= TCP_CONNECTIONS {
                    return Err(BindError::Capacity);
                }
                let socket = tcp_socket(self.local)?;
                let stream = tokio::select! {
                    biased;
                    _ = self.stop.cancelled() => return Err(BindError::Stopped),
                    result = tokio::time::timeout(CONNECT_TIMEOUT, socket.connect(remote)) =>
                        result.map_err(|_| BindError::ConnectTimedOut)?.map_err(|_| BindError::ConnectFailed)?
                };
                let local_connection = stream.local_addr().map_err(|_| BindError::ConnectFailed)?;
                let connection = TcpConnection::from_stream(
                    stream,
                    sip_address(local_connection, Transport::Tcp),
                    Some(self.stop.child_token()),
                )
                .map_err(|_| BindError::ConnectFailed)?;
                let mut sockets = self.sockets.lock().await;
                sockets.retain(|owner| owner.strong_count() > 0);
                sockets.push(observed_owner(connection.inner.clone()));
                let connection = SipConnection::Tcp(connection);
                self.endpoint
                    .as_ref()
                    .ok_or(BindError::Stopped)?
                    .inner()
                    .transport_layer
                    .add_connection(connection.clone());
                cache.insert(remote, connection.clone());
                connection
            }
        };
        prepared.bind_connection(self.endpoint()?, connection);
        Ok(prepared)
    }

    /// Keep this owner on failure. A still-owned call/endpoint reference cannot
    /// be bypassed by discarding the host and admitting a replacement.
    pub async fn close(&mut self) -> Result<(), BindError> {
        if self.closed {
            return Ok(());
        }
        if let Some(endpoint) = self.endpoint.as_mut() {
            let endpoint = Arc::get_mut(endpoint).ok_or(BindError::Endpoint(SipIoError::InUse))?;
            endpoint.shutdown().await.map_err(BindError::Endpoint)?;
        }
        if let Some(accepting) = self.accepting.as_mut() {
            let result = accepting.await;
            self.accepting.take();
            result.map_err(|_| BindError::Endpoint(SipIoError::DriverFailed))?;
        }
        // Retain originals until every close succeeds, including across a
        // dropped close observer. Never abort a stack send/receive task.
        let connections = self
            .connections
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for connection in connections {
            connection
                .close()
                .await
                .map_err(|_| BindError::ConnectFailed)?;
        }
        self.connections.lock().await.clear();
        self.datagram.take();
        self.endpoint.take();
        // The library retires transport loops asynchronously. Observe their
        // final references before acknowledging socket release. A timeout
        // retains this host's stop fence; it never authorizes a replacement.
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            loop {
                if self
                    .sockets
                    .lock()
                    .await
                    .iter()
                    .all(|owner| owner.strong_count() == 0)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .map_err(|_| BindError::Endpoint(SipIoError::InUse))?;
        self.sockets.lock().await.clear();
        self.closed = true;
        Ok(())
    }
}
impl Drop for BoundSipTransport {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
