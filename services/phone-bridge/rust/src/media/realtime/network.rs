//! One bound socket per advertised host candidate: every received datagram has
//! its actual local destination and every send uses its selected source.
use super::{RealtimeError, Result};
use std::{
    io,
    net::{SocketAddr, UdpSocket},
};
use str0m::net::{Protocol, Transmit};

pub(super) trait Datagrams: Send {
    fn candidates(&self) -> Vec<SocketAddr>;
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<(usize, SocketAddr, SocketAddr)>>;
    /// Bounded nonblocking publication; never call back into route/PCM control.
    fn send(&mut self, packet: Transmit) -> Result<()>;
}
pub(super) struct Sockets {
    sockets: Vec<(SocketAddr, UdpSocket)>,
    cursor: usize,
}
impl Sockets {
    pub fn system() -> Result<Box<dyn Datagrams>> {
        Self::bind(addresses()?).map(|s| Box::new(s) as Box<dyn Datagrams>)
    }
    pub fn bind(addresses: Vec<SocketAddr>) -> Result<Self> {
        let mut sockets = Vec::new();
        for address in addresses {
            if address.ip().is_unspecified() || address.ip().is_multicast() {
                return Err(RealtimeError::InvalidRequest);
            }
            // Each successfully opened socket remains owned if a later bind
            // fails; stack unwinding releases the entire partial preparation.
            let socket = UdpSocket::bind(address).map_err(|_| RealtimeError::Failed)?;
            socket
                .set_nonblocking(true)
                .map_err(|_| RealtimeError::Failed)?;
            let local = socket.local_addr().map_err(|_| RealtimeError::Failed)?;
            sockets.push((local, socket));
        }
        if sockets.is_empty() {
            return Err(RealtimeError::Failed);
        }
        Ok(Self { sockets, cursor: 0 })
    }
}
impl Datagrams for Sockets {
    fn candidates(&self) -> Vec<SocketAddr> {
        self.sockets.iter().map(|(address, _)| *address).collect()
    }
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<(usize, SocketAddr, SocketAddr)>> {
        // Round-robin sockets and a bounded batch in the driver prevent a busy
        // interface from starving control, audio deadlines or another family.
        for _ in 0..self.sockets.len() {
            let (local, socket) = &self.sockets[self.cursor];
            self.cursor = (self.cursor + 1) % self.sockets.len();
            match socket.recv_from(buffer) {
                Ok((length, remote)) => return Ok(Some((length, remote, *local))),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(RealtimeError::Failed),
            }
        }
        Ok(None)
    }
    fn send(&mut self, packet: Transmit) -> Result<()> {
        if packet.proto != Protocol::Udp {
            return Err(RealtimeError::Failed);
        }
        let (_, socket) = self
            .sockets
            .iter()
            .find(|(local, _)| *local == packet.source)
            .ok_or(RealtimeError::Failed)?;
        match socket.send_to(&packet.contents, packet.destination) {
            Ok(length) if length == packet.contents.len() => Ok(()),
            _ => Err(RealtimeError::Failed),
        }
    }
}

#[cfg(windows)]
fn addresses() -> Result<Vec<SocketAddr>> {
    use windows_sys::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR},
        NetworkManagement::IpHelper::{
            GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
            GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
        },
        Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6},
    };
    // The documented initial 15KiB buffer is aligned for IP_ADAPTER_ADDRESSES.
    // Adapter churn may grow it; only the OS-reported size is retried.
    let mut bytes = 15 * 1024u32;
    for _ in 0..3 {
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
        let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let result = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                std::ptr::null(),
                first,
                &mut bytes,
            )
        };
        if result == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if result != NO_ERROR {
            return Err(RealtimeError::Failed);
        }
        let mut values = Vec::new();
        let mut adapter = first;
        // SAFETY: the successful OS call supplies linked nodes and sockaddr
        // records wholly inside the still-owned, suitably aligned buffer.
        unsafe {
            while !adapter.is_null() {
                if (*adapter).OperStatus == 1 {
                    let mut address = (*adapter).FirstUnicastAddress;
                    while !address.is_null() {
                        let socket = (*address).Address;
                        if !socket.lpSockaddr.is_null() {
                            let value = match (*socket.lpSockaddr).sa_family {
                                AF_INET
                                    if socket.iSockaddrLength as usize
                                        >= std::mem::size_of::<SOCKADDR_IN>() =>
                                {
                                    let socket = &*socket.lpSockaddr.cast::<SOCKADDR_IN>();
                                    Some(SocketAddr::from((
                                        socket.sin_addr.S_un.S_addr.to_ne_bytes(),
                                        0,
                                    )))
                                }
                                AF_INET6
                                    if socket.iSockaddrLength as usize
                                        >= std::mem::size_of::<SOCKADDR_IN6>() =>
                                {
                                    let socket = &*socket.lpSockaddr.cast::<SOCKADDR_IN6>();
                                    Some(SocketAddr::V6(std::net::SocketAddrV6::new(
                                        socket.sin6_addr.u.Byte.into(),
                                        0,
                                        0,
                                        socket.Anonymous.sin6_scope_id,
                                    )))
                                }
                                _ => None,
                            };
                            if let Some(value) = value {
                                if !value.ip().is_unspecified()
                                    && !value.ip().is_multicast()
                                    && !values.contains(&value)
                                {
                                    values.push(value);
                                }
                            }
                        }
                        address = (*address).Next;
                    }
                }
                adapter = (*adapter).Next;
            }
        }
        return Ok(values);
    }
    Err(RealtimeError::Failed)
}
#[cfg(not(windows))]
fn addresses() -> Result<Vec<SocketAddr>> {
    // The native companion's Windows implementation has no foreign-host
    // enumeration substitute. Explicit bindings remain portable for tests.
    Err(RealtimeError::NotReady)
}
