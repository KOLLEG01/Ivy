//! Included privately by the production binding module under cfg(test).
//! Fixture roots augment only this process's real platform verifier.
use super::*;
use crate::sip::{CallOutcome, DelayedInvite, DelayedInviteMedia};
use rsipstack::sip::{Method, Response, SipMessage, StatusCode, prelude::HeadersExt};
use std::{
    future::Future,
    pin::Pin,
    sync::{Condvar, Mutex as SyncMutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::Notify,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self, ClientConfig, ServerConfig,
        crypto::ring,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};

const ROOT: &[u8] = include_bytes!("../fixtures/phone-sip-tls-root.der");
const KEY: &[u8] = include_bytes!("../fixtures/phone-sip-tls-key.der");
const VALID: &[u8] = include_bytes!("../fixtures/phone-sip-tls-valid.der");
const EXPIRED: &[u8] = include_bytes!("../fixtures/phone-sip-tls-expired.der");
const WRONG: &[u8] = include_bytes!("../fixtures/phone-sip-tls-wrong-name.der");
const SELF_SIGNED: &[u8] = include_bytes!("../fixtures/phone-sip-tls-self-signed.der");

fn verifier() -> Arc<dyn rustls::client::danger::ServerCertVerifier> {
    Arc::new(
        rustls_platform_verifier::Verifier::new_with_extra_roots(
            vec![CertificateDer::from(ROOT.to_vec())],
            Arc::new(ring::default_provider()),
        )
        .unwrap(),
    )
}
fn config(verifier: Arc<dyn rustls::client::danger::ServerCertVerifier>) -> ClientConfig {
    ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth()
}
fn binding(
    address: &str,
    verifier: Arc<dyn rustls::client::danger::ServerCertVerifier>,
) -> BoundSipTransport {
    let mut host =
        BoundSipTransport::tls(address.parse().unwrap(), EndpointOption::default()).unwrap();
    host.tls = Some(TlsClient::with_config(config(verifier.clone()), verifier));
    host
}
fn server(cert: &'static [u8]) -> TlsAcceptor {
    let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![
                CertificateDer::from(cert.to_vec()),
                CertificateDer::from(ROOT.to_vec()),
            ],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec())),
        )
        .unwrap();
    TlsAcceptor::from(Arc::new(config))
}
fn next_hop(name: &str, port: u16) -> SipAddr {
    SipAddr::new(
        Transport::Tls,
        rsipstack::sip::HostWithPort {
            host: match name.parse::<std::net::IpAddr>() {
                Ok(ip) => rsipstack::sip::Host::IpAddr(ip),
                Err(_) => rsipstack::sip::Host::Domain(name.into()),
            },
            port: Some(port.into()),
        },
    )
}
fn invite(local: SocketAddr, id: &str) -> Request {
    format!("INVITE sips:callee@call.example.test SIP/2.0\r\nVia: SIP/2.0/TLS {local};branch=z9hG4bK-{id};rport\r\nFrom: <sips:caller@account.example.test>;tag=local-{id}\r\nTo: <sips:callee@call.example.test>\r\nCall-ID: {id}\r\nCSeq: 1 INVITE\r\nMax-Forwards: 70\r\nContact: <sips:caller@{local}>\r\nContent-Length: 0\r\n\r\n").try_into().unwrap()
}
async fn received<S: tokio::io::AsyncRead + Unpin>(stream: &mut BufReader<S>) -> Request {
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
    let message: SipMessage = std::str::from_utf8(&bytes).unwrap().try_into().unwrap();
    let SipMessage::Request(request) = message else {
        panic!("Expected request")
    };
    assert!(request.body.is_empty());
    request
}
fn response(request: &Request, code: StatusCode) -> Response {
    let headers = request
        .headers
        .iter()
        .filter(|h| {
            matches!(
                h,
                rsipstack::sip::Header::Via(_)
                    | rsipstack::sip::Header::From(_)
                    | rsipstack::sip::Header::CallId(_)
                    | rsipstack::sip::Header::CSeq(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut response = Response {
        status_code: code,
        version: request.version,
        wire_reason: None,
        headers: headers.into(),
        body: Vec::new(),
    };
    let to = request.to_header().unwrap();
    response.headers.push(
        rsipstack::sip::headers::To::from(if request.method == Method::Invite {
            format!("{};tag=remote", to.value())
        } else {
            to.value().into()
        })
        .into(),
    );
    response
        .headers
        .push(rsipstack::sip::headers::ContentLength::from(0).into());
    response
}
struct NoMedia;
impl DelayedInviteMedia for NoMedia {
    fn is_closed(&self) -> bool {
        false
    }
    fn failed(&self) -> bool {
        false
    }
    fn negotiate_offer(&self, _: &str) -> Option<String> {
        panic!("No offer expected")
    }
    fn rejected_answer(&self, _: &str) -> Option<String> {
        panic!("No offer expected")
    }
    fn start(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        panic!("Rejected call must not start media")
    }
}
async fn released(host: &mut BoundSipTransport) {
    let local = host.local_addr();
    host.close().await.unwrap();
    host.close().await.unwrap();
    assert!(host.is_closed());
    drop(tcp_socket(local).expect("actual reserved endpoint released"));
}

#[tokio::test]
async fn register_uses_authenticated_ip_authority_and_retains_bound_tls_flow_for_removal() {
    use crate::sip::{RegistrationPhase, SipRegistration, SipRegistrationSettings};
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote = listener.local_addr().unwrap();
        let host = Arc::new(binding("0.0.0.0:0", verifier()));
        let local = host.local_addr();
        let peer = tokio::spawn(async move {
            let (stream, source) = listener.accept().await.unwrap();
            assert_eq!(source.port(), local.port());
            let mut stream = BufReader::new(server(VALID).accept(stream).await.unwrap());
            let first = received(&mut stream).await;
            assert_eq!(first.method, Method::Register);
            assert_eq!(first.uri.to_string(), "sip:account.example.test");
            assert!(
                first
                    .contact_header()
                    .unwrap()
                    .value()
                    .contains(&source.to_string())
            );
            let mut success = response(&first, StatusCode::OK);
            success
                .headers
                .push(rsipstack::sip::headers::Expires::from(60).into());
            stream
                .get_mut()
                .write_all(success.to_string().as_bytes())
                .await
                .unwrap();
            let remove = received(&mut stream).await;
            assert_eq!(remove.method, Method::Register);
            assert_eq!(remove.expires_header().unwrap().value(), "0");
            assert_eq!(
                first.call_id_header().unwrap(),
                remove.call_id_header().unwrap()
            );
            assert_eq!(remove.cseq_header().unwrap().seq().unwrap(), 2);
            stream
                .get_mut()
                .write_all(response(&remove, StatusCode::OK).to_string().as_bytes())
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(40), listener.accept())
                    .await
                    .is_err()
            );
            let mut byte = [0];
            assert!(matches!(stream.read(&mut byte).await, Ok(0) | Err(_)));
        });
        let settings = SipRegistrationSettings::new(
            "sip:public-user@account.example.test",
            &format!("sips:{remote}"),
            "contact-user".into(),
            "digest-user".into(),
            None,
            60,
            10,
            1,
            Transport::Tls,
        )
        .unwrap();
        let mut registration =
            SipRegistration::start(host.clone(), settings, "fixture-only".into(), None).unwrap();
        while registration.observation().phase != RegistrationPhase::Registered {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(registration.observation().remote.unwrap().address, remote);
        registration.close().await.unwrap();
        assert_eq!(registration.observation().remote_removed, Some(true));
        drop(registration);
        let mut host = Arc::try_unwrap(host)
            .ok()
            .expect("registration released TLS host");
        released(&mut host).await;
        peer.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn dns_proxy_and_ip_san_use_the_real_bound_flow() {
    for (bind, authority, proxy) in [
        ("127.0.0.1:0", "sip.example.test", false),
        ("0.0.0.0:0", "proxy.example.test", true),
        ("127.0.0.1:0", "127.0.0.1", false),
        ("[::1]:0", "::1", false),
    ] {
        let peer = TcpListener::bind(if bind.starts_with('[') {
            "[::1]:0"
        } else {
            "127.0.0.1:0"
        })
        .await
        .unwrap();
        let remote = peer.local_addr().unwrap();
        let acceptor = server(VALID);
        let mut host = binding(bind, verifier());
        let local = host.local_addr();
        assert_ne!(local.port(), 0);
        assert_eq!(local.ip(), bind.parse::<SocketAddr>().unwrap().ip());
        let peer_task = tokio::spawn(async move {
            let (stream, source) = peer.accept().await.unwrap();
            assert_eq!(source.port(), local.port());
            let mut stream = BufReader::new(acceptor.accept(stream).await.unwrap());
            let first = received(&mut stream).await;
            assert_eq!(first.uri.to_string(), "sips:callee@call.example.test");
            assert_eq!(first.method, Method::Invite);
            let final_response = response(&first, StatusCode::from(486));
            stream
                .get_mut()
                .write_all(final_response.to_string().as_bytes())
                .await
                .unwrap();
            assert_eq!(received(&mut stream).await.method, Method::Ack);
            // A second prepared call reuses the already authenticated flow.
            let second = received(&mut stream).await;
            assert_eq!(second.method, Method::Invite);
            stream
                .get_mut()
                .write_all(
                    response(&second, StatusCode::from(486))
                        .to_string()
                        .as_bytes(),
                )
                .await
                .unwrap();
            assert_eq!(received(&mut stream).await.method, Method::Ack);
            assert!(
                tokio::time::timeout(Duration::from_millis(30), peer.accept())
                    .await
                    .is_err()
            );
            let mut byte = [0];
            assert!(matches!(stream.read(&mut byte).await, Ok(0) | Err(_)));
        });
        for id in ["tls-first", "tls-second"] {
            let prepared = host
                .prepare_tls_invite(
                    invite(local, id),
                    remote,
                    next_hop(authority, remote.port()),
                    proxy,
                )
                .await
                .unwrap();
            assert_eq!(
                host.close().await,
                Err(BindError::Endpoint(SipIoError::InUse))
            );
            let call = DelayedInvite::start(
                host.endpoint().unwrap(),
                prepared,
                None,
                5,
                Arc::new(NoMedia),
            )
            .unwrap();
            assert_eq!(call.wait_answer().await, CallOutcome::Ended);
            call.close().await.unwrap();
            drop(call);
        }
        released(&mut host).await;
        tokio::time::timeout(Duration::from_secs(2), peer_task)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn routed_alias_is_authenticated_before_reusing_the_original_flow() {
    let peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote = peer.local_addr().unwrap();
    let acceptor = server(VALID);
    let mut host = binding("127.0.0.1:0", verifier());
    let local = host.local_addr();
    let task = tokio::spawn(async move {
        let (stream, source) = peer.accept().await.unwrap();
        assert_eq!(source, local);
        let stream = acceptor.accept(stream).await.unwrap();
        assert_eq!(stream.get_ref().1.server_name(), Some("sip.example.test"));
        let mut stream = BufReader::new(stream);
        for id in ["first-authority", "alias-after-refusal"] {
            let request = received(&mut stream).await;
            assert_eq!(request.call_id_header().unwrap().value(), id);
            assert_eq!(request.uri.to_string(), "sips:callee@call.example.test");
            stream
                .get_mut()
                .write_all(
                    response(&request, StatusCode::from(486))
                        .to_string()
                        .as_bytes(),
                )
                .await
                .unwrap();
            assert_eq!(received(&mut stream).await.method, Method::Ack);
        }
        let mut byte = [0];
        assert!(matches!(stream.read(&mut byte).await, Ok(0) | Err(_)));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), peer.accept())
                .await
                .is_err()
        );
    });
    let prepared = host
        .prepare_tls_invite(
            invite(local, "first-authority"),
            remote,
            next_hop("sip.example.test", remote.port()),
            false,
        )
        .await
        .unwrap();
    let call = DelayedInvite::start(
        host.endpoint().unwrap(),
        prepared,
        None,
        5,
        Arc::new(NoMedia),
    )
    .unwrap();
    assert_eq!(call.wait_answer().await, CallOutcome::Ended);
    call.close().await.unwrap();
    drop(call);
    let alias = host
        .prepare_tls_invite(
            invite(local, "alias-after-refusal"),
            remote,
            next_hop("proxy.example.test", remote.port()),
            true,
        )
        .await
        .unwrap();
    // A third authority on the same IP cannot bypass its own name check.
    let invalid = host
        .prepare_tls_invite(
            invite(host.local_addr(), "wrong-authority"),
            remote,
            next_hop("wrong.example.test", remote.port()),
            false,
        )
        .await;
    assert!(matches!(invalid, Err(BindError::TlsRejected)));
    // Refusal sent no INVITE and did not close the established good flow.
    let call =
        DelayedInvite::start(host.endpoint().unwrap(), alias, None, 5, Arc::new(NoMedia)).unwrap();
    assert_eq!(call.wait_answer().await, CallOutcome::Ended);
    call.close().await.unwrap();
    drop(call);
    released(&mut host).await;
    task.await.unwrap();
}

#[tokio::test]
async fn platform_default_and_bad_certificates_refuse_before_application_bytes() {
    for (certificate, with_root, authority) in [
        (VALID, false, "sip.example.test"),
        (SELF_SIGNED, true, "sip.example.test"),
        (EXPIRED, true, "sip.example.test"),
        (WRONG, true, "sip.example.test"),
        (WRONG, true, "127.0.0.1"),
        (VALID, true, "wrong.example.test"),
        (VALID, true, "127.0.0.2"),
    ] {
        let peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote = peer.local_addr().unwrap();
        let acceptor = server(certificate);
        let task = tokio::spawn(async move {
            let (stream, _) = peer.accept().await.unwrap();
            match acceptor.accept(stream).await {
                Err(_) => 0,
                Ok(mut stream) => {
                    let mut bytes = [0; 8192];
                    stream.read(&mut bytes).await.unwrap_or(0)
                }
            }
        });
        let mut host = if with_root {
            binding("127.0.0.1:0", verifier())
        } else {
            BoundSipTransport::tls("127.0.0.1:0".parse().unwrap(), EndpointOption::default())
                .unwrap()
        };
        let name = next_hop(authority, remote.port());
        let result = host
            .prepare_tls_invite(invite(host.local_addr(), "refused"), remote, name, false)
            .await;
        if authority == "127.0.0.2" {
            assert!(matches!(result, Err(BindError::InvalidDestination)));
            task.abort();
            let _ = task.await;
        } else {
            assert!(matches!(result, Err(BindError::TlsRejected)));
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), task)
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        }
        assert!(host.connections.lock().await.is_empty());
        released(&mut host).await;
    }
}

#[tokio::test]
async fn invalid_tls_destination_is_rejected_before_any_connection() {
    let peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote = peer.local_addr().unwrap();
    let mut host = binding("127.0.0.1:0", verifier());
    let direct = SipAddr::new(Transport::Tls, remote.into());
    assert!(matches!(
        host.prepare_invite(invite(host.local_addr(), "no-name"), direct, None)
            .await,
        Err(BindError::InvalidDestination)
    ));
    for name in ["bad name", "sip.example.test\0"] {
        let authority = SipAddr::new(
            Transport::Tls,
            rsipstack::sip::HostWithPort {
                host: rsipstack::sip::Host::Domain(name.into()),
                port: Some(remote.port().into()),
            },
        );
        assert!(matches!(
            host.prepare_tls_invite(
                invite(host.local_addr(), "bad-name"),
                remote,
                authority,
                false
            )
            .await,
            Err(BindError::InvalidDestination)
        ));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(30), peer.accept())
            .await
            .is_err()
    );
    // Client capability is preserved: a TLS channel does not start a server.
    assert!(
        tokio::net::TcpStream::connect(host.local_addr())
            .await
            .is_err()
    );
    released(&mut host).await;
}

#[derive(Debug)]
struct VerificationGate {
    verifier: Arc<dyn rustls::client::danger::ServerCertVerifier>,
    blocked: SyncMutex<bool>,
    released: Condvar,
    entered: Notify,
}
impl VerificationGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            verifier: verifier(),
            blocked: SyncMutex::new(true),
            released: Condvar::new(),
            entered: Notify::new(),
        })
    }
    fn release(&self) {
        *self.blocked.lock().unwrap() = false;
        self.released.notify_all();
    }
}
impl Drop for VerificationGate {
    fn drop(&mut self) {
        self.release();
    }
}
impl rustls::client::danger::ServerCertVerifier for VerificationGate {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        self.entered.notify_one();
        let mut blocked = self.blocked.lock().unwrap();
        while *blocked {
            blocked = self.released.wait(blocked).unwrap();
        }
        self.verifier
            .verify_server_cert(end, intermediates, name, ocsp, now)
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.verifier.verify_tls12_signature(m, c, d)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.verifier.verify_tls13_signature(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.verifier.supported_verify_schemes()
    }
}
struct ReleaseGate(Arc<VerificationGate>);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}
async fn held_handshake() -> (TcpListener, SocketAddr, TlsAcceptor) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote = listener.local_addr().unwrap();
    (listener, remote, server(VALID))
}

#[tokio::test]
async fn five_second_bound_closes_socket_even_while_platform_verification_is_blocked() {
    let (peer, remote, acceptor) = held_handshake().await;
    let peer_task = tokio::spawn(async move {
        let (stream, _) = peer.accept().await.unwrap();
        assert!(acceptor.accept(stream).await.is_err());
    });
    let gate = VerificationGate::new();
    let release = ReleaseGate(gate.clone());
    let mut host = binding("127.0.0.1:0", gate.clone());
    let start = std::time::Instant::now();
    let result = host
        .prepare_tls_invite(
            invite(host.local_addr(), "timeout"),
            remote,
            next_hop("sip.example.test", remote.port()),
            false,
        )
        .await;
    assert!(matches!(result, Err(BindError::ConnectTimedOut)));
    assert!(start.elapsed() >= CONNECT_TIMEOUT && start.elapsed() < Duration::from_secs(7));
    tokio::time::timeout(Duration::from_secs(2), peer_task)
        .await
        .unwrap()
        .unwrap();
    assert!(host.connections.lock().await.is_empty());
    drop(release); // Late successful chain/name verification cannot publish a flow.
    released(&mut host).await;
}

#[tokio::test]
async fn cancelled_preparation_closes_socket_and_retains_worker_until_confirmed() {
    let (peer, remote, acceptor) = held_handshake().await;
    let peer_task = tokio::spawn(async move {
        let (stream, _) = peer.accept().await.unwrap();
        assert!(acceptor.accept(stream).await.is_err());
    });
    let gate = VerificationGate::new();
    let release = ReleaseGate(gate.clone());
    let host = Arc::new(binding("127.0.0.1:0", gate.clone()));
    let task = tokio::spawn({
        let host = host.clone();
        async move {
            host.prepare_tls_invite(
                invite(host.local_addr(), "cancelled"),
                remote,
                next_hop("sip.example.test", remote.port()),
                false,
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    tokio::time::timeout(Duration::from_secs(2), peer_task)
        .await
        .unwrap()
        .unwrap();
    let mut host = Arc::try_unwrap(host).ok().unwrap();
    assert_eq!(
        host.close().await,
        Err(BindError::Endpoint(SipIoError::InUse))
    );
    assert!(!host.is_closed());
    drop(release);
    released(&mut host).await;
}

#[tokio::test]
async fn incomplete_tls_handshake_times_out_without_an_invite_or_credentials() {
    let peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote = peer.local_addr().unwrap();
    let peer_task = tokio::spawn(async move {
        let (mut stream, _) = peer.accept().await.unwrap();
        let mut bytes = Vec::new();
        let _ = stream.read_to_end(&mut bytes).await;
        assert!(!bytes.windows(7).any(|bytes| bytes == b"INVITE "));
        assert!(!bytes.windows(14).any(|bytes| bytes == b"Authorization:"));
    });
    let mut host = binding("127.0.0.1:0", verifier());
    assert!(matches!(
        host.prepare_tls_invite(
            invite(host.local_addr(), "incomplete"),
            remote,
            next_hop("sip.example.test", remote.port()),
            false
        )
        .await,
        Err(BindError::ConnectTimedOut)
    ));
    released(&mut host).await;
    tokio::time::timeout(Duration::from_secs(2), peer_task)
        .await
        .unwrap()
        .unwrap();
}
