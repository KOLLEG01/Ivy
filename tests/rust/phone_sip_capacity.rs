//! Actual loopback transactions exercise capacity, control progress and the
//! original unknown-result fence without a SIP account, device or provider.
use super::*;
use rsipstack::sip::{Method, Response, SipMessage, StatusCode, prelude::*};
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    sync::{Semaphore, mpsc},
};

const OFFER: &str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=capacity\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 40000 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
const WIRE: &str = "capacity-original";
struct Media(AtomicBool);
impl DelayedInviteMedia for Media {
    fn is_closed(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn failed(&self) -> bool {
        false
    }
    fn negotiate_offer(&self, value: &str) -> Option<String> {
        (!value.is_empty()).then(|| OFFER.into())
    }
    fn accept_answer(&self, value: &str) -> bool {
        !value.is_empty()
    }
    fn initial_offer(&self) -> Option<String> {
        Some(OFFER.into())
    }
    fn start(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async { true })
    }
}
impl HostedSipMedia for Media {
    fn request_close(&self) {
        self.0.store(true, Ordering::Release);
    }
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), SipHostError>> + Send + '_>> {
        Box::pin(async {
            self.request_close();
            Ok(())
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    HoldResponses,
    FailResponses,
    FailConstructor,
}
async fn until(ready: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Original SIP owner did not settle");
}
fn raw(
    method: &str,
    sequence: u32,
    branch: &str,
    from: SocketAddr,
    target: SocketAddr,
    to: &str,
    body: &str,
    tag: &str,
) -> String {
    let content = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/sdp\r\n"
    };
    format!(
        "{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {from};rport;branch={branch}\r\nFrom: <sip:peer@{from}>;tag={tag}\r\nTo: {to}\r\nCall-ID: {WIRE}\r\nCSeq: {sequence} {method}\r\nContact: <sip:peer@{from}>\r\nMax-Forwards: 70\r\n{content}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}
struct Fixture {
    host: SipHost,
    peer: UdpSocket,
    local: SocketAddr,
    to: String,
    calls: mpsc::Receiver<SipHostCall>,
    started: Arc<AtomicUsize>,
    gate: Arc<Semaphore>,
    book: Arc<Retirements>,
}
impl Fixture {
    async fn new(mode: Mode) -> Self {
        let host = SipHost::new();
        let local = host
            .configure(
                SipHostConfiguration::new(
                    SipHostBinding::new("127.0.0.1:0".parse().unwrap(), Transport::Udp, None, 2)
                        .unwrap(),
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (send, calls) = mpsc::channel(1);
        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(AtomicUsize::new(0));
        let weak = Arc::downgrade(&host.0);
        let held = gate.clone();
        let count = started.clone();
        host.start_requests(Arc::new(move |mut original, _| {
            let weak = weak.clone();
            let send = send.clone();
            let gate = held.clone();
            let count = count.clone();
            Box::pin(async move {
                if original.original.method == Method::Info {
                    count.fetch_add(1, Ordering::AcqRel);
                    if mode == Mode::HoldResponses {
                        gate.acquire().await.unwrap().forget();
                    }
                    if mode == Mode::FailResponses {
                        // A missing original connection deterministically fails
                        // the real stack reply on every supported platform.
                        original.connection.take();
                    }
                    return match original.reply(StatusCode::OK).await {
                        Ok(_) => Ok(()),
                        Err(_) => Err(original),
                    };
                }
                let Some(handle) = weak.upgrade() else {
                    return Err(original);
                };
                let host = SipHost(handle);
                let call = host
                    .prepare_incoming_async(
                        uuid::Uuid::new_v4().to_string(),
                        WIRE.into(),
                        move || async move {
                            if mode == Mode::FailConstructor {
                                Err(SipHostError::CleanupUnconfirmed)
                            } else {
                                Ok(Arc::new(Media(AtomicBool::new(false)))
                                    as Arc<dyn HostedSipMedia>)
                            }
                        },
                    )
                    .unwrap();
                host.adopt_incoming(&call, original, 30)?;
                assert!(send.send(call).await.is_ok());
                Ok(())
            })
        }))
        .unwrap();
        let book = host.shared().lock().unwrap().retirements.clone();
        Self {
            host,
            peer,
            local,
            to: format!("<sip:fixture@{local}>"),
            calls,
            started,
            gate,
            book,
        }
    }
    async fn send(&self, method: &str, sequence: u32, branch: &str, body: &str) {
        let bytes = raw(
            method,
            sequence,
            branch,
            self.peer.local_addr().unwrap(),
            self.local,
            &self.to,
            body,
            "peer-tag",
        );
        self.peer
            .send_to(bytes.as_bytes(), self.local)
            .await
            .unwrap();
    }
    async fn message(&self) -> SipMessage {
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut bytes = [0; 8192];
            let (n, _) = self.peer.recv_from(&mut bytes).await.unwrap();
            SipMessage::try_from(std::str::from_utf8(&bytes[..n]).unwrap()).unwrap()
        })
        .await
        .expect("Original loopback SIP response missing")
    }
    async fn response(&self, status: u16, sequence: u32, method: Method) -> Response {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let SipMessage::Response(response) = self.message().await {
                    let cseq = response.cseq_header().unwrap();
                    if response.status_code.code() == status
                        && cseq.seq().unwrap() == sequence
                        && cseq.method().unwrap() == method
                    {
                        return response;
                    }
                }
            }
        })
        .await
        .expect("Expected original SIP response missing")
    }
    async fn request(&self, method: Method) -> Request {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let SipMessage::Request(request) = self.message().await {
                    if request.method == method {
                        return request;
                    }
                }
            }
        })
        .await
        .expect("Expected original SIP request missing")
    }
    async fn incoming(&mut self, answer: bool, failed: bool) -> SipHostCall {
        self.send("INVITE", 1, "z9hG4bK-cap-initial", OFFER).await;
        let call = tokio::time::timeout(Duration::from_secs(3), self.calls.recv())
            .await
            .unwrap()
            .unwrap();
        if failed {
            return call;
        }
        let ringing = self.response(180, 1, Method::Invite).await;
        self.to = ringing.to_header().unwrap().value().into();
        if answer {
            let waiting = tokio::spawn(self.host.begin_answer(&call, || Ok(())).unwrap());
            self.response(200, 1, Method::Invite).await;
            self.send("ACK", 1, "z9hG4bK-cap-initial-ack", "").await;
            assert_eq!(waiting.await.unwrap().unwrap(), CallOutcome::Connected);
            until(|| call.observation().phase == SipHostCallPhase::Connected).await;
        }
        call
    }
    async fn answer_request(&self, request: &Request, body: &str) {
        let bytes = format!(
            "SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:peer@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
            request.via_header().unwrap().value(),
            request.from_header().unwrap().value(),
            request.to_header().unwrap().value(),
            request.call_id_header().unwrap().value(),
            request.cseq_header().unwrap().value(),
            self.peer.local_addr().unwrap(),
            body.len()
        );
        self.peer
            .send_to(bytes.as_bytes(), self.local)
            .await
            .unwrap();
    }
    async fn outgoing(&mut self) -> SipHostCall {
        let call = self
            .host
            .prepare(uuid::Uuid::new_v4().to_string(), || {
                Ok(Arc::new(Media(AtomicBool::new(false))))
            })
            .unwrap();
        let peer = self.peer.local_addr().unwrap();
        let request = Request::try_from(
            raw(
                "INVITE",
                1,
                "z9hG4bK-cap-outgoing",
                self.local,
                peer,
                &format!("<sip:fixture@{peer}>"),
                OFFER,
                "local-tag",
            )
            .as_str(),
        )
        .unwrap();
        let waiting = tokio::spawn(
            self.host
                .begin_dial(&call, request, peer.into(), None, 30)
                .unwrap(),
        );
        let invite = self.request(Method::Invite).await;
        let response = format!(
            "SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=peer-tag\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:peer@{peer}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{OFFER}",
            invite.via_header().unwrap().value(),
            invite.from_header().unwrap().value(),
            invite.to_header().unwrap().value(),
            invite.call_id_header().unwrap().value(),
            invite.cseq_header().unwrap().value(),
            OFFER.len()
        );
        self.peer
            .send_to(response.as_bytes(), self.local)
            .await
            .unwrap();
        self.request(Method::Ack).await;
        self.to = invite.from_header().unwrap().value().into();
        assert_eq!(waiting.await.unwrap().unwrap(), CallOutcome::Connected);
        call
    }
    async fn remote_bye(&self, call: &SipHostCall) {
        self.send("BYE", 100, "z9hG4bK-cap-bye", "").await;
        self.response(200, 100, Method::Bye).await;
        self.host.release(call).await.unwrap();
        until(|| self.book.usage().1 == 0).await;
        assert!(self.host.observation().calls.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_response_owners_preserve_both_dialog_byes_and_release_original_capacity() {
    for outgoing in [false, true] {
        let mut fixture = Fixture::new(Mode::HoldResponses).await;
        let call = if outgoing {
            fixture.outgoing().await
        } else {
            fixture.incoming(true, false).await
        };
        for sequence in 2..18 {
            fixture
                .send(
                    "INFO",
                    sequence,
                    &format!("z9hG4bK-cap-held-{sequence}"),
                    "",
                )
                .await;
        }
        until(|| fixture.started.load(Ordering::Acquire) == 16).await;
        for sequence in 18..82 {
            fixture
                .send(
                    "INFO",
                    sequence,
                    &format!("z9hG4bK-cap-overflow-{sequence}"),
                    "",
                )
                .await;
        }
        fixture.remote_bye(&call).await;
        assert_eq!(fixture.started.load(Ordering::Acquire), 16);
        assert_eq!(fixture.book.usage().0, 16);
        fixture.gate.add_permits(16);
        until(|| fixture.book.usage().0 == 0).await;
        fixture.host.close().await.unwrap();
        assert_eq!(fixture.book.usage(), (0, 0, 0, 0, 0, false));
        assert!(UdpSocket::bind(fixture.local).await.is_ok());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_response_owners_are_bounded_and_keep_their_original_socket_fence() {
    let mut fixture = Fixture::new(Mode::FailResponses).await;
    let call = fixture.incoming(true, false).await;
    for sequence in 2..18 {
        fixture
            .send(
                "INFO",
                sequence,
                &format!("z9hG4bK-cap-failed-{sequence}"),
                "",
            )
            .await;
        until(|| fixture.book.usage().2 == (sequence - 1) as usize).await;
    }
    for sequence in 18..82 {
        fixture
            .send(
                "INFO",
                sequence,
                &format!("z9hG4bK-cap-not-admitted-{sequence}"),
                "",
            )
            .await;
    }
    fixture.remote_bye(&call).await;
    assert_eq!(fixture.started.load(Ordering::Acquire), 16);
    assert_eq!(
        fixture.host.close().await,
        Err(SipHostError::CleanupUnconfirmed)
    );
    let book = fixture.book.clone();
    let local = fixture.local;
    drop(fixture);
    until(|| book.usage().5).await;
    assert_eq!(book.usage(), (16, 0, 16, 0, 0, true));
    assert!(UdpSocket::bind(local).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_incoming_actor_has_one_retirement_owner_and_cannot_be_readmitted() {
    let mut fixture = Fixture::new(Mode::FailConstructor).await;
    let call = fixture.incoming(false, true).await;
    until(|| fixture.book.usage().4 == 1).await;
    assert_eq!(call.observation().phase, SipHostCallPhase::OutcomeUnknown);
    assert_eq!(
        fixture.host.release(&call).await,
        Err(SipHostError::CleanupUnconfirmed)
    );
    assert!(matches!(
        fixture.host.prepare(call.0.id.clone(), || panic!(
            "Original unknown ID cannot run a factory"
        )),
        Err(SipHostError::OperationConflict)
    ));
    assert_eq!(
        fixture.host.close().await,
        Err(SipHostError::CleanupUnconfirmed)
    );
    let book = fixture.book.clone();
    let local = fixture.local;
    drop(call);
    drop(fixture);
    until(|| book.usage().5).await;
    assert_eq!(book.usage(), (0, 1, 0, 0, 1, true));
    assert!(UdpSocket::bind(local).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_offer_limit_rejects_only_new_offers_and_accepts_out_of_order_original_acks() {
    let mut fixture = Fixture::new(Mode::Normal).await;
    let call = fixture.incoming(true, false).await;
    for sequence in 2..6 {
        fixture
            .send(
                "INVITE",
                sequence,
                &format!("z9hG4bK-cap-offer-{sequence}"),
                OFFER,
            )
            .await;
        fixture.response(200, sequence, Method::Invite).await;
    }
    fixture.send("INVITE", 6, "z9hG4bK-cap-excess", OFFER).await;
    fixture.response(491, 6, Method::Invite).await;
    fixture.send("ACK", 5, "z9hG4bK-cap-last-ack", "").await;
    // The original ACK and a new transaction have separate stack drivers.
    // A 491 before ACK retirement permits a new offer; no oldest ACK is sent.
    let admitted = tokio::time::timeout(Duration::from_secs(3), async {
        for sequence in 7..32 {
            fixture
                .send(
                    "INVITE",
                    sequence,
                    &format!("z9hG4bK-cap-reclaimed-{sequence}"),
                    OFFER,
                )
                .await;
            loop {
                let SipMessage::Response(response) = fixture.message().await else {
                    continue;
                };
                let cseq = response.cseq_header().unwrap();
                if cseq.seq().unwrap() != sequence || cseq.method().unwrap() != Method::Invite {
                    continue;
                }
                match response.status_code.code() {
                    200 => return sequence,
                    491 => break,
                    status => panic!("Unexpected new-offer response after original ACK: {status}"),
                }
            }
        }
        panic!("Original out-of-order ACK did not free its negotiation owner")
    })
    .await
    .unwrap();
    for sequence in [admitted, 4, 3, 2] {
        fixture
            .send("ACK", sequence, &format!("z9hG4bK-cap-ack-{sequence}"), "")
            .await;
    }
    fixture.remote_bye(&call).await;
    fixture.host.close().await.unwrap();
    assert_eq!(fixture.book.usage(), (0, 0, 0, 0, 0, false));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_offer_admission_precedes_media_change_and_counts_selected_ack_owners() {
    let mut fixture = Fixture::new(Mode::Normal).await;
    let call = fixture.incoming(true, false).await;
    let changes = AtomicUsize::new(0);
    for _ in 0..4 {
        call.request_codec_upgrade(|| {
            changes.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap();
        let invite = fixture.request(Method::Invite).await;
        fixture.answer_request(&invite, OFFER).await;
        fixture.request(Method::Ack).await;
    }
    assert_eq!(
        call.request_codec_upgrade(|| {
            changes.fetch_add(1, Ordering::AcqRel);
        }),
        Err(SipHostError::OperationConflict)
    );
    assert_eq!(changes.load(Ordering::Acquire), 4);
    fixture.host.hangup(&call).unwrap();
    let bye = fixture.request(Method::Bye).await;
    fixture.answer_request(&bye, "").await;
    fixture.host.release(&call).await.unwrap();
    fixture.host.close().await.unwrap();
    assert_eq!(fixture.book.usage(), (0, 0, 0, 0, 0, false));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_of_original_ringing_invite_progresses_under_dialog_offer_pressure() {
    let mut fixture = Fixture::new(Mode::Normal).await;
    let call = fixture.incoming(false, false).await;
    for sequence in 2..50 {
        fixture
            .send(
                "INVITE",
                sequence,
                &format!("z9hG4bK-cap-ringing-offer-{sequence}"),
                OFFER,
            )
            .await;
    }
    fixture.send("CANCEL", 1, "z9hG4bK-cap-initial", "").await;
    fixture.response(487, 1, Method::Invite).await;
    fixture.host.release(&call).await.unwrap();
    fixture.host.close().await.unwrap();
    assert_eq!(fixture.book.usage(), (0, 0, 0, 0, 0, false));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outgoing_failed_control_replies_retain_originals_with_bounded_dialog_capacity() {
    let mut fixture = Fixture::new(Mode::Normal).await;
    let call = fixture.outgoing().await;
    let endpoint = fixture
        .host
        .shared()
        .lock()
        .unwrap()
        .binding
        .as_ref()
        .unwrap()
        .endpoint()
        .unwrap();
    for sequence in 2..10 {
        let request = Request::try_from(
            raw(
                "OPTIONS",
                sequence,
                &format!("z9hG4bK-cap-bad-options-{sequence}"),
                fixture.peer.local_addr().unwrap(),
                fixture.local,
                &fixture.to,
                "",
                "peer-tag",
            )
            .as_str(),
        )
        .unwrap();
        let key = rsipstack::transaction::key::TransactionKey::from_request(
            &request,
            rsipstack::transaction::key::TransactionRole::Server,
        )
        .unwrap();
        let mut original = Transaction::new_server(key, request, endpoint.inner().clone(), None);
        original.destination = Some("127.0.0.1:0".parse::<SocketAddr>().unwrap().into());
        assert!(fixture.host.deliver_control(original).is_ok());
        until(|| fixture.book.usage().2 == (sequence - 1) as usize).await;
    }
    assert_eq!(call.observation().phase, SipHostCallPhase::OutcomeUnknown);
    assert_eq!(
        fixture.host.release(&call).await,
        Err(SipHostError::CleanupUnconfirmed)
    );
    assert_eq!(
        fixture.host.close().await,
        Err(SipHostError::CleanupUnconfirmed)
    );
    let book = fixture.book.clone();
    let local = fixture.local;
    drop(endpoint);
    drop(call);
    drop(fixture);
    until(|| book.usage().5).await;
    assert_eq!(book.usage(), (0, 0, 8, 0, 0, true));
    assert!(UdpSocket::bind(local).await.is_err());
}
