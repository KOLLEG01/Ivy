use ivy_phone_bridge::sip::{
    CallOutcome, DelayedInviteMedia, HostedSipMedia, RegistrationPeer, RegistrationPhase,
    RegistrationTransport, SipHost, SipHostBinding, SipHostCallPhase, SipHostConfiguration,
    SipHostError, SipRegistrationPlan, SipRegistrationSettings,
};
use rsipstack::{
    sip::{Method, Request, Response, headers::*, prelude::*},
    transport::SipAddr,
};
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};
use tokio::{net::UdpSocket, sync::Semaphore};

const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
const OTHER: &str = "11234567-89ab-cdef-0123-456789abcdef";
const OFFER: &str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 40000 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
const ANSWER: &str = "v=0\r\no=- 2 2 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 40002 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
struct Media {
    closing: AtomicBool,
    closed: AtomicBool,
    starts: AtomicUsize,
    closes: AtomicUsize,
    drain: Semaphore,
    fail: bool,
}
impl Media {
    fn new(permits: usize, fail: bool) -> Arc<Self> {
        Arc::new(Self {
            closing: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            starts: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            drain: Semaphore::new(permits),
            fail,
        })
    }
}
impl DelayedInviteMedia for Media {
    fn is_closed(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }
    fn failed(&self) -> bool {
        false
    }
    fn negotiate_offer(&self, offer: &str) -> Option<String> {
        (offer == OFFER).then(|| ANSWER.into())
    }
    fn rejected_answer(&self, _: &str) -> Option<String> {
        None
    }
    fn start(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async move {
            self.starts.fetch_add(1, Ordering::AcqRel);
            true
        })
    }
}
impl HostedSipMedia for Media {
    fn request_close(&self) {
        self.closing.store(true, Ordering::Release);
    }
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), SipHostError>> + Send + '_>> {
        Box::pin(async move {
            self.closes.fetch_add(1, Ordering::AcqRel);
            let permit = self.drain.acquire().await.unwrap();
            permit.forget();
            if self.fail {
                return Err(SipHostError::CleanupUnconfirmed);
            }
            self.closed.store(true, Ordering::Release);
            Ok(())
        })
    }
}
fn configuration(maximum: usize, registration: Option<SocketAddr>) -> SipHostConfiguration {
    let registration = registration.map(|remote| {
        SipRegistrationPlan::new(
            SipRegistrationSettings::new(
                "sip:public-user@account.example.test",
                &format!("sip:{remote}"),
                "contact-user".into(),
                "digest-user".into(),
                None,
                60,
                10,
                0,
                rsipstack::sip::Transport::Udp,
            )
            .unwrap(),
            "fixture-only".into(),
        )
        .unwrap()
    });
    SipHostConfiguration::new(
        SipHostBinding::new(
            "127.0.0.1:0".parse().unwrap(),
            rsipstack::sip::Transport::Udp,
            None,
            maximum,
        )
        .unwrap(),
        registration,
    )
    .unwrap()
}
async fn until(ready: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("host transition deadline");
}
async fn received(peer: &UdpSocket) -> (Request, SocketAddr) {
    let mut bytes = [0; 8192];
    let (count, remote) = tokio::time::timeout(Duration::from_secs(3), peer.recv_from(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    (
        std::str::from_utf8(&bytes[..count])
            .unwrap()
            .try_into()
            .unwrap(),
        remote,
    )
}
fn response(request: &Request, code: u16, extra: &str, body: &str) -> Response {
    let via = request.top_via_header().unwrap();
    let from = request.from_header().unwrap().value();
    let to = request.to_header().unwrap().value();
    let call = request.call_id_header().unwrap().value();
    let cseq = request.cseq_header().unwrap().value();
    format!("SIP/2.0 {code} Fixture\r\nVia: {}\r\nFrom: {from}\r\nTo: {to};tag=remote\r\nCall-ID: {call}\r\nCSeq: {cseq}\r\n{extra}Content-Length: {}\r\n\r\n{body}",via.value(),body.len()).try_into().unwrap()
}
async fn send(peer: &UdpSocket, remote: SocketAddr, response: &Response) {
    peer.send_to(response.to_string().as_bytes(), remote)
        .await
        .unwrap();
}
fn invite(local: SocketAddr) -> Request {
    format!("INVITE sip:callee@call.example.test SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-host-fixture;rport\r\nFrom: <sip:caller@account.example.test>;tag=host-fixture\r\nTo: <sip:callee@call.example.test>\r\nCall-ID: host-call-fixture\r\nCSeq: 1 INVITE\r\nContact: <sip:caller@{local}>\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n").try_into().unwrap()
}
async fn cancel_observer<F: Future>(future: F) {
    let mut future = Box::pin(future);
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(future);
}

#[tokio::test]
async fn configure_waiter_cannot_erase_the_once_latch_or_original_socket() {
    let host = SipHost::new();
    cancel_observer(host.configure(configuration(1, None))).await;
    until(|| host.observation().endpoint.is_some()).await;
    let local = host.observation().endpoint.unwrap();
    assert_ne!(local.port(), 0);
    assert_eq!(
        host.configure(configuration(1, None)).await,
        Err(SipHostError::OperationConflict)
    );
    assert!(UdpSocket::bind(local).await.is_err());
    host.close().await.unwrap();
    host.close().await.unwrap();
    assert!(host.observation().closed);
    drop(UdpSocket::bind(local).await.unwrap());

    let occupied = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let host = SipHost::new();
    let config = SipHostConfiguration::new(
        SipHostBinding::new(
            occupied.local_addr().unwrap(),
            rsipstack::sip::Transport::Udp,
            None,
            1,
        )
        .unwrap(),
        None,
    )
    .unwrap();
    assert!(matches!(
        host.configure(config).await,
        Err(SipHostError::Binding(_))
    ));
    assert!(host.observation().configured);
    assert_eq!(
        host.configure(configuration(1, None)).await,
        Err(SipHostError::OperationConflict)
    );
    host.close().await.unwrap();
}

#[tokio::test]
async fn admission_waits_for_original_media_cleanup_and_rejects_stale_or_foreign_handles() {
    let host = SipHost::new();
    host.configure(configuration(1, None)).await.unwrap();
    let media = Media::new(0, false);
    let original = host.prepare(ID.into(), || Ok(media.clone())).unwrap();
    assert_eq!(original.observation().phase, SipHostCallPhase::Prepared);
    assert!(matches!(
        host.prepare(OTHER.into(), || panic!(
            "rejected admission cannot construct media"
        )),
        Err(SipHostError::OperationConflict)
    ));
    assert!(matches!(
        host.prepare("bad-id".into(), || panic!(
            "invalid ID cannot construct media"
        )),
        Err(SipHostError::InvalidRequest)
    ));
    host.hangup(&original).unwrap();
    cancel_observer(host.release(&original)).await;
    until(|| media.closes.load(Ordering::Acquire) == 1).await;
    assert_eq!(host.observation().calls.len(), 1);
    assert!(matches!(
        host.prepare(OTHER.into(), || panic!("pending release retains slot")),
        Err(SipHostError::OperationConflict)
    ));
    media.drain.add_permits(1);
    until(|| host.observation().calls.is_empty()).await;
    let next_media = Media::new(1, false);
    let next = host.prepare(ID.into(), || Ok(next_media.clone())).unwrap();
    assert_eq!(host.hangup(&original), Err(SipHostError::NotReady));
    let foreign = SipHost::new();
    foreign.configure(configuration(1, None)).await.unwrap();
    let other = foreign
        .prepare(ID.into(), || Ok(Media::new(1, false)))
        .unwrap();
    assert_eq!(host.release(&other).await, Err(SipHostError::NotReady));
    assert_eq!(next.observation().phase, SipHostCallPhase::Prepared);
    host.release(&next).await.unwrap();
    assert_eq!(next_media.closes.load(Ordering::Acquire), 1);
    assert_eq!(host.observation().calls.len(), 0);
    host.close().await.unwrap();
    foreign.close().await.unwrap();
}

#[tokio::test]
async fn original_maximum_of_thirty_two_slots_preserves_insertion_order() {
    let host = SipHost::new();
    host.configure(configuration(32, None)).await.unwrap();
    for at in 0..32 {
        let id = format!("{at:08x}-89ab-cdef-0123-456789abcdef");
        host.prepare(id, || Ok(Media::new(1, false))).unwrap();
    }
    assert_eq!(host.observation().calls.len(), 32);
    assert!(matches!(
        host.prepare("ffffffff-89ab-cdef-0123-456789abcdef".into(), || panic!(
            "no thirty-third media owner"
        )),
        Err(SipHostError::OperationConflict)
    ));
    for (at, call) in host.observation().calls.iter().enumerate() {
        assert!(call.id.starts_with(&format!("{at:08x}")));
    }
    host.close().await.unwrap();
    assert!(host.observation().calls.is_empty());
}

#[tokio::test]
async fn reconnect_requires_an_idle_host_and_survives_a_cancelled_waiter() {
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let host = SipHost::new();
    let local = host
        .configure(configuration(1, Some(peer.local_addr().unwrap())))
        .await
        .unwrap();
    let (first, from) = received(&peer).await;
    assert_eq!(from, local);
    send(&peer, from, &response(&first, 200, "Expires: 60\r\n", "")).await;
    until(|| {
        host.observation()
            .registration
            .is_some_and(|r| r.phase == RegistrationPhase::Registered)
    })
    .await;
    let media = Media::new(1, false);
    let call = host.prepare(ID.into(), || Ok(media.clone())).unwrap();
    assert_eq!(
        host.reconnect_registration().await,
        Err(SipHostError::OperationConflict)
    );
    host.release(&call).await.unwrap();
    cancel_observer(host.reconnect_registration()).await;
    assert!(matches!(
        host.prepare(OTHER.into(), || panic!("reconnect blocks admission")),
        Err(SipHostError::OperationConflict)
    ));
    assert_eq!(
        host.reconnect_registration().await,
        Err(SipHostError::OperationConflict)
    );
    let (remove, source) = received(&peer).await;
    assert_eq!(source, local);
    assert_eq!(remove.expires_header().unwrap().value(), "0");
    assert_eq!(
        first.call_id_header().unwrap(),
        remove.call_id_header().unwrap()
    );
    send(&peer, source, &response(&remove, 200, "", "")).await;
    let (next, source) = received(&peer).await;
    assert_eq!(source, local);
    assert_ne!(
        first.call_id_header().unwrap(),
        next.call_id_header().unwrap()
    );
    send(&peer, source, &response(&next, 200, "Expires: 60\r\n", "")).await;
    until(|| {
        host.observation()
            .registration
            .is_some_and(|r| r.phase == RegistrationPhase::Registered && r.remote_removed.is_none())
    })
    .await;
    assert!(host.matches_registered_peer(RegistrationPeer {
        address: source,
        transport: RegistrationTransport::Udp
    }));
    let closing = host.clone();
    let close = tokio::spawn(async move { closing.close().await });
    let (remove, source) = received(&peer).await;
    assert_eq!(remove.expires_header().unwrap().value(), "0");
    send(&peer, source, &response(&remove, 200, "", "")).await;
    close.await.unwrap().unwrap();
    assert!(!host.matches_registered_peer(RegistrationPeer {
        address: source,
        transport: RegistrationTransport::Udp
    }));
    drop(UdpSocket::bind(local).await.unwrap());
}

#[tokio::test]
async fn unknown_remote_unregister_does_not_block_locally_drained_reconnect() {
    tokio::time::timeout(Duration::from_secs(6), async {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let host = SipHost::new();
        let local = host
            .configure(configuration(1, Some(peer.local_addr().unwrap())))
            .await
            .unwrap();
        let (first, source) = received(&peer).await;
        send(&peer, source, &response(&first, 200, "Expires: 60\r\n", "")).await;
        until(|| {
            host.observation()
                .registration
                .is_some_and(|r| r.phase == RegistrationPhase::Registered)
        })
        .await;
        cancel_observer(host.reconnect_registration()).await;
        let (remove, source) = received(&peer).await;
        assert_eq!(remove.expires_header().unwrap().value(), "0");
        assert_eq!(source, local);
        let started = tokio::time::Instant::now();
        let next = loop {
            let (request, source) = received(&peer).await;
            assert_eq!(source, local);
            if request.expires_header().unwrap().value() != "0" {
                break request;
            }
        };
        assert!(started.elapsed() >= Duration::from_millis(2800));
        assert_ne!(
            next.call_id_header().unwrap(),
            first.call_id_header().unwrap()
        );
        send(&peer, source, &response(&next, 200, "Expires: 60\r\n", "")).await;
        until(|| {
            host.observation()
                .registration
                .is_some_and(|r| r.phase == RegistrationPhase::Registered)
        })
        .await;
        let closing = host.clone();
        let close = tokio::spawn(async move { closing.close().await });
        let (remove, source) = received(&peer).await;
        send(&peer, source, &response(&remove, 200, "", "")).await;
        close.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn failed_original_cleanup_keeps_the_slot_and_binding_fenced() {
    let host = SipHost::new();
    let local = host.configure(configuration(1, None)).await.unwrap();
    let media = Media::new(1, true);
    let call = host.prepare(ID.into(), || Ok(media.clone())).unwrap();
    assert_eq!(
        host.release(&call).await,
        Err(SipHostError::CleanupUnconfirmed)
    );
    assert_eq!(
        host.release(&call).await,
        Err(SipHostError::CleanupUnconfirmed)
    );
    assert_eq!(media.closes.load(Ordering::Acquire), 1);
    assert_eq!(host.observation().calls.len(), 1);
    assert!(matches!(
        host.prepare(OTHER.into(), || panic!(
            "unknown cleanup keeps original slot"
        )),
        Err(SipHostError::OperationConflict)
    ));
    assert_eq!(host.close().await, Err(SipHostError::CleanupUnconfirmed));
    assert!(!host.observation().closed);
    assert!(UdpSocket::bind(local).await.is_err());
    drop(host);
    tokio::task::yield_now().await;
    assert!(
        UdpSocket::bind(local).await.is_err(),
        "last handle Drop preserves failed-close fence until native runtime exit"
    );
}

#[tokio::test]
async fn original_delayed_invite_survives_dropped_dial_and_release_observers() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let host = SipHost::new();
        let local = host.configure(configuration(1, None)).await.unwrap();
        let media = Media::new(0, false);
        let call = host.prepare(ID.into(), || Ok(media.clone())).unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        cancel_observer(host.dial_delayed(
            &call,
            invite(local),
            peer.local_addr().unwrap().into(),
            None,
            10,
        ))
        .await;
        let (original, source) = received(&peer).await;
        assert_eq!(source, local);
        assert_eq!(original.method, Method::Invite);
        assert_eq!(host.observation().calls[0].phase, SipHostCallPhase::Dialing);
        host.hangup(&call).unwrap();
        cancel_observer(host.release(&call)).await;
        let (cancel, source) = received(&peer).await;
        assert_eq!(cancel.method, Method::Cancel);
        assert_eq!(source, local);
        send(&peer, source, &response(&cancel, 200, "", "")).await;
        send(&peer, source, &response(&original, 487, "", "")).await;
        let (ack, source) = received(&peer).await;
        assert_eq!(ack.method, Method::Ack);
        assert_eq!(source, local);
        assert_eq!(
            ack.call_id_header().unwrap(),
            original.call_id_header().unwrap()
        );
        until(|| media.closes.load(Ordering::Acquire) == 1).await;
        assert_eq!(host.observation().calls.len(), 1);
        assert!(matches!(
            host.prepare(OTHER.into(), || panic!("media has not acknowledged drain")),
            Err(SipHostError::OperationConflict)
        ));
        media.drain.add_permits(1);
        until(|| host.observation().calls.is_empty()).await;
        host.close().await.unwrap();
        drop(UdpSocket::bind(local).await.unwrap());
        assert_eq!(media.starts.load(Ordering::Acquire), 0);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn connected_dialog_controls_return_to_original_call_and_slot_waits_for_release() {
    tokio::time::timeout(Duration::from_secs(5),async {
        let host=SipHost::new();let local=host.configure(configuration(1,None)).await.unwrap();
        let mut incoming=host.incoming_transactions().unwrap();
        let media=Media::new(1,false);let call=host.prepare(ID.into(),||Ok(media.clone())).unwrap();
        let peer=UdpSocket::bind("127.0.0.1:0").await.unwrap();let remote=peer.local_addr().unwrap();
        let selected=host.clone();let selected_call=call.clone();
        let dial=tokio::spawn(async move {selected.dial_delayed(&selected_call,invite(local),remote.into(),None,10).await});
        let (original,source)=received(&peer).await;
        send(&peer,source,&response(&original,200,&format!("Contact: <sip:callee@{remote}>\r\nContent-Type: application/sdp\r\n"),OFFER)).await;
        let (ack,_)=received(&peer).await;assert_eq!(ack.method,Method::Ack);assert_eq!(ack.body,ANSWER.as_bytes());
        assert_eq!(dial.await.unwrap().unwrap(),CallOutcome::Connected);
        assert_eq!(host.observation().calls[0].phase,SipHostCallPhase::Connected);
        let bye:Request=format!("BYE sip:caller@{local} SIP/2.0\r\nVia: SIP/2.0/UDP {remote};branch=z9hG4bK-host-bye\r\nFrom: <sip:callee@call.example.test>;tag=remote\r\nTo: <sip:caller@account.example.test>;tag=host-fixture\r\nCall-ID: host-call-fixture\r\nCSeq: 9 BYE\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n").try_into().unwrap();
        peer.send_to(bye.to_string().as_bytes(),local).await.unwrap();
        let transaction=incoming.recv().await.unwrap();assert!(host.deliver_control(transaction).is_ok());
        let mut bytes=[0;8192];let (count,_)=peer.recv_from(&mut bytes).await.unwrap();
        let ended:Response=std::str::from_utf8(&bytes[..count]).unwrap().try_into().unwrap();assert_eq!(ended.status_code.code(),200);
        until(||host.observation().calls[0].phase==SipHostCallPhase::LocalEnded).await;
        assert_eq!(host.observation().calls.len(),1);assert_eq!(media.starts.load(Ordering::Acquire),1);
        host.release(&call).await.unwrap();drop(incoming);host.close().await.unwrap();
    }).await.unwrap();
}

#[tokio::test]
async fn hangup_before_route_preparation_cannot_dispatch_an_invite_later() {
    let host = SipHost::new();
    let local = host.configure(configuration(1, None)).await.unwrap();
    let media = Media::new(1, false);
    let call = host.prepare(ID.into(), || Ok(media.clone())).unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    cancel_observer(host.dial_delayed(
        &call,
        invite(local),
        peer.local_addr().unwrap().into(),
        None,
        10,
    ))
    .await;
    host.hangup(&call).unwrap();
    host.release(&call).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(40), received(&peer))
            .await
            .is_err()
    );
    assert_eq!(media.starts.load(Ordering::Acquire), 0);
    host.close().await.unwrap();
}
