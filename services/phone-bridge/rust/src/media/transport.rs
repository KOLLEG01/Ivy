//! One call's owned RTP/RTCP socket pair. The media session owns codec, peer
//! negotiation, queue/generation policy and RTCP semantics above this channel.
use super::{RtpHeader, RtpPacket, RtpReceiveQueue};
use crate::sip::same_ip;
use std::{
    net::{SocketAddr, UdpSocket as StdUdpSocket},
    sync::{Arc, Mutex, OnceLock},
};
use tokio::{net::UdpSocket, sync::Notify};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtpTransportError {
    InvalidBinding,
    InvalidPacket,
    Closed,
    PeerUnavailable,
    AddressInUse,
    Io,
    WouldBlock,
}
impl std::fmt::Display for RtpTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for RtpTransportError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtpPeer {
    rtp: SocketAddr,
    rtcp: SocketAddr,
}
impl RtpPeer {
    pub fn new(rtp: SocketAddr, rtcp: SocketAddr) -> Result<Self, RtpTransportError> {
        if rtp.port() == 0 || rtcp.port() == 0 {
            return Err(RtpTransportError::PeerUnavailable);
        }
        Ok(Self { rtp, rtcp })
    }
    pub fn rtp(self) -> SocketAddr {
        self.rtp
    }
    pub fn rtcp(self) -> SocketAddr {
        self.rtcp
    }
}

/// Runs on the owned receive task, outside the channel lock, without an extra
/// PCM/datagram queue. The sink must finish bounded synchronous work and must
/// not retain a strong transport handle; the session retains it separately.
/// Close waits for an already running sink, including when its observer leaves.
pub trait RtpDatagramSink: Send + Sync + 'static {
    fn rtp(&self, source: SocketAddr, packet: RtpPacket<'_>);
    fn rtcp(&self, source: SocketAddr, datagram: &[u8]);
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RtpTransportCounters {
    pub rtp_received: u64,
    pub rtcp_received: u64,
    pub rtp_sent: u64,
    pub rtcp_sent: u64,
    pub foreign_rtp: u64,
    pub foreign_rtcp: u64,
    pub malformed_rtp: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtpTransportObservation {
    pub local_rtp: SocketAddr,
    pub local_rtcp: SocketAddr,
    pub peer: Option<RtpPeer>,
    pub closed: bool,
    pub failed: bool,
    pub cleanup_confirmed: bool,
    pub counters: RtpTransportCounters,
}
struct ReceiveSockets {
    rtp: Arc<UdpSocket>,
    rtcp: Arc<UdpSocket>,
}
struct State {
    sockets: Option<ReceiveSockets>,
    peer: Option<RtpPeer>,
    closed: bool,
    failed: bool,
    counters: RtpTransportCounters,
}
struct Inner {
    local_rtp: SocketAddr,
    local_rtcp: SocketAddr,
    state: Mutex<State>,
    stop: CancellationToken,
    closed: OnceLock<()>,
    changed: Notify,
}
impl Inner {
    fn request_close(&self) {
        self.stop.cancel();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.closed = true;
        // The receive task retains the only remaining socket references until
        // callbacks and actual I/O have quiesced. No caller receives a socket.
        state.sockets.take();
    }
}
struct Handle {
    inner: Arc<Inner>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.inner.request_close();
    }
}
#[derive(Clone)]
pub struct RtpTransport {
    handle: Arc<Handle>,
}
impl RtpTransport {
    /// Binds an even RTP port and its following RTCP port within the configured
    /// inclusive range. Partial pairs are dropped before the next attempt.
    /// This local setup runs inside the session's independently owned admission.
    pub fn bind(
        interface: SocketAddr,
        first: u16,
        last: u16,
        sink: Arc<dyn RtpDatagramSink>,
    ) -> Result<Self, RtpTransportError> {
        if interface.port() != 0
            || first < 1024
            || last > 65534
            || first > last
            || first % 2 != 0
            || last % 2 != 0
        {
            return Err(RtpTransportError::InvalidBinding);
        }
        let mut selected = None;
        for port in (first..=last).step_by(2) {
            let mut rtp_addr = interface;
            rtp_addr.set_port(port);
            let mut rtcp_addr = interface;
            rtcp_addr.set_port(port + 1);
            let rtp = match StdUdpSocket::bind(rtp_addr) {
                Ok(socket) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(_) => return Err(RtpTransportError::Io),
            };
            let rtcp = match StdUdpSocket::bind(rtcp_addr) {
                Ok(socket) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(_) => return Err(RtpTransportError::Io),
            };
            rtp.set_nonblocking(true)
                .map_err(|_| RtpTransportError::Io)?;
            rtcp.set_nonblocking(true)
                .map_err(|_| RtpTransportError::Io)?;
            selected = Some(ReceiveSockets {
                rtp: Arc::new(UdpSocket::from_std(rtp).map_err(|_| RtpTransportError::Io)?),
                rtcp: Arc::new(UdpSocket::from_std(rtcp).map_err(|_| RtpTransportError::Io)?),
            });
            break;
        }
        let sockets = selected.ok_or(RtpTransportError::AddressInUse)?;
        let inner = Arc::new(Inner {
            local_rtp: sockets
                .rtp
                .local_addr()
                .map_err(|_| RtpTransportError::Io)?,
            local_rtcp: sockets
                .rtcp
                .local_addr()
                .map_err(|_| RtpTransportError::Io)?,
            state: Mutex::new(State {
                sockets: Some(ReceiveSockets {
                    rtp: sockets.rtp.clone(),
                    rtcp: sockets.rtcp.clone(),
                }),
                peer: None,
                closed: false,
                failed: false,
                counters: Default::default(),
            }),
            stop: CancellationToken::new(),
            closed: OnceLock::new(),
            changed: Notify::new(),
        });
        let task_inner = inner.clone();
        let worker = tokio::spawn(async move { receive(task_inner, sockets, sink).await });
        let completion = inner.clone();
        tokio::spawn(async move {
            let failed = !matches!(worker.await, Ok(Ok(())));
            completion.request_close();
            completion
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .failed |= failed;
            let _ = completion.closed.set(());
            completion.changed.notify_waiters();
        });
        Ok(Self {
            handle: Arc::new(Handle { inner }),
        })
    }
    pub fn observation(&self) -> RtpTransportObservation {
        let inner = &self.handle.inner;
        let state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        RtpTransportObservation {
            local_rtp: inner.local_rtp,
            local_rtcp: inner.local_rtcp,
            peer: state.peer,
            closed: state.closed,
            failed: state.failed,
            cleanup_confirmed: inner.closed.get().is_some(),
            counters: state.counters.clone(),
        }
    }
    pub fn set_peer(&self, peer: Option<RtpPeer>) -> Result<(), RtpTransportError> {
        let mut state = self
            .handle
            .inner
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if state.closed {
            return Err(RtpTransportError::Closed);
        }
        state.peer = peer;
        Ok(())
    }
    pub fn send_rtp(&self, header: RtpHeader, payload: &[u8]) -> Result<(), RtpTransportError> {
        if payload.is_empty() || payload.len() > RtpReceiveQueue::MAXIMUM_PAYLOAD {
            return Err(RtpTransportError::InvalidPacket);
        }
        let mut datagram = [0_u8; RtpReceiveQueue::MAXIMUM_PAYLOAD + RtpHeader::BYTES];
        let size = header
            .write(payload, &mut datagram)
            .map_err(|_| RtpTransportError::InvalidPacket)?;
        let result = self.send(&datagram[..size], false);
        datagram.fill(0);
        result
    }
    /// RTCP composition/report semantics are owned by the session, independently
    /// from RTP audio authority. This channel only retains the negotiated peer.
    pub fn send_rtcp(&self, datagram: &[u8]) -> Result<(), RtpTransportError> {
        if datagram.len() < 4 || datagram.len() > 65535 {
            return Err(RtpTransportError::InvalidPacket);
        }
        self.send(datagram, true)
    }
    fn send(&self, datagram: &[u8], control: bool) -> Result<(), RtpTransportError> {
        let mut state = self
            .handle
            .inner
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if state.closed {
            return Err(RtpTransportError::Closed);
        }
        let peer = state.peer.ok_or(RtpTransportError::PeerUnavailable)?;
        let sockets = state.sockets.as_ref().ok_or(RtpTransportError::Closed)?;
        let (socket, destination) = if control {
            (&sockets.rtcp, peer.rtcp)
        } else {
            (&sockets.rtp, peer.rtp)
        };
        // A synchronous nonblocking send and close share this short lock. No
        // packet can be newly dispatched after request_close has returned.
        let sent = send_to(socket, datagram, destination).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                RtpTransportError::WouldBlock
            } else {
                RtpTransportError::Io
            }
        })?;
        if sent != datagram.len() {
            return Err(RtpTransportError::Io);
        }
        if control {
            state.counters.rtcp_sent += 1;
        } else {
            state.counters.rtp_sent += 1;
        }
        Ok(())
    }
    pub fn request_close(&self) {
        self.handle.inner.request_close();
    }
    /// Cancelling this observer never aborts the original receive/cleanup owner.
    pub async fn close(&self) {
        self.request_close();
        self.wait_closed().await;
    }
    /// Observe original I/O quiescence without requesting a new close. A media
    /// supervisor uses this to detect receive/callback failure even while idle.
    pub async fn wait_closed(&self) {
        let inner = &self.handle.inner;
        loop {
            let changed = inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if inner.closed.get().is_some() {
                return;
            }
            changed.await;
        }
    }
}
fn same_endpoint(left: SocketAddr, right: SocketAddr) -> bool {
    left.port() == right.port() && same_ip(left, right)
}
fn send_to(socket: &UdpSocket, datagram: &[u8], destination: SocketAddr) -> std::io::Result<usize> {
    // Borrow the original nonblocking socket only for this synchronous send.
    // No duplicate descriptor can extend a Windows bind past close. The caller
    // holds its Arc and lifecycle lock; this temporary cannot escape or close it.
    #[cfg(windows)]
    let borrowed = {
        use std::os::windows::io::{AsRawSocket, FromRawSocket};
        // SAFETY: socket remains open for the call; ManuallyDrop never owns or
        // closes its descriptor. send_to borrows the wrapper and retains no I/O.
        std::mem::ManuallyDrop::new(unsafe {
            StdUdpSocket::from_raw_socket(socket.as_raw_socket())
        })
    };
    #[cfg(unix)]
    let borrowed = {
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: same bounded borrow as the Windows branch above.
        std::mem::ManuallyDrop::new(unsafe { StdUdpSocket::from_raw_fd(socket.as_raw_fd()) })
    };
    borrowed.send_to(datagram, destination)
}
async fn receive(
    inner: Arc<Inner>,
    sockets: ReceiveSockets,
    sink: Arc<dyn RtpDatagramSink>,
) -> Result<(), RtpTransportError> {
    let mut rtp = vec![0_u8; 65536];
    let mut rtcp = vec![0_u8; 65536];
    let result = loop {
        let (control, size, source) = tokio::select! {
            _=inner.stop.cancelled()=>break Ok(()),
            received=sockets.rtp.recv_from(&mut rtp)=>match received {Ok((size,source))=>(false,size,source),Err(_)=>break Err(RtpTransportError::Io)},
            received=sockets.rtcp.recv_from(&mut rtcp)=>match received {Ok((size,source))=>(true,size,source),Err(_)=>break Err(RtpTransportError::Io)},
        };
        // Parse without any retained payload or callback under the lifecycle lock.
        let packet = if control {
            None
        } else {
            RtpPacket::parse(&rtp[..size]).ok()
        };
        {
            let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.closed {
                break Ok(());
            }
            if control {
                state.counters.rtcp_received += 1;
            } else {
                state.counters.rtp_received += 1;
            }
            let accepted = state.peer.is_some_and(|peer| {
                same_endpoint(source, if control { peer.rtcp } else { peer.rtp })
            });
            if !accepted {
                if control {
                    state.counters.foreign_rtcp += 1;
                } else {
                    state.counters.foreign_rtp += 1;
                }
                continue;
            }
            if !control && packet.is_none() {
                state.counters.malformed_rtp += 1;
                continue;
            }
        }
        if inner.stop.is_cancelled() {
            break Ok(());
        }
        if control {
            sink.rtcp(source, &rtcp[..size]);
            rtcp[..size].fill(0);
        } else {
            sink.rtp(source, packet.unwrap());
            rtp[..size].fill(0);
        }
    };
    rtp.fill(0);
    rtcp.fill(0);
    result
}
