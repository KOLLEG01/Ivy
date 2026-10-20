//! SDK address selection for the native SIP host's always-present binding.
//! SIPSorcery 10.0.15 notice: native/phone-runtime/licenses/SIPSorcery-10.0.15.md.
use super::MediaError;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime},
};

/// A route lookup sends no datagram and preserves an IPv6 numeric scope.
/// Providers must not re-enter call control or retain a media socket.
pub trait SipSdpRoute: Send + Sync + 'static {
    fn local_address(&self, remote: SocketAddr) -> Result<Option<SocketAddr>, MediaError>;
}

#[derive(Default)]
struct Cache(HashMap<SocketAddr, (SocketAddr, SystemTime)>);
impl Cache {
    fn get(&mut self, remote: SocketAddr, now: SystemTime) -> Option<SocketAddr> {
        let &(local, recorded) = self.0.get(&remote)?;
        // The SDK compares DateTime.Now, including a clock moving backwards.
        if now
            .duration_since(recorded)
            .is_ok_and(|age| age >= Duration::from_secs(300))
        {
            self.0.remove(&remote);
            None
        } else {
            Some(local)
        }
    }
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

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
            .get(remote, SystemTime::now())
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
        let any = if destination.is_ipv4() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        };
        let socket =
            UdpSocket::bind(SocketAddr::new(any, 0)).map_err(|_| MediaError::AddressUnavailable)?;
        if socket.connect(destination).is_err() {
            return Ok(None);
        }
        let Ok(mut local) = socket.local_addr() else {
            return Ok(None);
        };
        local.set_port(0);
        cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .0
            .entry(remote)
            .or_insert((local, SystemTime::now()));
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
    fn route_cache_expires_at_five_minutes_and_retains_backwards_clock_policy() {
        let remote = "127.0.0.2:0".parse().unwrap();
        let local = "127.0.0.1:0".parse().unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let mut cache = Cache::default();
        cache.0.insert(remote, (local, now));
        assert_eq!(cache.get(remote, now - Duration::from_secs(1)), Some(local));
        assert_eq!(
            cache.get(remote, now + Duration::from_secs(299)),
            Some(local)
        );
        assert_eq!(cache.get(remote, now + Duration::from_secs(300)), None);
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
