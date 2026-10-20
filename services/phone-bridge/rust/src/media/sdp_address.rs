//! SDK address selection for the native SIP host's always-present binding.
//! SIPSorcery 10.0.15 notice: native/phone-runtime/licenses/SIPSorcery-10.0.15.md.
use super::MediaError;
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, SocketAddr},
    sync::{Mutex, OnceLock},
};

/// A route lookup sends no datagram and preserves an IPv6 numeric scope.
/// Providers must not re-enter call control or retain a media socket.
pub trait SipSdpRoute: Send + Sync + 'static {
    fn local_address(&self, remote: SocketAddr) -> Result<Option<SocketAddr>, MediaError>;
}

#[derive(Default)]
struct Cache(HashMap<SocketAddr, (SocketAddr, i64)>);
impl Cache {
    fn get(&mut self, remote: SocketAddr, now: i64) -> Option<SocketAddr> {
        let &(local, recorded) = self.0.get(&remote)?;
        // The SDK compares DateTime.Now, including a clock moving backwards.
        if now - recorded >= 300 * 10_000_000 {
            self.0.remove(&remote);
            None
        } else {
            Some(local)
        }
    }
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn probe_address<S>(
    socket: io::Result<S>,
    destination: SocketAddr,
    connect: impl FnOnce(&S, SocketAddr) -> io::Result<()>,
    local_address: impl FnOnce(&S) -> io::Result<Option<SocketAddr>>,
) -> Result<Option<SocketAddr>, MediaError> {
    // UdpClient construction is outside the SDK's SocketException catch.
    // Connect includes the implicit local bind; only that and endpoint lookup
    // are allowed to report a missing route and use the configured binding.
    let socket = socket.map_err(|_| MediaError::AddressUnavailable)?;
    if connect(&socket, destination).is_err() {
        return Ok(None);
    }
    let Ok(Some(mut local)) = local_address(&socket) else {
        return Ok(None);
    };
    local.set_port(0);
    Ok(Some(local))
}

fn cache_time() -> Result<i64, MediaError> {
    #[cfg(windows)]
    {
        use super::RtcpRuntime;
        Ok(super::WindowsRtcpRuntime.now()?.local_ticks())
    }
    #[cfg(not(windows))]
    {
        let ticks = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map_err(|_| MediaError::ClockUnavailable)?
            .as_nanos()
            / 100;
        i64::try_from(ticks)
            .ok()
            .and_then(|ticks| ticks.checked_add(super::RtcpWallTime::UNIX_TICKS))
            .ok_or(MediaError::ClockUnavailable)
    }
}

/// Like the SDK's static NetworkAddressChanged subscription, this one
/// process-lifetime registration invalidates the shared successful-route cache.
#[cfg(windows)]
fn watch_addresses() -> Result<(), MediaError> {
    static WATCH: OnceLock<Result<usize, u32>> = OnceLock::new();
    unsafe extern "system" fn changed(_: *mut std::ffi::c_void, _: *mut std::ffi::c_void, _: i32) {
        if let Some(cache) = CACHE.get() {
            cache.lock().unwrap_or_else(|p| p.into_inner()).0.clear();
        }
    }
    #[link(name = "iphlpapi")]
    unsafe extern "system" {
        fn NotifyUnicastIpAddressChange(
            family: u16,
            callback: unsafe extern "system" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, i32),
            context: *mut std::ffi::c_void,
            initial: u8,
            handle: *mut *mut std::ffi::c_void,
        ) -> u32;
    }
    WATCH
        .get_or_init(|| {
            let mut handle = std::ptr::null_mut();
            // SAFETY: AF_UNSPEC watches both families. Callback and cache are
            // process-static; callback ignores borrowed OS rows and has no context.
            // The OS deregisters this static SDK-equivalent watch at process exit.
            let code = unsafe {
                NotifyUnicastIpAddressChange(0, changed, std::ptr::null_mut(), 0, &mut handle)
            };
            if code == 0 {
                Ok(handle as usize)
            } else {
                Err(code)
            }
        })
        .as_ref()
        .map(|_| ())
        .map_err(|_| MediaError::AddressUnavailable)
}

#[derive(Default)]
pub struct SystemSipSdpRoute;
impl SipSdpRoute for SystemSipSdpRoute {
    fn local_address(&self, mut remote: SocketAddr) -> Result<Option<SocketAddr>, MediaError> {
        if remote.ip().is_unspecified() {
            return Ok(None);
        }
        remote.set_port(0);
        let cache = CACHE.get_or_init(|| Mutex::new(Cache::default()));
        #[cfg(windows)]
        watch_addresses()?;
        if let Some(local) = cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(remote, cache_time()?)
        {
            return Ok(Some(local));
        }
        let mut destination = remote;
        if let SocketAddr::V6(v6) = destination {
            if let Some(ip) = v6.ip().to_ipv4_mapped() {
                destination = SocketAddr::new(IpAddr::V4(ip), 0);
            }
        }
        destination.set_port(5060);
        let domain = if destination.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let Some(local) = probe_address(
            Socket::new(domain, Type::DGRAM, Some(Protocol::UDP)),
            destination,
            |socket, remote| socket.connect(&remote.into()),
            |socket| socket.local_addr().map(|address| address.as_socket()),
        )?
        else {
            return Ok(None);
        };
        cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .0
            .entry(remote)
            .or_insert((local, cache_time()?));
        Ok(Some(local))
    }
}

pub(super) fn advertised_address(
    binding: SocketAddr,
    offer: Option<SocketAddr>,
    routes: &dyn SipSdpRoute,
) -> Result<SocketAddr, MediaError> {
    let route = match offer.filter(|address| !address.ip().is_unspecified()) {
        Some(address) => routes.local_address(address)?,
        None => None,
    };
    Ok(route
        .filter(|address| !address.ip().is_unspecified())
        .unwrap_or(binding))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn route_probe_separates_constructor_connect_bind_and_endpoint_failures() {
        use ivy_contracts::json::{IvyJson, stringify};
        use std::{cell::RefCell, rc::Rc};
        struct Probe(Rc<RefCell<Vec<&'static str>>>);
        impl Drop for Probe {
            fn drop(&mut self) {
                self.0.borrow_mut().push("dispose");
            }
        }
        let mut rows = Vec::new();
        for (family, remote, local) in [
            ("v4", "127.0.0.2:5060", "127.0.0.1:49152"),
            ("v6", "[fe80::2%17]:5060", "[fe80::1%17]:49152"),
        ] {
            let destination: SocketAddr = remote.parse().unwrap();
            let local: SocketAddr = local.parse().unwrap();
            for stage in [
                "construct_socket",
                "connect_socket",
                "implicit_bind_socket",
                "local_socket",
                "local_missing",
                "success",
            ] {
                let events = Rc::new(RefCell::new(vec!["construct"]));
                let socket = if stage == "construct_socket" {
                    Err(io::Error::from_raw_os_error(10055))
                } else {
                    Ok(Probe(events.clone()))
                };
                let result = probe_address(
                    socket,
                    destination,
                    |socket, selected| {
                        assert_eq!(selected, destination);
                        socket.0.borrow_mut().push("connect");
                        if stage == "connect_socket" {
                            return Err(io::Error::from_raw_os_error(10051));
                        }
                        socket.0.borrow_mut().push("implicit-bind");
                        if stage == "implicit_bind_socket" {
                            return Err(io::Error::from_raw_os_error(10048));
                        }
                        Ok(())
                    },
                    |socket| {
                        socket.0.borrow_mut().push("local");
                        match stage {
                            "local_socket" => Err(io::Error::from_raw_os_error(10022)),
                            "local_missing" => Ok(None),
                            _ => Ok(Some(local)),
                        }
                    },
                );
                let mut advertised = local;
                advertised.set_port(0);
                let expected = match stage {
                    "construct_socket" => Err(MediaError::AddressUnavailable),
                    "success" => Ok(Some(advertised)),
                    _ => Ok(None),
                };
                assert_eq!(result, expected, "{family}/{stage}");
                let expected_events: &[&str] = match stage {
                    "construct_socket" => &["construct"],
                    "connect_socket" => &["construct", "connect", "dispose"],
                    "implicit_bind_socket" => &["construct", "connect", "implicit-bind", "dispose"],
                    _ => &["construct", "connect", "implicit-bind", "local", "dispose"],
                };
                assert_eq!(events.borrow().as_slice(), expected_events);
                rows.push(IvyJson::object([
                    ("family", family.into()),
                    ("stage", stage.into()),
                    (
                        "state",
                        match result {
                            Err(_) => "error",
                            Ok(None) => "none",
                            Ok(Some(_)) => "address",
                        }
                        .into(),
                    ),
                    (
                        "local",
                        result
                            .ok()
                            .flatten()
                            .map(|address| address.to_string().into())
                            .unwrap_or(IvyJson::Null),
                    ),
                    (
                        "events",
                        IvyJson::Array(
                            events
                                .borrow()
                                .iter()
                                .map(|event| (*event).into())
                                .collect(),
                        ),
                    ),
                ]));
            }
        }
        if let Ok(path) = std::env::var("IVY_PHONE_ROUTE_FAILURE_TRACE") {
            std::fs::write(path, stringify(&IvyJson::Array(rows), 65536).unwrap()).unwrap();
        }
    }
    #[test]
    fn route_cache_expires_at_five_minutes_and_retains_backwards_clock_policy() {
        let remote = "127.0.0.2:0".parse().unwrap();
        let local = "127.0.0.1:0".parse().unwrap();
        let now = 1000 * 10_000_000;
        let mut cache = Cache::default();
        cache.0.insert(remote, (local, now));
        assert_eq!(cache.get(remote, now - 10_000_000), Some(local));
        assert_eq!(cache.get(remote, now + 299 * 10_000_000), Some(local));
        assert_eq!(cache.get(remote, now + 300 * 10_000_000), None);
        assert!(cache.0.is_empty());
    }
    #[test]
    fn scope_and_configured_binding_survive_missing_or_unspecified_route() {
        struct Route(Option<SocketAddr>);
        impl SipSdpRoute for Route {
            fn local_address(&self, _: SocketAddr) -> Result<Option<SocketAddr>, MediaError> {
                Ok(self.0)
            }
        }
        let bound = "[fe80::1%12]:0".parse().unwrap();
        let remote = "[fe80::2%17]:0".parse().unwrap();
        let routed = "[fe80::3%17]:0".parse().unwrap();
        assert_eq!(
            advertised_address(bound, None, &Route(Some(routed))),
            Ok(bound)
        );
        assert_eq!(
            advertised_address(bound, Some(remote), &Route(None)),
            Ok(bound)
        );
        assert_eq!(
            advertised_address(bound, Some(remote), &Route(Some("[::]:0".parse().unwrap()))),
            Ok(bound)
        );
        assert_eq!(
            advertised_address(bound, Some(remote), &Route(Some(routed))),
            Ok(routed)
        );
        assert_eq!(
            advertised_address(
                bound,
                Some("0.0.0.0:0".parse().unwrap()),
                &Route(Some(routed))
            ),
            Ok(bound)
        );
        struct Failed;
        impl SipSdpRoute for Failed {
            fn local_address(&self, _: SocketAddr) -> Result<Option<SocketAddr>, MediaError> {
                Err(MediaError::AddressUnavailable)
            }
        }
        assert_eq!(
            advertised_address(bound, Some(remote), &Failed),
            Err(MediaError::AddressUnavailable)
        );
        let mut local = bound;
        local.set_port(50000);
        let identity = crate::media::SdpIdentity::new(local, 1, 0, 17, "fixture").unwrap();
        let document = crate::media::AudioNegotiator::new(&[crate::media::Codec::Pcma])
            .unwrap()
            .create_offer(&identity);
        assert_eq!(document.origin().address_type(), "IP4");
        assert_eq!(document.origin().address(), Some("127.0.0.1"));
        assert_eq!(document.connection().unwrap().address_type(), "IP6");
        assert_eq!(document.connection().unwrap().address(), "fe80::1%12");
    }
}
