use super::*;
use ivy_phone_bridge::{
    media::{
        MediaError, RtcpRuntime, RtcpWallTime, SdpDirection, SipMediaError, SipMediaIdentity,
        SipMediaSession, SipMediaSettings,
    },
    sip::{CallOutcome, DelayedInviteMedia, SipHost, SipHostError},
};
use std::net::{IpAddr, SocketAddr};

struct Clock;
impl RtcpRuntime for Clock {
    fn now(&self) -> Result<RtcpWallTime, MediaError> {
        let ticks = RtcpWallTime::UNIX_TICKS
            + i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
                    / 100,
            )
            .unwrap();
        RtcpWallTime::new(ticks, ticks)
    }
    fn interval_ms(&self) -> Result<u16, MediaError> {
        Ok(6000)
    }
}
async fn media(port: CallAudioPort) -> SipMediaSession {
    SipMediaSession::bind(
        SipMediaSettings {
            call_id: CALL.into(),
            interface: "127.0.0.1:0".parse().unwrap(),
            connection_address: IpAddr::from([127, 0, 0, 1]),
            first_port: 55000,
            last_port: 55998,
            reorder_ms: 20,
            preferences: vec![Codec::Pcmu, Codec::Pcma],
            identity: SipMediaIdentity {
                session_id: 1,
                source: 17,
                sequence: 100,
                timestamp: 200,
                cname: "fixture".into(),
            },
        },
        port,
        None,
        Arc::new(Clock),
        Arc::new(Events::default()),
    )
    .await
    .unwrap()
}
fn offer(port: impl std::fmt::Display, extra: &str) -> String {
    format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {port} RTP/AVP 0\r\n{extra}"
    )
}
fn set(extra: &str, port: &str, codec: &str, address: &str) -> IvyJson {
    IvyJson::object([
        ("op", "set".into()),
        (
            "sdp",
            offer(port, extra)
                .replace("c=IN IP4 127.0.0.1", &format!("c=IN IP4 {address}"))
                .replace("RTP/AVP 0", &format!("RTP/AVP {codec}"))
                .into(),
        ),
    ])
}
fn op(name: &str) -> IvyJson {
    IvyJson::object([("op", name.into())])
}
fn cases() -> Vec<IvyJson> {
    let mut cases = vec![vec![
        set("", "$PORT", "0", "127.0.0.1"),
        set("a=inactive\r\n", "$PORT", "0", "127.0.0.1"),
        op("answer"),
        set("", "$PORT", "0", "127.0.0.1"),
    ]];
    for direction in [
        "a=sendonly\r\n",
        "a=recvonly\r\n",
        "a=rtcp:1234 IN IP4 127.0.0.2\r\na=rtcp-mux\r\n",
    ] {
        cases.push(vec![
            set(direction, "$PORT", "0", "127.0.0.1"),
            op("answer"),
        ]);
    }
    for port in ["0", "9", "65535", "-1", "65536"] {
        cases.push(vec![
            set("", port, "0", "127.0.0.1"),
            op("answer"),
            set("", "$PORT", "0", "127.0.0.1"),
        ]);
    }
    for ip in [
        "0.0.0.0",
        "localhost",
        "::1",
        "127.1",
        "0177.0.0.1",
        "0x7f000001",
        "2130706433",
        "[::1]",
        "fe80::1%12",
        "fe80::1%nonexistent",
    ] {
        cases.push(vec![set("", "$PORT", "0", ip), op("answer")]);
    }
    cases.push(vec![
        set("", "$PORT", "0", "127.0.0.1"),
        set("", "0", "111", "127.0.0.1"),
        op("answer"),
        set("", "$PORT", "0", "127.0.0.1"),
    ]);
    cases.push(vec![
        set(
            "a=ssrc:111 cname:old\r\na=rtpmap:101 telephone-event/8000\r\n",
            "$PORT",
            "0 101",
            "127.0.0.1",
        ),
        set(
            "a=ssrc:222 cname:new\r\na=rtpmap:110 telephone-event/8000\r\n",
            "0",
            "111 110",
            "127.0.0.1",
        ),
        op("answer"),
    ]);
    cases.push(vec![
        IvyJson::object([("op", "status".into()), ("value", "RecvOnly".into())]),
        set("", "$PORT", "0", "127.0.0.1"),
        op("offer"),
        op("answer"),
    ]);
    cases.push(vec![
        set("a=inactive\r\n", "$PORT", "0", "127.0.0.1"),
        op("offer"),
        op("answer"),
    ]);
    cases.push(vec![
        IvyJson::object([
            ("op", "set".into()),
            (
                "sdp",
                offer("$PORT", "")
                    .replace("c=IN IP4 127.0.0.1\r\n", "")
                    .into(),
            ),
        ]),
        op("answer"),
    ]);
    cases
        .into_iter()
        .map(|steps| IvyJson::object([("steps", IvyJson::Array(steps))]))
        .collect()
}
async fn pair() -> (UdpSocket, UdpSocket) {
    for _ in 0..32 {
        let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let p = a.local_addr().unwrap().port();
        if p < 65535 {
            if let Ok(b) = UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], p + 1))).await {
                return (a, b);
            }
        }
    }
    panic!("owned pair unavailable")
}
fn endpoint(p: Option<SocketAddr>, port: u16) -> IvyJson {
    p.map(|p| {
        let ip = match p {
            SocketAddr::V6(v) if v.scope_id() != 0 => format!("{}%{}", v.ip(), v.scope_id()),
            _ => p.ip().to_string(),
        };
        format!(
            "{}:{}",
            ip,
            if p.port() == port {
                "peer".into()
            } else if p.port() == port + 1 {
                "control".into()
            } else {
                p.port().to_string()
            }
        )
        .into()
    })
    .unwrap_or(IvyJson::Null)
}
fn direction(d: SdpDirection) -> IvyJson {
    match d {
        SdpDirection::SendRecv => "SendRecv",
        SdpDirection::SendOnly => "SendOnly",
        SdpDirection::RecvOnly => "RecvOnly",
        SdpDirection::Inactive => "Inactive",
    }
    .into()
}
fn shape(s: &SipMediaSession, port: u16) -> IvyJson {
    let o = s.observation();
    IvyJson::object([
        ("rtp", endpoint(o.remote_rtp, port)),
        ("rtcp", endpoint(o.remote_rtcp, port)),
        ("local", direction(o.local_direction)),
        (
            "remote",
            o.remote_direction.map(direction).unwrap_or(IvyJson::Null),
        ),
        (
            "controlClosed",
            o.receive.rtcp_session.unwrap().closed.into(),
        ),
        (
            "sendCodec",
            o.send_format
                .map(|f| f.name().into())
                .unwrap_or(IvyJson::Null),
        ),
        ("failed", o.failed.into()),
        (
            "declared",
            IvyJson::Array(
                o.receive
                    .rtcp
                    .declared_sources
                    .iter()
                    .map(|s| IvyJson::Number(f64::from(*s)))
                    .collect(),
            ),
        ),
        (
            "eventPayload",
            IvyJson::Number(f64::from(o.receive.selected_event_payload)),
        ),
    ])
}
fn description(s: Option<SdpDocument>, port: u16) -> IvyJson {
    s.map(|s| {
        IvyJson::object([
            ("version", IvyJson::Number(s.origin().version() as f64)),
            (
                "port",
                if s.media()[0].port() == i32::from(port) {
                    "local".into()
                } else {
                    s.media()[0].port().to_string().into()
                },
            ),
            ("direction", direction(s.media()[0].direction().unwrap())),
        ])
    })
    .unwrap_or(IvyJson::Null)
}
fn original_session(inputs: &[IvyJson]) -> Option<IvyJson> {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original session oracle unavailable; differential not executed");
        return None;
    }
    let mut child = Command::new("pwsh")
        .args([
            "-NoProfile",
            "-File",
            "tests/fixtures/phone-session-oracle.ps1",
        ])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            stringify(&IvyJson::Array(inputs.to_vec()), 16_000_000)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        parse(
            std::str::from_utf8(&out.stdout)
                .unwrap()
                .trim_start_matches('\u{feff}'),
            32_000_000,
        )
        .unwrap(),
    )
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn descriptions_retain_original_endpoint_direction_rejection_and_version_effects() {
    let inputs = cases();
    let Some(old) = original_session(&inputs) else {
        return;
    };
    trace(
        "session-original-description-inputs",
        IvyJson::object([
            ("inputs", IvyJson::Array(inputs.clone())),
            ("original", old.clone()),
        ]),
    );
    let mut rows = Vec::new();
    for (input, expected) in inputs.iter().zip(old.as_array().unwrap()) {
        assert_eq!(
            expected.get("ok").unwrap().as_bool(),
            Some(true),
            "{expected:?}"
        );
        let (a, _control) = pair().await;
        let remote = a.local_addr().unwrap().port();
        let port = CallAudioPort::new(CALL.into(), Arc::new(Factory::routes(vec![]))).unwrap();
        let m = media(port).await;
        m.create_offer().unwrap();
        let local = m.observation().receive.transport.local_rtp.port();
        for (step, expected) in input
            .get("steps")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .zip(expected.get("trace").unwrap().as_array().unwrap())
        {
            let name = step.get("op").unwrap().as_str().unwrap();
            let mut result = IvyJson::Null;
            let mut body = IvyJson::Null;
            match name.as_str() {
                "set" => {
                    let text = step
                        .get("sdp")
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .replace("$PORT", &remote.to_string());
                    let doc = SdpDocument::parse(&text).unwrap();
                    result = match m.set_remote_description(SdpType::Offer, &doc) {
                        Ok(()) => "OK".into(),
                        Err(SipMediaError::Negotiation(e)) => e.as_str().into(),
                        Err(e) => panic!("{e:?}"),
                    };
                }
                "offer" => body = description(m.create_offer(), local),
                "answer" => body = description(m.create_answer(), local),
                "status" => m.set_stream_status(SdpDirection::RecvOnly).unwrap(),
                _ => unreachable!(),
            }
            let actual = shape(&m, remote);
            let state = expected.get("state").unwrap();
            let want = IvyJson::object(
                [
                    "rtp",
                    "rtcp",
                    "local",
                    "remote",
                    "controlClosed",
                    "sendCodec",
                    "failed",
                    "declared",
                    "eventPayload",
                ]
                .map(|k| (k, state.get(k).unwrap().clone())),
            );
            assert_eq!(
                result,
                *expected.get("result").unwrap(),
                "op={name}, input={input:?}"
            );
            assert_eq!(
                body,
                *expected.get("description").unwrap(),
                "input={input:?}"
            );
            assert_eq!(actual, want, "input={input:?}");
            rows.push(actual);
        }
        m.close().await.unwrap();
    }
    trace(
        "session-original-descriptions",
        IvyJson::object([
            ("inputs", IvyJson::Array(inputs)),
            ("original", old),
            ("rust", IvyJson::Array(rows)),
        ]),
    );
}
async fn packet(socket: &UdpSocket) -> Vec<u8> {
    let mut b = vec![0; 2048];
    let (n, _) = timeout(Duration::from_secs(2), socket.recv_from(&mut b))
        .await
        .unwrap()
        .unwrap();
    b.truncate(n);
    b
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_waiting_screening_and_bridge_keep_original_identity_and_pair() {
    let route = Arc::new(Route::new());
    let port =
        CallAudioPort::new(CALL.into(), Arc::new(Factory::routes(vec![route.clone()]))).unwrap();
    let m = media(port.clone()).await;
    m.prepare_audio(settings(), None, CancellationToken::new())
        .unwrap()
        .wait()
        .await
        .unwrap();
    m.begin_waiting().unwrap();
    let (rtp, control) = pair().await;
    let answer = m
        .negotiate_offer(&offer(rtp.local_addr().unwrap().port(), ""))
        .unwrap();
    assert_eq!(
        SdpDocument::parse(&answer).unwrap().media()[0].port(),
        i32::from(m.observation().receive.transport.local_rtp.port())
    );
    assert!(m.start().await);
    let first = packet(&rtp).await;
    let header = RtpPacket::parse(&first).unwrap().header;
    assert_eq!(
        (header.source, header.sequence, header.timestamp),
        (17, 100, 200)
    );
    assert!(!port.state().open);
    assert_eq!(route.state.lock().unwrap().reads, 0);
    let original = m.observation().receive.transport.local_rtp;
    let mut inbound = vec![0; 172];
    ivy_phone_bridge::media::RtpHeader {
        marker: false,
        payload_type: 0,
        sequence: 10,
        timestamp: 1600,
        source: 222,
    }
    .write(&[255; 160], &mut inbound)
    .unwrap();
    rtp.send_to(&inbound, original).await.unwrap();
    wait(|| m.observation().receive.received_packets == 1).await;
    assert_eq!(route.state.lock().unwrap().writes, 0);
    let bytes = wav(48000, 1, 4800);
    let file = TempWav::new(&bytes);
    m.prepare_screening(&file.0, &ivy_contracts::hash::digest(&bytes), 1, 1, true)
        .unwrap();
    assert!(
        m.prepare_screening(&file.0, &ivy_contracts::hash::digest(&bytes), 1, 1, true)
            .is_err()
    );
    let _ = packet(&rtp).await;
    assert_eq!(route.state.lock().unwrap().reads, 0);
    m.end_waiting().unwrap();
    port.grant_authority().unwrap();
    m.bridge_screening();
    wait(|| route.state.lock().unwrap().reads > 0).await;
    ivy_phone_bridge::media::RtpHeader {
        marker: false,
        payload_type: 0,
        sequence: 11,
        timestamp: 1760,
        source: 222,
    }
    .write(&[255; 160], &mut inbound)
    .unwrap();
    rtp.send_to(&inbound, original).await.unwrap();
    wait(|| route.state.lock().unwrap().writes == 1).await;
    let changed = offer(rtp.local_addr().unwrap().port(), "").replace("RTP/AVP 0", "RTP/AVP 8");
    m.set_remote_description(SdpType::Offer, &SdpDocument::parse(&changed).unwrap())
        .unwrap();
    loop {
        let bytes = packet(&rtp).await;
        let current = RtpPacket::parse(&bytes).unwrap();
        assert_eq!(current.header.source, 17);
        if current.header.payload_type == 8 {
            assert!(current.header.sequence > header.sequence);
            break;
        }
    }
    assert_eq!(m.observation().receive.transport.local_rtp, original);
    m.close().await.unwrap();
    assert!(m.observation().cleanup_confirmed);
    assert_eq!(route.state.lock().unwrap().disposals, 1);
    let bye = packet(&control).await;
    assert!(
        ivy_phone_bridge::media::RtcpCompound::parse(&bye)
            .unwrap()
            .bye_packet
            .is_some()
    );
    trace(
        "session-progress-pair",
        IvyJson::object([
            ("waitingWithoutCapture", true.into()),
            ("screeningWithoutCapture", true.into()),
            ("bridgedCapture", true.into()),
            ("sameSourceAndPair", true.into()),
            ("oneCleanup", true.into()),
            ("actualReceiveBlockedWhileWaiting", true.into()),
            ("actualReceiveAfterBridge", true.into()),
        ]),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incompatible_inactive_offer_gates_wire_clock_but_keeps_application_and_rtcp_identity() {
    let port = CallAudioPort::new(CALL.into(), Arc::new(Factory::routes(vec![]))).unwrap();
    let m = media(port).await;
    let (rtp, control) = pair().await;
    let remote = rtp.local_addr().unwrap().port();
    m.negotiate_offer(&offer(remote, "")).unwrap();
    assert!(m.start().await);
    let mut last = packet(&rtp).await;
    let bad = offer(0, "").replace("RTP/AVP 0", "RTP/AVP 111");
    assert_eq!(
        m.set_remote_description(SdpType::Offer, &SdpDocument::parse(&bad).unwrap()),
        Err(SipMediaError::Negotiation(
            ivy_phone_bridge::media::NegotiationError::AudioIncompatible
        ))
    );
    let mut buffer = [0; 2048];
    while let Ok(n) = rtp.try_recv(&mut buffer) {
        last = buffer[..n].to_vec();
    }
    let previous = RtpPacket::parse(&last).unwrap().header;
    let before = m.observation();
    sleep(Duration::from_millis(65)).await;
    let after = m.observation();
    assert!(!after.failed);
    assert!(after.sent_frames > before.sent_frames);
    assert_eq!(
        after.receive.transport.counters.rtp_sent,
        before.receive.transport.counters.rtp_sent
    );
    assert!(after.receive.rtcp_session.unwrap().closed);
    let bye = packet(&control).await;
    let parsed = ivy_phone_bridge::media::RtcpCompound::parse(&bye).unwrap();
    assert_eq!(parsed.bye_packet.unwrap().len(), 8);
    m.negotiate_offer(&offer(remote, "")).unwrap();
    let next = packet(&rtp).await;
    let next = RtpPacket::parse(&next).unwrap().header;
    assert_eq!(next.sequence, previous.sequence.wrapping_add(1));
    assert_eq!(next.timestamp, previous.timestamp.wrapping_add(160));
    let now = m.observation();
    assert!(now.receive.rtcp_session.as_ref().unwrap().closed);
    assert!(
        now.receive.rtcp_session.as_ref().unwrap().sent.packets
            > before.receive.rtcp_session.as_ref().unwrap().sent.packets
    );
    assert_eq!(now.receive.rtcp.reports_sent, 1);
    m.close().await.unwrap();
    assert!(
        timeout(Duration::from_millis(30), control.recv_from(&mut buffer))
            .await
            .is_err()
    );
    trace(
        "session-inactive-resume",
        IvyJson::object([
            ("rejectedOfferSideEffects", true.into()),
            ("inactiveWirePackets", n(0)),
            ("applicationFramesContinue", true.into()),
            ("wireClockRetained", true.into()),
            ("closedRtcpStillCountsResumedRtp", true.into()),
            ("oneEmptyReasonBye", true.into()),
        ]),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_sdp_keeps_codec_but_updates_original_rtcp_source_and_event_selection() {
    let port = CallAudioPort::new(CALL.into(), Arc::new(Factory::routes(vec![]))).unwrap();
    let m = media(port).await;
    let (rtp, control) = pair().await;
    let remote = rtp.local_addr().unwrap().port();
    m.negotiate_offer(&offer(remote, "a=ssrc:111 cname:old\r\n"))
        .unwrap();
    let bad = offer(
        0,
        "a=ssrc:222 cname:new\r\na=rtpmap:110 telephone-event/8000\r\n",
    )
    .replace("RTP/AVP 0", "RTP/AVP 111 110");
    assert!(
        m.set_remote_description(SdpType::Offer, &SdpDocument::parse(&bad).unwrap())
            .is_err()
    );
    let observed = m.observation();
    assert_eq!(observed.send_format.unwrap().name(), "PCMU");
    assert_eq!(observed.receive.rtcp.declared_sources, vec![222]);
    assert_eq!(observed.receive.selected_event_payload, 110);
    assert_eq!(observed.receive.rtcp.learned_source, Some(0));
    let bytes = ivy_phone_bridge::media::RtcpReportBuilder::new(222, "peer")
        .unwrap()
        .receiver(&[])
        .unwrap();
    control
        .send_to(&bytes, observed.receive.transport.local_rtcp)
        .await
        .unwrap();
    wait(|| m.observation().receive.rtcp.reports_received == 1).await;
    let observed = m.observation();
    assert!(observed.receive.rtcp_session.as_ref().unwrap().closed);
    assert!(
        observed
            .receive
            .rtcp_session
            .as_ref()
            .unwrap()
            .started_local_ticks
            .is_some()
    );
    assert!(!observed.failed);
    m.close().await.unwrap();
    trace(
        "session-rejected-sources",
        IvyJson::object([
            ("codecRetained", true.into()),
            ("newSourceAdmitted", true.into()),
            ("newEventSelected", true.into()),
            ("closedControlStillObservesOriginalReport", true.into()),
        ]),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_slot_waits_for_late_preparation_and_retains_uncertain_media_cleanup() {
    for bad in [false, true] {
        let mut route = Route::new();
        route.state.get_mut().unwrap().bad_close = bad;
        let route = Arc::new(route);
        let gate = Arc::new(Gate::default());
        let mut factory = Factory::routes(vec![route.clone()]);
        factory.gate = Some(gate.clone());
        let port = CallAudioPort::new(CALL.into(), Arc::new(factory)).unwrap();
        let m = Arc::new(media(port).await);
        let host = SipHost::new();
        host.configure(super::super::sip_host::configuration(1, None))
            .await
            .unwrap();
        let call = host.prepare(CALL.into(), || Ok(m.clone())).unwrap();
        let prepare = m
            .prepare_audio(settings(), None, CancellationToken::new())
            .unwrap();
        gate.wait().await;
        let release = tokio::spawn({
            let host = host.clone();
            let call = call.clone();
            async move { host.release(&call).await }
        });
        wait(|| m.is_closed()).await;
        release.abort();
        let _ = release.await;
        assert_eq!(host.observation().calls.len(), 1);
        assert!(matches!(
            host.prepare("22222222-2222-4222-8222-222222222222".into(), || panic!(
                "original slot still owned"
            )),
            Err(SipHostError::OperationConflict)
        ));
        gate.release();
        let _ = prepare.wait().await;
        if bad {
            wait(|| m.failed()).await;
            assert_eq!(
                host.release(&call).await,
                Err(SipHostError::CleanupUnconfirmed)
            );
            assert_eq!(host.observation().calls.len(), 1);
            assert!(!m.observation().cleanup_confirmed);
            let _ = host.close().await;
        } else {
            wait(|| host.observation().calls.is_empty()).await;
            host.close().await.unwrap();
            assert!(m.observation().cleanup_confirmed);
        }
        assert_eq!(route.state.lock().unwrap().disposals, 1);
        assert_eq!(route.state.lock().unwrap().resumes, 0);
    }
    trace(
        "session-host-cleanup",
        IvyJson::object([
            ("latePrepareRetained", true.into()),
            ("cancelledReleaseObserverRetainedWork", true.into()),
            ("badCleanupKeepsSlot", true.into()),
            ("noReplacementRoute", true.into()),
        ]),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concrete_hosted_media_answers_delayed_invite_and_sends_on_its_original_pair() {
    use super::super::sip_host::{configuration, invite, received, response, send};
    use rsipstack::sip::{Method, prelude::*};
    let host = SipHost::new();
    let local = host.configure(configuration(1, None)).await.unwrap();
    let retained = Arc::new(std::sync::OnceLock::new());
    let original = retained.clone();
    let creating_host = host.clone();
    let call = host
        .prepare_async(CALL.into(), move || async move {
            assert_eq!(creating_host.observation().calls.len(), 1);
            let port = CallAudioPort::new(CALL.into(), Arc::new(Factory::routes(vec![]))).unwrap();
            let m = Arc::new(media(port).await);
            assert!(original.set(m.clone()).is_ok());
            Ok(m as Arc<dyn ivy_phone_bridge::sip::HostedSipMedia>)
        })
        .unwrap();
    call.prepared().await.unwrap();
    let m = retained.get().unwrap();
    let local_rtp = m.observation().receive.transport.local_rtp;
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let remote = peer.local_addr().unwrap();
    let (rtp, _control) = pair().await;
    let dial = tokio::spawn({
        let h = host.clone();
        let c = call.clone();
        async move {
            h.dial_delayed(&c, invite(local), remote.into(), None, 10)
                .await
        }
    });
    let (original, source) = received(&peer).await;
    assert_eq!(source, local);
    send(
        &peer,
        source,
        &response(
            &original,
            200,
            &format!("Contact: <sip:callee@{remote}>\r\nContent-Type: application/sdp\r\n"),
            &offer(rtp.local_addr().unwrap().port(), ""),
        ),
    )
    .await;
    let (ack, source) = received(&peer).await;
    assert_eq!(source, local);
    assert_eq!(ack.method, Method::Ack);
    assert_eq!(
        ack.call_id_header().unwrap(),
        original.call_id_header().unwrap()
    );
    assert_eq!(
        SdpDocument::parse(std::str::from_utf8(&ack.body).unwrap())
            .unwrap()
            .media()[0]
            .port(),
        i32::from(local_rtp.port())
    );
    assert_eq!(dial.await.unwrap().unwrap(), CallOutcome::Connected);
    assert_eq!(
        RtpPacket::parse(&packet(&rtp).await).unwrap().header.source,
        17
    );
    assert_eq!(m.observation().receive.transport.local_rtp, local_rtp);
    host.hangup(&call).unwrap();
    let (bye, source) = received(&peer).await;
    assert_eq!(bye.method, Method::Bye);
    send(&peer, source, &response(&bye, 200, "", "")).await;
    host.release(&call).await.unwrap();
    host.close().await.unwrap();
    assert!(m.observation().cleanup_confirmed);
    trace(
        "session-real-host",
        IvyJson::object([
            ("actualInviteAckBye", true.into()),
            ("realSdpAnswer", true.into()),
            ("actualRtp", true.into()),
            ("slotReservedBeforeAsyncMediaConstruction", true.into()),
            ("sameSignallingAndMediaSockets", true.into()),
            ("cleanupConfirmed", true.into()),
        ]),
    );
}
