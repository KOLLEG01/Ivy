use ivy_phone_bridge::sip::{
    BoundSipTransport, RegistrationObservation, RegistrationPeer, RegistrationPhase,
    RegistrationTransport, SipRegistration, SipRegistrationSettings,
};
use rsipstack::{
    sip::{Header, Method, Request, Response, Transport, headers::*, prelude::*},
    transaction::endpoint::EndpointOption,
    transport::SipAddr,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UdpSocket},
};

enum Peer {
    Udp(UdpSocket, Option<SocketAddr>),
    Tcp(BufReader<TcpStream>),
}
impl Peer {
    async fn receive(&mut self) -> Request {
        let bytes = match self {
            Self::Udp(socket, remote) => {
                let mut bytes = vec![0; 8192];
                let (size, source) = socket.recv_from(&mut bytes).await.unwrap();
                if let Some(previous) = remote {
                    assert_eq!(*previous, source, "bound source must not change");
                }
                *remote = Some(source);
                bytes.truncate(size);
                bytes
            }
            Self::Tcp(stream) => {
                let mut bytes = Vec::new();
                loop {
                    let mut line = String::new();
                    assert!(stream.read_line(&mut line).await.unwrap() > 0);
                    bytes.extend_from_slice(line.as_bytes());
                    assert!(bytes.len() < 8192);
                    if line == "\r\n" {
                        break;
                    }
                }
                bytes
            }
        };
        let request: Request = std::str::from_utf8(&bytes).unwrap().try_into().unwrap();
        assert_eq!(request.method, Method::Register);
        assert!(request.body.is_empty());
        assert_eq!(request.content_length_header().unwrap().value(), "0");
        request
    }
    async fn send(&mut self, response: &Response) {
        let bytes = response.to_string();
        match self {
            Self::Udp(socket, remote) => {
                socket
                    .send_to(bytes.as_bytes(), remote.unwrap())
                    .await
                    .unwrap();
            }
            Self::Tcp(stream) => stream.get_mut().write_all(bytes.as_bytes()).await.unwrap(),
        }
    }
    fn source(&self) -> SocketAddr {
        match self {
            Self::Udp(_, source) => source.unwrap(),
            Self::Tcp(stream) => stream.get_ref().peer_addr().unwrap(),
        }
    }
}
enum Listener {
    Udp(UdpSocket),
    Tcp(TcpListener),
}
impl Listener {
    async fn new(tcp: bool) -> Self {
        if tcp {
            Self::Tcp(TcpListener::bind("127.0.0.1:0").await.unwrap())
        } else {
            Self::Udp(UdpSocket::bind("127.0.0.1:0").await.unwrap())
        }
    }
    fn address(&self) -> SocketAddr {
        match self {
            Self::Udp(socket) => socket.local_addr().unwrap(),
            Self::Tcp(socket) => socket.local_addr().unwrap(),
        }
    }
    async fn accept(self) -> Peer {
        match self {
            Self::Udp(socket) => Peer::Udp(socket, None),
            Self::Tcp(socket) => Peer::Tcp(BufReader::new(socket.accept().await.unwrap().0)),
        }
    }
}
fn response(request: &Request, code: u16, extra: &str) -> Response {
    let via = request.top_via_header().unwrap().value();
    let from = request.from_header().unwrap().value();
    let to = request.to_header().unwrap().value();
    let call = request.call_id_header().unwrap().value();
    let cseq = request.cseq_header().unwrap().value();
    format!("SIP/2.0 {code} Fixture\r\nVia: {via}\r\nFrom: {from}\r\nTo: {to};tag=registrar\r\nCall-ID: {call}\r\nCSeq: {cseq}\r\n{extra}Content-Length: 0\r\n\r\n").try_into().unwrap()
}
fn challenge(request: &Request, proxy: bool) -> Response {
    response(
        request,
        if proxy { 407 } else { 401 },
        &format!(
            "{}: Digest realm=\"challenge-realm\", nonce=\"synthetic-register-nonce\", algorithm=MD5, qop=\"auth\"\r\n",
            if proxy {
                "Proxy-Authenticate"
            } else {
                "WWW-Authenticate"
            }
        ),
    )
}
fn branch(request: &Request) -> String {
    request
        .top_via_header()
        .unwrap()
        .typed()
        .unwrap()
        .branch()
        .unwrap()
        .value()
        .into()
}
fn related(first: &Request, next: &Request, sequence: u32, same_from: bool) {
    assert_eq!(first.uri, next.uri);
    assert_eq!(
        first.call_id_header().unwrap(),
        next.call_id_header().unwrap()
    );
    assert_eq!(
        first.contact_header().unwrap(),
        next.contact_header().unwrap()
    );
    assert_eq!(next.cseq_header().unwrap().seq().unwrap(), sequence);
    assert_ne!(branch(first), branch(next));
    assert_eq!(
        first.from_header().unwrap() == next.from_header().unwrap(),
        same_from
    );
}
fn digest(request: &Request, proxy: bool) {
    let raw = request
        .headers
        .iter()
        .find_map(|header| match header {
            Header::Authorization(value) if !proxy => Some(value.value()),
            Header::ProxyAuthorization(value) if proxy => Some(value.value()),
            _ => None,
        })
        .expect("matching authentication header");
    let parsed = rsipstack::sip::typed::Authorization::parse(raw).unwrap();
    assert_eq!(parsed.username, "digest-user");
    assert_eq!(parsed.realm, "challenge-realm");
    assert_eq!(parsed.uri.to_string(), "sip:account.example.test");
    assert!(rsipstack::dialog::authenticate::verify_digest(
        &parsed,
        "fixture-only",
        &Method::Register,
        raw
    ));
}
fn settings(
    remote: SocketAddr,
    protocol: Transport,
    expiry: u32,
    keepalive: u32,
) -> SipRegistrationSettings {
    SipRegistrationSettings::new(
        "sip:public-user@account.example.test",
        &format!("sip:{remote};transport={protocol}"),
        "contact-user".into(),
        "digest-user".into(),
        Some("ignored-setting-realm"),
        expiry,
        10,
        keepalive,
        protocol,
    )
    .unwrap()
}
async fn bound(tcp: bool, wildcard: bool) -> Arc<BoundSipTransport> {
    let address = if wildcard { "0.0.0.0:0" } else { "127.0.0.1:0" }
        .parse()
        .unwrap();
    Arc::new(if tcp {
        BoundSipTransport::tcp(address, EndpointOption::default()).unwrap()
    } else {
        BoundSipTransport::udp(address, EndpointOption::default())
            .await
            .unwrap()
    })
}
async fn observed(registration: &SipRegistration, ready: impl Fn(RegistrationObservation) -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if ready(registration.observation()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("registration observation deadline");
}
async fn released(bound: Arc<BoundSipTransport>) {
    let mut bound = Arc::try_unwrap(bound)
        .ok()
        .expect("registration released original host");
    let local = bound.local_addr();
    bound.close().await.unwrap();
    assert!(bound.is_closed());
    if bound.protocol() == Transport::Udp {
        drop(
            UdpSocket::bind(local)
                .await
                .expect("actual endpoint released"),
        );
    }
}

#[tokio::test]
async fn identity_digest_proxy_and_removal_stay_on_original_udp_and_tcp_flows() {
    tokio::time::timeout(Duration::from_secs(15), async {
        for tcp in [false, true] {
            for proxy in [false, true] {
                let listener = Listener::new(tcp).await;
                let remote = listener.address();
                let host = bound(tcp, true).await;
                let local = host.local_addr();
                let protocol = if tcp {Transport::Tcp} else {Transport::Udp};
                let server = tokio::spawn(async move {
                    let mut peer = listener.accept().await;
                    let first = peer.receive().await;
                    // Independently captured from the retained original 10.0.15.0 template.
                    assert_eq!(first.uri.to_string(), "sip:account.example.test");
                    assert_eq!(first.from_header().unwrap().typed().unwrap().uri.user(), Some("public-user"));
                    assert_eq!(first.to_header().unwrap().typed().unwrap().uri.user(), Some("public-user"));
                    let contact = first.contact_header().unwrap().typed().unwrap().uri;
                    assert_eq!(contact.user(), Some("contact-user"));
                    assert_eq!(contact.host_with_port.to_string(), peer.source().to_string());
                    assert_eq!(peer.source().port(), local.port());
                    assert!(peer.source().ip().is_loopback());
                    assert_eq!(first.user_agent_header().unwrap().value(), "sipsorcery_v10.0.15.0");
                    assert_eq!(first.headers.iter().find_map(|header| if let Header::Allow(value) = header {Some(value.value())} else {None}).unwrap(), "ACK, BYE, CANCEL, INFO, INVITE, NOTIFY, OPTIONS, PRACK, REFER, REGISTER, SUBSCRIBE");
                    assert_eq!(first.max_forwards_header().unwrap().value(), "70");
                    assert_eq!(first.expires_header().unwrap().value(), "60");
                    peer.send(&challenge(&first, proxy)).await;
                    let authenticated = peer.receive().await;
                    related(&first, &authenticated, 2, true);
                    digest(&authenticated, proxy);
                    // Foreign identity/CSeq must not grant the lease.
                    for header in [CallId::from("foreign-call").into(), CSeq::from("999 REGISTER").into(), From::from("<sip:public-user@account.example.test>;tag=foreign").into()] {
                        let mut foreign = response(&authenticated, 200, "Expires: 0\r\n");
                        foreign.headers.unique_push(header);
                        peer.send(&foreign).await;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    peer.send(&response(&authenticated, 200, "Expires: 60\r\n")).await;
                    let remove = peer.receive().await;
                    related(&authenticated, &remove, 3, false);
                    assert_eq!(remove.expires_header().unwrap().value(), "0");
                    peer.send(&challenge(&remove, proxy)).await;
                    let authenticated_remove = peer.receive().await;
                    related(&remove, &authenticated_remove, 4, true);
                    digest(&authenticated_remove, proxy);
                    assert_eq!(authenticated_remove.expires_header().unwrap().value(), "0");
                    peer.send(&response(&authenticated_remove, 200, "Expires: 0\r\n")).await;
                    // Completed close must not send another unregister.
                    assert!(tokio::time::timeout(Duration::from_millis(40), peer.receive()).await.is_err());
                });
                let configured = if proxy {settings("127.0.0.2:9".parse().unwrap(), protocol, 60, 0)} else {settings(remote, protocol, 60, 0)};
                let mut registration = SipRegistration::start(host.clone(), configured, "fixture-only".into(), proxy.then(|| SipAddr::new(protocol, remote.into()))).unwrap();
                observed(&registration, |state| state.phase == RegistrationPhase::Registered).await;
                let state = registration.observation();
                assert_eq!(state.remote.unwrap().address, remote);
                assert_eq!(state.local.unwrap().address.port(), local.port());
                let matching = RegistrationPeer {address: SocketAddr::new(remote.ip(), 1), transport: if tcp {RegistrationTransport::Tcp} else {RegistrationTransport::Udp}};
                assert!(registration.matches_peer(matching));
                assert!(!registration.matches_peer(RegistrationPeer {address: "127.0.0.2:1".parse().unwrap(), ..matching}));
                registration.close().await.unwrap();
                registration.close().await.unwrap();
                assert_eq!(registration.observation().remote_removed, Some(true));
                assert!(!registration.matches_peer(matching));
                server.await.unwrap();
                drop(registration);
                released(host).await;
            }
        }
    }).await.unwrap();
}

#[tokio::test]
async fn keepalive_uses_confirmed_source_then_stops_at_public_lease_expiry() {
    tokio::time::timeout(Duration::from_secs(9), async {
        let listener = Listener::new(false).await;
        let remote = listener.address();
        let host = bound(false, true).await;
        let mut registration = SipRegistration::start(host.clone(), settings(remote, Transport::Udp, 60, 1), "fixture-only".into(), None).unwrap();
        let mut peer = listener.accept().await;
        let first = peer.receive().await;
        let contact = first.contact_header().unwrap().value();
        peer.send(&response(&first, 200, &format!("Expires: 100\r\nContact: \"Other, identity\" <sip:other@127.0.0.2>;expires=3, {contact};expires=10\r\n"))).await;
        observed(&registration, |state| state.phase == RegistrationPhase::Registered).await;
        assert_eq!(registration.observation().granted_expiry_seconds, Some(3));
        let Peer::Udp(socket, source) = &peer else {unreachable!()};
        let mut bytes = [0; 8192];
        for _ in 0..2 {
            let (count, from) = tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut bytes)).await.unwrap().unwrap();
            assert_eq!(&bytes[..count], b"\r\n\r\n");
            assert_eq!(Some(from), *source);
        }
        assert!(registration.observation().last_keepalive_at.is_some());
        // The tick exactly at expiry may race receipt; drain at most one already-sent tick.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(registration.observation().phase, RegistrationPhase::Failed);
        let diagnostic = registration.observation().last_keepalive_at;
        while tokio::time::timeout(Duration::from_millis(10), socket.recv_from(&mut bytes)).await.is_ok() {}
        tokio::time::sleep(Duration::from_millis(1050)).await;
        assert_eq!(registration.observation().last_keepalive_at, diagnostic);
        assert!(tokio::time::timeout(Duration::from_millis(20), socket.recv_from(&mut bytes)).await.is_err());
        // Private refresh still follows matching Contact=10 (at five seconds).
        let refresh = peer.receive().await;
        related(&first, &refresh, 2, false);
        assert_eq!(refresh.expires_header().unwrap().value(), "10");
        peer.send(&response(&refresh, 200, "Expires: 9000000000\r\nContact: <sip:other@127.0.0.2>;expires=8000000000\r\n")).await;
        observed(&registration, |state| state.granted_expiry_seconds == Some(8_000_000_000)).await;
        registration.request_stop();
        let close = tokio::spawn(async move {registration.close().await.unwrap(); registration});
        let remove = peer.receive().await;
        assert_eq!(remove.expires_header().unwrap().value(), "0");
        peer.send(&response(&remove, 200, "")).await;
        let registration = close.await.unwrap();
        assert_eq!(registration.observation().remote_removed, Some(true));
        drop(registration);
        drop(peer);
        released(host).await;
    }).await.unwrap();
}

#[tokio::test]
async fn interval_too_brief_rebuilds_with_original_attempt_budget_and_unclamped_doubling() {
    tokio::time::timeout(Duration::from_secs(5), async {
        for expiry in [60, 6000] {
            let listener = Listener::new(false).await;
            let remote = listener.address();
            let host = bound(false, false).await;
            let mut registration = SipRegistration::start(
                host.clone(),
                settings(remote, Transport::Udp, expiry, 0),
                "fixture-only".into(),
                None,
            )
            .unwrap();
            let mut peer = listener.accept().await;
            let first = peer.receive().await;
            peer.send(&response(
                &first,
                423,
                if expiry == 60 {
                    "Min-Expires: 120\r\n"
                } else {
                    ""
                },
            ))
            .await;
            let second = peer.receive().await;
            related(&first, &second, 2, false);
            assert_eq!(
                second.expires_header().unwrap().value(),
                if expiry == 60 { "120" } else { "12000" }
            );
            peer.send(&challenge(&second, false)).await;
            let third = peer.receive().await;
            related(&second, &third, 3, true);
            digest(&third, false);
            // Fourth attempt in the same cycle is forbidden even for a fresh challenge.
            peer.send(&challenge(&third, false)).await;
            observed(&registration, |state| {
                state.phase == RegistrationPhase::Failed
            })
            .await;
            assert_eq!(registration.observation().response_code, Some(401));
            assert!(
                tokio::time::timeout(Duration::from_millis(40), peer.receive())
                    .await
                    .is_err()
            );
            registration.close().await.unwrap();
            drop(registration);
            drop(peer);
            released(host).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn forbidden_first_attempt_retries_after_configured_delay_on_same_host() {
    tokio::time::timeout(Duration::from_secs(14), async {
        let listener = Listener::new(true).await;
        let remote = listener.address();
        let host = bound(true, false).await;
        let mut registration = SipRegistration::start(
            host.clone(),
            settings(remote, Transport::Tcp, 60, 1),
            "fixture-only".into(),
            None,
        )
        .unwrap();
        let mut peer = listener.accept().await;
        let first = peer.receive().await;
        peer.send(&response(&first, 403, "")).await;
        observed(&registration, |state| state.response_code == Some(403)).await;
        assert!(!registration.matches_peer(RegistrationPeer {
            address: remote,
            transport: RegistrationTransport::Tcp
        }));
        // Also proves that TCP receives neither UDP keepalives nor an early retry.
        let started = tokio::time::Instant::now();
        assert!(
            tokio::time::timeout(Duration::from_millis(500), peer.receive())
                .await
                .is_err()
        );
        let retry = peer.receive().await;
        assert!(started.elapsed() >= Duration::from_millis(9500));
        related(&first, &retry, 2, false);
        peer.send(&response(&retry, 200, "Expires: 60\r\n")).await;
        observed(&registration, |state| {
            state.phase == RegistrationPhase::Registered
        })
        .await;
        registration.request_stop();
        let close = tokio::spawn(async move {
            registration.close().await.unwrap();
            registration
        });
        let remove = peer.receive().await;
        related(&retry, &remove, 3, false);
        peer.send(&response(&remove, 200, "")).await;
        let registration = close.await.unwrap();
        assert_eq!(registration.observation().remote_removed, Some(true));
        drop(registration);
        drop(peer);
        released(host).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_close_retains_original_owner_and_lost_unregister_stays_unknown() {
    tokio::time::timeout(Duration::from_secs(6), async {
        let listener = Listener::new(false).await;
        let remote = listener.address();
        let host = bound(false, false).await;
        let mut registration = SipRegistration::start(
            host.clone(),
            settings(remote, Transport::Udp, 60, 1),
            "fixture-only".into(),
            None,
        )
        .unwrap();
        let mut peer = listener.accept().await;
        let first = peer.receive().await;
        peer.send(&response(&first, 200, "Expires: 60\r\n")).await;
        observed(&registration, |state| {
            state.phase == RegistrationPhase::Registered
        })
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), registration.close())
                .await
                .is_err()
        );
        assert!(!registration.is_closed());
        assert!(
            Arc::strong_count(&host) > 1,
            "observer cannot release host during removal"
        );
        let remove = peer.receive().await;
        related(&first, &remove, 2, false);
        assert_eq!(remove.expires_header().unwrap().value(), "0");
        let started = tokio::time::Instant::now();
        registration.close().await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(2800));
        assert_eq!(registration.observation().phase, RegistrationPhase::Stopped);
        assert_eq!(registration.observation().remote_removed, None);
        registration.close().await.unwrap();
        drop(registration);
        drop(peer);
        released(host).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn non_200_success_does_not_grant_a_registration_or_remote_removal() {
    let listener = Listener::new(false).await;
    let remote = listener.address();
    let host = bound(false, false).await;
    let mut registration = SipRegistration::start(
        host.clone(),
        settings(remote, Transport::Udp, 60, 0),
        "fixture-only".into(),
        None,
    )
    .unwrap();
    let mut peer = listener.accept().await;
    let first = tokio::time::timeout(Duration::from_secs(2), peer.receive())
        .await
        .unwrap();
    peer.send(&response(&first, 202, "Expires: 60\r\n")).await;
    observed(&registration, |state| {
        state.phase == RegistrationPhase::Failed
    })
    .await;
    assert_eq!(registration.observation().response_code, Some(202));
    registration.close().await.unwrap();
    assert_eq!(registration.observation().remote_removed, None);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), peer.receive())
            .await
            .is_err()
    );
    drop(registration);
    drop(peer);
    released(host).await;
}
