//! Original UDP channel admission, deferred completion and shared failure cache.
use super::RegistrationKeepaliveError;
use rsipstack::{platform::net::UdpSocket as StackSocket, sip::Transport, transport::SipAddr};
use std::{
    collections::HashMap,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{net::UdpSocket, runtime::Handle, sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;

type LocalClock = Arc<dyn Fn() -> Result<i64, ()> + Send + Sync>;
const FAILURE_TICKS: i64 = 300_000_000;

fn canonical(peer: SocketAddr) -> SocketAddr {
    match peer {
        SocketAddr::V6(v6) => v6
            .ip()
            .to_ipv4_mapped()
            .map(|ip| SocketAddr::new(ip.into(), v6.port()))
            .unwrap_or_else(|| {
                std::net::SocketAddrV6::new(*v6.ip(), v6.port(), 0, v6.scope_id()).into()
            }),
        _ => peer,
    }
}

#[derive(Default)]
pub(super) struct FailedDestinations(Mutex<HashMap<SocketAddr, i64>>);
impl FailedDestinations {
    fn shared() -> Arc<Self> {
        static CACHE: OnceLock<Arc<FailedDestinations>> = OnceLock::new();
        CACHE.get_or_init(|| Arc::new(Self::default())).clone()
    }
    fn contains(&self, peer: SocketAddr) -> bool {
        self.0.lock().unwrap().contains_key(&canonical(peer))
    }
    fn receive_error(&self, peer: Option<SocketAddr>, now: &LocalClock) -> Result<(), ()> {
        let Some(peer) = peer.filter(|p| p.port() != 0) else {
            return Ok(());
        };
        let now = now()?;
        // TryAdd preserves the first failure, including across channel owners.
        self.0.lock().unwrap().entry(canonical(peer)).or_insert(now);
        Ok(())
    }
    fn expired(&self, now: &LocalClock) -> Result<Vec<SocketAddr>, ()> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .try_fold(Vec::new(), |mut keys, (peer, at)| {
                if now()?.checked_sub(*at).ok_or(())? > FAILURE_TICKS {
                    keys.push(*peer);
                }
                Ok(keys)
            })
    }
    fn remove(&self, keys: Vec<SocketAddr>) {
        // The original snapshots keys then TryRemove's them unconditionally.
        let mut entries = self.0.lock().unwrap();
        for key in keys {
            entries.remove(&key);
        }
    }
}

#[derive(Default)]
struct Pending {
    closed: bool,
    count: usize,
    #[cfg(test)]
    completions: Vec<Option<Result<usize, Option<i32>>>>,
}
#[derive(Default)]
struct Sends {
    state: Mutex<Pending>,
    changed: Notify,
}
struct SendLease(Arc<Sends>);
impl Drop for SendLease {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().count -= 1;
        self.0.changed.notify_waiters();
    }
}

/// The stack, signalling flow and keepalive all retain this one socket adapter.
/// Completion workers own only the original socket and their retirement lease.
pub(super) struct SipUdpSocket {
    socket: Arc<UdpSocket>,
    local: SocketAddr,
    runtime: Handle,
    failures: Arc<FailedDestinations>,
    clock: LocalClock,
    sends: Arc<Sends>,
    stop: CancellationToken,
    sweep: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}
impl SipUdpSocket {
    pub(super) fn new(socket: Arc<UdpSocket>) -> Result<Arc<Self>, std::io::Error> {
        Self::with_policy(socket, FailedDestinations::shared(), Arc::new(local_ticks))
    }
    fn with_policy(
        socket: Arc<UdpSocket>,
        failures: Arc<FailedDestinations>,
        clock: LocalClock,
    ) -> Result<Arc<Self>, std::io::Error> {
        let local = socket.local_addr()?;
        let runtime = Handle::current();
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let cache = failures.clone();
        let time = clock.clone();
        let sweep = runtime.spawn(async move {
            loop {
                if stopped.is_cancelled() {
                    break;
                }
                let Ok(keys) = cache.expired(&time) else {
                    break;
                };
                cache.remove(keys);
                tokio::select! { biased;
                    _ = stopped.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
            }
        });
        Ok(Arc::new(Self {
            socket,
            local,
            runtime,
            failures,
            clock,
            sends: Arc::new(Sends::default()),
            stop,
            sweep: tokio::sync::Mutex::new(Some(sweep)),
        }))
    }
    pub(super) fn begin_send(
        &self,
        bytes: &[u8],
        destination: SocketAddr,
    ) -> Result<(), RegistrationKeepaliveError> {
        if bytes.is_empty() {
            return Err(RegistrationKeepaliveError::SendFailed);
        }
        let destination = canonical(destination);
        // Cache membership precedes the disposed-socket check in SendAsync.
        if self.failures.contains(destination) {
            return Err(RegistrationKeepaliveError::SocketError(10061));
        }
        let (lease, attempt) = {
            let mut pending = self.sends.state.lock().unwrap();
            if pending.closed {
                return Err(RegistrationKeepaliveError::SocketError(10101));
            }
            pending.count += 1;
            (
                SendLease(self.sends.clone()),
                try_send(&self.socket, bytes, destination),
            )
        };
        if !matches!(&attempt, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock) {
            #[cfg(test)]
            lease
                .0
                .state
                .lock()
                .unwrap()
                .completions
                .push(Some(attempt.map_err(|e| e.raw_os_error())));
            drop(lease);
            return Ok(());
        }
        self.defer_send(bytes.to_vec(), destination, lease);
        Ok(())
    }
    fn defer_send(&self, bytes: Vec<u8>, destination: SocketAddr, lease: SendLease) {
        let socket = self.socket.clone();
        let stop = self.stop.clone();
        self.runtime.spawn(async move {
            let completion = tokio::select! { biased;
                _ = stop.cancelled() => None,
                result = socket.send_to(&bytes, destination) => Some(result)
            };
            // EndSendTo failures do not change admission, registration
            // diagnostics, or the receive-populated failed-destination cache.
            #[cfg(test)]
            lease
                .0
                .state
                .lock()
                .unwrap()
                .completions
                .push(completion.map(|r| r.map_err(|e| e.raw_os_error())));
            #[cfg(not(test))]
            let _ = completion;
            drop(socket);
            drop(lease);
        });
    }
    pub(super) fn request_stop(&self) {
        self.sends.state.lock().unwrap().closed = true;
        self.stop.cancel();
    }
    async fn wait_sends(&self) {
        loop {
            let changed = self.sends.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.sends.state.lock().unwrap().count == 0 {
                break;
            }
            changed.await;
        }
    }
    #[cfg(test)]
    pub(super) async fn completions(&self) -> Vec<Option<Result<usize, Option<i32>>>> {
        self.wait_sends().await;
        self.sends.state.lock().unwrap().completions.clone()
    }
    pub(super) async fn drain(&self) -> Result<(), ()> {
        self.wait_sends().await;
        let mut sweep = self.sweep.lock().await;
        if let Some(worker) = sweep.as_mut() {
            let result = worker.await;
            sweep.take();
            result.map_err(|_| ())?;
        }
        Ok(())
    }
}
impl Drop for SipUdpSocket {
    fn drop(&mut self) {
        self.request_stop();
    }
}

fn try_send(socket: &UdpSocket, bytes: &[u8], peer: SocketAddr) -> std::io::Result<usize> {
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    {
        #[repr(C, align(8))]
        struct Address([u8; 28]);
        let mut address = Address([0; 28]);
        let (family, length) = match peer {
            SocketAddr::V4(_) => (2_u16, 16),
            SocketAddr::V6(_) => {
                #[cfg(windows)]
                let family = 23_u16;
                #[cfg(target_os = "linux")]
                let family = 10_u16;
                #[cfg(target_os = "macos")]
                let family = 30_u16;
                (family, 28)
            }
        };
        #[cfg(not(target_os = "macos"))]
        address.0[..2].copy_from_slice(&family.to_ne_bytes());
        #[cfg(target_os = "macos")]
        {
            address.0[0] = length as u8;
            address.0[1] = family as u8;
        }
        address.0[2..4].copy_from_slice(&peer.port().to_be_bytes());
        match peer {
            SocketAddr::V4(v4) => address.0[4..8].copy_from_slice(&v4.ip().octets()),
            SocketAddr::V6(v6) => {
                address.0[8..24].copy_from_slice(&v6.ip().octets());
                address.0[24..28].copy_from_slice(&v6.scope_id().to_ne_bytes());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            #[link(name = "ws2_32")]
            unsafe extern "system" {
                fn sendto(
                    socket: usize,
                    bytes: *const u8,
                    length: i32,
                    flags: i32,
                    peer: *const u8,
                    peer_length: i32,
                ) -> i32;
                fn WSAGetLastError() -> i32;
            }
            let size = i32::try_from(bytes.len())
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
            // The Tokio-owned socket is nonblocking and remains borrowed for
            // this call. No duplicate handle, close, or retained pointer.
            let result = unsafe {
                sendto(
                    socket.as_raw_socket() as usize,
                    bytes.as_ptr(),
                    size,
                    0,
                    address.0.as_ptr(),
                    length,
                )
            };
            if result < 0 {
                return Err(std::io::Error::from_raw_os_error(unsafe {
                    WSAGetLastError()
                }));
            }
            Ok(result as usize)
        }
        #[cfg(unix)]
        {
            use std::{ffi::c_void, os::fd::AsRawFd};
            unsafe extern "C" {
                fn sendto(
                    socket: i32,
                    bytes: *const u8,
                    length: usize,
                    flags: i32,
                    peer: *const c_void,
                    peer_length: u32,
                ) -> isize;
            }
            // Same borrowed nonblocking descriptor; all pointers are valid
            // initialized buffers for this synchronous syscall only.
            let result = unsafe {
                sendto(
                    socket.as_raw_fd(),
                    bytes.as_ptr(),
                    bytes.len(),
                    0,
                    address.0.as_ptr().cast(),
                    length as u32,
                )
            };
            if result < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(result as usize)
        }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        socket.try_send_to(bytes, peer)
    }
}

// These signatures implement the stack's async-trait ABI without adding a
// second macro dependency or changing the Shared-owned workspace lockfile.
impl StackSocket for SipUdpSocket {
    fn recv_from<'a, 'b, 'f>(
        &'a self,
        buf: &'b mut [u8],
    ) -> Pin<Box<dyn Future<Output = rsipstack::Result<(usize, SocketAddr)>> + Send + 'f>>
    where
        'a: 'f,
        'b: 'f,
        Self: 'f,
    {
        Box::pin(async move {
            match self.socket.recv_from(buf).await {
                Ok(value) => Ok(value),
                Err(error) => {
                    // Tokio provides no peer on error. The original modern
                    // EndReceiveFrom likewise throws before updating ref peer.
                    // Never infer it from an earlier send or receive.
                    let _ = self.failures.receive_error(None, &self.clock);
                    Err(error.into())
                }
            }
        })
    }
    fn send_to<'a, 'b, 'f>(
        &'a self,
        buf: &'b [u8],
        destination: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = rsipstack::Result<usize>> + Send + 'f>>
    where
        'a: 'f,
        'b: 'f,
        Self: 'f,
    {
        let result = self
            .begin_send(buf, destination)
            .map(|_| buf.len())
            .map_err(|error| {
                rsipstack::Error::TransportLayerError(
                    format!("{error:?}"),
                    SipAddr::new(Transport::Udp, self.local.into()),
                )
            });
        Box::pin(async move { result })
    }
    fn local_addr(&self) -> rsipstack::Result<SocketAddr> {
        Ok(self.local)
    }
}

fn local_ticks() -> Result<i64, ()> {
    #[cfg(windows)]
    {
        use crate::media::{RtcpRuntime, WindowsRtcpRuntime};
        WindowsRtcpRuntime
            .now()
            .map(|time| time.local_ticks())
            .map_err(|_| ())
    }
    #[cfg(all(unix, target_pointer_width = "64"))]
    {
        use std::{
            ffi::{c_char, c_int, c_long},
            time::{SystemTime, UNIX_EPOCH},
        };
        #[repr(C)]
        struct Tm {
            sec: c_int,
            min: c_int,
            hour: c_int,
            day: c_int,
            month: c_int,
            year: c_int,
            weekday: c_int,
            yearday: c_int,
            dst: c_int,
            offset: c_long,
            zone: *const c_char,
        }
        unsafe extern "C" {
            fn localtime_r(time: *const i64, output: *mut Tm) -> *mut Tm;
        }
        let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(value) => i128::try_from(value.as_nanos()).map_err(|_| ())?,
            Err(value) => -i128::try_from(value.duration().as_nanos()).map_err(|_| ())?,
        };
        let seconds = i64::try_from(nanos.div_euclid(1_000_000_000)).map_err(|_| ())?;
        let mut local = std::mem::MaybeUninit::<Tm>::uninit();
        // Supported Unix targets use 64-bit time_t/long and the POSIX tm
        // extension layout. localtime_r retains neither pointer.
        let local = unsafe {
            if localtime_r(&seconds, local.as_mut_ptr()).is_null() {
                return Err(());
            }
            local.assume_init()
        };
        let ticks = 621_355_968_000_000_000_i128
            + nanos.div_euclid(100)
            + i128::from(local.offset) * 10_000_000;
        let ticks = i64::try_from(ticks).map_err(|_| ())?;
        (0..=3_155_378_975_999_999_999)
            .contains(&ticks)
            .then_some(ticks)
            .ok_or(())
    }
    #[cfg(not(any(windows, all(unix, target_pointer_width = "64"))))]
    {
        Err(())
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_sip_udp.rs"]
mod tests;
