use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_phone_bridge::media::{
    AudioFormat, AudioNegotiator, Codec, NegotiationError, SdpDirection, SdpDocument, SdpFormat,
    SdpIdentity, SdpMedia, SdpType, TelephoneEventFormat, bounded_sdp, valid_sdp,
};

const PREFIX: &str =
    "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n";
fn n(value: impl Into<f64>) -> IvyJson {
    IvyJson::Number(value.into())
}
fn text(value: Option<&str>) -> IvyJson {
    value.map(Into::into).unwrap_or(IvyJson::Null)
}
fn get(value: &IvyJson, key: &str) -> IvyJson {
    value.get(key).cloned().unwrap_or(IvyJson::Null)
}
fn s(value: &IvyJson, key: &str) -> Option<String> {
    value.get(key).and_then(|value| value.as_str().ok())
}
fn items(value: &IvyJson) -> &[IvyJson] {
    value.as_array().unwrap_or(&[])
}
fn parse_case(body: impl Into<String>) -> IvyJson {
    IvyJson::object([("op", "parse".into()), ("sdp", body.into().into())])
}
fn offer(body: &str) -> IvyJson {
    IvyJson::object([
        ("type", "offer".into()),
        ("sdp", format!("{PREFIX}{body}").into()),
    ])
}
fn session(preference: &[Codec], steps: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([
        ("op", "session".into()),
        (
            "preferences",
            IvyJson::Array(preference.iter().map(|codec| codec.name().into()).collect()),
        ),
        ("steps", IvyJson::Array(steps)),
    ])
}
fn parser_cases() -> Vec<IvyJson> {
    let mut cases = Vec::new();
    for body in [
        "",
        "garbage",
        "v=0",
        "v=1\ns=a\n",
        "v=0\no=bad\ns=a\n",
        "m=audio 31000 RTP/AVP 0",
    ] {
        cases.push(parse_case(body));
    }
    for payload in -1..=128 {
        cases.push(parse_case(format!(
            "{PREFIX}m=audio 31000 RTP/AVP {payload}\r\n"
        )));
    }
    for kind in [
        "invalid",
        "data",
        "control",
        "audio",
        "AUDIO",
        "video",
        "text",
        "application",
        "image",
        "message",
        "bogus",
    ] {
        cases.push(parse_case(format!(
            "{PREFIX}m={kind} 31000 RTP/AVP 0 8 111\r\na=rtpmap:111 opus/48000/2\r\n"
        )));
    }
    for line in [
        "m=audio 31000/2 RTP/AVP 0",
        "m=audio -1 RTP/AVP 0",
        "m=audio 65536 RTP/AVP 0",
        "m=audio 31000 RTP/AVP",
        "m=audio 31000 RTP/AVP 0 0 127 128 -1 foo",
        "m=audio 31000 RTP/AVP 00 +8 09",
        "m=audio x RTP/AVP 0",
        "m=audio 31000",
        "m=audio",
        "m=",
    ] {
        cases.push(parse_case(format!("{PREFIX}{line}\r\n")));
    }
    for map in [
        "PCMU/8000",
        "pcmu/8000/1",
        "PCMU/16000",
        "G722/8000",
        "G722/16000",
        "opus/48000",
        "opus/48000/1",
        "opus/48000/2",
        "opus/48000/3",
        "telephone-event/48000",
        "telephone-event/8000/2",
        "telephone-event",
        "EVS/16000/1",
        "EVS/48000/1",
        "PCMA /8000",
        "PCMA/0",
        "test/8000",
    ] {
        cases.push(parse_case(format!(
            "{PREFIX}m=audio 31000 RTP/AVP 110\r\na=rtpmap:110 {map}\r\n"
        )));
    }
    for attribute in [
        "a=rtpmap:0 PCMA/8000",
        "a=rtpmap:110 pcmu/8000",
        "a=fmtp:110 a",
        "a=fmtp:0 a",
        "a=fmtp:0",
        "a=rtpmap:0",
        "a=rtpmap:x pcmu/8000",
        "a=rtpmap:110\topus/48000/2",
        "a=RTPMAP:110 opus/48000/2",
        "a=rtpmap:110  opus/48000/2",
        "a=rtpmap:110 /8000",
        "a=rtpmap:999 unknown/1",
        "a=rtcp-fb:0 nack",
        "a=rtcp-mux",
        "a=sendonly",
        "a= SENDONLY",
        "a=mid:0",
        "a=mid:",
        "a=mid:one\r\na=mid:two",
        "a=sendonly\r\na=recvonly",
        "a=ice-ufrag:foo",
        "a=fingerprint:sha-256 aa:bb",
        "a=ssrc:123 cname:test",
        "a=ssrc:123 msid:abc def",
        "a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level",
        "a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:dGVzdA==",
        "a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "c=IN IP4 127.0.0.1/127/2",
        "c=IN IP6 fe80::1%1",
        "c=IN IP4 hostname.invalid",
        "c=invalid",
        "a=foo:bar\r\na=foo:bar\r\nx=unknown",
        "a=rtcp:31001",
        "a=ptime:20\r\na=maxptime:120",
        "b=AS:64",
        "i=description",
    ] {
        cases.push(parse_case(format!(
            "{PREFIX}m=audio 31000 RTP/AVP 0\r\n{attribute}\r\n"
        )));
    }
    for header in [
        "o=user 12 abc IN IP4 127.0.0.1",
        "o=user 12 -1 IN IP4 127.0.0.1",
        "o=user 12 18446744073709551615 IN IP4 127.0.0.1",
        "o=user 12 3 XX IP6 ::1",
        "o=user 12 3 IN IP6 ::1 extra",
        "s=",
        "s= hello ",
        "v=not",
        "t=1 2\r\nt=3 4",
        "i=info\r\nu=https://example.invalid/\r\ne=a@example.invalid\r\np=none\r\nb=AS:64\r\na=group:BUNDLE 0",
        "a=ice-lite\r\na=ice-ufrag:test\r\na=ice-pwd:foo\r\na=fingerprint:sha-256 AA:BB\r\na=setup:actpass",
    ] {
        cases.push(parse_case(format!(
            "{PREFIX}{header}\r\nm=audio 31000 RTP/AVP 0\r\n"
        )));
    }
    for direction in [
        "sendrecv", "sendonly", "recvonly", "inactive", "SENDONLY", "bogus",
    ] {
        cases.push(parse_case(format!(
            "{PREFIX}a={direction}\r\nm=audio 31000 RTP/AVP 0\r\n"
        )));
    }
    for name in ["H264", "H265", "VP9", "AV1", "unknown"] {
        cases.push(parse_case(format!("{PREFIX}m=video 31000 RTP/AVP 96\r\na=rtpmap:96 {name}/90000\r\na=fmtp:96 profile-level-id=42e01f;packetization-mode=1\r\na=rtcp-fb:96 nack pli\r\n")));
    }
    cases.push(parse_case(format!(
        "{PREFIX}m=video 31000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n"
    )));
    for map in ["PCMU/8000", "opus/48000/2"] {
        cases.push(parse_case(format!(
            "{PREFIX}m=audio 31000 RTP/AVP 31\r\na=rtpmap:31 {map}\r\n"
        )));
    }
    for suite in [
        "AES_CM_128_HMAC_SHA1_80",
        "AES_CM_128_HMAC_SHA1_32",
        "AES_256_CM_HMAC_SHA1_80",
        "AES_256_CM_HMAC_SHA1_32",
    ] {
        for key in [
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            cases.push(parse_case(format!(
                "{PREFIX}m=audio 31000 RTP/AVP 0\r\na=crypto:1 {suite} inline:{key}\r\n"
            )));
        }
    }
    for tag in [0, 1, 999_999_999, 1_000_000_000, 2_147_483_647] {
        cases.push(parse_case(format!("{PREFIX}m=audio 31000 RTP/AVP 0\r\na=crypto:{tag} AES_CM_128_HMAC_SHA1_80 inline:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\r\n")));
    }
    for ending in ["\r\n", "\n", "\r"] {
        cases.push(parse_case(
            format!("{PREFIX}m=audio 31000 RTP/AVP 0\r\n").replace("\r\n", ending),
        ));
    }
    for media in [
        "m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=sctp-port:5000\r\na=max-message-size:262144",
        "m=message 31000 TCP/MSRP *",
        "m=message 31000 TCP/MSRP *\r\na=path:msrp://127.0.0.1:31000/test;tcp\r\na=accept-types:text/plain",
        "m=audio 31000 RTP/AVP 111 0 8\r\na=rtpmap:111 opus/48000/2",
        "m=audio 31000 RTP/AVP 111\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 a\r\na=rtpmap:111 OPUS/8000/1\r\na=fmtp:111 b",
        "m=audio 31000 RTP/AVP 0\r\nm=video 32000 RTP/AVP 96\r\na=rtpmap:96 VP8/90000\r\nm=audio 33000 RTP/AVP 8\r\nc=IN IP6 ::1\r\na=inactive",
    ] {
        cases.push(parse_case(format!("{PREFIX}{media}\r\n")));
    }
    for body in [
        "".into(),
        "x".repeat(65_536),
        "x".repeat(65_537),
        "😀".repeat(32_768),
        "😀".repeat(32_769),
        "\u{0085}".repeat(2),
    ] {
        for mime in [
            Some("application/sdp"),
            Some(" Application/SDP ;charset=utf-8"),
            Some("text/plain"),
            None,
        ] {
            cases.push(IvyJson::object([
                ("op", "valid".into()),
                ("sdp", body.clone().into()),
                ("mime", text(mime)),
            ]));
        }
    }
    cases
}
fn session_cases() -> Vec<IvyJson> {
    let preferences = [Codec::Opus, Codec::G722, Codec::Pcma, Codec::Pcmu];
    let mut cases = Vec::new();
    for payload in [24, 26, 31, 34, 96, 127] {
        for map in ["PCMU/8000", "opus/48000/2"] {
            cases.push(session(
                &preferences,
                vec![offer(&format!(
                    "m=audio 31000 RTP/AVP {payload}\r\na=rtpmap:{payload} {map}\r\n"
                ))],
            ));
        }
    }
    for (id, map) in [
        (0, None),
        (8, None),
        (9, None),
        (110, None),
        (110, Some("PCMA/8000")),
        (110, Some("pcma/8000/1")),
        (110, Some("PCMA/16000")),
        (110, Some("PCMU/8000/2")),
        (0, Some("PCMA/8000")),
        (8, Some("PCMU/8000")),
        (110, Some("G722/8000")),
        (110, Some("G722/16000")),
        (110, Some("G722/8000/2")),
        (111, Some("opus/48000")),
        (111, Some("opus/48000/1")),
        (111, Some("opus/48000/2")),
        (111, Some("OPUS/48000/3")),
        (111, Some("OPUS/16000/2")),
        (110, Some("telephone-event/8000")),
        (110, Some("test/8000")),
    ] {
        cases.push(session(
            &preferences,
            vec![offer(&format!(
                "m=audio 31000 RTP/AVP {id}\r\n{}",
                map.map(|map| format!("a=rtpmap:{id} {map}\r\n"))
                    .unwrap_or_default()
            ))],
        ));
    }
    for port in [0, 1, 31000, 65535, 65536, -1] {
        cases.push(session(
            &preferences,
            vec![offer(&format!("m=audio {port} RTP/AVP 0\r\n"))],
        ));
    }
    for transport in [
        "RTP/AVP",
        "RTP/AVPF",
        "RTP/SAVP",
        "UDP/TLS/RTP/SAVPF",
        "foo",
    ] {
        cases.push(session(
            &preferences,
            vec![offer(&format!("m=audio 31000 {transport} 0\r\n"))],
        ));
    }
    for map in [
        "telephone-event/8000",
        "telephone-event/48000",
        "TELEPHONE-EVENT/48000",
        "telephone-event/16000",
        "telephone-event/8000/2",
        "telephone-event/0",
        "telephone-event",
    ] {
        cases.push(session(&preferences, vec![offer(&format!("m=audio 31000 RTP/AVP 111 110\r\na=rtpmap:111 opus/48000/2\r\na=rtpmap:110 {map}\r\na=fmtp:110 0-15\r\n"))]));
    }
    for (payload, map) in [(111, Some("opus/48000/2")), (8, None), (9, None)] {
        for order in ["110 112", "112 110"] {
            cases.push(session(&preferences, vec![offer(&format!("m=audio 31000 RTP/AVP {payload} {order}\r\n{}a=rtpmap:110 telephone-event/8000\r\na=rtpmap:112 telephone-event/48000\r\na=fmtp:110 0-16\r\na=fmtp:112 0-16\r\n",map.map(|m|format!("a=rtpmap:{payload} {m}\r\n")).unwrap_or_default()))]));
        }
    }
    for body in [
        "",
        "m=video 32000 RTP/AVP 96\r\na=rtpmap:96 VP8/90000",
        "m=audio 31000 RTP/AVP 0\r\nm=video 32000 RTP/AVP 96\r\na=rtpmap:96 VP8/90000",
        "m=video 32000 RTP/AVP 96\r\na=rtpmap:96 VP8/90000\r\nm=audio 31000 RTP/AVP 0",
        "m=audio 31000 RTP/AVP 0\r\nm=audio 32000 RTP/AVP 8",
        "m=audio 31000 RTP/AVP 0\r\na=mid:audio\r\na=sendonly",
        "m=audio 31000 RTP/AVP 0\r\na=inactive",
        "m=audio 31000 RTP/AVP 0\r\nc=IN IP4 0.0.0.0",
        "m=audio 31000 RTP/AVP 0\r\na=recvonly",
        "m=audio 31000 RTP/AVP 0\r\na=rtcp:31007\r\na=ptime:40",
        "m=audio 31000 RTP/AVP 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=sctp-port:5000",
        "m=audio 31000 RTP/AVP 0\r\nm=image 9 udptl t38",
        "m=audio 31000 RTP/AVP 0\r\na=fmtp:0 a\r\na=foo:bar\r\na=ptime:40",
        "m=audio 31000 RTP/AVP 0\r\na=rtpmap:110 pcma/8000",
        "m=audio 31000 RTP/AVP 0\r\nc=IN IP6 ::",
    ] {
        cases.push(session(&preferences, vec![offer(body)]));
    }
    for preference in [
        &[Codec::Pcma][..],
        &[Codec::Opus][..],
        &[Codec::G722][..],
        &[Codec::Pcmu, Codec::Pcma][..],
        &preferences[..],
    ] {
        cases.push(session(
            preference,
            vec![offer(
                "m=audio 31000 RTP/AVP 0 8 9 111\r\na=rtpmap:111 opus/48000/2",
            )],
        ));
    }
    for kind in ["offer", "answer"] {
        let steps = ["0", "9", "110", "8", "111"]
            .iter()
            .map(|id| {
                IvyJson::object([
                    ("type", kind.into()),
                    (
                        "sdp",
                        format!(
                            "{PREFIX}m=audio 31000 RTP/AVP {id}\r\n{}",
                            if *id == "111" {
                                "a=rtpmap:111 opus/48000/2\r\n"
                            } else {
                                ""
                            }
                        )
                        .into(),
                    ),
                ])
            })
            .collect();
        cases.push(session(&preferences, steps));
    }
    for enabled in [true, false] {
        cases.push(session(
            &[Codec::Opus, Codec::G722],
            vec![
                offer("m=audio 31000 RTP/AVP 9"),
                IvyJson::object([
                    ("type", "offer".into()),
                    ("challenge", IvyJson::Bool(enabled)),
                    (
                        "sdp",
                        format!("{PREFIX}m=audio 31000 RTP/AVP 9 8\r\n").into(),
                    ),
                ]),
                IvyJson::object([
                    ("type", "offer".into()),
                    ("challenge", IvyJson::Bool(false)),
                    ("sdp", format!("{PREFIX}m=audio 31000 RTP/AVP 9\r\n").into()),
                ]),
            ],
        ));
    }
    cases
}

fn format_shape(format: &SdpFormat) -> IvyJson {
    IvyJson::object([
        ("payload", n(format.payload())),
        ("name", text(format.name())),
        ("rtpmap", format.rtpmap().into()),
        ("fmtp", text(format.parameters())),
    ])
}
fn formats(formats: &[SdpFormat]) -> IvyJson {
    IvyJson::Array(formats.iter().map(format_shape).collect())
}
fn direction(direction: Option<SdpDirection>) -> IvyJson {
    text(direction.map(|dir| match dir {
        SdpDirection::SendRecv => "SendRecv",
        SdpDirection::SendOnly => "SendOnly",
        SdpDirection::RecvOnly => "RecvOnly",
        SdpDirection::Inactive => "Inactive",
    }))
}
fn media_shape(media: &SdpMedia) -> IvyJson {
    IvyJson::object([
        ("kind", media.kind().as_str().into()),
        ("port", n(if media.port() == 0 { 0 } else { 1 })),
        ("transport", media.transport().into()),
        ("direction", direction(media.direction())),
        ("mid", text(media.mid())),
        ("formats", formats(media.formats())),
    ])
}
fn document_shape(document: Option<&SdpDocument>) -> IvyJson {
    document
        .map(|document| IvyJson::Array(document.media().iter().map(media_shape).collect()))
        .unwrap_or(IvyJson::Null)
}
fn events(formats: &[TelephoneEventFormat]) -> IvyJson {
    IvyJson::Array(
        formats
            .iter()
            .map(|format| {
                IvyJson::object([
                    ("payload", n(format.payload)),
                    ("clockRate", n(format.clock_rate)),
                ])
            })
            .collect(),
    )
}
fn audio_shape(format: &AudioFormat) -> IvyJson {
    IvyJson::object([
        ("codec", format.name().into()),
        ("payload", n(format.payload())),
        ("clockRate", n(format.rtp_rate())),
        ("pcmRate", n(format.pcm_rate())),
        ("channels", n(format.channels())),
        ("parameters", text(format.parameters())),
    ])
}
fn actual(case: &IvyJson) -> IvyJson {
    let operation = s(case, "op").unwrap();
    if operation == "valid" {
        return IvyJson::object([(
            "valid",
            IvyJson::Bool(valid_sdp(
                s(case, "mime").as_deref(),
                &s(case, "sdp").unwrap(),
            )),
        )]);
    }
    if operation == "parse" {
        let body = s(case, "sdp").unwrap();
        return IvyJson::object([
            (
                "canonical",
                text(
                    SdpDocument::parse(&body)
                        .ok()
                        .map(|doc| doc.to_sdp())
                        .as_deref(),
                ),
            ),
            (
                "rejected",
                text(SdpDocument::rejected_answer(&body).as_deref()),
            ),
        ]);
    }
    let preference: Vec<_> = items(&get(case, "preferences"))
        .iter()
        .map(|v| match v.as_str().unwrap().as_str() {
            "OPUS" => Codec::Opus,
            "G722" => Codec::G722,
            "PCMA" => Codec::Pcma,
            "PCMU" => Codec::Pcmu,
            _ => unreachable!(),
        })
        .collect();
    let mut session = AudioNegotiator::new(&preference).unwrap();
    let identity =
        SdpIdentity::new("127.0.0.1:31002".parse().unwrap(), 1, 0, 7, "fixture").unwrap();
    let offer = session.create_offer(&identity);
    let mut steps = Vec::new();
    for step in items(&get(case, "steps")) {
        if let Some(enabled) = step.get("challenge").and_then(|value| value.as_bool()) {
            session.challenge_codecs(enabled);
        }
        let kind = if s(step, "type").unwrap() == "offer" {
            SdpType::Offer
        } else {
            SdpType::Answer
        };
        let result = SdpDocument::parse(&s(step, "sdp").unwrap())
            .map_err(NegotiationError::from)
            .and_then(|description| session.set_remote_description(kind, &description));
        if result == Err(NegotiationError::InvalidSdp) {
            return IvyJson::object([("error", IvyJson::Bool(true))]);
        }
        let answer = if result.is_ok() && kind == SdpType::Offer {
            session.create_answer(&identity)
        } else {
            None
        };
        let state = session.state();
        steps.push(IvyJson::object([
            (
                "result",
                result
                    .err()
                    .map(NegotiationError::as_str)
                    .unwrap_or("OK")
                    .into(),
            ),
            ("sendCodec", text(state.map(|state| state.sender().name()))),
            (
                "sendPayload",
                state
                    .map(|state| n(state.sender().payload()))
                    .unwrap_or(IvyJson::Null),
            ),
            ("localFormats", formats(session.local_formats())),
            ("answer", document_shape(answer.as_ref())),
            (
                "receive",
                IvyJson::Array(
                    state
                        .map(|state| state.receive_formats().iter().map(audio_shape).collect())
                        .unwrap_or_default(),
                ),
            ),
            (
                "offeredEvents",
                IvyJson::Array(
                    state
                        .map(|state| {
                            state
                                .offered_event_payloads()
                                .iter()
                                .copied()
                                .map(n)
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
            ),
            (
                "remoteEvents",
                events(
                    state
                        .map(|state| state.remote_event_formats())
                        .unwrap_or(&[]),
                ),
            ),
            ("localEvents", events(session.local_event_formats())),
            ("eventPayload", n(session.negotiated_event_payload())),
        ]));
    }
    IvyJson::object([
        ("error", IvyJson::Bool(false)),
        ("offer", document_shape(Some(&offer))),
        ("steps", IvyJson::Array(steps)),
    ])
}

fn original_document(value: &IvyJson) -> IvyJson {
    if value.is_null() {
        return IvyJson::Null;
    }
    IvyJson::Array(
        items(&get(value, "media"))
            .iter()
            .map(|media| {
                IvyJson::object([
                    ("kind", get(media, "kind")),
                    (
                        "port",
                        n(if get(media, "port").as_f64().unwrap() == 0.0 {
                            0
                        } else {
                            1
                        }),
                    ),
                    ("transport", get(media, "transport")),
                    ("direction", get(media, "direction")),
                    ("mid", get(media, "mid")),
                    ("formats", get(media, "formats")),
                ])
            })
            .collect(),
    )
}
fn project(case: &IvyJson, expected: &IvyJson) -> IvyJson {
    match s(case, "op").unwrap().as_str() {
        "parse" => IvyJson::object([
            ("canonical", get(expected, "canonical")),
            ("rejected", get(expected, "rejected")),
        ]),
        "valid" => expected.clone(),
        _ if get(expected, "ok") == IvyJson::Bool(false) => {
            IvyJson::object([("error", IvyJson::Bool(true))])
        }
        _ => IvyJson::object([
            ("error", IvyJson::Bool(false)),
            ("offer", original_document(&get(expected, "offer"))),
            (
                "steps",
                IvyJson::Array(
                    items(&get(expected, "steps"))
                        .iter()
                        .map(|step| {
                            assert_eq!(
                                get(step, "failed"),
                                IvyJson::Bool(false),
                                "Original synthetic codec construction failed"
                            );
                            let diagnostics = get(step, "diagnostics");
                            let dtmf = get(&diagnostics, "dtmf");
                            IvyJson::object([
                                ("result", get(step, "result")),
                                ("sendCodec", get(step, "sendCodec")),
                                ("sendPayload", get(step, "sendPayload")),
                                ("localFormats", get(step, "localFormats")),
                                ("answer", original_document(&get(step, "answer"))),
                                ("receive", get(&diagnostics, "receiveFormats")),
                                ("offeredEvents", get(&dtmf, "offeredEventPayloadIds")),
                                ("remoteEvents", get(&dtmf, "remoteEventFormats")),
                                ("localEvents", get(&dtmf, "localEventFormats")),
                                ("eventPayload", get(&dtmf, "negotiatedEventPayloadId")),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]),
    }
}

#[tokio::test]
async fn original_sdp_parser_and_audio_negotiation_differential() {
    use std::process::Stdio;
    use tokio::{
        io::AsyncWriteExt,
        process::Command,
        time::{Duration, timeout},
    };
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original managed SDP oracle unavailable; this comparison was not executed.");
        return;
    }
    let cases: Vec<_> = parser_cases().into_iter().chain(session_cases()).collect();
    let mut input = Vec::new();
    for case in &cases {
        input.extend(stringify(case, 1_000_000).unwrap().as_bytes());
        input.push(b'\n');
    }
    let mut child = Command::new("pwsh")
        .args(["-NoProfile", "-File", "tests/fixtures/phone-sdp-oracle.ps1"])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let (_, output) = timeout(Duration::from_secs(60), async {
        tokio::join!(
            async move {
                stdin.write_all(&input).await.unwrap();
                stdin.shutdown().await.unwrap();
            },
            child.wait_with_output()
        )
    })
    .await
    .expect("Original synthetic SDP fixture exceeded its owned deadline");
    let output = output.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<_> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| parse(line, 4_000_000).unwrap())
        .collect();
    assert_eq!(expected.len(), cases.len());
    let records: Vec<_> = cases
        .iter()
        .zip(&expected)
        .map(|(case, expected)| {
            IvyJson::object([
                ("input", case.clone()),
                ("original", expected.clone()),
                ("expected", project(case, expected)),
                ("actual", actual(case)),
            ])
        })
        .collect();
    if let Some(path) = std::env::var_os("IVY_PHONE_SDP_ORACLE_TRACE") {
        std::fs::write(
            path,
            stringify(&IvyJson::Array(records.clone()), 32_000_000).unwrap(),
        )
        .unwrap();
    }
    let failures: Vec<_> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| get(record, "actual") != get(record, "expected"))
        .map(|(index, record)| {
            format!(
                "Case {index}: {}",
                stringify(record, 4_000_000)
                    .unwrap()
                    .chars()
                    .take(1800)
                    .collect::<String>()
            )
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} mismatches of {}: {}",
        failures.len(),
        records.len(),
        failures.into_iter().take(12).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn bounds_connections_and_all_media_rejection_use_one_document() {
    let body = format!(
        "{PREFIX}a=sendonly\r\nm=audio 31000/2 RTP/AVP 111 0\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 useinbandfec=1\r\na=foo:one\r\na=foo:one\r\nm=video 32000 RTP/AVP 96\r\nc=IN IP6 ::1\r\na=rtpmap:96 VP8/90000\r\n"
    );
    let parsed = SdpDocument::parse(&body).unwrap();
    assert_eq!(parsed.media()[0].port_count(), 2);
    assert_eq!(parsed.media()[0].direction(), Some(SdpDirection::SendOnly));
    assert_eq!(
        parsed.media()[0]
            .formats()
            .iter()
            .map(SdpFormat::payload)
            .collect::<Vec<_>>(),
        [0, 111]
    );
    assert_eq!(
        parsed.media()[0].extra_attributes(),
        ["a=foo:one", "a=foo:one"]
    );
    assert_eq!(parsed.media_connection(0).unwrap().address(), "127.0.0.1");
    assert_eq!(parsed.media_connection(1).unwrap().address(), "::1");
    let rejected = SdpDocument::parse(&SdpDocument::rejected_answer(&body).unwrap()).unwrap();
    assert!(
        rejected
            .media()
            .iter()
            .all(|track| track.port() == 0 && track.direction() == Some(SdpDirection::Inactive))
    );
    assert_eq!(rejected.origin().session_id(), "0");
    assert_eq!(rejected.connection().unwrap().address(), "0.0.0.0");
    assert_eq!(rejected.media()[1].connection().unwrap().address(), "::1");
    assert!(!bounded_sdp(&"😀".repeat(32_769)));
    assert!(valid_sdp(
        Some(" Application/SDP ; charset=utf-8"),
        &"😀".repeat(32_768)
    ));
    assert!(SdpDocument::rejected_answer(&"x".repeat(65_537)).is_none());
    assert!(SdpDocument::rejected_answer(" \u{0085}\r\n").is_none());
}

#[test]
fn each_direction_selects_its_own_priority_from_the_common_set() {
    let mut left_preferences = vec![Codec::Opus, Codec::G722, Codec::Pcma, Codec::Pcmu];
    let mut left = AudioNegotiator::new(&left_preferences).unwrap();
    let mut right =
        AudioNegotiator::new(&[Codec::G722, Codec::Pcma, Codec::Pcmu, Codec::Opus]).unwrap();
    left_preferences.reverse(); // admitted policy is an owned snapshot
    let a = SdpIdentity::new("127.0.0.1:31000".parse().unwrap(), 1, 0, 7, "left").unwrap();
    let b = SdpIdentity::new("127.0.0.1:31002".parse().unwrap(), 2, 0, 8, "right").unwrap();
    let offer = left.create_offer(&a);
    right
        .set_remote_description(SdpType::Offer, &offer)
        .unwrap();
    let answer = right.create_answer(&b).unwrap();
    left.set_remote_description(SdpType::Answer, &answer)
        .unwrap();
    assert_eq!(left.state().unwrap().sender().codec(), Codec::Opus);
    assert_eq!(right.state().unwrap().sender().codec(), Codec::G722);
    assert_eq!(left.state().unwrap().receive_formats().len(), 4);
    assert_eq!(right.state().unwrap().receive_formats().len(), 4);
    assert_eq!(offer.media()[0].port(), 31000);
    assert_eq!(answer.media()[0].port(), 31002);
    assert!(offer.to_sdp().contains("a=ssrc:7 cname:left\r\n"));
    assert!(answer.to_sdp().contains("a=ssrc:8 cname:right\r\n"));
}

#[test]
fn evs_policy_composes_channel_maps_packet_time_and_offer_answer_constraints() {
    let mut session = AudioNegotiator::new(&[Codec::Evs, Codec::Pcma]).unwrap();
    let identity = SdpIdentity::new("[::1]:31002".parse().unwrap(), 1, 0, 7, "fixture").unwrap();
    let offer = session.create_offer(&identity);
    assert_eq!(offer.media()[0].formats()[0].rtpmap(), "EVS/16000/1");
    assert_eq!(offer.media()[0].formats()[1].rtpmap(), "EVS/16000/2");
    assert_eq!(
        offer.media()[0].extra_attributes(),
        ["a=ptime:20", "a=maxptime:120"]
    );
    let remote = SdpDocument::parse(&format!("{PREFIX}m=audio 31000 RTP/AVP 121 120 8 110 112\r\na=rtpmap:121 evs/16000/2\r\na=fmtp:121 br=16.4-128;bw=nb-fb;channels=2;hf-only=1\r\na=rtpmap:120 EVS/16000\r\na=fmtp:120 br-send=7.2-24.4;br-recv=16.4-128;bw-send=nb;bw-recv=fb\r\na=rtpmap:110 telephone-event/8000\r\na=rtpmap:112 telephone-event/16000\r\n")).unwrap();
    session
        .set_remote_description(SdpType::Offer, &remote)
        .unwrap();
    let state = session.state().unwrap();
    assert_eq!(
        (
            state.sender().payload(),
            state.sender().channels(),
            state.sender().pcm_rate(),
            state.sender().rtp_rate()
        ),
        (120, 1, 48000, 16000)
    );
    assert_eq!(state.offered_event_payloads(), [110, 112]);
    assert_eq!(session.negotiated_event_payload(), 112);
    let answer = session.create_answer(&identity).unwrap();
    assert_eq!(answer.media()[0].formats()[0].rtpmap(), "evs/16000/2");
    assert_eq!(answer.media()[0].formats()[1].rtpmap(), "EVS/16000");
    assert!(
        answer.media()[0].formats()[1]
            .parameters()
            .unwrap()
            .contains("br-send=128;br-recv=7.2-24.4")
    );
    assert_eq!(
        answer.media()[0].extra_attributes(),
        ["a=ptime:20", "a=maxptime:120"]
    );
    let invalid = SdpDocument::parse(&format!(
        "{PREFIX}m=audio 31000 RTP/AVP 120\r\na=rtpmap:120 EVS/48000/1\r\n"
    ))
    .unwrap();
    assert_eq!(
        session.set_remote_description(SdpType::Offer, &invalid),
        Err(NegotiationError::AudioIncompatible)
    );
    assert_eq!(session.state().unwrap().sender().payload(), 120);
    let fallback = SdpDocument::parse(&format!("{PREFIX}m=audio 31000 RTP/AVP 120 8\r\na=rtpmap:120 EVS/16000/2\r\na=fmtp:120 channels=2\r\n")).unwrap();
    session
        .set_remote_description(SdpType::Offer, &fallback)
        .unwrap();
    assert_eq!(session.state().unwrap().sender().codec(), Codec::Pcma);
    assert!(
        session.create_answer(&identity).unwrap().media()[0]
            .extra_attributes()
            .is_empty()
    );
    assert!(
        SdpIdentity::new(
            "127.0.0.1:31000".parse().unwrap(),
            1,
            0,
            1,
            "bad\r\na=sendonly"
        )
        .is_err()
    );
}

#[test]
fn normalization_does_not_reapply_the_external_size_limit_to_static_expansion() {
    let track =
        "m=video 32000 RTP/AVP 0 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 24 26 28 31 32 33 34\r\n";
    let mut body = format!("{PREFIX}m=audio 31000 RTP/AVP 0\r\n");
    body.push_str(&track.repeat(600));
    assert!(bounded_sdp(&body));
    let document = SdpDocument::parse(&body).unwrap();
    assert!(document.to_sdp().encode_utf16().count() > 65_536);
    let mut session = AudioNegotiator::new(&[Codec::Pcmu]).unwrap();
    session
        .set_remote_description(SdpType::Offer, &document)
        .unwrap();
    assert_eq!(session.state().unwrap().sender().codec(), Codec::Pcmu);
    let mut state = 0x1234_5678_u32;
    for length in 0..4096 {
        let body: String = (0..length % 64)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                char::from_u32(state % 0x110000).unwrap_or('\u{fffd}')
            })
            .collect();
        if let Ok(document) = SdpDocument::parse(&body) {
            let _ = document.to_sdp();
            let _ = SdpDocument::rejected_answer(&body);
        }
    }
}
