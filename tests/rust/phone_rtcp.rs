use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_phone_bridge::media::{
    MediaError, RtcpCompound, RtcpReportBlock, RtcpReportBuilder, RtcpSenderInfo,
    RtcpTransmitCounters, RtpSendClock,
};
use std::{
    io::Write,
    process::{Command, Stdio},
    time::Duration,
};

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    })
}
fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}
fn n(value: impl Into<f64>) -> IvyJson {
    IvyJson::Number(value.into())
}
fn block(value: RtcpReportBlock, original_loss: i32) -> IvyJson {
    IvyJson::Array(vec![
        n(value.source),
        n(value.fraction_lost),
        n(original_loss),
        n(value.highest_sequence),
        n(value.jitter),
        n(value.last_sender_report),
        n(value.delay_since_sender_report),
    ])
}
fn original(cases: &[IvyJson]) -> Vec<IvyJson> {
    let mut child = Command::new("pwsh")
        .args([
            "-NoProfile",
            "-File",
            "tests/fixtures/phone-rtcp-oracle.ps1",
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
            stringify(&IvyJson::Array(cases.to_vec()), 4_000_000)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(std::str::from_utf8(&output.stdout).unwrap(), 4_000_000)
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec()
}

#[test]
fn valid_rtcp_sr_rr_sdes_bye_bytes_match_the_sealed_original_encoder() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original RTCP encoder unavailable; differential not executed");
        return;
    }
    let mut cases = Vec::new();
    let mut encoded = Vec::new();
    for source in [0, 0x12345678, u32::MAX] {
        for count in [0, 1, 8, 31] {
            for loss in [i32::MIN, -8388608, -1, 0, 1, 8388607, i32::MAX] {
                let blocks: Vec<_> = (0..count)
                    .map(|i| {
                        RtcpReportBlock::new(
                            i as u32,
                            (i * 7) as u8,
                            loss,
                            u32::MAX - (i as u32),
                            (i * 480) as u32,
                            0x80000000,
                            u32::MAX,
                        )
                    })
                    .collect();
                let json = IvyJson::Array(blocks.iter().copied().map(|v| block(v, loss)).collect());
                let sender = RtcpSenderInfo {
                    ntp_timestamp: 0x8123456789abcdef,
                    rtp_timestamp: u32::MAX,
                    packet_count: u32::MAX,
                    octet_count: u32::MAX - 1,
                };
                for kind in ["sr", "rr"] {
                    let mut value = vec![
                        ("op", "write".into()),
                        ("kind", kind.into()),
                        ("source", n(source)),
                        ("blocks", json.clone()),
                    ];
                    if kind == "sr" {
                        value.extend([
                            ("ntp", "8123456789abcdef".into()),
                            ("rtp", n(sender.rtp_timestamp)),
                            ("packets", n(sender.packet_count)),
                            ("octets", n(sender.octet_count)),
                        ]);
                    }
                    cases.push(IvyJson::object(value));
                    encoded.push(if kind == "sr" {
                        RtcpReportBuilder::sender_packet(source, sender, &blocks).unwrap()
                    } else {
                        RtcpReportBuilder::receiver_packet(source, &blocks).unwrap()
                    });
                }
            }
        }
    }
    for text in [
        "fixture".into(),
        "x".repeat(255),
        "x".repeat(256),
        "é".repeat(130),
        "😀".repeat(70),
        "a\0b".into(),
        "日本語".repeat(50),
        format!("{}😀x", "x".repeat(251)),
        format!("{}😀x", "x".repeat(252)),
        format!("{}😀x", "x".repeat(253)),
        format!("{}😀x", "x".repeat(254)),
        format!("{}😀x", "x".repeat(255)),
    ] {
        let builder = RtcpReportBuilder::new(9, &text).unwrap();
        for kind in ["sdes", "bye"] {
            cases.push(IvyJson::object([
                ("op", "write".into()),
                ("kind", kind.into()),
                ("source", n(9)),
                ("text", text.clone().into()),
            ]));
            encoded.push(if kind == "sdes" {
                builder.sdes_packet()
            } else {
                builder.bye_packet(&text)
            });
        }
    }
    cases.push(IvyJson::object([
        ("op", "write".into()),
        ("kind", "bye".into()),
        ("source", n(9)),
        ("text", "".into()),
    ]));
    encoded.push(RtcpReportBuilder::new(9, "fixture").unwrap().bye_packet(""));
    let results = original(&cases);
    assert_eq!(results.len(), encoded.len());
    if let Some(path) = std::env::var_os("IVY_PHONE_RTCP_WIRE_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("cases", n(cases.len() as u32)),
                    ("inputs", IvyJson::Array(cases.clone())),
                    ("original", IvyJson::Array(results.clone())),
                    (
                        "rust",
                        IvyJson::Array(encoded.iter().map(|b| hex(b).into()).collect()),
                    ),
                ]),
                4_000_000,
            )
            .unwrap(),
        )
        .unwrap();
    }
    for (index, (expected, bytes)) in results.iter().zip(&encoded).enumerate() {
        assert_eq!(
            expected.get("ok").unwrap().as_bool(),
            Some(true),
            "original rejected case{index}: {expected:?}"
        );
        assert_eq!(
            expected.get("hex").unwrap().as_str().unwrap(),
            hex(bytes),
            "RTCP wire case{index}"
        );
    }
}

#[test]
fn rtcp_compound_bounds_transmit_wrap_and_unsigned_loss_projection_remain_explicit() {
    let builder = RtcpReportBuilder::new(9, "fixture").unwrap();
    assert!(RtcpReportBuilder::new(9, "").is_err());
    let sample = RtcpReportBlock::new(17, 255, -1, u32::MAX, u32::MAX, u32::MAX, u32::MAX);
    assert_eq!(sample.packets_lost(), 16777215); // Original diagnostic quirk is retained.
    let blocks = vec![sample; 32];
    assert!(builder.receiver(&blocks).is_err());
    let sender = RtcpSenderInfo {
        ntp_timestamp: u64::MAX,
        rtp_timestamp: u32::MAX,
        packet_count: u32::MAX,
        octet_count: u32::MAX,
    };
    assert!(builder.sender(sender, &blocks).is_err());
    assert_eq!(
        builder.receiver(&[]).unwrap(),
        [
            RtcpReportBuilder::receiver_packet(9, &[]).unwrap(),
            builder.sdes_packet()
        ]
        .concat()
    );
    assert_eq!(
        builder.sender(sender, &[sample]).unwrap(),
        [
            RtcpReportBuilder::sender_packet(9, sender, &[sample]).unwrap(),
            builder.sdes_packet()
        ]
        .concat()
    );
    let mut counters = RtcpTransmitCounters {
        packets: u32::MAX,
        octets: u32::MAX - 2,
        last_sequence: u16::MAX,
        last_timestamp: u32::MAX,
    };
    counters.record(0, 0, 160).unwrap();
    assert_eq!(counters.packets, 0);
    assert_eq!(counters.octets, 157);
    assert_eq!(
        counters.sender_info(123),
        RtcpSenderInfo {
            ntp_timestamp: 123,
            rtp_timestamp: 0,
            packet_count: 0,
            octet_count: 157
        }
    );
    let before = counters;
    assert!(counters.record(1, 160, 8193).is_err());
    assert_eq!(counters, before);
}

#[test]
fn send_clock_preserves_after_work_catchup_and_suspension_boundary_without_an_idle_spin() {
    let ms = Duration::from_millis;
    let mut clock = RtpSendClock::new(Duration::ZERO).unwrap();
    assert_eq!(clock.next(), ms(20));
    assert!(!clock.due(ms(19)).unwrap());
    assert!(clock.due(ms(20)).unwrap());
    // Completion, not initial wake, decides whether a long encode suspended.
    clock.advance(ms(80)).unwrap();
    assert_eq!(clock.next(), ms(40)); // Exactly40ms behind next: retain catch-up.
    for next in [60, 80, 100] {
        assert!(clock.due(ms(80)).unwrap());
        clock.advance(ms(80)).unwrap();
        assert_eq!(clock.next(), ms(next));
    }
    assert!(!clock.due(ms(80)).unwrap());
    let mut clock = RtpSendClock::new(Duration::ZERO).unwrap();
    assert!(clock.due(ms(20)).unwrap());
    clock.advance(ms(81)).unwrap();
    assert_eq!(clock.next(), ms(101));
    assert!(!clock.due(ms(81)).unwrap());
    for tick in 1..=5000_u64 {
        let now = ms(101 + (tick - 1) * 20);
        assert!(clock.due(now).unwrap());
        clock.advance(now).unwrap(); // Even without a configured codec.
        assert!(!clock.due(now).unwrap());
    }
    assert_eq!(clock.due(ms(1)), Err(MediaError::ClockWentBackwards));
    assert!(matches!(
        RtpSendClock::new(Duration::MAX),
        Err(MediaError::ClockOverflow)
    ));
    let mut clock = RtpSendClock::new(Duration::ZERO).unwrap();
    assert_eq!(clock.advance(ms(19)), Err(MediaError::InvalidPacket));
    assert_eq!(clock.next(), ms(20));
    clock.advance(ms(10000)).unwrap();
    assert_eq!(clock.next(), ms(10020));
}

fn with_length(mut bytes: Vec<u8>) -> Vec<u8> {
    let words = u16::try_from(bytes.len() / 4 - 1).unwrap();
    bytes[2..4].copy_from_slice(&words.to_be_bytes());
    bytes
}
fn sample(source: u32) -> RtcpReportBlock {
    RtcpReportBlock::new(source, 17, -1, 1000, 77, 0x81234567, 100)
}
fn sender() -> RtcpSenderInfo {
    RtcpSenderInfo {
        ntp_timestamp: 0x8123456789abcdef,
        rtp_timestamp: 777,
        packet_count: 123,
        octet_count: 987,
    }
}
pub(crate) fn invalid_compounds() -> Vec<(String, Vec<u8>)> {
    let builder = RtcpReportBuilder::new(9, "fixture").unwrap();
    let rr = RtcpReportBuilder::receiver_packet(9, &[sample(900)]).unwrap();
    let mut cases = Vec::new();
    for version in [0, 1, 3] {
        let mut bytes = rr.clone();
        bytes[0] = (bytes[0] & 63) | (version << 6);
        cases.push((format!("version-{version}"), bytes));
    }
    for count in [1, 31] {
        for kind in [200, 201] {
            let mut bytes = if kind == 200 {
                RtcpReportBuilder::sender_packet(9, sender(), &[]).unwrap()
            } else {
                RtcpReportBuilder::receiver_packet(9, &[]).unwrap()
            };
            bytes[0] |= count;
            cases.push((format!("missing-{kind}-blocks-{count}"), bytes));
        }
    }
    for suffix in [
        vec![0],
        vec![0, 0],
        vec![0, 0, 0],
        vec![0x80, 201, 0, 1],
        vec![0x81, 202, 0, 0],
        vec![0x80, 207, 0xff, 0xff],
    ] {
        cases.push((
            format!("valid-prefix-suffix-{}", hex(&suffix)),
            [rr.clone(), suffix].concat(),
        ));
    }
    for padding in [0, 1, 2, 3, 5, 8, 255] {
        let mut bytes = rr.clone();
        bytes[0] |= 32;
        bytes.extend_from_slice(&[0, 0, 0, padding]);
        cases.push((format!("padding-{padding}"), with_length(bytes)));
    }
    let mut early = rr.clone();
    early[0] |= 32;
    early.extend_from_slice(&[0, 0, 0, 4]);
    early = with_length(early);
    cases.push(("early-padding".into(), [early, rr.clone()].concat()));
    let mut sdes = builder.sdes_packet();
    sdes[0] = 0x82;
    cases.push(("sdes-missing-chunk".into(), sdes));
    let mut sdes = builder.sdes_packet();
    sdes[9] = 255;
    cases.push(("sdes-item-overrun".into(), sdes));
    let mut sdes = builder.sdes_packet();
    *sdes.last_mut().unwrap() = 1;
    cases.push(("sdes-nonzero-alignment".into(), sdes));
    cases.push((
        "sdes-private-prefix-overrun".into(),
        vec![0x81, 202, 0, 3, 0, 0, 0, 9, 8, 2, 2, 1, 0, 0, 0, 0],
    ));
    let mut bye = builder.bye_packet("fixture");
    bye[8] = 255;
    cases.push(("bye-reason-overrun".into(), bye));
    let mut bye = builder.bye_packet("");
    bye[0] = 0x82;
    cases.push(("bye-missing-source".into(), bye));
    let mut bytes = rr;
    bytes[2..4].copy_from_slice(&u16::MAX.to_be_bytes());
    cases.push(("maximum-declared-length-short-data".into(), bytes));
    cases
}

#[test]
fn rtcp_validation_checks_all_boundaries_before_any_partial_result_is_returned() {
    let builder = RtcpReportBuilder::new(9, "fixture").unwrap();
    let mut truncations = 0;
    for count in [0, 1, 31] {
        let blocks = vec![sample(900); count];
        for bytes in [
            RtcpReportBuilder::sender_packet(9, sender(), &blocks).unwrap(),
            RtcpReportBuilder::receiver_packet(9, &blocks).unwrap(),
        ] {
            for length in 0..bytes.len() {
                assert!(
                    RtcpCompound::parse(&bytes[..length]).is_err(),
                    "cut{length}/{length_all}",
                    length_all = bytes.len()
                );
                truncations += 1;
            }
            let mut extended = bytes.clone();
            extended.extend_from_slice(&[7; 12]);
            extended = with_length(extended);
            assert!(RtcpCompound::parse(&extended).is_ok());
            for padding in [4, 8, 252] {
                let mut padded = bytes.clone();
                padded[0] |= 32;
                padded.extend(std::iter::repeat_n(0, padding));
                *padded.last_mut().unwrap() = padding as u8;
                assert!(RtcpCompound::parse(&with_length(padded)).is_ok());
            }
        }
    }
    for bytes in [
        builder.sdes_packet(),
        builder.bye_packet("fixture"),
        builder.bye_packet(&"x".repeat(255)),
    ] {
        for length in 0..bytes.len() {
            assert!(RtcpCompound::parse(&bytes[..length]).is_err());
            truncations += 1;
        }
        assert!(RtcpCompound::parse(&bytes).is_ok());
    }
    assert!(truncations > 1800);
    for (name, bytes) in invalid_compounds() {
        assert!(RtcpCompound::parse(&bytes).is_err(), "{name}");
    }
    // Zero counts, optional/unknown SDES items and multiple chunks are valid.
    assert!(RtcpCompound::parse(&[0x80, 202, 0, 0]).is_ok());
    assert!(RtcpCompound::parse(&[0x80, 203, 0, 0]).is_ok());
    let multi = [
        0x82, 202, 0, 5, 0, 0, 0, 9, 1, 1, b'a', 0, 0, 0, 0, 10, 7, 1, b'b', 8, 1, 0, 0, 0,
    ];
    let parsed = RtcpCompound::parse(&multi).unwrap();
    assert_eq!(parsed.cname, Some(&b"a"[..]));
    let bye = [0x82, 203, 0, 3, 0, 0, 0, 9, 0, 0, 0, 10, 0, 0, 0, 0];
    let parsed = RtcpCompound::parse(&bye).unwrap();
    assert_eq!(parsed.bye.unwrap().sources().collect::<Vec<_>>(), [9, 10]);
    assert_eq!(parsed.bye.unwrap().reason, Some(&b""[..]));
    // The maximum word count is bounded by supplied bytes, never a sentinel.
    let mut maximum = vec![0; 262144];
    maximum[..4].copy_from_slice(&[0x80, 207, 255, 255]);
    assert_eq!(RtcpCompound::parse(&maximum).unwrap().packet_count, 1);
    let smallest = [0x80, 207, 0, 0];
    let repeated = smallest.repeat(16384);
    assert_eq!(RtcpCompound::parse(&repeated).unwrap().packet_count, 16384);
    // Untrusted buffers may never panic or make non-progressing loops.
    let mut seed = 0x9e3779b9_u32;
    for length in 0..4096 {
        let bytes: Vec<_> = (0..length)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect();
        let _ = RtcpCompound::parse(&bytes);
    }
}

fn parsed_block(value: RtcpReportBlock) -> IvyJson {
    IvyJson::object([
        ("source", n(value.source)),
        ("fraction", n(value.fraction_lost)),
        ("lost", n(value.packets_lost())),
        ("highest", n(value.highest_sequence)),
        ("jitter", n(value.jitter)),
        ("last", n(value.last_sender_report)),
        ("delay", n(value.delay_since_sender_report)),
    ])
}
fn parsed_fields(value: RtcpCompound<'_>) -> IvyJson {
    let sender = value
        .sender
        .map(|r| {
            IvyJson::object([
                ("source", n(r.source)),
                ("ntp", format!("{:016x}", r.info.ntp_timestamp).into()),
                ("rtp", n(r.info.rtp_timestamp)),
                ("packets", n(r.info.packet_count)),
                ("octets", n(r.info.octet_count)),
                (
                    "blocks",
                    IvyJson::Array(r.blocks.iter().map(parsed_block).collect()),
                ),
            ])
        })
        .unwrap_or(IvyJson::Null);
    let receiver = value
        .receiver
        .map(|r| {
            IvyJson::object([
                ("source", n(r.source)),
                (
                    "blocks",
                    IvyJson::Array(r.blocks.iter().map(parsed_block).collect()),
                ),
            ])
        })
        .unwrap_or(IvyJson::Null);
    IvyJson::object([
        ("sender", sender),
        ("receiver", receiver),
        (
            "cname",
            value
                .cname
                .map(|b| String::from_utf8_lossy(b).into_owned().into())
                .unwrap_or(IvyJson::Null),
        ),
        (
            "bye",
            value
                .bye
                .and_then(|b| b.reason)
                .map(|b| String::from_utf8_lossy(b).into_owned().into())
                .unwrap_or(IvyJson::Null),
        ),
    ])
}
fn projected_fields(value: RtcpCompound<'_>) -> IvyJson {
    IvyJson::Array(
        value
            .reception_reports()
            .map(|b| {
                b.iter()
                    .take(8)
                    .map(|b| {
                        IvyJson::object([
                            ("ssrc", n(b.source)),
                            ("fractionLost256", n(b.fraction_lost)),
                            ("packetsLost", n(b.packets_lost())),
                            ("extendedHighestSequence", n(b.highest_sequence)),
                            ("jitterTimestampUnits", n(b.jitter)),
                        ])
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

#[test]
fn original_parser_and_actual_application_event_traces_keep_unknown_exceptions_separate() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original RTCP application unavailable; differential not executed");
        return;
    }
    let a = RtcpReportBuilder::receiver_packet(9, &[sample(900)]).unwrap();
    let b = RtcpReportBuilder::receiver_packet(9, &[sample(1000)]).unwrap();
    let c = RtcpReportBuilder::sender_packet(9, sender(), &[sample(1100)]).unwrap();
    let d = RtcpReportBuilder::sender_packet(9, sender(), &[sample(1200)]).unwrap();
    let e = RtcpReportBuilder::sender_packet(9, sender(), &[]).unwrap();
    let many = RtcpReportBuilder::receiver_packet(
        9,
        &(0..12).map(|i| sample(1400 + i)).collect::<Vec<_>>(),
    )
    .unwrap();
    let builder = RtcpReportBuilder::new(9, "fixture").unwrap();
    let sdes = builder.sdes_packet();
    let bye = builder.bye_packet("fixture");
    let unknown = unhex("80cf000100000009");
    let cases: Vec<(&str, Vec<u8>, bool, bool)> = vec![
        ("rr", a.clone(), true, false),
        ("sr", c.clone(), true, false),
        ("rr-sdes", [a.clone(), sdes.clone()].concat(), true, false),
        ("rr-rr", [a.clone(), b.clone()].concat(), true, false),
        ("sr-sr", [c.clone(), d].concat(), true, false),
        ("sr-rr", [c.clone(), b.clone()].concat(), true, false),
        ("rr-sr", [b.clone(), c].concat(), true, false),
        ("empty-sr-rr", [e, b.clone()].concat(), true, false),
        ("cap12", many, true, false),
        (
            "unknown-first",
            [unknown.clone(), a.clone()].concat(),
            false,
            true,
        ),
        (
            "unknown-middle",
            [a.clone(), unknown.clone(), sdes.clone()].concat(),
            true,
            true,
        ),
        (
            "unknown-last",
            [a.clone(), unknown.clone()].concat(),
            true,
            true,
        ),
        ("unknown-only", unknown.clone(), false, true),
        (
            "known-unknown-known",
            [a.clone(), unknown, b].concat(),
            true,
            true,
        ),
        ("sdes-only", sdes, false, false),
        ("bye-only", bye, false, false),
        ("zero-rr", unhex("80c9000100000009"), false, false),
    ];
    let mut inputs: Vec<_> = cases
        .iter()
        .map(|(_, b, _, _)| IvyJson::object([("op", "parse".into()), ("hex", hex(b).into())]))
        .collect();
    inputs.extend(cases.iter().map(|(_, b, _, _)| {
        IvyJson::object([
            ("op", "receive".into()),
            (
                "datagrams",
                IvyJson::Array(vec![hex(b).into(), hex(&a).into()]),
            ),
        ])
    }));
    let results = original(&inputs);
    let mut corrected = Vec::new();
    for (i, (name, bytes, event, exception)) in cases.iter().enumerate() {
        let parsed = RtcpCompound::parse(bytes).unwrap();
        let fields = parsed_fields(parsed);
        let original_parse = &results[i];
        assert_eq!(
            original_parse.get("ok").unwrap().as_bool(),
            Some(true),
            "{name}"
        );
        if !exception {
            for key in ["sender", "receiver", "cname", "bye"] {
                assert_eq!(fields.get(key), original_parse.get(key), "{name} {key}");
            }
        } else {
            assert_eq!(
                original_parse.get("consumed").unwrap().as_f64(),
                Some(i32::MAX as f64),
                "{name}"
            );
        }
        let application = &results[cases.len() + i];
        assert_eq!(
            application.get("ok").unwrap().as_bool(),
            Some(true),
            "{name}: {application:?}"
        );
        let snapshots = application.get("snapshots").unwrap().as_array().unwrap();
        for (index, snapshot) in snapshots.iter().enumerate() {
            let expected = usize::from(*event) + index;
            assert_eq!(
                snapshot.get("events").unwrap().as_f64(),
                Some(expected as f64),
                "{name} event{index}"
            );
            let rtcp = snapshot.get("rtcp").unwrap();
            assert_eq!(
                rtcp.get("reportsReceived").unwrap().as_f64(),
                Some(expected as f64),
                "{name} count{index}"
            );
            assert_eq!(rtcp.get("reportsSent").unwrap().as_f64(), Some(0.0));
            assert_eq!(
                matches!(rtcp.get("lastReportAt"), Some(IvyJson::Null)),
                expected == 0
            );
            if index == 1 {
                assert_eq!(
                    rtcp.get("peerReceptionReports"),
                    Some(&projected_fields(RtcpCompound::parse(&a).unwrap()))
                );
            } else if *event && !exception {
                assert_eq!(
                    rtcp.get("peerReceptionReports"),
                    Some(&projected_fields(parsed)),
                    "{name} projection"
                );
            }
        }
        corrected.push(IvyJson::object([
            ("name", (*name).into()),
            ("boundedPackets", n(parsed.packet_count as u32)),
            ("fields", fields),
            ("projection", projected_fields(parsed)),
            ("unknownProgressException", (*exception).into()),
            ("reportEventOwnerIntegrated", false.into()),
        ]));
    }
    // A known tail is newly reachable; the unsupported packet itself is valid.
    assert_eq!(
        RtcpCompound::parse(&cases[9].1)
            .unwrap()
            .receiver
            .unwrap()
            .source,
        9
    );
    assert_eq!(
        RtcpCompound::parse(&cases[13].1)
            .unwrap()
            .receiver
            .unwrap()
            .blocks
            .iter()
            .next()
            .unwrap()
            .source,
        1000
    );
    if let Some(path) = std::env::var_os("IVY_PHONE_RTCP_RECEIVE_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("inputs", IvyJson::Array(inputs)),
                    ("original", IvyJson::Array(results)),
                    ("corrected", IvyJson::Array(corrected)),
                ]),
                4_000_000,
            )
            .unwrap(),
        )
        .unwrap();
    }
}
