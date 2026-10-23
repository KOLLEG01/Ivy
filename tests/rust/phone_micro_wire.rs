use super::*;
use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02X}").unwrap();
    }
    result
}
fn number(n: impl Into<f64>) -> IvyJson {
    IvyJson::Number(n.into())
}
fn success(pairs: impl IntoIterator<Item = (&'static str, IvyJson)>) -> IvyJson {
    IvyJson::object(std::iter::once(("ok", IvyJson::Bool(true))).chain(pairs))
}
fn failure(error: impl ToString) -> IvyJson {
    IvyJson::object([
        ("ok", IvyJson::Bool(false)),
        ("error", error.to_string().into()),
    ])
}
fn inventory(port: i32, hub: u16) -> String {
    format!(
        "Imported USB devices\n====================\nPort {hub:02}: device in use at high-speed\n         Codex Micro\n           -> usbip://127.0.0.1:{port}/1-7\n           -> remote bus/dev: 001/007\n           -> serial: IvyPhoneMicro\n           -> mode: low-latency\n"
    )
}

#[test]
fn published_descriptors_reports_controls_and_inventory_are_preserved() {
    let frame_inputs: Vec<Vec<u8>> = (0..=PAYLOAD_SIZE * 3 + 1)
        .chain([
            1024,
            MAXIMUM_MESSAGE_BYTES - 1,
            MAXIMUM_MESSAGE_BYTES,
            MAXIMUM_MESSAGE_BYTES + 1,
        ])
        .map(|len| (0..len).map(|i| (i.wrapping_mul(73) & 255) as u8).collect())
        .collect();
    let expected_frames = IvyJson::Array(
        frame_inputs
            .iter()
            .map(|b| match frame(b) {
                Ok(reports) => success([(
                    "reports",
                    IvyJson::Array(reports.iter().map(|b| IvyJson::from(hex(b))).collect()),
                )]),
                Err(e) => failure(e),
            })
            .collect(),
    );
    let original = inventory(3241, 3);
    let mut inventory_inputs = vec![
        (None, 3241),
        (Some(String::new()), 3241),
        (Some("unexpected output".into()), 3241),
        (Some(original.clone()), 3241),
        (Some(original.clone()), 3240),
        (Some(inventory(-1, 3)), -1),
        (Some(inventory(i32::MAX, 3)), i32::MAX),
        (Some(inventory(i32::MIN, 3)), i32::MIN),
    ];
    for hub in [0, 1, 9, 10, 99, 100, 255, 256, 999, 1000] {
        inventory_inputs.push((Some(inventory(3241, hub)), 3241));
        inventory_inputs.push((
            Some(format!("{}{}", inventory(3240, hub), inventory(3241, 3))),
            3241,
        ));
    }
    for (from, to) in [
        ("IvyPhoneMicro", "foreign"),
        ("IvyPhoneMicro", "IvyPhoneMicro-extra"),
        ("001/007", "001/001"),
        ("127.0.0.1", "127.0.0.10"),
        ("127.0.0.1", "localhost"),
        ("/1-7", "/1-70"),
        (":3241/", ":03241/"),
        ("Port 03:", "Port 003:"),
        ("Port 03:", "Port 0003:"),
        ("Port 03:", "port 03:"),
        ("Port 03:", " Port 03:"),
        ("Port 03:", "Port +3:"),
        ("Port 03:", "Port ٣:"),
        ("Port 03:", "Port 03:\r"),
        ("Port 03:", "Port 03:\r\r"),
        ("-> usbip:", "-> USBIP:"),
        ("-> usbip:", "->  usbip:"),
        ("-> usbip://127.0.0.1:3241/1-7", "-> usbip://"),
        ("-> serial: IvyPhoneMicro", "-> serial: IvyPhoneMicro "),
        (
            "-> remote bus/dev: 001/007",
            "-> remote bus/dev: 001/007\rX",
        ),
        (
            "-> usbip://127.0.0.1:3241/1-7",
            "-> usbip://127.0.0.1:3241/1-7\n-> usbip://foreign/1-7",
        ),
        (
            "-> serial: IvyPhoneMicro",
            "-> serial: IvyPhoneMicro\n-> serial: IvyPhoneMicro",
        ),
    ] {
        inventory_inputs.push((Some(original.replace(from, to)), 3241));
    }
    for whitespace in [
        '\t', '\n', '\r', '\u{000b}', '\u{000c}', '\u{0085}', '\u{00a0}', '\u{1680}', '\u{2000}',
        '\u{2007}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}', '\u{feff}',
        '\u{200b}', '\u{001c}',
    ] {
        inventory_inputs.push((Some(whitespace.to_string()), 3241));
        inventory_inputs.push((
            Some(original.replace("           ->", &format!("{whitespace}->"))),
            3241,
        ));
    }
    inventory_inputs.extend([
        (Some(original.replace('\n', "\r\n")), 3241),
        (Some(original.trim_end_matches('\n').to_owned()), 3241),
        (Some(format!("{original}{}", inventory(3241, 4))), 3241),
        (Some(format!("{original}{}", inventory(3240, 3))), 3241),
        (Some(format!("{}{}", inventory(3240, 4), original)), 3241),
        (
            Some(format!(
                "{}{}",
                inventory(3240, 4).replace("-> usbip", "x-> usbip"),
                original
            )),
            3241,
        ),
        (Some(" ".repeat(65536)), 3241),
        (Some(" ".repeat(65537)), 3241),
        (
            Some(format!(
                "{}{}",
                original,
                "🙂".repeat((65536 - original.len()) / 2)
            )),
            3241,
        ),
        (
            Some(format!(
                "{}{}",
                original,
                "🙂".repeat((65536 - original.len()) / 2 + 1)
            )),
            3241,
        ),
    ]);
    // Deterministic mutations exercise exact regex anchoring and full identity,
    // including malformed foreign blocks that must not become an absent device.
    let mutation_points: Vec<_> = original
        .char_indices()
        .filter_map(|(i, c)| (c != '\n').then_some(i))
        .collect();
    for &at in &mutation_points {
        let mut text = original.clone();
        text.insert(at, '\r');
        inventory_inputs.push((Some(text), 3241));
        let mut text = original.clone();
        text.remove(at);
        inventory_inputs.push((Some(text), 3241));
    }
    let expected_inventories = IvyJson::Array(
        inventory_inputs
            .iter()
            .map(|(output, port)| match own_port(output.as_deref(), *port) {
                Ok(port) => success([("port", port.map_or(IvyJson::Null, number))]),
                Err(e) => failure(e),
            })
            .collect(),
    );
    let mut control_inputs = Vec::new();
    let mut add = |kind: u8,
                   request: u8,
                   value: u16,
                   index: u16,
                   length: u16,
                   direction: u32,
                   capacity: u32,
                   payload: Vec<u8>| {
        let mut setup = [kind, request, 0, 0, 0, 0, 0, 0];
        setup[2..4].copy_from_slice(&value.to_le_bytes());
        setup[4..6].copy_from_slice(&index.to_le_bytes());
        setup[6..8].copy_from_slice(&length.to_le_bytes());
        control_inputs.push((setup, direction, capacity, payload));
    };
    for value in [
        0, 0x100, 0x101, 0x200, 0x300, 0x301, 0x302, 0x303, 0x304, 0x3ff, 0x400, 0x2100, 0x2200,
    ] {
        for kind in [0x80, 0x81] {
            for length in [0, 1, 2, 8, 9, 17, 18, 19, 26, 27, 28, 40, 41, 42, 255] {
                add(kind, 6, value, 0, length, 1, u32::from(length), Vec::new());
            }
        }
    }
    for kind in [0, 1, 2, 0x21, 0x80, 0x81, 0x82, 0xa1] {
        for request in [0, 1, 2, 3, 5, 8, 9, 10, 11] {
            for index in [0, 1, 2, 0x81] {
                for length in [0, 1, 2] {
                    let direction = u32::from(kind >> 7);
                    add(
                        kind,
                        request,
                        0,
                        index,
                        length,
                        direction,
                        u32::from(length),
                        if direction == 0 {
                            vec![0; usize::from(length)]
                        } else {
                            Vec::new()
                        },
                    );
                }
            }
        }
    }
    add(0x80, 6, 0x100, 0, 18, 0, 18, vec![0; 18]);
    add(0x80, 6, 0x100, 0, 18, 2, 18, Vec::new());
    add(0x80, 6, 0x100, 0, 18, 1, 17, Vec::new());
    add(0x80, 6, 0x100, 0, 18, 1, 4096, vec![1; 99]);
    add(0, 9, 0, 0, 0, 0, 0, vec![1]);
    for index in [0, 1] {
        for length in [63, 64, 65] {
            let mut output = vec![0; usize::from(length)];
            let notification = frame(b"{\"m\":\"ignored\"}\n").unwrap()[0];
            let count = output.len().min(64);
            output[..count].copy_from_slice(&notification[..count]);
            add(0x21, 9, 0x0206, index, length, 0, u32::from(length), output);
        }
    }
    let expected_controls = IvyJson::Array(
        control_inputs
            .iter()
            .map(|(setup, direction, capacity, bytes)| {
                match control(*direction, *capacity, setup, bytes) {
                    Ok(ControlTransfer::Reply { status, payload }) => success([
                        ("status", number(status)),
                        ("payload", hex(&payload).into()),
                    ]),
                    Ok(ControlTransfer::HidOutput(report)) => {
                        payload(&report).unwrap();
                        success([("status", number(0)), ("payload", "".into())])
                    }
                    Err(e) => failure(e),
                }
            })
            .collect(),
    );
    let fixture = parse(
        include_str!("../fixtures/phone-micro-wire-cases.json"),
        1024 * 1024,
    )
    .unwrap();
    let descriptor = IvyJson::object([
        ("report", hex(&report_descriptor()).into()),
        ("device", hex(&device()).into()),
        ("configuration", hex(&configuration()).into()),
        ("export", hex(&export()).into()),
        (
            "strings",
            IvyJson::Array(
                (-1..257)
                    .map(|i| string_descriptor(i).map_or(IvyJson::Null, |b| hex(&b).into()))
                    .collect(),
            ),
        ),
        ("busId", BUS_ID.into()),
        ("serial", SERIAL.into()),
        ("deviceId", number(DEVICE_ID)),
    ]);
    assert_eq!(fixture.get("descriptors"), Some(&descriptor));
    let frames = fixture.get("frames").unwrap();
    assert_eq!(
        expected_frames.as_array().unwrap().len(),
        frames.get("count").unwrap().as_f64().unwrap() as usize
    );
    let frame_bytes = stringify(&expected_frames, 16 * 1024 * 1024).unwrap();
    assert_eq!(
        Sha256::digest(frame_bytes.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        frames.get("sha256").unwrap().as_str().unwrap()
    );
    for (name, actual) in [
        ("inventories", expected_inventories),
        ("controls", expected_controls),
    ] {
        let actual = actual.as_array().unwrap();
        let expected = fixture.get(name).unwrap().as_array().unwrap();
        assert_eq!(actual.len(), expected.len(), "{name}");
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert_eq!(
                actual,
                expected,
                "{name}[{index}]: {} != {}",
                stringify(actual, 65536).unwrap(),
                stringify(expected, 65536).unwrap()
            );
        }
    }
    assert_eq!(
        fixture.get("microphone"),
        Some(&IvyJson::Array(
            [false, true]
                .map(|pressed| IvyJson::Array(vec![hex(&microphone(pressed)).into()]))
                .to_vec()
        ))
    );
}

#[test]
fn report_padding_and_exact_transfer_boundaries_preserve_payload() {
    for count in 0..=PAYLOAD_SIZE {
        let mut report = [0xff; REPORT_SIZE];
        report[..3].copy_from_slice(&[6, 2, count as u8]);
        for i in 0..count {
            report[3 + i] = i as u8;
        }
        assert_eq!(
            payload(&report).unwrap(),
            (0..count as u8).collect::<Vec<_>>()
        );
    }
    for size in [0, 1, 2, 3, 63, 65, 128] {
        assert!(payload(&vec![0; size]).is_err());
    }
    for (id, kind, count) in [(5, 2, 0), (6, 3, 0), (6, 2, 62), (6, 2, 255)] {
        let mut report = [0; REPORT_SIZE];
        report[..3].copy_from_slice(&[id, kind, count]);
        assert!(payload(&report).is_err());
    }
}
