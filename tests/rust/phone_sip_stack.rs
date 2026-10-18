#[cfg(test)]
mod tests {
    use ivy_phone_bridge::sip::{
        BindError, BoundSipTransport, CallError, CallObservation, CallOutcome, DelayedInvite,
        DelayedInviteMedia, DigestCredentials, FinalPhase, InviteExchange, PreparedInvite,
        SipEndpoint, SipError, SipIoError,
    };
    use rsipstack::{
        EndpointBuilder,
        sip::{
            Method, Request, Response, SipMessage, StatusCode,
            headers::*,
            prelude::{HeadersExt, ToTypedHeader},
        },
        transaction::{
            endpoint::EndpointOption,
            key::{TransactionKey, TransactionRole},
            transaction::Transaction,
        },
        transport::{SipAddr, TransportLayer, udp::UdpConnection},
    };
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::{TcpListener, TcpStream, UdpSocket},
        sync::{Notify, Semaphore},
    };
    use tokio_util::sync::CancellationToken;

    const OFFER: &str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 40000 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
    const ANSWER: &str = "v=0\r\no=- 2 2 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 40002 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
    const REJECTED: &str = "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=fixture\r\nc=IN IP4 0.0.0.0\r\nt=0 0\r\nm=audio 0 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\na=inactive\r\n";

    enum Peer {
        Udp(UdpSocket, Option<std::net::SocketAddr>),
        Tcp(BufReader<TcpStream>),
    }
    impl Peer {
        async fn receive(&mut self) -> Request {
            match self.message().await {
                SipMessage::Request(request) => request,
                _ => panic!("request expected"),
            }
        }
        async fn message(&mut self) -> SipMessage {
            match self {
                Self::Udp(socket, remote) => {
                    let mut bytes = [0; 8192];
                    let (count, address) = socket.recv_from(&mut bytes).await.unwrap();
                    assert!(address.ip().is_loopback());
                    *remote = Some(address);
                    std::str::from_utf8(&bytes[..count])
                        .unwrap()
                        .try_into()
                        .unwrap()
                }
                Self::Tcp(stream) => {
                    let mut bytes = Vec::new();
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        assert!(stream.read_line(&mut line).await.unwrap() > 0);
                        if let Some((name, value)) = line.split_once(':') {
                            if name.eq_ignore_ascii_case("Content-Length") {
                                length = value.trim().parse::<usize>().unwrap();
                            }
                        }
                        bytes.extend_from_slice(line.as_bytes());
                        assert!(bytes.len() + length <= 8192);
                        if line == "\r\n" {
                            break;
                        }
                    }
                    let headers = bytes.len();
                    bytes.resize(headers + length, 0);
                    stream.read_exact(&mut bytes[headers..]).await.unwrap();
                    std::str::from_utf8(&bytes).unwrap().try_into().unwrap()
                }
            }
        }
        async fn send(&mut self, response: &Response) {
            self.send_message(&SipMessage::Response(response.clone()))
                .await;
        }
        async fn send_message(&mut self, message: &SipMessage) {
            let bytes = message.to_string();
            match self {
                Self::Udp(socket, remote) => {
                    socket
                        .send_to(bytes.as_bytes(), remote.unwrap())
                        .await
                        .unwrap();
                }
                Self::Tcp(stream) => {
                    stream.get_mut().write_all(bytes.as_bytes()).await.unwrap();
                }
            }
        }
    }

    struct Media {
        negotiated: AtomicUsize,
        starts: AtomicUsize,
        started: Notify,
        ready: Semaphore,
    }
    impl Media {
        fn new(permits: usize) -> Self {
            Self {
                negotiated: AtomicUsize::new(0),
                starts: AtomicUsize::new(0),
                started: Notify::new(),
                ready: Semaphore::new(permits),
            }
        }
    }
    impl DelayedInviteMedia for Media {
        fn is_closed(&self) -> bool {
            false
        }
        fn failed(&self) -> bool {
            false
        }
        fn negotiate_offer(&self, offer: &str) -> Option<String> {
            self.negotiated.fetch_add(1, Ordering::SeqCst);
            (offer == OFFER).then(|| ANSWER.to_owned())
        }
        fn rejected_answer(&self, offer: &str) -> Option<String> {
            (offer == OFFER).then(|| REJECTED.to_owned())
        }
        fn start(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
            Box::pin(async move {
                self.starts.fetch_add(1, Ordering::SeqCst);
                self.started.notify_one();
                let permit = self.ready.acquire().await.unwrap();
                permit.forget();
                true
            })
        }
    }
    async fn observed(call: &DelayedInvite, ready: impl Fn(CallObservation) -> bool) {
        let mut changes = call.observations();
        loop {
            if ready(*changes.borrow()) {
                return;
            }
            changes.changed().await.unwrap();
        }
    }
    fn prepared(request: Request) -> PreparedInvite {
        let destination = SipAddr::try_from(&request.destination()).unwrap();
        PreparedInvite::new(request, destination, None).unwrap()
    }
    async fn network(
        tcp: bool,
        options: EndpointOption,
    ) -> (SipEndpoint, Peer, u16, u16, CancellationToken) {
        let token = CancellationToken::new();
        let transport = TransportLayer::new(token.child_token());
        let (peer, local_port, peer_port) = if tcp {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let peer_port = listener.local_addr().unwrap().port();
            let target: SipAddr = (&format!("sip:peer@127.0.0.1:{peer_port};transport=tcp")
                .parse::<rsipstack::sip::Uri>()
                .unwrap())
                .try_into()
                .unwrap();
            let (connection, _) = transport.lookup(&target, None).await.unwrap();
            let local_port = connection.get_addr().addr.port.map(u16::from).unwrap();
            let (stream, address) = listener.accept().await.unwrap();
            assert!(address.ip().is_loopback());
            (Peer::Tcp(BufReader::new(stream)), local_port, peer_port)
        } else {
            let udp = UdpConnection::create_connection(
                "127.0.0.1:0".parse().unwrap(),
                None,
                Some(token.child_token()),
            )
            .await
            .unwrap();
            let local_port = udp.get_addr().addr.port.map(u16::from).unwrap();
            transport.add_transport(udp.into());
            let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let peer_port = peer.local_addr().unwrap().port();
            (Peer::Udp(peer, None), local_port, peer_port)
        };
        (
            SipEndpoint::start(transport, token.clone(), options),
            peer,
            local_port,
            peer_port,
            token,
        )
    }
    fn addressed(body: &str, local_port: u16, peer_port: u16, tcp: bool) -> Request {
        let mut request = original(body);
        if !body.is_empty() {
            request
                .headers
                .unique_push(ContentType::new("application/sdp").into());
        }
        request.uri = format!(
            "sip:peer@127.0.0.1:{peer_port}{}",
            if tcp { ";transport=tcp" } else { "" }
        )
        .try_into()
        .unwrap();
        request.headers.unique_push(
            Via::new(format!(
                "SIP/2.0/{} 127.0.0.1:{local_port};branch=z9hG4bKoriginal",
                if tcp { "TCP" } else { "UDP" }
            ))
            .into(),
        );
        request
            .headers
            .unique_push(Contact::new(format!("<sip:fixture@127.0.0.1:{local_port}>")).into());
        request
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
            .to_owned()
    }

    #[tokio::test]
    async fn authenticated_initial_and_delayed_offers_use_owned_udp_and_tcp_transactions() {
        tokio::time::timeout(Duration::from_secs(15), async {
            for tcp in [false, true] {
                for status in [StatusCode::Unauthorized, StatusCode::ProxyAuthenticationRequired] {
                    for offer in ["", OFFER] {
                        let (mut endpoint, mut peer, local_port, peer_port, _) = network(tcp, EndpointOption::default()).await;
                        let peer_status = status.clone();
                        let peer_task = tokio::spawn(async move {
                            let original = peer.receive().await;
                            assert_eq!(original.method, Method::Invite);
                            assert_eq!(original.body, offer.as_bytes());
                            let mut challenge = response(&original, peer_status.clone(), peer_port, "");
                            let digest = "Digest realm=\"fixture\", nonce=\"original-fixture-nonce\", algorithm=MD5, qop=\"auth\"";
                            challenge.headers.unique_push(if peer_status == StatusCode::Unauthorized { WwwAuthenticate::new(digest).into() } else { ProxyAuthenticate::new(digest).into() });
                            peer.send(&challenge).await;
                            let challenge_ack = peer.receive().await;
                            assert_eq!(challenge_ack.method, Method::Ack);
                            assert!(challenge_ack.body.is_empty());
                            assert_eq!(branch(&challenge_ack), branch(&original));
                            let continued = peer.receive().await;
                            assert_eq!(continued.method, Method::Invite);
                            assert_eq!(continued.body, original.body);
                            assert_eq!(continued.call_id_header().unwrap(), original.call_id_header().unwrap());
                            assert_eq!(continued.from_header().unwrap(), original.from_header().unwrap());
                            assert_eq!(continued.cseq_header().unwrap().seq().unwrap(), 2);
                            assert_ne!(branch(&continued), branch(&original));
                            let authorization = continued.headers.iter().find(|h| if peer_status == StatusCode::Unauthorized { matches!(h, Header::Authorization(_)) } else { matches!(h, Header::ProxyAuthorization(_)) }).unwrap();
                            let raw = match authorization { Header::Authorization(h) => h.value(), Header::ProxyAuthorization(h) => h.value(), _ => unreachable!() };
                            let parsed = rsipstack::sip::typed::Authorization::parse(raw).unwrap();
                            assert_eq!(parsed.username, "digest-fixture");
                            assert!(rsipstack::dialog::authenticate::verify_digest(&parsed, "fixture-only", &Method::Invite, raw));
                            let mut accepted = response(&continued, StatusCode::OK, peer_port, if offer.is_empty() { OFFER } else { ANSWER });
                            if tcp {
                                // An unreachable Contact cannot force a new TCP connection for ACK.
                                accepted.headers.unique_push(Contact::new("<sip:peer@unreachable.invalid:9;transport=tcp>").into());
                            }
                            peer.send(&accepted).await;
                            let first_ack = peer.receive().await;
                            assert_eq!(first_ack.method, Method::Ack);
                            assert_eq!(first_ack.cseq_header().unwrap().seq().unwrap(), 2);
                            assert_eq!(first_ack.body, if offer.is_empty() { ANSWER.as_bytes() } else { b"" });
                            let mut fork = accepted.clone();
                            fork.headers.unique_push(To::new("<sip:peer@example.invalid>;tag=foreign-fork").into());
                            peer.send(&fork).await;
                            peer.send(&accepted).await;
                            let repeated_ack = peer.receive().await;
                            assert_eq!(repeated_ack, first_ack);
                        });
                        let request = addressed(offer, local_port, peer_port, tcp);
                        let mut invite = InviteExchange::new(request.clone()).unwrap();
                        let mut original = endpoint.client(request, None).unwrap();
                        let challenge = original.next_response().await.unwrap();
                        assert_eq!(challenge.status_code, status);
                        let continuation = invite.authenticate(original.completed().await.unwrap(), &challenge, "digest-fixture", "fixture-only").await.unwrap();
                        let mut current = endpoint.continuation(continuation, None).unwrap();
                        original.retire().await.unwrap();
                        let accepted = current.next_response().await.unwrap();
                        assert_eq!(accepted.status_code.code(), 200);
                        invite.select_final(&accepted).unwrap();
                        let ack = invite.prepare_ack(&accepted, if offer.is_empty() { Some(ANSWER) } else { None }, endpoint.inner()).unwrap();
                        current.send_ack(&ack).await.unwrap();
                        let fork = current.next_response().await.unwrap();
                        assert!(invite.ack_for_retransmission(&fork).is_none());
                        let repeated = current.next_response().await.unwrap();
                        current.send_ack(invite.ack_for_retransmission(&repeated).unwrap()).await.unwrap();
                        peer_task.await.unwrap();
                        current.retire().await.unwrap();
                        assert_eq!(endpoint.inner().get_stats().running_transactions, 0);
                        endpoint.shutdown().await.unwrap();
                    }
                }
            }
        }).await.expect("authenticated loopback matrix deadline");
    }

    #[tokio::test]
    async fn cancelled_waiter_preserves_invite_retransmit_and_completed_original() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let options = EndpointOption {
                t1: Duration::from_millis(40),
                ..Default::default()
            };
            let (mut endpoint, mut peer, local_port, peer_port, _) = network(false, options).await;
            let request = addressed("", local_port, peer_port, false);
            let mut client = endpoint.client(request, None).unwrap();
            let original = peer.receive().await;
            assert!(
                tokio::time::timeout(Duration::from_millis(5), client.next_response())
                    .await
                    .is_err()
            );
            assert_eq!(
                peer.receive().await,
                original,
                "the owning driver continues after its waiter was cancelled"
            );
            let refusal = response(&original, StatusCode::BusyHere, peer_port, "");
            peer.send(&refusal).await;
            assert_eq!(
                client.next_response().await.unwrap().status_code.code(),
                486
            );
            let ack = peer.receive().await;
            assert_eq!(ack.method, Method::Ack);
            // The completed driver can be collected while a later observer
            // waits; cancelling that observer must retain its original too.
            assert!(
                tokio::time::timeout(Duration::from_millis(5), client.next_response())
                    .await
                    .is_err()
            );
            assert_eq!(client.completed().await.unwrap().original, original);
            client.retire().await.unwrap();
            endpoint.shutdown().await.unwrap();
            endpoint.shutdown().await.unwrap();
            assert!(matches!(
                endpoint.client(original, None),
                Err(SipIoError::EndpointStopped)
            ));
        })
        .await
        .expect("cancelled waiter/retransmit deadline");
    }

    #[tokio::test]
    async fn response_overflow_is_explicit_and_endpoint_stop_wakes_owned_clients() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (mut endpoint, mut peer, local_port, peer_port, token) =
                network(false, EndpointOption::default()).await;
            let mut incoming = endpoint.incoming_transactions().unwrap();
            assert!(matches!(
                endpoint.incoming_transactions(),
                Err(SipIoError::InUse)
            ));
            let request = addressed("", local_port, peer_port, false);
            let mut client = endpoint.client(request, None).unwrap();
            let original = peer.receive().await;
            let accepted = response(&original, StatusCode::OK, peer_port, OFFER);
            for _ in 0..10 {
                peer.send(&accepted).await;
            }
            // A later request on the same UDP socket is a processing barrier.
            // It proves the burst was inspected before observing overflow.
            if let Peer::Udp(socket, remote) = &peer {
                let mut barrier = original.clone();
                barrier.method = Method::Options;
                barrier.headers.unique_push(CSeq::new("1 OPTIONS").into());
                socket
                    .send_to(barrier.to_string().as_bytes(), remote.unwrap())
                    .await
                    .unwrap();
            }
            let barrier = incoming.recv().await.unwrap();
            assert_eq!(barrier.original.method, Method::Options);
            assert!(matches!(
                client.next_response().await,
                Err(SipIoError::ResponseOverflow)
            ));
            client.retire().await.unwrap();
            drop(barrier);
            let mut next = original;
            next.headers
                .unique_push(CallId::new("next-original").into());
            let mut client = endpoint.client(next, None).unwrap();
            token.cancel();
            assert!(matches!(
                client.next_response().await,
                Err(SipIoError::EndpointStopped)
            ));
            client.retire().await.unwrap();
            assert_eq!(endpoint.inner().get_stats().running_transactions, 0);
            endpoint.shutdown().await.unwrap();
        })
        .await
        .expect("overflow/shutdown deadline");
    }

    #[tokio::test]
    async fn local_transaction_timeouts_are_not_remote_final_responses() {
        tokio::time::timeout(Duration::from_secs(5), async {
            for method in [Method::Invite, Method::Bye] {
                for remote in [false, true] {
                    let options = EndpointOption {
                        t1: Duration::from_millis(10),
                        t1x64: Duration::from_millis(80),
                        ..Default::default()
                    };
                    let (mut endpoint, mut peer, local, peer_port, _) =
                        network(false, options).await;
                    let mut request = addressed("", local, peer_port, false);
                    request.method = method;
                    request
                        .headers
                        .unique_push(CSeq::new(format!("1 {method}")).into());
                    let mut transaction = endpoint.client(request, None).unwrap();
                    let original = peer.receive().await;
                    if remote {
                        peer.send(&response(
                            &original,
                            StatusCode::RequestTimeout,
                            peer_port,
                            "",
                        ))
                        .await;
                        assert_eq!(
                            transaction
                                .next_response()
                                .await
                                .unwrap()
                                .status_code
                                .code(),
                            408
                        );
                        assert!(transaction.completed().await.is_ok());
                    } else {
                        assert!(matches!(
                            transaction.next_response().await,
                            Err(SipIoError::TimedOut)
                        ));
                        assert!(matches!(
                            transaction.completed().await,
                            Err(SipIoError::TimedOut)
                        ));
                    }
                    transaction.retire().await.unwrap();
                    endpoint.shutdown().await.unwrap();
                }
            }
        })
        .await
        .expect("local/remote timeout distinction deadline");
    }

    fn response(request: &Request, status: StatusCode, peer: u16, body: &str) -> Response {
        let mut response = Response {
            version: request.version.clone(),
            status_code: status,
            wire_reason: None,
            headers: request.headers.clone(),
            body: body.as_bytes().to_vec(),
        };
        response
            .headers
            .unique_push(To::new("<sip:peer@example.invalid>;tag=original-peer").into());
        response
            .headers
            .unique_push(Contact::new(format!("<sip:peer@127.0.0.1:{peer}>")).into());
        response
            .headers
            .unique_push(ContentLength::from(body.len() as u32).into());
        if !body.is_empty() {
            response
                .headers
                .unique_push(ContentType::new("application/sdp").into());
        }
        response
    }

    #[tokio::test]
    async fn cancelled_delayed_offer_keeps_original_udp_transaction_and_ack_body() {
        tokio::time::timeout(Duration::from_secs(8), async {
            let mut bound =
                BoundSipTransport::udp("127.0.0.1:0".parse().unwrap(), EndpointOption::default())
                    .await
                    .unwrap();
            let local = bound.local_addr().port();
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let peer_port = socket.local_addr().unwrap().port();
            let mut peer = Peer::Udp(socket, None);
            let endpoint = bound.endpoint().unwrap();
            let media = Arc::new(Media::new(0));
            let credentials =
                DigestCredentials::new("fixture".into(), "fixture-only".into()).unwrap();
            let mut request = addressed("", local, peer_port, false);
            let destination = SipAddr::try_from(&request.destination()).unwrap();
            request.uri = "sip:peer@prepared.invalid".try_into().unwrap();
            let call = DelayedInvite::start(
                endpoint.clone(),
                bound
                    .prepare_invite(request, destination, None)
                    .await
                    .unwrap(),
                Some(credentials),
                5,
                media.clone(),
            )
            .unwrap();
            let original = peer.receive().await;
            peer.send(&response(&original, StatusCode::Ringing, peer_port, ""))
                .await;
            call.hangup().await.unwrap();
            assert!(call.observation().ending);
            assert_eq!(call.try_close().await, Err(CallError::CleanupUnconfirmed));
            let cancel = peer.receive().await;
            assert_eq!(cancel.method, Method::Cancel);
            assert_eq!(cancel.uri, original.uri);
            assert_eq!(
                cancel.call_id_header().unwrap(),
                original.call_id_header().unwrap()
            );
            assert_eq!(
                cancel.top_via_header().unwrap(),
                original.top_via_header().unwrap()
            );
            assert_eq!(
                cancel.cseq_header().unwrap().seq().unwrap(),
                original.cseq_header().unwrap().seq().unwrap()
            );
            peer.send(&response(&cancel, StatusCode::OK, peer_port, ""))
                .await;
            assert!(
                !call.observation().cleanup_confirmed,
                "CANCEL response cannot release original ownership"
            );
            let accepted = response(&original, StatusCode::OK, peer_port, OFFER);
            peer.send(&accepted).await;
            let ack = peer.receive().await;
            assert_eq!(ack.method, Method::Ack);
            assert_eq!(ack.body, REJECTED.as_bytes());
            peer.send(&accepted).await;
            let mut replay = None;
            let mut bye = None;
            while replay.is_none() || bye.is_none() {
                let request = peer.receive().await;
                match request.method {
                    Method::Ack => replay = Some(request),
                    Method::Bye => bye = Some(request),
                    method => panic!("Unexpected {method}"),
                }
            }
            assert_eq!(replay.unwrap(), ack);
            let bye = bye.unwrap();
            assert_eq!(
                bye.call_id_header().unwrap(),
                original.call_id_header().unwrap()
            );
            assert_eq!(bye.to_header().unwrap(), ack.to_header().unwrap());
            let mut challenge = response(&bye, StatusCode::Unauthorized, peer_port, "");
            challenge.headers.unique_push(
                WwwAuthenticate::new(
                    "Digest realm=\"fixture\", nonce=\"bye-fixture\", algorithm=MD5, qop=\"auth\"",
                )
                .into(),
            );
            peer.send(&challenge).await;
            let authenticated = peer.receive().await;
            assert_eq!(authenticated.method, Method::Bye);
            assert_eq!(
                authenticated.cseq_header().unwrap().seq().unwrap(),
                bye.cseq_header().unwrap().seq().unwrap() + 1
            );
            assert!(
                authenticated
                    .headers
                    .iter()
                    .any(|h| matches!(h, Header::Authorization(_)))
            );
            let authorization = authenticated
                .headers
                .iter()
                .find_map(|h| match h {
                    Header::Authorization(value) => Some(value.value()),
                    _ => None,
                })
                .unwrap();
            assert!(rsipstack::dialog::authenticate::verify_digest(
                &rsipstack::sip::typed::Authorization::parse(authorization).unwrap(),
                "fixture-only",
                &Method::Bye,
                authorization
            ));
            assert_eq!(call.wait_answer().await, CallOutcome::Ended);
            assert_eq!(call.try_close().await, Err(CallError::CleanupUnconfirmed));
            peer.send(&response(&authenticated, StatusCode::OK, peer_port, ""))
                .await;
            call.close().await.unwrap();
            assert!(call.observation().closed);
            assert!(!call.observation().active);
            assert_eq!(media.negotiated.load(Ordering::SeqCst), 0);
            assert_eq!(media.starts.load(Ordering::SeqCst), 0);
            assert_eq!(endpoint.inner().get_stats().running_transactions, 0);
            drop(endpoint);
            bound.close().await.unwrap();
            assert!(bound.is_closed());
        })
        .await
        .expect("owned cancelled call deadline");
    }

    #[tokio::test]
    async fn hangup_and_bye_complete_while_owned_media_start_is_pending() {
        tokio::time::timeout(Duration::from_secs(8), async {
            for tcp in [false, true] {
                let (endpoint, mut peer, local, peer_port, _) = network(tcp, EndpointOption::default()).await;
                let endpoint = Arc::new(endpoint); let media = Arc::new(Media::new(0));
                let credentials = DigestCredentials::new("digest-fixture".into(), "fixture-only".into()).unwrap();
                let call = DelayedInvite::start(endpoint.clone(), prepared(addressed("", local, peer_port, tcp)), Some(credentials), 5, media.clone()).unwrap();
                let original = peer.receive().await;
                let mut challenge = response(&original, StatusCode::ProxyAuthenticationRequired, peer_port, "");
                challenge.headers.unique_push(ProxyAuthenticate::new("Digest realm=\"fixture\", nonce=\"call-fixture\", algorithm=MD5, qop=\"auth\"").into());
                peer.send(&challenge).await;
                assert_eq!(peer.receive().await.method, Method::Ack);
                let authenticated = peer.receive().await;
                assert_eq!(authenticated.method, Method::Invite);
                assert_eq!(authenticated.body, b"");
                assert_eq!(authenticated.call_id_header().unwrap(), original.call_id_header().unwrap());
                peer.send(&response(&authenticated, StatusCode::OK, peer_port, OFFER)).await;
                let ack = peer.receive().await;
                assert_eq!(ack.method, Method::Ack); assert_eq!(ack.body, ANSWER.as_bytes());
                media.started.notified().await;
                assert_eq!(call.observation().phase, FinalPhase::StartingMedia);
                assert!(!call.observation().active);
                call.hangup().await.unwrap();
                let bye = peer.receive().await;
                assert_eq!(bye.method, Method::Bye, "BYE cannot wait for the blocked media-start future");
                assert_eq!(bye.to_header().unwrap(), ack.to_header().unwrap());
                peer.send(&response(&bye, StatusCode::CallTransactionDoesNotExist, peer_port, "")).await;
                observed(&call, |state| state.cleanup_confirmed).await;
                assert!(!call.observation().quiescent);
                assert_eq!(call.try_close().await, Err(CallError::CleanupUnconfirmed));
                media.ready.add_permits(1);
                assert_eq!(call.wait_answer().await, CallOutcome::Ended);
                call.close().await.unwrap(); call.close().await.unwrap();
                assert!(!call.observation().active);
                assert!(!call.observation().uncertain);
                assert_eq!(media.starts.load(Ordering::SeqCst), 1);
                assert_eq!(media.negotiated.load(Ordering::SeqCst), 1);
                let mut endpoint = Arc::try_unwrap(endpoint).ok().expect("stopped media and call actor released endpoint");
                endpoint.shutdown().await.unwrap();
            }
        }).await.expect("owned media-start/hangup deadline");
    }

    fn remote_bye(
        ack: &Request,
        local: u16,
        peer_port: u16,
        sequence: u32,
        via_branch: &str,
    ) -> Request {
        let mut bye = ack.clone();
        bye.method = Method::Bye;
        bye.body.clear();
        bye.uri = format!("sip:fixture@127.0.0.1:{local}").try_into().unwrap();
        bye.headers
            .unique_push(From::new(ack.to_header().unwrap().value()).into());
        bye.headers
            .unique_push(To::new(ack.from_header().unwrap().value()).into());
        bye.headers
            .unique_push(CSeq::new(format!("{sequence} BYE")).into());
        bye.headers.unique_push(
            Via::new(format!(
                "SIP/2.0/UDP 127.0.0.1:{peer_port};branch=z9hG4bK{via_branch}"
            ))
            .into(),
        );
        bye.headers
            .retain(|h| !matches!(h, Header::ContentType(_) | Header::ContentLength(_)));
        bye.headers.unique_push(ContentLength::from(0).into());
        bye
    }
    async fn status(peer: &mut Peer) -> u16 {
        match peer.message().await {
            SipMessage::Response(response) => response.status_code.code(),
            _ => panic!("response expected"),
        }
    }
    #[tokio::test]
    async fn early_remote_bye_is_tag_and_sequence_fenced_before_media_start_finishes() {
        tokio::time::timeout(Duration::from_secs(8), async {
            let (endpoint, mut peer, local, peer_port, _) =
                network(false, EndpointOption::default()).await;
            let mut incoming = endpoint.incoming_transactions().unwrap();
            let endpoint = Arc::new(endpoint);
            let media = Arc::new(Media::new(0));
            let mut request = addressed("", local, peer_port, false);
            let destination = SipAddr::try_from(&request.destination()).unwrap();
            request.uri = "sip:peer@prepared.invalid".try_into().unwrap();
            let call = DelayedInvite::start(
                endpoint.clone(),
                PreparedInvite::new(request, destination, None).unwrap(),
                None,
                5,
                media.clone(),
            )
            .unwrap();
            let original = peer.receive().await;
            assert_eq!(
                original.uri.to_string(),
                "sip:peer@prepared.invalid",
                "resolved target must not rewrite the original digest URI"
            );
            peer.send(&response(&original, StatusCode::OK, peer_port, OFFER))
                .await;
            let ack = peer.receive().await;
            assert_eq!(ack.method, Method::Ack);
            media.started.notified().await;
            let mut foreign = remote_bye(&ack, local, peer_port, 1, "foreign");
            foreign
                .headers
                .unique_push(From::new("<sip:peer@example.invalid>;tag=foreign-peer").into());
            peer.send_message(&SipMessage::Request(foreign)).await;
            let mut returned = call
                .deliver_control(incoming.recv().await.unwrap())
                .err()
                .expect("foreign transaction stays with host dispatcher");
            returned
                .reply(StatusCode::CallTransactionDoesNotExist)
                .await
                .unwrap();
            drop(returned);
            assert_eq!(status(&mut peer).await, 481);
            assert!(!call.observation().cleanup_confirmed);
            peer.send_message(&SipMessage::Request(remote_bye(
                &ack, local, peer_port, 0, "stale",
            )))
            .await;
            assert!(call.deliver_control(incoming.recv().await.unwrap()).is_ok());
            assert_eq!(status(&mut peer).await, 500);
            assert!(!call.observation().cleanup_confirmed);
            peer.send_message(&SipMessage::Request(remote_bye(
                &ack, local, peer_port, 1, "current",
            )))
            .await;
            assert!(call.deliver_control(incoming.recv().await.unwrap()).is_ok());
            assert_eq!(status(&mut peer).await, 200);
            assert_eq!(call.wait_answer().await, CallOutcome::Ended);
            assert_eq!(call.try_close().await, Err(CallError::CleanupUnconfirmed));
            media.ready.add_permits(1);
            call.close().await.unwrap();
            assert!(!call.observation().active);
            assert!(
                tokio::time::timeout(Duration::from_millis(25), peer.message())
                    .await
                    .is_err(),
                "remote BYE suppresses local BYE"
            );
            let mut endpoint = Arc::try_unwrap(endpoint)
                .ok()
                .expect("remote BYE call owner stopped");
            endpoint.shutdown().await.unwrap();
        })
        .await
        .expect("remote BYE/media-start deadline");
    }

    #[tokio::test]
    async fn cancelled_native_waiter_and_wrong_sdp_type_do_not_release_or_start_media() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (endpoint, mut peer, local, peer_port, _) =
                network(false, EndpointOption::default()).await;
            let endpoint = Arc::new(endpoint);
            let media = Arc::new(Media::new(0));
            let call = DelayedInvite::start(
                endpoint.clone(),
                prepared(addressed("", local, peer_port, false)),
                None,
                5,
                media.clone(),
            )
            .unwrap();
            let original = peer.receive().await;
            assert!(
                tokio::time::timeout(Duration::from_millis(5), call.wait_answer())
                    .await
                    .is_err()
            );
            assert_eq!(call.try_close().await, Err(CallError::CleanupUnconfirmed));
            let mut accepted = response(&original, StatusCode::OK, peer_port, OFFER);
            accepted
                .headers
                .unique_push(ContentType::new("text/plain").into());
            peer.send(&accepted).await;
            let ack = peer.receive().await;
            assert_eq!(ack.method, Method::Ack);
            assert_eq!(ack.body, REJECTED.as_bytes());
            let bye = peer.receive().await;
            assert_eq!(bye.method, Method::Bye);
            peer.send(&response(&bye, StatusCode::OK, peer_port, ""))
                .await;
            assert_eq!(call.wait_answer().await, CallOutcome::Ended);
            call.close().await.unwrap();
            assert_eq!(media.negotiated.load(Ordering::SeqCst), 0);
            assert_eq!(media.starts.load(Ordering::SeqCst), 0);
            let mut endpoint = Arc::try_unwrap(endpoint)
                .ok()
                .expect("original owner released after confirmed BYE");
            endpoint.shutdown().await.unwrap();
        })
        .await
        .expect("cancelled native waiter/invalid SDP deadline");
    }

    #[tokio::test]
    async fn local_invite_timeout_preserves_unknown_original_call_owner() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let options = EndpointOption {
                t1: Duration::from_millis(10),
                t1x64: Duration::from_millis(80),
                ..Default::default()
            };
            let (endpoint, mut peer, local, peer_port, _) = network(false, options).await;
            let endpoint = Arc::new(endpoint);
            let media = Arc::new(Media::new(0));
            let call = DelayedInvite::start(
                endpoint.clone(),
                prepared(addressed("", local, peer_port, false)),
                None,
                5,
                media.clone(),
            )
            .unwrap();
            assert_eq!(peer.receive().await.method, Method::Invite);
            assert_eq!(call.wait_answer().await, CallOutcome::Unknown);
            let observation = call.observation();
            assert!(observation.uncertain);
            assert!(!observation.cleanup_confirmed);
            assert!(!observation.active);
            assert_eq!(call.try_close().await, Err(CallError::CleanupUnconfirmed));
            assert!(
                Arc::try_unwrap(endpoint).is_err(),
                "uncertain original still owns its endpoint"
            );
            assert_eq!(media.starts.load(Ordering::SeqCst), 0);
            // Test-runtime shutdown models native process loss; this case does
            // not claim a normal call release or remote cleanup confirmation.
        })
        .await
        .expect("unknown original call deadline");
    }

    #[tokio::test]
    async fn bound_udp_and_tcp_report_and_release_the_actual_incoming_socket() {
        tokio::time::timeout(Duration::from_secs(8), async {
            for tcp in [false, true] {
                for ip in ["127.0.0.1", "0.0.0.0"] {
                    let configured: std::net::SocketAddr = format!("{ip}:0").parse().unwrap();
                    let mut bound = if tcp {
                        BoundSipTransport::tcp(configured, EndpointOption::default()).unwrap()
                    } else {
                        BoundSipTransport::udp(configured, EndpointOption::default())
                            .await
                            .unwrap()
                    };
                    let local = bound.local_addr();
                    assert_eq!(local.ip(), configured.ip());
                    assert_ne!(local.port(), 0);
                    assert_eq!(bound.listener_failure(), None);
                    let endpoint = bound.endpoint().unwrap();
                    let mut incoming = endpoint.incoming_transactions().unwrap();
                    let reachable =
                        std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), local.port());
                    let (mut peer, peer_port) = if tcp {
                        let stream = TcpStream::connect(reachable).await.unwrap();
                        let port = stream.local_addr().unwrap().port();
                        (Peer::Tcp(BufReader::new(stream)), port)
                    } else {
                        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                        let port = socket.local_addr().unwrap().port();
                        (Peer::Udp(socket, Some(reachable)), port)
                    };
                    let mut request = addressed("", peer_port, local.port(), tcp);
                    request.method = Method::Options;
                    request.headers.unique_push(CSeq::new("1 OPTIONS").into());
                    peer.send_message(&SipMessage::Request(request)).await;
                    let mut original = incoming.recv().await.unwrap();
                    let connection = original.connection.as_ref().unwrap();
                    assert_eq!(
                        connection.get_addr().get_socketaddr().unwrap().port(),
                        local.port()
                    );
                    let replied = tokio::spawn(async move {
                        original.reply(StatusCode::OK).await.unwrap();
                    });
                    assert_eq!(status(&mut peer).await, 200);
                    replied.await.unwrap();
                    drop(peer);
                    drop(incoming);
                    drop(endpoint);
                    bound.close().await.unwrap();
                    bound.close().await.unwrap();
                    assert!(bound.is_closed());
                    if tcp {
                        TcpListener::bind(local).await.unwrap();
                    } else {
                        UdpSocket::bind(local).await.unwrap();
                    }
                }
            }
        })
        .await
        .expect("owned incoming bind/release deadline");
    }

    #[tokio::test]
    async fn prepared_tcp_invite_and_cancel_use_the_original_bound_port_and_flow() {
        tokio::time::timeout(Duration::from_secs(8), async {
            let mut bound =
                BoundSipTransport::tcp("127.0.0.1:0".parse().unwrap(), EndpointOption::default())
                    .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let peer_port = listener.local_addr().unwrap().port();
            let local = bound.local_addr();
            let mut request = addressed("", local.port(), peer_port, true);
            let destination = SipAddr::try_from(&request.destination()).unwrap();
            request.uri = "sip:peer@prepared.invalid;transport=tcp"
                .try_into()
                .unwrap();
            let original_uri = request.uri.clone();
            let prepared = bound
                .prepare_invite(request, destination, None)
                .await
                .unwrap();
            assert_eq!(
                bound.close().await,
                Err(BindError::Endpoint(SipIoError::InUse))
            );
            assert!(!bound.is_closed());
            let (stream, remote) = listener.accept().await.unwrap();
            assert_eq!(remote, local);
            let mut peer = Peer::Tcp(BufReader::new(stream));
            let endpoint = bound.endpoint().unwrap();
            let media = Arc::new(Media::new(0));
            let call =
                DelayedInvite::start(endpoint.clone(), prepared, None, 5, media.clone()).unwrap();
            let original = peer.receive().await;
            assert_eq!(original.uri, original_uri);
            peer.send(&response(&original, StatusCode::Ringing, peer_port, ""))
                .await;
            call.hangup().await.unwrap();
            let cancel = peer.receive().await;
            assert_eq!(cancel.method, Method::Cancel);
            assert_eq!(cancel.uri, original.uri);
            assert_eq!(branch(&cancel), branch(&original));
            peer.send(&response(&cancel, StatusCode::OK, peer_port, ""))
                .await;
            peer.send(&response(&original, StatusCode::from(487), peer_port, ""))
                .await;
            assert_eq!(peer.receive().await.method, Method::Ack);
            assert_eq!(call.wait_answer().await, CallOutcome::Ended);
            call.close().await.unwrap();
            assert_eq!(media.starts.load(Ordering::SeqCst), 0);
            drop(peer);
            drop(endpoint);
            bound.close().await.unwrap();
            assert!(bound.is_closed());
        })
        .await
        .expect("bound original TCP flow deadline");
    }

    #[tokio::test]
    async fn prepared_socket_owner_and_next_hop_validation_precede_invite_dispatch() {
        tokio::time::timeout(Duration::from_secs(8), async {
            let mut owner =
                BoundSipTransport::udp("127.0.0.1:0".parse().unwrap(), EndpointOption::default())
                    .await
                    .unwrap();
            let mut foreign =
                BoundSipTransport::udp("127.0.0.1:0".parse().unwrap(), EndpointOption::default())
                    .await
                    .unwrap();
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let request = addressed(
                "",
                owner.local_addr().port(),
                socket.local_addr().unwrap().port(),
                false,
            );
            let destination = SipAddr::try_from(&request.destination()).unwrap();
            let wrong_proxy: SipAddr = "127.0.0.1:1"
                .parse::<std::net::SocketAddr>()
                .unwrap()
                .into();
            assert!(matches!(
                owner
                    .prepare_invite(request.clone(), destination.clone(), Some(wrong_proxy))
                    .await,
                Err(BindError::InvalidDestination)
            ));
            let prepared = owner
                .prepare_invite(request, destination, None)
                .await
                .unwrap();
            assert_eq!(
                owner.close().await,
                Err(BindError::Endpoint(SipIoError::InUse))
            );
            assert!(matches!(
                DelayedInvite::start(
                    foreign.endpoint().unwrap(),
                    prepared,
                    None,
                    5,
                    Arc::new(Media::new(0))
                ),
                Err(SipIoError::InvalidRequest)
            ));
            let mut bytes = [0; 1024];
            assert!(
                tokio::time::timeout(Duration::from_millis(50), socket.recv_from(&mut bytes))
                    .await
                    .is_err()
            );
            owner.close().await.unwrap();
            foreign.close().await.unwrap();
        })
        .await
        .expect("prepared socket ownership deadline");
    }

    fn original(body: &str) -> Request {
        format!("INVITE sip:peer@127.0.0.1:5060 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5062;branch=z9hG4bKoriginal\r\nFrom: <sip:fixture@example.invalid>;tag=local-original\r\nTo: <sip:peer@example.invalid>\r\nCall-ID: isolated-original\r\nCSeq: 1 INVITE\r\nContact: <sip:fixture@127.0.0.1:5062>\r\nMax-Forwards: 70\r\nContent-Length: {}\r\n\r\n{body}",body.len()).try_into().unwrap()
    }

    #[tokio::test]
    async fn authentication_preserves_original_identity_body_and_single_continuation() {
        for status in [
            StatusCode::Unauthorized,
            StatusCode::ProxyAuthenticationRequired,
        ] {
            for body in ["", OFFER] {
                let endpoint = EndpointBuilder::new().build();
                let request = original(body);
                let key = TransactionKey::from_request(&request, TransactionRole::Client).unwrap();
                let transaction =
                    Transaction::new_client(key, request.clone(), endpoint.inner.clone(), None);
                let mut exchange = InviteExchange::new(request.clone()).unwrap();
                let mut challenge = response(&request, status.clone(), 5060, "");
                let value = "Digest realm=\"fixture\", nonce=\"original-fixture-nonce\", algorithm=MD5, qop=\"auth\"";
                challenge
                    .headers
                    .unique_push(if status == StatusCode::Unauthorized {
                        WwwAuthenticate::new(value).into()
                    } else {
                        ProxyAuthenticate::new(value).into()
                    });
                let mut foreign = challenge.clone();
                foreign
                    .headers
                    .unique_push(CallId::new("foreign-call").into());
                assert!(matches!(
                    exchange
                        .authenticate(&transaction, &foreign, "fixture", "fixture-only")
                        .await,
                    Err(SipError::InvalidChallenge)
                ));
                for (username, password) in [
                    ("", "fixture-only"),
                    ("with\ncontrol", "fixture-only"),
                    ("fixture", "with\u{0080}control"),
                ] {
                    assert!(matches!(
                        exchange
                            .authenticate(&transaction, &challenge, username, password)
                            .await,
                        Err(SipError::InvalidCredentials)
                    ));
                }
                assert!(matches!(
                    exchange
                        .authenticate(
                            &transaction,
                            &challenge,
                            &"\u{1f600}".repeat(129),
                            "fixture-only"
                        )
                        .await,
                    Err(SipError::InvalidCredentials)
                ));
                assert!(matches!(
                    exchange
                        .authenticate(
                            &transaction,
                            &challenge,
                            "fixture",
                            &"\u{1f600}".repeat(2049)
                        )
                        .await,
                    Err(SipError::InvalidCredentials)
                ));
                let next = exchange
                    .authenticate(&transaction, &challenge, "fixture-auth", "fixture-only")
                    .await
                    .unwrap();
                assert_eq!(
                    next.original.call_id_header().unwrap(),
                    request.call_id_header().unwrap()
                );
                assert_eq!(
                    next.original.from_header().unwrap(),
                    request.from_header().unwrap()
                );
                assert_eq!(next.original.cseq_header().unwrap().seq().unwrap(), 2);
                assert_eq!(next.original.body, request.body);
                assert_ne!(
                    next.original.top_via_header().unwrap(),
                    request.top_via_header().unwrap()
                );
                assert!(next.original.headers.iter().any(|h| {
                    if status == StatusCode::Unauthorized {
                        matches!(h, Header::Authorization(_))
                    } else {
                        matches!(h, Header::ProxyAuthorization(_))
                    }
                }));
                assert!(matches!(
                    exchange
                        .authenticate(&transaction, &challenge, "fixture-auth", "fixture-only")
                        .await,
                    Err(SipError::InvalidChallenge)
                ));
                let mut repeated = challenge.clone();
                repeated.headers.unique_push(CSeq::new("2 INVITE").into());
                assert!(matches!(
                    exchange
                        .authenticate(&next, &repeated, "fixture-auth", "fixture-only")
                        .await,
                    Err(SipError::InvalidChallenge)
                ));
            }
        }
    }

    #[tokio::test]
    async fn bounded_empty_or_space_passwords_are_preserved_as_digest_inputs() {
        let token = CancellationToken::new();
        let endpoint = EndpointBuilder::new()
            .with_cancel_token(token.clone())
            .build();
        for body in ["", OFFER] {
            for status in [
                StatusCode::Unauthorized,
                StatusCode::ProxyAuthenticationRequired,
            ] {
                for password in [String::new(), " ".into(), "\u{1f600}".repeat(2048)] {
                    DigestCredentials::new("fixture-auth".into(), password.clone()).unwrap();
                    let request = original(body);
                    let key =
                        TransactionKey::from_request(&request, TransactionRole::Client).unwrap();
                    let transaction =
                        Transaction::new_client(key, request.clone(), endpoint.inner.clone(), None);
                    let mut exchange = InviteExchange::new(request.clone()).unwrap();
                    let mut challenge = response(&request, status.clone(), 5060, "");
                    let value = "Digest realm=\"fixture\", nonce=\"secret-boundary-fixture\", algorithm=MD5, qop=\"auth\"";
                    challenge
                        .headers
                        .unique_push(if status == StatusCode::Unauthorized {
                            WwwAuthenticate::new(value).into()
                        } else {
                            ProxyAuthenticate::new(value).into()
                        });
                    let continued = exchange
                        .authenticate(&transaction, &challenge, "fixture-auth", &password)
                        .await
                        .unwrap();
                    let raw = continued
                        .original
                        .headers
                        .iter()
                        .find_map(|header| match header {
                            Header::Authorization(value) => Some(value.value()),
                            Header::ProxyAuthorization(value) => Some(value.value()),
                            _ => None,
                        })
                        .unwrap();
                    let parsed = rsipstack::sip::typed::Authorization::parse(raw).unwrap();
                    assert!(rsipstack::dialog::authenticate::verify_digest(
                        &parsed,
                        &password,
                        &Method::Invite,
                        raw
                    ));
                    assert_eq!(continued.original.body, request.body);
                    assert_eq!(continued.original.cseq_header().unwrap().seq().unwrap(), 2);
                }
            }
        }
        token.cancel();
    }

    #[tokio::test]
    async fn saved_ack_fences_foreign_dialogs_and_preserves_original_route_credentials_and_utf8() {
        let endpoint = EndpointBuilder::new().build();
        let mut request = original("");
        request
            .headers
            .push(WwwAuthenticate::new("Digest realm=\"fixture\", nonce=\"fixture\"").into());
        request
            .headers
            .push(ProxyAuthenticate::new("Digest realm=\"fixture\", nonce=\"fixture\"").into());
        request.headers.push(Authorization::new("Digest username=\"fixture\", realm=\"fixture\", nonce=\"fixture\", uri=\"sip:peer@127.0.0.1:5060\", response=\"fixture\"").into());
        request
            .headers
            .push(Route::new("<sip:original-route@127.0.0.1:5060;lr>").into());
        let mut exchange = InviteExchange::new(request.clone()).unwrap();
        let mut final_response = response(&request, StatusCode::OK, 5060, OFFER);
        final_response
            .headers
            .retain(|h| !matches!(h, Header::Contact(_)));
        exchange.select_final(&final_response).unwrap();
        let mut foreign = final_response.clone();
        foreign
            .headers
            .unique_push(To::new("<sip:peer@example.invalid>;tag=foreign").into());
        assert_eq!(
            exchange.prepare_ack(&foreign, Some(REJECTED), &endpoint.inner),
            Err(SipError::InvalidAcknowledgement)
        );
        let answer = "v=0\r\ns=\u{1f600}\r\n";
        let ack = exchange
            .prepare_ack(&final_response, Some(answer), &endpoint.inner)
            .unwrap();
        assert_eq!(ack.uri, request.uri); // original missing-Contact fallback.
        assert_eq!(ack.body, answer.as_bytes());
        assert!(ack.headers.iter().any(|h| matches!(h,Header::ContentLength(length) if length.value()==answer.len().to_string())));
        assert_eq!(ack.from_header().unwrap(), request.from_header().unwrap());
        assert!(
            ack.headers
                .iter()
                .any(|h| matches!(h, Header::Authorization(_)))
        );
        assert_eq!(
            ack.headers
                .iter()
                .filter(|h| matches!(h, Header::Route(_)))
                .collect::<Vec<_>>(),
            request
                .headers
                .iter()
                .filter(|h| matches!(h, Header::Route(_)))
                .collect::<Vec<_>>()
        );
        assert!(exchange.ack_for_retransmission(&foreign).is_none());
        assert_eq!(exchange.ack_for_retransmission(&final_response), Some(&ack));
        assert_eq!(
            exchange.prepare_ack(&final_response, Some(answer), &endpoint.inner),
            Err(SipError::InvalidAcknowledgement)
        );
        let cancel = exchange.cancel_request().unwrap();
        assert_eq!(
            cancel.top_via_header().unwrap(),
            request.top_via_header().unwrap()
        );
        assert_eq!(cancel.cseq_header().unwrap().seq().unwrap(), 1);
        assert!(cancel.body.is_empty());
        assert!(!cancel.headers.iter().any(|h| matches!(
            h,
            Header::Authorization(_)
                | Header::ProxyAuthorization(_)
                | Header::WwwAuthenticate(_)
                | Header::ProxyAuthenticate(_)
                | Header::ContentType(_)
        )));
    }

    #[tokio::test]
    async fn initial_offer_acks_are_empty_and_bodyless_invites_require_real_identity() {
        let endpoint = EndpointBuilder::new().build();
        let request = original(OFFER);
        let mut exchange = InviteExchange::new(request.clone()).unwrap();
        let final_response = response(&request, StatusCode::OK, 5060, OFFER);
        exchange.select_final(&final_response).unwrap();
        assert_eq!(
            exchange.prepare_ack(&final_response, Some(OFFER), &endpoint.inner),
            Err(SipError::InvalidAcknowledgement)
        );
        assert!(
            exchange
                .prepare_ack(&final_response, None, &endpoint.inner)
                .unwrap()
                .body
                .is_empty()
        );
        let whitespace = original(" ");
        assert!(matches!(
            InviteExchange::new(whitespace),
            Err(SipError::InvalidInvite)
        ));
        let mut invalid = original("");
        invalid.method = Method::Options;
        assert!(matches!(
            InviteExchange::new(invalid),
            Err(SipError::InvalidInvite)
        ));
        let mut invalid = original("");
        invalid
            .headers
            .unique_push(From::new("<sip:fixture@example.invalid>").into());
        assert!(matches!(
            InviteExchange::new(invalid),
            Err(SipError::InvalidInvite)
        ));
    }
}
