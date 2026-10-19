use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_phone_bridge::media::{
    MediaError, RtcpReportBlock, RtcpReportBuilder, RtcpSenderInfo, RtcpTransmitCounters,
    RtpSendClock,
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
    if let Some(path) = std::env::var_os("IVY_PHONE_RTCP_WIRE_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("cases", n(cases.len() as u32)),
                    ("inputs", IvyJson::Array(cases)),
                    ("original", IvyJson::Array(results)),
                ]),
                4_000_000,
            )
            .unwrap(),
        )
        .unwrap();
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
