#[cfg(test)]
mod tests {
    use ivy_phone_bridge::sip::{CallAction, DelayedOfferCall, InviteExchange, SipError};
    use rsipstack::{
        EndpointBuilder,
        sip::{Method, Request, Response, SipMessage, StatusCode, headers::*, prelude::HeadersExt},
        transaction::{
            endpoint::{EndpointOption, MessageInspector},
            key::{TransactionKey, TransactionRole},
            transaction::Transaction,
        },
        transport::{SipAddr, TransportLayer, udp::UdpConnection},
    };
    use std::time::Duration;
    use tokio::{net::UdpSocket, sync::mpsc};
    use tokio_util::sync::CancellationToken;

    const OFFER: &str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 40000 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
    const REJECTED: &str = "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=fixture\r\nc=IN IP4 0.0.0.0\r\nt=0 0\r\nm=audio 0 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\na=inactive\r\n";

    struct Observe(mpsc::Sender<Response>);
    impl MessageInspector for Observe {
        fn before_send(&self, msg: SipMessage, _: Option<&SipAddr>) -> SipMessage {
            msg
        }
        fn after_received(&self, msg: SipMessage, _: Option<&SipAddr>) -> SipMessage {
            if let SipMessage::Response(response) = &msg {
                if response.status_code.code() == 200
                    && response.cseq_header().unwrap().method().unwrap() == Method::Invite
                {
                    self.0
                        .try_send(response.clone())
                        .expect("bounded fixture response queue");
                }
            }
            msg
        }
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
            let (observed_tx, mut observed) = mpsc::channel(8);
            let endpoint = EndpointBuilder::new().with_transport_layer(transport)
                .with_cancel_token(token.child_token())
                .with_option(EndpointOption { auto_ack_2xx: false, ..Default::default() })
                .with_inspector(Box::new(Observe(observed_tx))).build();
            let inner = endpoint.inner.clone();
            let server = tokio::spawn(async move { inner.serve().await });
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
            let key = TransactionKey::from_request(&invite, TransactionRole::Client).unwrap();
            let mut transaction = Transaction::new_client(key, invite.clone(), endpoint.inner.clone(), None);
            let mut lifecycle = DelayedOfferCall::default();
            lifecycle.start().unwrap(); lifecycle.sending(1).unwrap();
            transaction.send().await.unwrap();
            loop {
                let SipMessage::Response(response) = transaction.receive().await.unwrap() else { panic!("Response required") };
                if response.status_code.code() >= 180 { break; }
            }
            assert!(lifecycle.provisional(1, false).is_empty());
            assert_eq!(lifecycle.hangup(), vec![CallAction::Cancel]);
            let cancel = exchange.cancel_request().unwrap();
            transaction.send_cancel(cancel).await.unwrap();
            let final_response = loop {
                let SipMessage::Response(response) = transaction.receive().await.unwrap() else { panic!("Response required") };
                if response.status_code.code() == 200 { break response; }
            };
            observed.recv().await.unwrap(); // same first 200, observed before dispatch.
            assert!(lifecycle.final_response(1, 200));
            assert_eq!(lifecycle.negotiated(true, false).unwrap(), CallAction::Acknowledge { reject_media: true });
            exchange.select_final(&final_response).unwrap();
            let ack = exchange.prepare_ack(&final_response, Some(REJECTED), &endpoint.inner).unwrap();
            let destination = SipAddr::try_from(&ack.destination()).unwrap();
            let (connection, destination) = endpoint.inner.transport_layer.lookup(&destination, None).await.unwrap();
            connection.send(ack.clone().into(), Some(&destination)).await.unwrap();
            assert_eq!(lifecycle.acknowledged(true, false).unwrap(), vec![CallAction::Bye { authenticated: false }]);
            let mut bye = ack.clone();
            bye.method = Method::Bye; bye.body.clear();
            bye.headers.retain(|h| !matches!(h, Header::ContentType(_) | Header::ContentLength(_)));
            bye.headers.unique_push(ContentLength::from(0).into());
            bye.headers.unique_push(CSeq::new("2 BYE").into());
            bye.headers.unique_push(Via::new(format!("SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bKbye")).into());
            let key = TransactionKey::from_request(&bye, TransactionRole::Client).unwrap();
            let mut ending = Transaction::new_client(key, bye, endpoint.inner.clone(), None);
            ending.send().await.unwrap();
            let repeated = observed.recv().await.unwrap();
            assert_eq!(repeated.call_id_header().unwrap(), final_response.call_id_header().unwrap());
            assert_eq!(repeated.to_header().unwrap(), final_response.to_header().unwrap());
            let cached = exchange.ack_for_retransmission(&repeated).unwrap();
            assert_eq!(cached, &ack);
            connection.send(cached.clone().into(), Some(&destination)).await.unwrap();
            loop {
                let SipMessage::Response(response) = ending.receive().await.unwrap() else { panic!("Response required") };
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
            token.cancel(); server.await.unwrap().unwrap();
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
