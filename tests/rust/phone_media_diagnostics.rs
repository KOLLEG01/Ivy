use super::*;
use ivy_phone_bridge::media::{RtcpReportBlock, native_view};

fn scenario(preferences: &[&str], steps: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([
        (
            "preferences",
            IvyJson::Array(preferences.iter().map(|s| (*s).into()).collect()),
        ),
        ("steps", IvyJson::Array(steps)),
    ])
}
fn snapshot() -> IvyJson {
    op("snapshot")
}
fn set_sdp(formats: &str, extra: &str) -> IvyJson {
    IvyJson::object([("op","set".into()),("sdp",format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio $PORT RTP/AVP {formats}\r\n{extra}").into())])
}
fn counters(value: &str) -> IvyJson {
    IvyJson::object([
        ("op", "counters".into()),
        ("sent", value.into()),
        ("received", value.into()),
        ("reports", value.into()),
        ("ticks", "638000000000000001".into()),
    ])
}
fn corpus() -> Vec<IvyJson> {
    let mut cases = vec![scenario(
        &["PCMU", "PCMA"],
        vec![
            snapshot(),
            op("offer"),
            set_sdp(
                "0 101 110",
                "a=rtpmap:101 telephone-event/8000\r\na=rtpmap:110 telephone-event/48000\r\n",
            ),
            op("answer"),
            set_sdp("0", ""),
            op("offer"),
            op("close"),
            op("dispose"),
        ],
    )];
    for (prefs, formats, extra) in [
        (
            vec!["PCMU", "PCMA"],
            "8 0 110",
            "a=rtpmap:110 telephone-event/8000\r\n",
        ),
        (
            vec!["G722", "PCMA"],
            "9 8 101",
            "a=rtpmap:101 telephone-event/8000\r\n",
        ),
        (
            vec!["OPUS", "PCMA"],
            "111 8 101",
            "a=rtpmap:111 opus/48000/2\r\na=fmtp:111 useinbandfec=1\r\na=rtpmap:101 telephone-event/8000\r\n",
        ),
        (
            vec!["PCMU", "PCMA"],
            "0 101",
            "a=rtpmap:101 telephone-event/0\r\n",
        ),
        (
            vec!["PCMU", "PCMA"],
            "8 0",
            "a=rtpmap:0 telephone-event/8000\r\n",
        ),
    ] {
        cases.push(scenario(
            &prefs,
            vec![
                snapshot(),
                set_sdp(formats, extra),
                op("answer"),
                op("offer"),
                op("close"),
                op("dispose"),
            ],
        ));
    }
    cases.push(scenario(
        &["PCMU", "PCMA"],
        vec![
            set_sdp("0 101", "a=rtpmap:101 telephone-event/8000\r\n"),
            op("answer"),
            set_sdp("9 110", "a=rtpmap:110 telephone-event/8000\r\n"),
            op("offer"),
            set_sdp(
                "0 111",
                "a=rtpmap:111 telephone-event/8000\r\na=inactive\r\n",
            ),
            op("answer"),
            op("offer"),
        ],
    ));
    cases.push(scenario(
        &["PCMU", "PCMA"],
        vec![
            set_sdp("0 101", "a=rtpmap:101 telephone-event/8000\r\n"),
            IvyJson::object([("op", "status".into()), ("value", "Inactive".into())]),
            op("answer"),
            op("offer"),
            IvyJson::object([("op", "challenge".into()), ("enabled", true.into())]),
            op("offer"),
            IvyJson::object([("op", "challenge".into()), ("enabled", false.into())]),
            op("offer"),
        ],
    ));
    cases
}
fn normalized(v: &ivy_phone_bridge::media::SipMediaObservation, peer: u16) -> String {
    native_view::diagnostics_json(v, None)
        .unwrap()
        .replace(&v.receive.transport.local_rtp.to_string(), "local")
        .replace(&format!("127.0.0.1:{peer}"), "peer")
}
async fn run(case: &IvyJson) -> Vec<IvyJson> {
    let factory = Arc::new(Factory::routes(vec![]));
    let preferences = case
        .get("preferences")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| match v.as_str().unwrap().as_str() {
            "PCMU" => Codec::Pcmu,
            "PCMA" => Codec::Pcma,
            "G722" => Codec::G722,
            "OPUS" => Codec::Opus,
            _ => unreachable!(),
        })
        .collect();
    let m = SipMediaSession::bind(
        SipMediaSettings {
            call_id: CALL.into(),
            interface: "127.0.0.1:0".parse().unwrap(),
            connection_address: "127.0.0.1".parse().unwrap(),
            first_port: 50000,
            last_port: 50998,
            reorder_ms: 20,
            preferences,
            identity: SipMediaIdentity {
                session_id: 1,
                source: 17,
                sequence: 100,
                timestamp: 200,
                cname: "fixture".into(),
            },
        },
        CallAudioPort::new(CALL.into(), factory.clone()).unwrap(),
        None,
        Arc::new(Clock),
        Arc::new(Events::default()),
    )
    .await
    .unwrap();
    m.enable_dtmf(|_, _| Ok(()), || false).unwrap();
    let (peer, _control) = pair().await;
    let port = peer.local_addr().unwrap().port();
    let foreign = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut rows = Vec::new();
    for step in case.get("steps").unwrap().as_array().unwrap() {
        let op = step.get("op").unwrap().as_str().unwrap();
        let mut result = IvyJson::Null;
        let mut controlled = None;
        match op.as_str() {
            "snapshot" => {}
            "offer" => {
                m.create_offer();
            }
            "answer" => {
                m.create_answer();
            }
            "set" => {
                let sdp = SdpDocument::parse(
                    &step
                        .get("sdp")
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .replace("$PORT", &port.to_string()),
                )
                .unwrap();
                result = match m.set_remote_description(SdpType::Offer, &sdp) {
                    Ok(()) => "OK".into(),
                    Err(SipMediaError::Negotiation(e)) => e.as_str().into(),
                    Err(e) => panic!("unexpected media status error {e:?}"),
                };
            }
            "status" => {
                m.set_stream_status(SdpDirection::Inactive).unwrap();
            }
            "challenge" => m.challenge_codecs(step.get("enabled").unwrap().as_bool().unwrap()),
            "close" => m.request_close(),
            "dispose" => m.close().await.unwrap(),
            "send" => {
                let raw = step.get("hex").unwrap().as_str().unwrap();
                let bytes = (0..raw.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).unwrap())
                    .collect::<Vec<_>>();
                let before = m.observation().receive.raw_rtp_packets;
                let source = if step.get("foreign").and_then(IvyJson::as_bool) == Some(true) {
                    &foreign
                } else {
                    &peer
                };
                source
                    .send_to(&bytes, m.observation().receive.transport.local_rtp)
                    .await
                    .unwrap();
                crate::sip_receiver::wait(|| m.observation().receive.raw_rtp_packets > before)
                    .await;
            }
            "counters" => {
                let number = |key| {
                    step.get(key)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .parse::<i64>()
                        .unwrap()
                };
                let mut v = m.observation();
                v.sent_frames = number("sent") as u64;
                v.receive.received_packets = number("received") as u64;
                let r = &mut v.receive;
                r.rtcp.reports_received = number("reports") as u64;
                r.rtcp.reports_sent = number("reports") as u64;
                r.rtcp.last_report_at = Some(
                    std::time::UNIX_EPOCH
                        + Duration::from_nanos(
                            ((number("ticks") - 621_355_968_000_000_000) as u64) * 100,
                        ),
                );
                r.rtcp.peer_reports = vec![RtcpReportBlock::new(
                    u32::MAX,
                    255,
                    -1,
                    u32::MAX,
                    u32::MAX,
                    0,
                    0,
                )];
                r.raw_payloads = vec![(8, number("received") as u64)];
                r.raw_rtp_packets = number("reports") as u64;
                r.raw_offered_event_packets = number("reports") as u64;
                r.parsed_event_packets = number("reports") as u64;
                r.library_event_packets = number("reports") as u64;
                r.native_unsupported_payload_packets = number("reports") as u64;
                r.inband_analyzed_packets = number("reports") as u64;
                r.inband_detected_digits = number("reports") as u64;
                r.detected_events = number("reports") as u64;
                controlled = Some(v);
            }
            _ => unreachable!(),
        }
        let view = controlled.unwrap_or_else(|| m.observation());
        let json = normalized(&view, port);
        rows.push(IvyJson::object([
            ("result", result),
            ("diagnostics", json.into()),
        ]));
    }
    m.close().await.unwrap();
    assert_eq!(factory.opens.load(Ordering::SeqCst), 0);
    rows
}
fn compare(actual: &[IvyJson], original: &IvyJson, label: &str) {
    let expected = original.as_array().unwrap();
    assert_eq!(actual.len(), expected.len());
    for (index, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(
            a.get("result"),
            b.get("result"),
            "{label} step {index} result"
        );
        let a = a.get("diagnostics").unwrap().as_str().unwrap();
        let b = b.get("diagnostics").unwrap().as_str().unwrap();
        assert_eq!(
            parse(&a, 65536).unwrap(),
            parse(&b, 65536).unwrap(),
            "{label} step {index} diagnostics"
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_media_diagnostics_follow_sdp_codecs_endpoints_close_and_disposal() {
    let inputs = corpus();
    let expected =
        original_session_fixture("tests/fixtures/phone-media-diagnostics-oracle.ps1", &inputs);
    let mut actual = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        let rows = run(input).await;
        if let Some(e) = &expected {
            compare(
                &rows,
                &e.as_array().unwrap()[index],
                &format!("scenario {index}"),
            );
        }
        actual.push(IvyJson::Array(rows));
    }
    trace(
        "media-diagnostics-sdp",
        IvyJson::object([
            ("inputs", IvyJson::Array(inputs)),
            ("rust", IvyJson::Array(actual)),
            ("original", expected.unwrap_or(IvyJson::Null)),
        ]),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_raw_channel_and_selected_alternate_event_counters_are_distinct() {
    let mut steps = vec![
        op("offer"),
        set_sdp(
            "0 101 110",
            "a=rtpmap:101 telephone-event/8000\r\na=rtpmap:110 telephone-event/48000\r\na=ssrc:111 cname:peer\r\n",
        ),
        op("answer"),
    ];
    let packet = |payload, data: &[u8], sequence, foreign: bool| {
        let mut raw = vec![0; 12 + data.len()];
        ivy_phone_bridge::media::RtpHeader {
            marker: false,
            payload_type: payload,
            sequence,
            timestamp: u32::from(sequence) * 160,
            source: 111,
        }
        .write(data, &mut raw)
        .unwrap();
        IvyJson::object([
            ("op", "send".into()),
            ("hex", hex(&raw).into()),
            ("foreign", foreign.into()),
        ])
    };
    steps.extend([
        packet(0, &[0xff; 160], 1, false),
        packet(101, &[5, 10, 0, 80], 2, false),
        packet(101, &[5, 0x8a, 0, 160], 2, false),
        packet(110, &[5, 0x8a, 0, 160], 2, false),
        packet(110, &[6, 0x8a, 0, 160], 3, false),
        packet(101, &[16, 0x8a, 0, 160], 4, false),
        packet(120, &[1, 2, 3, 4], 5, false),
        packet(101, &[9, 0x8a, 0, 160], 6, true),
        packet(0, &[0xff; 160], 7, true),
    ]);
    steps.push(set_sdp("0", "a=ssrc:111 cname:peer\r\n"));
    steps.push(packet(101, &[7, 0x8a, 0, 160], 8, false));
    let input = scenario(&["PCMU", "PCMA"], steps);
    let expected = original_session_fixture(
        "tests/fixtures/phone-media-diagnostics-oracle.ps1",
        &[input.clone()],
    );
    let actual = run(&input).await;
    if let Some(e) = &expected {
        compare(&actual, &e.as_array().unwrap()[0], "wire");
    }
    trace(
        "media-diagnostics-wire",
        IvyJson::object([
            ("input", input),
            ("rust", IvyJson::Array(actual)),
            ("original", expected.unwrap_or(IvyJson::Null)),
        ]),
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_media_diagnostic_integer_lexemes_and_report_fraction_are_preserved() {
    let inputs: Vec<_> = [
        "9007199254740993",
        "9223372036854775807",
        "-9223372036854775808",
    ]
    .into_iter()
    .map(|value| scenario(&["PCMU", "PCMA"], vec![counters(value)]))
    .collect();
    let expected =
        original_session_fixture("tests/fixtures/phone-media-diagnostics-oracle.ps1", &inputs);
    let mut rows = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        let actual = run(input).await;
        let value = input.get("steps").unwrap().as_array().unwrap()[0]
            .get("sent")
            .unwrap()
            .as_str()
            .unwrap();
        let raw = actual[0].get("diagnostics").unwrap().as_str().unwrap();
        assert!(raw.contains(&format!("\"sentPackets\":{value},")));
        assert!(raw.contains("2022-09-28T22:13:20.0000001+00:00"));
        if let Some(expected) = &expected {
            let expected = &expected.as_array().unwrap()[index];
            compare(&actual, expected, "integer DTO");
            let raw = expected.as_array().unwrap()[0]
                .get("diagnostics")
                .unwrap()
                .as_str()
                .unwrap();
            assert!(raw.contains(&format!("\"sentPackets\":{value},")));
        }
        rows.push(IvyJson::Array(actual));
    }
    trace(
        "media-diagnostics-integers",
        IvyJson::object([
            ("inputs", IvyJson::Array(inputs)),
            ("rust", IvyJson::Array(rows)),
            ("original", expected.unwrap_or(IvyJson::Null)),
        ]),
    );
}
