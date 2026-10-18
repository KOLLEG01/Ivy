#[cfg(test)]
mod tests {
    use ivy_phone_bridge::sip::{
        CallAction, DelayedOfferCall, InviteExchange, SipEndpoint, SipError, SipIoError,
    };
    use rsipstack::{
        EndpointBuilder,
        sip::{
            Method, Request, Response, StatusCode,
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
    use std::time::Duration;
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::{TcpListener, TcpStream, UdpSocket},
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
            let bytes = response.to_string();
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
                    let options = EndpointOption { t1: Duration::from_millis(10), t1x64: Duration::from_millis(80), ..Default::default() };
                    let (mut endpoint, mut peer, local, peer_port, _) = network(false, options).await;
                    let mut request = addressed("", local, peer_port, false);
                    request.method = method;
                    request.headers.unique_push(CSeq::new(format!("1 {method}")).into());
                    let mut transaction = endpoint.client(request, None).unwrap();
                    let original = peer.receive().await;
                    if remote {
                        peer.send(&response(&original, StatusCode::RequestTimeout, peer_port, "")).await;
                        assert_eq!(transaction.next_response().await.unwrap().status_code.code(), 408);
                        assert!(transaction.completed().await.is_ok());
                    } else {
                        assert!(matches!(transaction.next_response().await, Err(SipIoError::TimedOut)));
                        assert!(matches!(transaction.completed().await, Err(SipIoError::TimedOut)));
                    }
                    transaction.retire().await.unwrap(); endpoint.shutdown().await.unwrap();
                }
            }
        }).await.expect("local/remote timeout distinction deadline");
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
            let token = CancellationToken::new();
            let transport = TransportLayer::new(token.child_token());
            let udp = UdpConnection::create_connection("127.0.0.1:0".parse().unwrap(), None, Some(token.child_token())).await.unwrap();
            let port = udp.get_addr().addr.port.map(u16::from).unwrap();
            transport.add_transport(udp.into());
            let mut endpoint = SipEndpoint::start(transport, token.child_token(), EndpointOption::default());
            let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let peer_port = peer.local_addr().unwrap().port();
            let peer_task = tokio::spawn(async move {
                let mut bytes = [0; 8192];
                let mut original = None;
                let mut wire = Vec::new();
                let mut ack_count = 0;
                let mut bye_seen = false;
                while ack_count < 2 || !bye_seen {
                    let (count, address) = peer.recv_from(&mut bytes).await.unwrap();
                    assert!(address.ip().is_loopback());
                    let request = Request::try_from(std::str::from_utf8(&bytes[..count]).unwrap()).unwrap();
                    wire.push(request.clone());
                    match request.method {
                        Method::Invite => {
                            original = Some(request.clone());
                            let ringing = response(&request, StatusCode::Ringing, peer_port, "");
                            peer.send_to(ringing.to_string().as_bytes(), address).await.unwrap();
                        }
                        Method::Cancel => {
                            let original = original.as_ref().unwrap();
                            assert_eq!(request.call_id_header().unwrap(), original.call_id_header().unwrap());
                            assert_eq!(request.cseq_header().unwrap().seq().unwrap(), original.cseq_header().unwrap().seq().unwrap());
                            assert_eq!(request.top_via_header().unwrap(), original.top_via_header().unwrap());
                            let accepted = response(&request, StatusCode::OK, peer_port, "");
                            peer.send_to(accepted.to_string().as_bytes(), address).await.unwrap();
                            let final_response = response(original, StatusCode::OK, peer_port, OFFER);
                            peer.send_to(final_response.to_string().as_bytes(), address).await.unwrap();
                        }
                        Method::Ack => {
                            ack_count += 1;
                            assert_eq!(request.body, REJECTED.as_bytes());
                            assert_eq!(request.to_header().unwrap().tag().unwrap().unwrap().value(), "original-peer");
                            if ack_count == 1 {
                                let final_response = response(original.as_ref().unwrap(), StatusCode::OK, peer_port, OFFER);
                                peer.send_to(final_response.to_string().as_bytes(), address).await.unwrap();
                            }
                        }
                        Method::Bye => {
                            bye_seen = true;
                            assert!(ack_count > 0, "BYE cannot overtake negotiated ACK");
                            let accepted = response(&request, StatusCode::OK, peer_port, "");
                            peer.send_to(accepted.to_string().as_bytes(), address).await.unwrap();
                        }
                        other => panic!("Unexpected request {other}"),
                    }
                }
                wire
            });
            let invite: Request = format!("INVITE sip:peer@127.0.0.1:{peer_port} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bKoriginal\r\nFrom: <sip:fixture@example.invalid>;tag=local-original\r\nTo: <sip:peer@example.invalid>\r\nCall-ID: isolated-original\r\nCSeq: 1 INVITE\r\nContact: <sip:fixture@127.0.0.1:{port}>\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n").try_into().unwrap();
            let mut exchange = InviteExchange::new(invite.clone()).unwrap();
            let mut transaction = endpoint.client(invite.clone(), None).unwrap();
            let mut lifecycle = DelayedOfferCall::default();
            lifecycle.start().unwrap(); lifecycle.sending(1).unwrap();
            assert!(matches!(endpoint.client(invite, None), Err(SipIoError::DuplicateTransaction)));
            assert_eq!(endpoint.shutdown().await, Err(SipIoError::InUse));
            loop {
                let response = transaction.next_response().await.unwrap();
                if response.status_code.code() >= 180 { break; }
            }
            assert!(lifecycle.provisional(1, false).is_empty());
            assert_eq!(lifecycle.hangup(), vec![CallAction::Cancel]);
            let cancel = exchange.cancel_request().unwrap();
            let mut cancellation = endpoint.client(cancel, None).unwrap();
            let final_response = loop {
                let response = transaction.next_response().await.unwrap();
                if response.status_code.code() == 200 { break response; }
            };
            assert_eq!(cancellation.next_response().await.unwrap().status_code.code(), 200);
            assert!(!lifecycle.cleanup_confirmed(), "CANCEL response does not settle the original call");
            assert!(lifecycle.final_response(1, 200));
            assert_eq!(lifecycle.negotiated(true, false).unwrap(), CallAction::Acknowledge { reject_media: true });
            exchange.select_final(&final_response).unwrap();
            let ack = exchange.prepare_ack(&final_response, Some(REJECTED), endpoint.inner()).unwrap();
            transaction.send_ack(&ack).await.unwrap();
            assert_eq!(lifecycle.acknowledged(true, false).unwrap(), vec![CallAction::Bye { authenticated: false }]);
            let mut bye = ack.clone();
            bye.method = Method::Bye; bye.body.clear();
            bye.headers.retain(|h| !matches!(h, Header::ContentType(_) | Header::ContentLength(_)));
            bye.headers.unique_push(ContentLength::from(0).into());
            bye.headers.unique_push(CSeq::new("2 BYE").into());
            bye.headers.unique_push(Via::new(format!("SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bKbye")).into());
            let mut ending = endpoint.client(bye, None).unwrap();
            let repeated = transaction.next_response().await.unwrap();
            assert_eq!(repeated.call_id_header().unwrap(), final_response.call_id_header().unwrap());
            assert_eq!(repeated.to_header().unwrap(), final_response.to_header().unwrap());
            let cached = exchange.ack_for_retransmission(&repeated).unwrap();
            assert_eq!(cached, &ack);
            transaction.send_ack(cached).await.unwrap();
            loop {
                let response = ending.next_response().await.unwrap();
                if response.status_code.code() >= 200 {
                    lifecycle.bye_response(response.status_code.code(), false).unwrap(); break;
                }
            }
            assert!(!lifecycle.is_active()); assert!(lifecycle.cleanup_confirmed());
            lifecycle.close().unwrap();
            let wire = peer_task.await.unwrap();
            let acks: Vec<_> = wire.iter().filter(|r| r.method == Method::Ack).collect();
            assert_eq!(acks.len(), 2); assert_eq!(acks[0], acks[1]);
            assert_eq!(wire.iter().filter(|r| r.method == Method::Invite).count(), 1);
            cancellation.retire().await.unwrap(); ending.retire().await.unwrap(); transaction.retire().await.unwrap();
            assert_eq!(endpoint.inner().get_stats().running_transactions, 0);
            endpoint.shutdown().await.unwrap();
        }).await.expect("loopback call deadline");
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
                    ("fixture", ""),
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
