//! Registration authority is a confirmed lease, measured on one monotonic clock.
//! A local stop or a successful keepalive never proves a remote registration.
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationTransport {
    Udp,
    Tcp,
    Tls,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistrationPeer {
    pub address: SocketAddr,
    pub transport: RegistrationTransport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationPhase {
    Registering,
    Registered,
    Failed,
    Stopped,
}

/// A flow is selected by a successful REGISTER response, not by configuration
/// or an untrusted incoming INVITE. Tokens fence late keepalive completions.
#[derive(Clone, Debug)]
pub struct RegistrationFlow {
    owner: Arc<()>,
    generation: u64,
    local: RegistrationPeer,
    remote: RegistrationPeer,
}
impl PartialEq for RegistrationFlow {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
            && self.generation == other.generation
            && self.local == other.local
            && self.remote == other.remote
    }
}
impl Eq for RegistrationFlow {}
impl RegistrationFlow {
    pub fn local(&self) -> RegistrationPeer {
        self.local
    }
    pub fn remote(&self) -> RegistrationPeer {
        self.remote
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistrationObservation {
    pub phase: RegistrationPhase,
    pub response_code: Option<u16>,
    pub remote_removed: Option<bool>,
    pub local: Option<RegistrationPeer>,
    pub remote: Option<RegistrationPeer>,
    pub granted_expiry_seconds: Option<u64>,
    /// UTC milliseconds, supplied by the owner only for diagnostics.
    pub last_keepalive_at: Option<i64>,
    pub keepalive_failed: bool,
}

/// All methods run under the original registration owner's serialization.
/// `now` is an offset from that owner's unchanged monotonic clock origin.
pub struct RegistrationLease {
    observation: RegistrationObservation,
    confirmed_at: Duration,
    valid_for: Duration,
    flow: Option<RegistrationFlow>,
    owner: Arc<()>,
    generation: u64,
}
impl Default for RegistrationLease {
    fn default() -> Self {
        Self {
            observation: RegistrationObservation {
                phase: RegistrationPhase::Registering,
                response_code: None,
                remote_removed: None,
                local: None,
                remote: None,
                granted_expiry_seconds: None,
                last_keepalive_at: None,
                keepalive_failed: false,
            },
            confirmed_at: Duration::ZERO,
            valid_for: Duration::ZERO,
            flow: None,
            owner: Arc::new(()),
            generation: 0,
        }
    }
}
impl RegistrationLease {
    pub(super) fn original_flow(&self) -> Option<RegistrationFlow> {
        self.flow.clone()
    }
    pub fn observation(&mut self, now: Duration) -> RegistrationObservation {
        if self.observation.phase == RegistrationPhase::Registered
            && now
                .checked_sub(self.confirmed_at)
                .is_none_or(|elapsed| elapsed >= self.valid_for)
        {
            self.observation.phase = RegistrationPhase::Failed;
        }
        self.observation
    }

    /// The transaction owner must first match a real remote final response to
    /// its original REGISTER. Synthetic stack timeout responses are failures.
    pub fn confirmed(
        &mut self,
        now: Duration,
        response_code: u16,
        granted_expiry_seconds: u64,
        local: RegistrationPeer,
        remote: RegistrationPeer,
    ) {
        if self.observation.phase == RegistrationPhase::Stopped {
            return;
        }
        if !(200..=299).contains(&response_code)
            || local.transport != remote.transport
            || local.address.port() == 0
            || remote.address.port() == 0
        {
            self.failed(Some(response_code));
            return;
        }
        // A new confirmation invalidates even a keepalive already in flight
        // on an identically spelled endpoint. Never recycle an old token.
        let Some(generation) = self.generation.checked_add(1) else {
            self.failed(None);
            return;
        };
        self.generation = generation;
        self.confirmed_at = now;
        self.valid_for = Duration::from_secs(granted_expiry_seconds);
        self.flow = Some(RegistrationFlow {
            owner: self.owner.clone(),
            generation,
            local,
            remote,
        });
        self.observation.phase = if granted_expiry_seconds > 0 {
            RegistrationPhase::Registered
        } else {
            RegistrationPhase::Failed
        };
        self.observation.response_code = Some(response_code);
        self.observation.local = Some(local);
        self.observation.remote = Some(remote);
        self.observation.granted_expiry_seconds = Some(granted_expiry_seconds);
    }

    pub fn failed(&mut self, response_code: Option<u16>) {
        if self.observation.phase != RegistrationPhase::Stopped {
            self.observation.phase = RegistrationPhase::Failed;
            self.observation.response_code = response_code;
        }
    }

    /// Keep the original policy: trust the confirmed peer's address and
    /// transport while its lease is valid; its source port may differ.
    pub fn matches_peer(&mut self, now: Duration, peer: RegistrationPeer) -> bool {
        self.observation(now).phase == RegistrationPhase::Registered
            && self.flow.as_ref().is_some_and(|flow| {
                flow.remote.transport == peer.transport
                    && flow.remote.address.ip() == peer.address.ip()
            })
    }

    pub fn keepalive_flow(&mut self, now: Duration) -> Option<RegistrationFlow> {
        (self.observation(now).phase == RegistrationPhase::Registered)
            .then(|| self.flow.clone())
            .flatten()
            .filter(|flow| flow.remote.transport == RegistrationTransport::Udp)
    }

    /// Completion only updates diagnostics. It cannot renew an expired lease
    /// or replace the flow selected by a newer registration confirmation.
    pub fn keepalive_completed(
        &mut self,
        original: RegistrationFlow,
        utc_milliseconds: i64,
        success: bool,
    ) {
        if self.observation.phase == RegistrationPhase::Stopped
            || self.flow.as_ref() != Some(&original)
        {
            return;
        }
        self.observation.keepalive_failed = !success;
        if success {
            self.observation.last_keepalive_at = Some(utc_milliseconds);
        }
    }

    /// The transaction owner separately retains its unregister intent and
    /// stops refresh/keepalive first. Losing local lease validity does not
    /// prove that its remote binding has disappeared.
    pub fn stop(&mut self) {
        self.observation.phase = RegistrationPhase::Stopped;
    }

    /// Call only for the matched, successful zero-expiry REGISTER completion.
    /// A timeout, local retirement or requested shutdown leaves this null.
    pub fn removal_confirmed(&mut self, original: &RegistrationFlow, response_code: u16) {
        if self.observation.phase == RegistrationPhase::Stopped
            && self.flow.as_ref() == Some(original)
            && (200..=299).contains(&response_code)
        {
            self.observation.response_code = Some(response_code);
            self.observation.remote_removed = Some(true);
        }
    }
}

/// The caller parses literal IPs and bounds the source list. Keep explicit
/// peers separate from the optional registered-peer rule.
pub struct IncomingPeers {
    addresses: Vec<IpAddr>,
    registered: bool,
}
impl IncomingPeers {
    pub fn new(addresses: Vec<IpAddr>, registered: bool) -> Option<Self> {
        (addresses.len() + usize::from(registered) <= 32).then_some(Self {
            addresses,
            registered,
        })
    }
    pub fn allows(
        &self,
        now: Duration,
        peer: RegistrationPeer,
        registration: Option<&mut RegistrationLease>,
    ) -> bool {
        peer.address.port() != 0
            && (self.addresses.contains(&peer.address.ip())
                || self.registered
                    && registration.is_some_and(|lease| lease.matches_peer(now, peer)))
    }
}

/// Preserve the existing registrar lifetime policy. Every Contact participates
/// in the minimum; a Contact without expires inherits the global or requested
/// value. A granted lifetime may exceed the requested value.
pub fn registration_expiry(
    requested_seconds: u64,
    response_seconds: Option<u64>,
    contacts: impl IntoIterator<Item = Option<u64>>,
) -> u64 {
    let fallback = response_seconds.unwrap_or(requested_seconds);
    contacts
        .into_iter()
        .map(|value| value.unwrap_or(fallback))
        .min()
        .unwrap_or(fallback)
}
