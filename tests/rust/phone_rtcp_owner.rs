use crate::{
    rtcp::{hex, invalid_compounds, original},
    sip_receiver::{Events, send, setup, wait},
};
use ivy_contracts::{IvyJson, json::stringify};
use ivy_phone_bridge::media::{
    AudioNegotiator, Codec, RtcpReceiveObservation, RtcpReportBlock, RtcpReportBuilder,
    RtcpSenderInfo, RtpPeer, SdpDocument, SdpType, SipRtpReceiver,
};
use std::{
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::net::UdpSocket;

fn n(value: impl Into<f64>) -> IvyJson {
    IvyJson::Number(value.into())
}
fn bytes(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}
fn block(source: u32) -> RtcpReportBlock {
    RtcpReportBlock::new(source, 17, -1, 1000, 77, 0x81234567, 100)
}
fn rr(source: u32, samples: &[u32]) -> Vec<u8> {
    RtcpReportBuilder::receiver_packet(
        source,
        &samples.iter().copied().map(block).collect::<Vec<_>>(),
    )
    .unwrap()
}
fn sr(source: u32, samples: &[u32]) -> Vec<u8> {
    RtcpReportBuilder::sender_packet(
        source,
        RtcpSenderInfo {
            ntp_timestamp: 0x8123456789abcdef,
            rtp_timestamp: 777,
            packet_count: 123,
            octet_count: 987,
        },
        &samples.iter().copied().map(block).collect::<Vec<_>>(),
    )
    .unwrap()
}
fn length(mut data: Vec<u8>) -> Vec<u8> {
    let words = (data.len() / 4 - 1) as u16;
    data[2..4].copy_from_slice(&words.to_be_bytes());
    data
}
fn extended(mut data: Vec<u8>) -> Vec<u8> {
    data.extend_from_slice(&[0; 4]);
    length(data)
}
fn projection(value: &RtcpReceiveObservation) -> IvyJson {
    IvyJson::Array(
        value
            .peer_reports
            .iter()
            .map(|b| {
                IvyJson::object([
                    ("ssrc", n(b.source)),
                    ("fractionLost256", n(b.fraction_lost)),
                    ("packetsLost", n(b.packets_lost())),
                    ("extendedHighestSequence", n(b.highest_sequence)),
                    ("jitterTimestampUnits", n(b.jitter)),
                ])
            })
            .collect(),
    )
}
fn snapshot(value: &RtcpReceiveObservation, events: usize) -> IvyJson {
    IvyJson::object([
        ("reportsReceived", n(value.reports_received as u32)),
        ("events", n(events as u32)),
        (
            "lastReportAtNanos",
            value
                .last_report_at
                .map(|t| {
                    t.duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                        .to_string()
                        .into()
                })
                .unwrap_or(IvyJson::Null),
        ),
        ("peerReceptionReports", projection(value)),
        (
            "learnedSource",
            value.learned_source.map(n).unwrap_or(IvyJson::Null),
        ),
        (
            "declaredSources",
            IvyJson::Array(value.declared_sources.iter().copied().map(n).collect()),
        ),
    ])
}
async fn control(receiver: &SipRtpReceiver, socket: &UdpSocket, events: &Events, data: &[u8]) {
    let before = events.controls();
    socket
        .send_to(data, receiver.observation().transport.local_rtcp)
        .await
        .unwrap();
    wait(|| events.controls() == before + 1).await;
}
fn configure(receiver: &SipRtpReceiver, peer: SocketAddr, control: SocketAddr, sources: &[u32]) {
    let declarations = sources
        .iter()
        .map(|s| format!("a=ssrc:{s} cname:fixture\r\n"))
        .collect::<String>();
    let sdp=SdpDocument::parse(&format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0 101 110\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=rtpmap:110 telephone-event/48000\r\na=sendrecv\r\n{declarations}",peer.port())).unwrap();
    let mut negotiation = AudioNegotiator::new(&[Codec::Pcmu]).unwrap();
    negotiation
        .set_remote_description(SdpType::Offer, &sdp)
        .unwrap();
    receiver
        .configure(
            negotiation.state().unwrap(),
            RtpPeer::new(peer, control).unwrap(),
        )
        .unwrap();
}
struct Case {
    name: &'static str,
    bytes: Vec<u8>,
    admitted: bool,
    reset: bool,
    old_admitted: bool,
    old_reset: bool,
    exception: bool,
    first: Option<u32>,
}
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn bound_application_reports_match_original_and_all_approved_compound_progress_cases() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original report owner unavailable; differential not executed");
        return;
    }
    let a = rr(9, &[900]);
    let b = rr(9, &[1000]);
    let c = sr(9, &[1100]);
    let d = sr(9, &[1200]);
    let builder = RtcpReportBuilder::new(9, "fixture").unwrap();
    let sdes = builder.sdes_packet();
    let bye = builder.bye_packet("");
    let unknown = bytes("80cf000100000009");
    let fir = bytes("84ce000400000009000000000000000901000000");
    let tmmbr = bytes("83cd000400000009000000000000000900000000");
    let twcc = bytes("8fcd000400000063000000090000000000000000");
    let mut cases = Vec::new();
    let mut add = |name, data, admitted, reset, old_admitted, old_reset, exception, first| {
        cases.push(Case {
            name,
            bytes: data,
            admitted,
            reset,
            old_admitted,
            old_reset,
            exception,
            first,
        })
    };
    for (name, data, first) in [
        ("rr", a.clone(), Some(900)),
        ("sr", c.clone(), Some(1100)),
        ("rr-sdes", [a.clone(), sdes.clone()].concat(), Some(900)),
        ("last-rr", [a.clone(), b.clone()].concat(), Some(1000)),
        ("last-sr", [c.clone(), d.clone()].concat(), Some(1200)),
        ("sr-before-rr", [c.clone(), b.clone()].concat(), Some(1100)),
        ("sr-after-rr", [b.clone(), c.clone()].concat(), Some(1100)),
        (
            "empty-sr-precedence",
            [sr(9, &[]), b.clone()].concat(),
            None,
        ),
        (
            "cap12",
            rr(9, &(1400..1412).collect::<Vec<_>>()),
            Some(1400),
        ),
        ("rr-source-fallback", rr(99, &[17]), Some(17)),
        ("rr-empty-sdes", [rr(9, &[]), sdes.clone()].concat(), None),
        ("nack", bytes("81cd0003000000090000006300000000"), None),
        (
            "pli-sdes",
            [bytes("81ce00020000000900000063"), sdes.clone()].concat(),
            None,
        ),
        ("twcc-media-source", twcc.clone(), None),
        (
            "twcc-run",
            bytes("8fcd00050000006300000009000000010000000000010000"),
            None,
        ),
        (
            "twcc-one-bit",
            bytes("8fcd000500000063000000090000000200000000e0000200"),
            None,
        ),
        (
            "twcc-two-bit",
            bytes("8fcd0006000000630000000900000002000000009800010002000000"),
            None,
        ),
        (
            "twcc-reserved",
            bytes("8fcd000500000063000000090000000100000000b0000000"),
            None,
        ),
        (
            "twcc-unused-chunk",
            bytes("8fcd00050000006300000009000000010000000040000001"),
            None,
        ),
        (
            "feedback-remb",
            bytes("8fce0005000000090000000052454d420100000000000009"),
            None,
        ),
        (
            "feedback-afb",
            bytes("8fce0003000000090000000041424344"),
            None,
        ),
        (
            "feedback-resync",
            [bytes("85cd00020000000900000063"), sdes.clone()].concat(),
            None,
        ),
    ] {
        add(name, data, true, false, true, false, false, first);
    }
    for (name, data) in [
        ("sr-no-block-fallback", sr(99, &[17])),
        ("sr-miss-precedes-rr", [sr(99, &[17]), a.clone()].concat()),
        (
            "rr-miss-precedes-feedback",
            [rr(99, &[]), fir.clone()].concat(),
        ),
        (
            "feedback-miss-precedes-twcc",
            [bytes("81ce00020000006300000009"), twcc.clone()].concat(),
        ),
        (
            "twcc-sender-is-not-media",
            bytes("8fcd000400000009000000630000000000000000"),
        ),
        ("unknown-only", unknown.clone()),
        ("sdes-only", sdes.clone()),
        ("bye-only", bye.clone()),
        ("zero-rr", rr(9, &[])),
        ("twelve-byte-pli", bytes("81ce00020000000900000063")),
    ] {
        add(name, data, false, false, false, false, false, None);
    }
    add(
        "unknown-first",
        [unknown.clone(), a.clone()].concat(),
        true,
        false,
        false,
        false,
        true,
        Some(900),
    );
    add(
        "unknown-middle",
        [a.clone(), unknown.clone(), sdes.clone()].concat(),
        true,
        false,
        true,
        false,
        true,
        Some(900),
    );
    add(
        "unknown-last",
        [a.clone(), unknown.clone()].concat(),
        true,
        false,
        true,
        false,
        true,
        Some(900),
    );
    add(
        "known-unknown-known",
        [a.clone(), unknown.clone(), b.clone()].concat(),
        true,
        false,
        true,
        false,
        true,
        Some(1000),
    );
    add(
        "rr-profile-tail",
        [extended(a.clone()), b.clone()].concat(),
        true,
        false,
        true,
        false,
        true,
        Some(1000),
    );
    add(
        "sr-profile-tail",
        [extended(c), d].concat(),
        true,
        false,
        true,
        false,
        true,
        Some(1200),
    );
    add(
        "optional-sdes-tail",
        [bytes("81ca0003000000090201620101610000"), b.clone()].concat(),
        true,
        false,
        false,
        false,
        true,
        Some(1000),
    );
    add(
        "multi-bye-tail",
        [bytes("82cb0002000000090000000a"), b.clone()].concat(),
        false,
        true,
        false,
        false,
        true,
        None,
    );
    add(
        "fir-tail",
        [fir, b.clone()].concat(),
        true,
        false,
        true,
        false,
        true,
        Some(1000),
    );
    add(
        "tmmbr-tail",
        [tmmbr, b.clone()].concat(),
        true,
        false,
        true,
        false,
        true,
        Some(1000),
    );
    add(
        "matched-bye",
        [a.clone(), bye.clone()].concat(),
        false,
        true,
        false,
        true,
        false,
        None,
    );
    add(
        "unmatched-bye",
        [rr(99, &[900]), bye.clone()].concat(),
        false,
        false,
        false,
        false,
        false,
        None,
    );
    add(
        "bye-second-source-is-not-reset",
        [a.clone(), bytes("82cb00020000006300000009")].concat(),
        false,
        false,
        false,
        false,
        false,
        None,
    );
    let mut chunks = sdes[4..].to_vec();
    chunks.extend_from_slice(&sdes[4..]);
    add(
        "multiple-sdes-chunks-tail",
        [length([vec![0x82, 202, 0, 0], chunks].concat()), b.clone()].concat(),
        true,
        false,
        false,
        false,
        true,
        Some(1000),
    );
    let mut padded = b.clone();
    padded[0] |= 32;
    padded.extend_from_slice(&[0, 0, 0, 4]);
    add(
        "last-padded-report",
        [unknown.clone(), length(padded)].concat(),
        true,
        false,
        false,
        false,
        true,
        Some(1000),
    );
    let inputs: Vec<_> = cases
        .iter()
        .map(|case| {
            IvyJson::object([
                ("op", "receive".into()),
                (
                    "datagrams",
                    IvyJson::Array(vec![hex(&case.bytes).into(), hex(&a).into()]),
                ),
            ])
        })
        .collect();
    let old = original(&inputs);
    let mut actual = Vec::new();
    assert_eq!(old.len(), cases.len());
    for (index, case) in cases.iter().enumerate() {
        let (receiver, audio, events, peer, socket) = setup(42000, false, false).await;
        receiver.set_local_source(17).unwrap();
        send(
            &peer,
            receiver.observation().transport.local_rtp,
            1,
            0,
            9,
            &[0; 160],
        )
        .await;
        wait(|| receiver.observation().rtcp.learned_source == Some(9)).await;
        let old_case = &old[index];
        assert_eq!(
            old_case.get("ok").unwrap().as_bool(),
            Some(true),
            "{}: {old_case:?}",
            case.name
        );
        let old_snaps = old_case.get("snapshots").unwrap().as_array().unwrap();
        let mut new_snaps = Vec::new();
        for (step, data) in [&case.bytes, &a].into_iter().enumerate() {
            let before = receiver.observation().rtcp;
            let earliest = SystemTime::now();
            control(&receiver, &socket, &events, data).await;
            let current = receiver.observation().rtcp;
            let old_count =
                usize::from(case.old_admitted) + if step == 1 && !case.old_reset { 1 } else { 0 };
            assert_eq!(
                old_snaps[step].get("events").unwrap().as_f64(),
                Some(old_count as f64),
                "{} old step{step}",
                case.name
            );
            assert_eq!(
                old_snaps[step].get("learnedSource").unwrap().as_f64(),
                Some(if case.old_reset { 0.0 } else { 9.0 }),
                "{} old source",
                case.name
            );
            let count = usize::from(case.admitted) + if step == 1 && !case.reset { 1 } else { 0 };
            assert_eq!(
                current.reports_received, count as u64,
                "{} step{step}",
                case.name
            );
            assert_eq!(events.reports(), count, "{} callbacks", case.name);
            assert_eq!(
                current.learned_source,
                Some(if case.reset { 0 } else { 9 }),
                "{} source",
                case.name
            );
            if count as u64 > before.reports_received {
                let time = current.last_report_at.unwrap();
                assert!(time >= earliest && time <= SystemTime::now());
            } else {
                assert_eq!(current.last_report_at, before.last_report_at);
                assert_eq!(current.peer_reports, before.peer_reports);
            }
            if step == 0 && case.admitted {
                assert_eq!(
                    current.peer_reports.first().map(|b| b.source),
                    case.first,
                    "{} first",
                    case.name
                );
            }
            if !case.exception {
                assert_eq!(
                    projection(&current),
                    *old_snaps[step]
                        .get("rtcp")
                        .unwrap()
                        .get("peerReceptionReports")
                        .unwrap(),
                    "{} step{step} projection",
                    case.name
                );
            }
            new_snaps.push(snapshot(&current, events.reports()));
            audio.set(false, step as u64 + 2);
        }
        if case.name == "cap12" {
            assert_eq!(
                old_snaps[0]
                    .get("rtcp")
                    .unwrap()
                    .get("peerReceptionReports")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                8
            );
        }
        actual.push(IvyJson::object([
            ("name", case.name.into()),
            ("approvedException", case.exception.into()),
            ("snapshots", IvyJson::Array(new_snaps)),
        ]));
        receiver.close().await;
    }
    if let Some(path) = std::env::var_os("IVY_PHONE_RTCP_OWNER_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("inputs", IvyJson::Array(inputs)),
                    ("original", IvyJson::Array(old)),
                    ("rust", IvyJson::Array(actual)),
                ]),
                4_000_000,
            )
            .unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn report_owner_atomic_validation_foreign_stale_closed_and_bye_effects_are_fenced() {
    let (receiver, audio, events, peer, socket) = setup(42200, false, false).await;
    receiver.set_local_source(17).unwrap();
    send(
        &peer,
        receiver.observation().transport.local_rtp,
        1,
        0,
        9,
        &[0; 160],
    )
    .await;
    wait(|| receiver.observation().rtcp.learned_source == Some(9)).await;
    control(&receiver, &socket, &events, &rr(9, &[900])).await;
    let peer_before = receiver.observation().transport.peer;
    let bye = RtcpReportBuilder::new(9, "fixture").unwrap().bye_packet("");
    let barrier = bytes("80cf000100000009");
    let mut bad = invalid_compounds();
    for raw in [
        "80cd00020000000900000000",
        "8fcd0003000000090000000900000000",
        "8fcd000400000009000000090000000100000000",
        "8fce0003000000090000000052454d42",
        "8fce0004000000090000000052454d4201000000",
    ] {
        bad.push((format!("feedback-{raw}"), bytes(raw)));
    }
    for (index, (name, data)) in bad.iter().enumerate() {
        let frozen = receiver.observation().rtcp;
        let count = events.controls();
        socket
            .send_to(
                &[bye.clone(), rr(9, &[1000]), data.clone()].concat(),
                receiver.observation().transport.local_rtcp,
            )
            .await
            .unwrap();
        control(&receiver, &socket, &events, &barrier).await;
        assert_eq!(events.controls(), count + 1, "{name}");
        assert_eq!(receiver.observation().rtcp, frozen, "{name}");
        assert_eq!(receiver.observation().transport.peer, peer_before);
        control(&receiver, &socket, &events, &rr(9, &[2000 + index as u32])).await;
        assert_eq!(
            receiver.observation().rtcp.reports_received,
            frozen.reports_received + 1,
            "{name} following"
        );
        assert_eq!(receiver.observation().rtcp.learned_source, Some(9));
        audio.set(false, index as u64 + 2);
    }
    let foreign = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let frozen = receiver.observation().rtcp;
    let before = receiver.observation().transport.counters.foreign_rtcp;
    foreign
        .send_to(&rr(9, &[7000]), receiver.observation().transport.local_rtcp)
        .await
        .unwrap();
    wait(|| receiver.observation().transport.counters.foreign_rtcp == before + 1).await;
    assert_eq!(receiver.observation().rtcp, frozen);
    let replacement = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    configure(
        &receiver,
        peer.local_addr().unwrap(),
        replacement.local_addr().unwrap(),
        &[],
    );
    let frozen = receiver.observation().rtcp;
    socket
        .send_to(
            &rr(17, &[8000]),
            receiver.observation().transport.local_rtcp,
        )
        .await
        .unwrap();
    wait(|| receiver.observation().transport.counters.foreign_rtcp == before + 2).await;
    assert_eq!(receiver.observation().rtcp, frozen);
    control(&receiver, &replacement, &events, &rr(17, &[9000])).await;
    receiver.close().await;
    let frozen = receiver.observation().rtcp;
    replacement
        .send_to(
            &rr(17, &[10000]),
            receiver.observation().transport.local_rtcp,
        )
        .await
        .unwrap();
    assert_eq!(receiver.observation().rtcp, frozen);
    assert!(receiver.set_local_source(17).is_err());
    let _rtp = std::net::UdpSocket::bind(receiver.observation().transport.local_rtp).unwrap();
    let _rtcp = std::net::UdpSocket::bind(receiver.observation().transport.local_rtcp).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn source_learning_event_selection_and_accepted_sdp_replacement_match_actual_original() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original source routing unavailable; differential not executed");
        return;
    }
    let packet = |source: u32, pt: u8, payload: &[u8]| {
        let mut b = vec![0; 12 + payload.len()];
        b[0] = 128;
        b[1] = pt;
        b[3] = 1;
        b[8..12].copy_from_slice(&source.to_be_bytes());
        b[12..].copy_from_slice(payload);
        IvyJson::object([("kind", "rtp".into()), ("hex", hex(&b).into())])
    };
    let replace = |sources: Vec<u32>| {
        IvyJson::object([
            ("kind", "renegotiate".into()),
            (
                "sources",
                IvyJson::Array(sources.into_iter().map(n).collect()),
            ),
        ])
    };
    let steps = vec![
        packet(9, 0, &[]),
        hex(&rr(9, &[900])).into(),
        packet(45, 101, &[1, 0xa0, 0, 160]),
        hex(&rr(45, &[900])).into(),
        packet(46, 110, &[2, 0xa0, 0, 160]),
        hex(&rr(46, &[900])).into(),
        replace(vec![60, 61]),
        hex(&rr(46, &[900])).into(),
        hex(&rr(60, &[900])).into(),
        hex(&rr(61, &[900])).into(),
        hex(&rr(17, &[900])).into(),
        hex(&rr(99, &[17])).into(),
        hex(&sr(99, &[17])).into(),
        hex(&rr(0, &[900])).into(),
        packet(31, 127, &[0]),
        hex(&rr(31, &[900])).into(),
        replace(vec![70]),
        hex(&rr(31, &[900])).into(),
        hex(&rr(60, &[900])).into(),
        hex(&rr(70, &[900])).into(),
    ];
    let input = IvyJson::object([
        ("op", "receive".into()),
        ("seed", false.into()),
        ("datagrams", IvyJson::Array(steps.clone())),
    ]);
    let old = original(&[input.clone()]).remove(0);
    assert_eq!(old.get("ok").unwrap().as_bool(), Some(true), "{old:?}");
    let old_snaps = old.get("snapshots").unwrap().as_array().unwrap();
    let (receiver, audio, events, peer, socket) = setup(42400, false, false).await;
    audio.set(true, 1); // A bare header must also be ignored by an open PCM queue.
    receiver.set_local_source(17).unwrap();
    assert!(receiver.set_local_source(18).is_err());
    let mut actual = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        if let IvyJson::String(_) = step {
            control(&receiver, &socket, &events, &bytes(&step.as_str().unwrap())).await;
        } else if step.get("kind").unwrap().as_str().unwrap() == "renegotiate" {
            let sources = step
                .get("sources")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap() as u32)
                .collect::<Vec<_>>();
            configure(
                &receiver,
                peer.local_addr().unwrap(),
                socket.local_addr().unwrap(),
                &sources,
            );
        } else {
            let before = events.controls();
            peer.send_to(
                &bytes(&step.get("hex").unwrap().as_str().unwrap()),
                receiver.observation().transport.local_rtp,
            )
            .await
            .unwrap();
            // A matching source-independent control callback is a completion
            // barrier only after observing that exact RTP packet's counter.
            wait(|| {
                receiver.observation().transport.counters.rtp_received
                    >= (if index == 0 {
                        1
                    } else if index == 2 {
                        2
                    } else if index == 4 {
                        3
                    } else {
                        4
                    })
            })
            .await;
            control(&receiver, &socket, &events, &bytes("80cf000100000009")).await;
            assert_eq!(events.controls(), before + 1);
        }
        let current = receiver.observation().rtcp;
        let expected = &old_snaps[index];
        assert_eq!(
            current.learned_source.map(f64::from),
            expected.get("learnedSource").unwrap().as_f64(),
            "source step{index}"
        );
        assert_eq!(
            n(current.reports_received as u32),
            *expected
                .get("rtcp")
                .unwrap()
                .get("reportsReceived")
                .unwrap(),
            "count step{index}"
        );
        assert_eq!(
            projection(&current),
            *expected
                .get("rtcp")
                .unwrap()
                .get("peerReceptionReports")
                .unwrap(),
            "projection step{index}"
        );
        assert_eq!(
            current.last_report_at.is_none(),
            matches!(
                expected.get("rtcp").unwrap().get("lastReportAt"),
                Some(IvyJson::Null)
            )
        );
        assert_eq!(
            IvyJson::Array(current.declared_sources.iter().copied().map(n).collect()),
            *expected.get("declaredSources").unwrap()
        );
        actual.push(snapshot(&current, events.reports()));
        assert!(!receiver.observation().failed);
        audio.set(false, index as u64 + 2);
    }
    receiver.close().await;
    if let Some(path) = std::env::var_os("IVY_PHONE_RTCP_ROUTING_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("input", input),
                    ("original", old),
                    ("rust", IvyJson::Array(actual)),
                ]),
                1_000_000,
            )
            .unwrap(),
        )
        .unwrap();
    }
}
