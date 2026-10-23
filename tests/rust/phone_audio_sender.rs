use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_phone_bridge::media::{RtcpTransmitCounters, RtpAudioFrames, RtpTransportError};
fn n(v: u32) -> IvyJson {
    IvyJson::Number(f64::from(v))
}
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        write!(out, "{b:02x}").unwrap();
        out
    })
}
fn bytes(value: &IvyJson) -> Vec<u8> {
    let value = value.as_str().unwrap();
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn packetization_preserves_public_wire_wrap_split_empty_and_suppressed_frames() {
    let mut inputs = Vec::new();
    let sizes = [
        0, 1, 1199, 1200, 1201, 2399, 2400, 2401, 3600, 8192, 160, 160, 160, 160,
    ];
    let durations = [0, 1, 3, 160, 320, 960, u32::MAX];
    for (sequence, timestamp) in [
        (0_u16, 0_u32),
        (u16::MAX, u32::MAX - 80),
        (32768, 0x7fffffff),
        (u16::MAX, u32::MAX),
    ] {
        let actions = sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| {
                let data: Vec<u8> = (0..size).map(|j| ((j * 37 + i * 13) & 255) as u8).collect();
                IvyJson::object([
                    ("hex", hex(&data).into()),
                    ("duration", n(durations[i % durations.len()])),
                    ("payload", n((i % 128) as u32)),
                    ("destination", (i != 10).into()),
                    ("receiveOnly", (i == 11).into()),
                ])
            })
            .collect();
        inputs.push(IvyJson::object([
            ("op", "frames".into()),
            ("sequence", n(u32::from(sequence))),
            ("timestamp", n(timestamp)),
            ("actions", IvyJson::Array(actions)),
        ]));
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-audio-frame-cases.json");
    let old = parse(&std::fs::read_to_string(path).unwrap(), 1024 * 1024).unwrap();
    let outputs = old.as_array().unwrap();
    assert_eq!(outputs.len(), inputs.len());
    let mut actual = Vec::new();
    let mut total_packets = 0;
    for (input, output) in inputs.iter().zip(outputs) {
        assert_eq!(
            output.get("ok").unwrap().as_bool(),
            Some(true),
            "{output:?}"
        );
        let mut sender = RtpAudioFrames::new(
            17,
            input.get("sequence").unwrap().as_f64().unwrap() as u16,
            input.get("timestamp").unwrap().as_f64().unwrap() as u32,
        );
        let mut counters = RtcpTransmitCounters::default();
        let mut trace = Vec::new();
        for (action, row) in input
            .get("actions")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .zip(output.get("trace").unwrap().as_array().unwrap())
        {
            let payload = bytes(action.get("hex").unwrap());
            let mut packets = Vec::new();
            let destination = action.get("destination").unwrap().as_bool().unwrap();
            let mut error = IvyJson::Null;
            if !action.get("receiveOnly").unwrap().as_bool().unwrap() {
                let result = sender.send(
                    action.get("duration").unwrap().as_f64().unwrap() as u32,
                    action.get("payload").unwrap().as_f64().unwrap() as u8,
                    &payload,
                    |header, part| {
                        if !destination {
                            return Err(RtpTransportError::PeerUnavailable);
                        }
                        counters
                            .record(header.sequence, header.timestamp, part.len())
                            .unwrap();
                        if destination {
                            let mut wire = vec![0; 12 + part.len()];
                            header.write(part, &mut wire).unwrap();
                            packets.push(IvyJson::object([
                                ("header", hex(&wire[..12]).into()),
                                ("payloadBytes", n(part.len() as u32)),
                                ("payloadSha256", ivy_contracts::hash::digest(part).into()),
                            ]));
                        }
                        Ok(())
                    },
                );
                if destination {
                    result.unwrap();
                } else {
                    assert_eq!(result, Err(RtpTransportError::PeerUnavailable));
                    error = "peer_unavailable".into();
                }
            }
            total_packets += packets.len();
            let snapshot = IvyJson::object([
                ("sequence", n(u32::from(sender.sequence()))),
                ("timestamp", n(sender.timestamp())),
                ("rtcpPackets", n(counters.packets)),
                ("rtcpOctets", n(counters.octets)),
                ("error", error),
                ("packets", IvyJson::Array(packets)),
            ]);
            assert_eq!(&snapshot, row, "action={action:?}");
            trace.push(snapshot);
        }
        actual.push(IvyJson::Array(trace));
    }
    let mut sender = RtpAudioFrames::new(17, 65535, u32::MAX);
    assert_eq!(
        sender.send(160, 128, &[1], |_, _| panic!("Invalid payload publication")),
        Err(RtpTransportError::InvalidPacket)
    );
    assert_eq!(
        sender.send(160, 0, &[0; 8193], |_, _| panic!("Oversized publication")),
        Err(RtpTransportError::InvalidPacket)
    );
    assert_eq!((sender.sequence(), sender.timestamp()), (65535, u32::MAX));
    if let Some(path) = std::env::var_os("IVY_PHONE_AUDIO_FRAME_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("inputs", IvyJson::Array(inputs)),
                    ("expected", old),
                    ("rust", IvyJson::Array(actual)),
                    ("packets", n(total_packets as u32)),
                ]),
                4_000_000,
            )
            .unwrap(),
        )
        .unwrap();
    }
}
