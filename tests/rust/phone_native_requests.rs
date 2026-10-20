use super::*;
use rsipstack::sip::Response;
use std::net::SocketAddr;

fn request(
    method: &str,
    peer: SocketAddr,
    target: SocketAddr,
    sequence: u32,
    tagged: bool,
) -> String {
    format!(
        "{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-owned-request-{sequence}\r\nFrom: <sip:peer@{peer}>;tag=foreign\r\nTo: <sip:fixture@{target}>{}\r\nCall-ID: {CALL}\r\nCSeq: {sequence} {method}\r\nContent-Type: application/octet-stream\r\nContent-Length: 19\r\n\r\nprivate-fixture-raw",
        if tagged { ";tag=foreign" } else { "" }
    )
}
async fn response(peer: &UdpSocket, timeout: Duration) -> Option<u16> {
    let mut bytes = [0u8; 4096];
    match tokio::time::timeout(timeout, peer.recv_from(&mut bytes)).await {
        Ok(Ok((n, _))) => Some(
            Response::try_from(std::str::from_utf8(&bytes[..n]).unwrap())
                .unwrap()
                .status_code
                .code(),
        ),
        Err(_) => None,
        Ok(Err(error)) => panic!("Owned response unavailable: {error}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_request_policy_matches_original_and_drains_before_releasing_binding() {
    let (owner, factory, routes, params) = actual_owner(47000, 47998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner.clone(),
        remaining.clone(),
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let dispatcher = NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap();
    let mut cases = Vec::new();
    for _ in 0..7 {
        cases.extend([
            ("OPTIONS", false, Some(200)),
            ("MESSAGE", false, Some(405)),
            ("NOTIFY", false, Some(405)),
        ]);
    }
    cases.extend([
        ("INFO", false, None),
        ("OPTIONS", true, None),
        ("MESSAGE", true, None),
        ("NOTIFY", true, None),
    ]);
    let inputs = IvyJson::Array(
        cases
            .iter()
            .map(|(method, tagged, code)| {
                IvyJson::object([
                    ("method", (*method).into()),
                    ("tagged", (*tagged).into()),
                    (
                        "response",
                        code.map_or(IvyJson::Null, |v| IvyJson::Number(v as f64)),
                    ),
                ])
            })
            .collect(),
    );
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let fixture = format!(
        r#"{{"callId":"{CALL}","requests":{}}}"#,
        stringify(&inputs, 65536).unwrap()
    );
    let steps = vec![
        ("configure".into(), params),
        ("call.prepare".into(), selected),
        ("fixture.requests".into(), fixture),
    ];
    let original = super::dial::original_raw(&steps);
    let mut rows = Vec::new();
    for (i, (method, params)) in steps.iter().enumerate().take(2) {
        let result = dispatcher
            .execute(command(method, params, i as u64 + 1, i as u64 + 1))
            .await;
        rows.push(IvyJson::object([(
            "json",
            result.outcome().result_json().unwrap().into(),
        )]));
    }
    let target = owner.observation().endpoint.unwrap();
    let call = owner.find(CALL).unwrap();
    let media = call.media().unwrap();
    let original_rtp = media.observation().receive.transport.local_rtp;
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut requests = Vec::new();
    for (i, (method, tagged, expected)) in cases.iter().enumerate() {
        peer.send_to(
            request(
                method,
                peer.local_addr().unwrap(),
                target,
                100 + i as u32,
                *tagged,
            )
            .as_bytes(),
            target,
        )
        .await
        .unwrap();
        let observed = response(
            &peer,
            Duration::from_millis(if expected.is_some() { 4000 } else { 150 }),
        )
        .await;
        assert_eq!(observed, *expected, "{method}, tagged={tagged}");
        requests.push(IvyJson::object([
            ("method", (*method).into()),
            ("tagged", (*tagged).into()),
            (
                "status",
                observed.map_or(IvyJson::Null, |v| IvyJson::Number(v as f64)),
            ),
        ]));
    }
    let diagnostics = call.signalling().unwrap();
    assert_eq!(diagnostics.received, 25);
    assert_eq!(diagnostics.sent, 21);
    assert_eq!(diagnostics.events.len(), 16);
    assert!(
        !ivy_phone_bridge::native_sip_status::signalling_json(&diagnostics)
            .unwrap()
            .contains("private-fixture-raw")
    );
    owner.release(&call).unwrap().wait().await.unwrap();
    handler.shutdown().await.unwrap();
    assert!(media.observation().closed);
    assert_eq!(
        media.observation().receive.transport.local_rtp,
        original_rtp
    );
    drop(
        UdpSocket::bind(target)
            .await
            .expect("Original SIP binding must be free after confirmed shutdown"),
    );
    rows.push(IvyJson::object([(
        "json",
        stringify(
            &IvyJson::object([
                ("requests", IvyJson::Array(requests)),
                ("closed", true.into()),
            ]),
            65536,
        )
        .unwrap()
        .into(),
    )]));
    trace(
        "native-request-policy",
        IvyJson::object([
            ("rust", IvyJson::Array(rows.clone())),
            ("original", original.clone().unwrap_or(IvyJson::Null)),
            ("unansweredObservationWindowMs", IvyJson::Number(150.0)),
            ("originalBindingReleased", true.into()),
        ]),
    );
    super::dial::compare(&rows, &original, &steps);
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    assert!(remaining.methods.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_requests_wait_for_original_configuration_and_close_without_a_call() {
    let (owner, factory, routes, params) = actual_owner(46000, 46998);
    let gate = Arc::new(Gate::default());
    let hold = gate.clone();
    let configure = owner
        .configure(&command("configure", &params, 1, 1), move |_| async move {
            hold.block().await;
            Ok(())
        })
        .unwrap();
    until(|| owner.observation().endpoint.is_some()).await;
    let target = owner.observation().endpoint.unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(
        request("MESSAGE", peer.local_addr().unwrap(), target, 200, false).as_bytes(),
        target,
    )
    .await
    .unwrap();
    assert!(response(&peer, Duration::from_millis(75)).await.is_none());
    gate.release();
    configure.wait().await.unwrap();
    assert_eq!(response(&peer, Duration::from_secs(4)).await, Some(405));
    let closing = owner.close();
    let observer = closing.clone();
    drop(observer);
    closing.wait().await.unwrap();
    drop(UdpSocket::bind(target).await.unwrap());
    assert_eq!(factory.starts.load(Ordering::SeqCst), 0);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    trace(
        "native-queued-request-drain",
        IvyJson::object([
            ("response", IvyJson::Number(405.0)),
            ("originalBindingReleased", true.into()),
        ]),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unhandled_invite_retains_original_uncertain_cleanup_fence() {
    let (owner, factory, routes, params) = actual_owner(45000, 45998);
    owner
        .configure(&command("configure", &params, 1, 1), |_| async { Ok(()) })
        .unwrap()
        .wait()
        .await
        .unwrap();
    let target = owner.observation().endpoint.unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(
        request("INVITE", peer.local_addr().unwrap(), target, 300, true).as_bytes(),
        target,
    )
    .await
    .unwrap();
    peer.send_to(
        request("OPTIONS", peer.local_addr().unwrap(), target, 301, false).as_bytes(),
        target,
    )
    .await
    .unwrap();
    assert_eq!(response(&peer, Duration::from_secs(4)).await, Some(200));
    assert_eq!(
        owner.close().wait().await,
        Err(SipHostError::CleanupUnconfirmed)
    );
    assert_eq!(
        owner.close().wait().await,
        Err(SipHostError::CleanupUnconfirmed)
    );
    assert!(UdpSocket::bind(target).await.is_err());
    assert_eq!(factory.starts.load(Ordering::SeqCst), 0);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    trace(
        "native-unhandled-invite-fence",
        IvyJson::object([
            ("cleanupConfirmed", false.into()),
            ("originalBindingRetained", true.into()),
        ]),
    );
}
