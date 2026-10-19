// This dependency-free target exercises the same policy source in isolation
// and as part of the native IPC Cargo target.
#[path = "../../services/phone-bridge/rust/src/sip/registration.rs"]
mod registration;
use registration::*;
use std::time::Duration;

fn at(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}
fn peer(address: &str, transport: RegistrationTransport) -> RegistrationPeer {
    RegistrationPeer {
        address: address.parse().unwrap(),
        transport,
    }
}
fn registered(transport: RegistrationTransport) -> RegistrationLease {
    let mut lease = RegistrationLease::default();
    lease.confirmed(
        at(100),
        200,
        120,
        peer("127.0.0.1:40000", transport),
        peer("127.0.0.2:5060", transport),
    );
    lease
}

#[test]
fn registered_ipv6_peer_identity_includes_original_scope_id() {
    let mut lease = RegistrationLease::default();
    lease.confirmed(
        at(100),
        200,
        120,
        peer("[fe80::2%12]:40000", RegistrationTransport::Udp),
        peer("[fe80::1%12]:5060", RegistrationTransport::Udp),
    );
    assert!(lease.matches_peer(
        at(101),
        peer("[fe80::1%12]:60000", RegistrationTransport::Udp)
    ));
    assert!(!lease.matches_peer(
        at(101),
        peer("[fe80::1%13]:5060", RegistrationTransport::Udp)
    ));
    assert!(!lease.matches_peer(at(101), peer("[fe80::1]:5060", RegistrationTransport::Udp)));
}

#[test]
fn contact_lifetimes_preserve_the_original_registrar_policy() {
    // Original RegistrationExpiry and SipRegistration.EffectiveExpiry:
    // Contact overrides global; no cap at the requested lifetime; all bindings
    // participate rather than silently choosing our URI or the first Contact.
    assert_eq!(registration_expiry(60, Some(0), [Some(120)]), 120);
    assert_eq!(registration_expiry(60, Some(120), [Some(0)]), 0);
    assert_eq!(registration_expiry(60, Some(120), [None]), 120);
    assert_eq!(registration_expiry(60, None, [None]), 60);
    assert_eq!(registration_expiry(60, None, []), 60);
    assert_eq!(registration_expiry(60, Some(150), [Some(240), None]), 150);
    assert_eq!(registration_expiry(60, None, [Some(240), Some(90)]), 90);
    assert_eq!(registration_expiry(60, Some(0), [Some(240), None]), 0);
    let long_lifetime = u64::from(u32::MAX) + 17;
    assert_eq!(
        registration_expiry(60, None, [Some(long_lifetime)]),
        long_lifetime
    );
    let mut lease = registered(RegistrationTransport::Udp);
    let local = peer("127.0.0.1:40000", RegistrationTransport::Udp);
    let remote = peer("127.0.0.2:5060", RegistrationTransport::Udp);
    lease.confirmed(at(100), 200, long_lifetime, local, remote);
    assert!(lease.matches_peer(at(100 + long_lifetime - 1), remote));
    assert!(!lease.matches_peer(at(100 + long_lifetime), remote));
}

#[test]
fn registered_peer_admission_expires_at_the_exact_monotonic_boundary() {
    let mut lease = registered(RegistrationTransport::Udp);
    let rule = IncomingPeers::new(vec![], true).unwrap();
    let same_address_new_port = peer("127.0.0.2:60000", RegistrationTransport::Udp);
    assert!(rule.allows(at(219), same_address_new_port, Some(&mut lease)));
    assert!(!rule.allows(
        at(219),
        peer("127.0.0.3:5060", RegistrationTransport::Udp),
        Some(&mut lease)
    ));
    assert!(!rule.allows(
        at(219),
        peer("127.0.0.2:5060", RegistrationTransport::Tcp),
        Some(&mut lease)
    ));
    assert!(!rule.allows(
        at(219),
        peer("127.0.0.2:0", RegistrationTransport::Udp),
        Some(&mut lease)
    ));
    assert!(!rule.allows(at(220), same_address_new_port, Some(&mut lease)));
    assert_eq!(lease.observation(at(220)).phase, RegistrationPhase::Failed);
    // A later observation in the old interval cannot revive expired authority.
    assert!(!rule.allows(at(219), same_address_new_port, Some(&mut lease)));
    assert!(!rule.allows(at(100), same_address_new_port, None));
}

#[test]
fn backward_clock_and_failed_refresh_revoke_only_the_registered_rule() {
    let mut lease = registered(RegistrationTransport::Udp);
    let remote = peer("127.0.0.2:5060", RegistrationTransport::Udp);
    let registered_rule = IncomingPeers::new(vec![], true).unwrap();
    let explicit_rule = IncomingPeers::new(vec![remote.address.ip()], false).unwrap();
    assert!(!registered_rule.allows(at(99), remote, Some(&mut lease)));
    assert!(!registered_rule.allows(at(100), remote, Some(&mut lease)));
    assert!(explicit_rule.allows(at(100), remote, Some(&mut lease)));
    lease.confirmed(
        at(101),
        200,
        60,
        peer("127.0.0.1:40000", RegistrationTransport::Udp),
        remote,
    );
    assert!(registered_rule.allows(at(101), remote, Some(&mut lease)));
    lease.failed(Some(403));
    assert!(!registered_rule.allows(at(102), remote, Some(&mut lease)));
    assert_eq!(lease.observation(at(102)).response_code, Some(403));
    assert!(explicit_rule.allows(at(102), remote, Some(&mut lease)));
    assert!(IncomingPeers::new(vec![remote.address.ip(); 32], true).is_none());
    assert!(IncomingPeers::new(vec![remote.address.ip(); 32], false).is_some());
}

#[test]
fn keepalive_completion_is_diagnostic_and_fenced_to_the_original_flow() {
    let mut lease = registered(RegistrationTransport::Udp);
    let first = lease.keepalive_flow(at(100)).unwrap();
    assert_eq!(first.local().address.port(), 40000);
    assert_eq!(first.remote().address.port(), 5060);
    lease.keepalive_completed(first.clone(), 1000, true);
    assert_eq!(lease.observation(at(101)).last_keepalive_at, Some(1000));
    lease.keepalive_completed(first.clone(), 1001, false);
    assert!(lease.observation(at(101)).keepalive_failed);
    assert_eq!(lease.observation(at(101)).last_keepalive_at, Some(1000));

    // The same endpoint spelling in a new REGISTER must not accept old work.
    lease.confirmed(at(110), 200, 120, first.local(), first.remote());
    let second = lease.keepalive_flow(at(110)).unwrap();
    assert_ne!(first, second);
    lease.keepalive_completed(first, 2000, true);
    assert_eq!(lease.observation(at(111)).last_keepalive_at, Some(1000));
    lease.keepalive_completed(second.clone(), 3000, true);
    assert!(!lease.observation(at(111)).keepalive_failed);
    assert_eq!(lease.observation(at(111)).last_keepalive_at, Some(3000));
    lease.keepalive_completed(second, 100_000, true);
    assert_eq!(lease.observation(at(230)).phase, RegistrationPhase::Failed);
    assert!(lease.keepalive_flow(at(230)).is_none());
    assert!(
        registered(RegistrationTransport::Tcp)
            .keepalive_flow(at(100))
            .is_none()
    );
    assert!(
        registered(RegistrationTransport::Tls)
            .keepalive_flow(at(100))
            .is_none()
    );
}

#[test]
fn local_stop_cannot_claim_remote_removal_or_accept_another_owners_completion() {
    let mut lease = registered(RegistrationTransport::Udp);
    let original = lease.keepalive_flow(at(100)).unwrap();
    let foreign = registered(RegistrationTransport::Udp)
        .keepalive_flow(at(100))
        .unwrap();
    assert_ne!(original, foreign);
    lease.stop();
    lease.stop();
    assert_eq!(lease.original_flow(), Some(original.clone()));
    lease.removal_confirmed(&foreign, 200);
    lease.removal_confirmed(&original, 408);
    assert_eq!(lease.observation(at(102)).remote_removed, None);
    lease.keepalive_completed(original.clone(), 1000, true);
    assert_eq!(lease.observation(at(102)).last_keepalive_at, None);
    lease.confirmed(at(103), 200, 60, original.local(), original.remote());
    lease.failed(Some(403));
    assert_eq!(lease.observation(at(103)).phase, RegistrationPhase::Stopped);
    lease.removal_confirmed(&original, 200);
    assert_eq!(lease.observation(at(104)).remote_removed, Some(true));
    assert!(lease.keepalive_flow(at(104)).is_none());
}

#[test]
fn zero_or_invalid_confirmation_never_grants_peer_authority() {
    for (status, lifetime, transport, port) in [
        (200, 0, RegistrationTransport::Udp, 5060),
        (403, 120, RegistrationTransport::Udp, 5060),
        (200, 120, RegistrationTransport::Tcp, 5060),
        (200, 120, RegistrationTransport::Udp, 0),
    ] {
        let mut lease = RegistrationLease::default();
        assert_eq!(
            lease.observation(at(100)).phase,
            RegistrationPhase::Registering
        );
        let local = peer("127.0.0.1:40000", RegistrationTransport::Udp);
        let remote = RegistrationPeer {
            address: std::net::SocketAddr::new("127.0.0.2".parse().unwrap(), port),
            transport,
        };
        lease.confirmed(at(100), status, lifetime, local, remote);
        assert_eq!(lease.observation(at(100)).phase, RegistrationPhase::Failed);
        assert!(!lease.matches_peer(at(100), remote));
        lease.stop();
        assert_eq!(lease.observation(at(100)).phase, RegistrationPhase::Stopped);
    }
}
