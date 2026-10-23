use super::*;
use ivy_contracts::{
    IvyJson as J,
    json::{parse, stringify},
};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::ReadBuf;
fn obj<const N: usize>(rows: [(&str, J); N]) -> J {
    J::object(rows)
}
fn num(n: usize) -> J {
    J::Number(n as f64)
}
fn number(row: &J, key: &str) -> usize {
    row.get(key).unwrap().as_f64().unwrap() as usize
}
fn hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for b in bytes {
        use std::fmt::Write;
        write!(out, "{b:02X}").unwrap()
    }
    out
}
fn decode(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}
fn trace(name: &str, value: &J) {
    if let Ok(root) = std::env::var("IVY_PHONE_MICRO_COMMAND_OUTPUT_TRACE") {
        std::fs::write(
            std::path::Path::new(&root).join(name),
            stringify(value, 32 * 1024 * 1024).unwrap(),
        )
        .unwrap()
    }
}
fn row(
    name: &str,
    bytes: &[u8],
    chunks: &[usize],
    cancelled: bool,
    cancel_at: usize,
    fail_at: usize,
) -> J {
    obj([
        ("name", name.into()),
        ("bytes", hex(bytes).into()),
        (
            "chunks",
            J::Array(chunks.iter().copied().map(num).collect()),
        ),
        ("cancelled", cancelled.into()),
        ("cancelAt", num(cancel_at)),
        ("failAt", num(fail_at)),
        ("console", false.into()),
    ])
}
fn cases() -> Vec<J> {
    let mut rows = Vec::new();
    let streams = [
        ("empty", vec![]),
        ("ascii", b"Port 001:\r\n-> usbip://127.0.0.1\n".to_vec()),
        ("unicode", "aé€𐐷\0\u{feff}z".as_bytes().to_vec()),
        (
            "utf8-bom",
            [vec![0xef, 0xbb, 0xbf], "é𐐷".as_bytes().to_vec()].concat(),
        ),
        ("double-bom", vec![0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf]),
        ("partial-bom", vec![0xef, 0xbb]),
        ("bom-mismatch", vec![0xef, 0xbb, b'a']),
        (
            "invalid-utf8",
            vec![
                0xff, 0xc0, 0xaf, 0xed, 0xa0, 0x80, 0xf4, 0x90, 0x80, 0x80, 0xe2, 0x82,
            ],
        ),
        (
            "utf16le",
            vec![0xff, 0xfe, b'a', 0, 0x3d, 0xd8, 0, 0xde, 0, 0],
        ),
        (
            "utf16be",
            vec![0xfe, 0xff, 0, b'a', 0xd8, 0x3d, 0xde, 0, 0, 0],
        ),
        (
            "utf16-invalid",
            vec![0xff, 0xfe, 0, 0xd8, 0x41, 0, 0, 0xdc, 0x42],
        ),
        (
            "utf32le",
            vec![0xff, 0xfe, 0, 0, 0x41, 0, 0, 0, 0, 0xf6, 1, 0],
        ),
        (
            "utf32be",
            vec![0, 0, 0xfe, 0xff, 0, 0, 0, 0x41, 0, 1, 0xf6, 0],
        ),
        (
            "utf32-invalid",
            vec![0, 0, 0xfe, 0xff, 0, 0, 0xd8, 0, 0, 0x11, 0, 0, 1, 2, 3],
        ),
        ("late-bom", vec![b'a', 0xef, 0xbb, 0xbf, b'b']),
    ];
    for (name, bytes) in streams {
        for chunks in [
            &[1][..],
            &[2][..],
            &[3][..],
            &[4][..],
            &[1, 3, 2, 7][..],
            &[1024][..],
        ] {
            rows.push(row(
                &format!("{name}-{chunks:?}"),
                &bytes,
                chunks,
                false,
                0,
                0,
            ))
        }
    }
    for length in [2047, 2048, 2049, 65535, 65536, 65537] {
        rows.push(row(
            &format!("ascii-bound-{length}"),
            &vec![b'x'; length],
            &[1024],
            false,
            0,
            0,
        ))
    }
    for length in [32767, 32768, 32769] {
        let bytes = "𐐷".repeat(length).into_bytes();
        rows.push(row(
            &format!("utf16-count-{length}"),
            &bytes,
            &[1024],
            false,
            0,
            0,
        ))
    }
    for (name, data, cancelled, cancel, fail) in [
        ("pre-cancel", b"abc".as_slice(), true, 0, 0),
        ("pre-cancel-io", b"abc".as_slice(), true, 0, 1),
        ("cancel-after-data", b"abc".as_slice(), false, 1, 0),
        ("cancel-at-eof", b"".as_slice(), false, 1, 0),
        ("io-first", b"abc".as_slice(), false, 0, 1),
        ("io-after-data", b"abc".as_slice(), false, 0, 2),
    ] {
        rows.push(row(name, data, &[1], cancelled, cancel, fail))
    }
    for fail in [65, 66, 67] {
        rows.push(row(
            &format!("limit-io-order-{fail}"),
            &vec![b'x'; 70000],
            &[1024],
            false,
            0,
            fail,
        ))
    }
    let mut tail = vec![b'x'; 1022];
    tail.extend([0xe2, 0x82]);
    for (label, cancel, fail) in [
        ("flush-full", 0, 0),
        ("flush-full-io", 0, 3),
        ("flush-full-cancel", 2, 0),
    ] {
        rows.push(row(label, &tail, &[1024], false, cancel, fail));
    }
    let text_rows = rows.clone();
    for mut row in text_rows {
        let name = row.get("name").unwrap().as_str().unwrap();
        row.insert("name", format!("console/{name}").into());
        row.insert("console", true.into());
        rows.push(row);
    }
    for length in [4095, 4096, 4097, 65535, 65536, 65537] {
        let mut value = row(
            &format!("console/full-{length}"),
            &vec![b'x'; length],
            &[4096],
            false,
            0,
            0,
        );
        value.insert("console", true.into());
        rows.push(value);
    }
    for length in [32767, 32768, 32769] {
        let mut value = row(
            &format!("console/full-scalars-{length}"),
            "𐐷".repeat(length).as_bytes(),
            &[4096],
            false,
            0,
            0,
        );
        value.insert("console", true.into());
        rows.push(value);
    }
    let mut tail = vec![b'x'; 4094];
    tail.extend([0xe2, 0x82]);
    for (name, cancel, fail) in [
        ("console/flush-full", 0, 0),
        ("console/flush-io", 0, 3),
        ("console/flush-cancel", 2, 0),
    ] {
        let mut value = row(name, &tail, &[4096], false, cancel, fail);
        value.insert("console", true.into());
        rows.push(value);
    }
    // Preserve console code-page, BOM and bounded-output cases.
    for cp in [0, 437, 850, 1252, 932, 936, 949, 950, 1361, 65001] {
        let streams = [
            ("bytes", (0..=255).collect::<Vec<u8>>()),
            ("lead", vec![b'a', 0x81]),
            ("pair", vec![b'a', 0x81, 0x40, 0x81, 0x81, 0x81]),
            ("trails", vec![0x81, 0x20, 0xff, 0x81, 0x00, 0x81]),
            ("bom8", vec![0xef, 0xbb, 0xbf, 0xc3, 0xa9]),
            ("bom16", vec![0xff, 0xfe, 0x3d, 0xd8, 0, 0xde]),
            ("bom32", vec![0, 0, 0xfe, 0xff, 0, 1, 0xf6, 0]),
        ];
        for (name, data) in streams {
            for chunks in [&[1][..], &[2][..], &[3][..], &[1, 3, 2, 7][..], &[4096][..]] {
                let mut value = row(
                    &format!("cp{cp}/{name}/{chunks:?}"),
                    &data,
                    chunks,
                    false,
                    0,
                    0,
                );
                value.insert("console", true.into());
                value.insert("codePage", num(cp));
                rows.push(value);
            }
        }
        for (name, length, cancel, fail) in [
            ("full-lead", 4096, 0, 0),
            ("full-lead-io", 4096, 0, 3),
            ("full-lead-cancel", 4096, 2, 0),
            ("limit", 65536, 0, 0),
            ("over-limit", 65537, 0, 0),
        ] {
            let mut data = vec![b'a'; length];
            data[length - 1] = 0x81;
            let mut value = row(
                &format!("cp{cp}/{name}"),
                &data,
                &[4096],
                false,
                cancel,
                fail,
            );
            value.insert("console", true.into());
            value.insert("codePage", num(cp));
            rows.push(value);
        }
    }
    rows
}
struct Bytes {
    data: Vec<u8>,
    chunks: Vec<usize>,
    stop: CancellationToken,
    cancel: usize,
    fail: usize,
    calls: usize,
    consumed: usize,
}
impl AsyncRead for Bytes {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.stop.is_cancelled() {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled")));
        }
        self.calls += 1;
        if self.calls == self.fail {
            return Poll::Ready(Err(io::Error::other("fixture io")));
        }
        if self.calls == self.cancel {
            self.stop.cancel()
        }
        let count = self.chunks[(self.calls - 1) % self.chunks.len()]
            .min(out.remaining())
            .min(self.data.len() - self.consumed);
        out.put_slice(&self.data[self.consumed..self.consumed + count]);
        self.consumed += count;
        Poll::Ready(Ok(()))
    }
}
async fn actual(row: &J) -> J {
    let stop = CancellationToken::new();
    if row.get("cancelled").and_then(J::as_bool).unwrap() {
        stop.cancel()
    }
    let mut bytes = Bytes {
        data: decode(&row.get("bytes").unwrap().as_str().unwrap()),
        chunks: row
            .get("chunks")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as usize)
            .collect(),
        stop: stop.clone(),
        cancel: number(row, "cancelAt"),
        fail: number(row, "failAt"),
        calls: 0,
        consumed: 0,
    };
    let result = if let Some(page) = row.get("codePage").and_then(J::as_f64) {
        read_micro_console_output(&mut bytes, &stop, page as u32).await
    } else if row.get("console").and_then(J::as_bool).unwrap_or(false) {
        read_micro_utf8_console_output(&mut bytes, &stop).await
    } else {
        read_micro_command_output(&mut bytes, &stop).await
    };
    let (value, error) = match result {
        Ok(s) => (s.into(), J::Null),
        Err(e) => {
            let (kind, message) = match e {
                MicroOutputError::Cancelled => ("cancelled", J::Null),
                MicroOutputError::Limit => ("io", "Bounded USBip command output exceeded.".into()),
                MicroOutputError::Io(e) => ("io", e.to_string().into()),
            };
            (J::Null, obj([("kind", kind.into()), ("message", message)]))
        }
    };
    obj([
        ("name", row.get("name").unwrap().clone()),
        ("value", value),
        ("error", error),
        ("calls", num(bytes.calls)),
        ("consumed", num(bytes.consumed)),
    ])
}
#[test]
fn bounded_utf16_output_boms_errors_and_stream_read_order_follow_public_cases() {
    let console_before = ivy_host_runtime::process_output::console_output_code_page();
    let rows = cases();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let actual = runtime.block_on(async {
        let mut result = Vec::new();
        for row in &rows {
            result.push(actual(row).await)
        }
        J::Array(result)
    });
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-command-output-cases.json");
    let expected = parse(&std::fs::read_to_string(path).unwrap(), 1024 * 1024).unwrap();
    let expected_rows = expected.get("expected").unwrap();
    assert_eq!(expected_rows.as_array().unwrap().len(), rows.len());
    let inputs = J::Array(
        rows.iter()
            .map(|row| {
                let mut row = row.clone();
                let bytes = decode(&row.get("bytes").unwrap().as_str().unwrap());
                row.insert(
                    "bytes",
                    obj([
                        ("byteLength", num(bytes.len())),
                        ("sha256", ivy_contracts::hash::digest(bytes).into()),
                    ]),
                );
                row
            })
            .collect(),
    );
    let expected_inputs = expected.get("cases").unwrap().as_array().unwrap();
    assert_eq!(inputs.as_array().unwrap().len(), expected_inputs.len());
    for (input, expected) in inputs.as_array().unwrap().iter().zip(expected_inputs) {
        assert_eq!(
            input,
            expected,
            "input {}",
            input.get("name").unwrap().as_str().unwrap()
        );
    }
    let compact = J::Array(
        actual
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let mut row = row.clone();
                if let Some(J::String(text)) = row.get("value")
                    && text.0.len() > 4096
                {
                    let bytes: Vec<_> = text.0.iter().flat_map(|unit| unit.to_le_bytes()).collect();
                    let value = obj([
                        ("utf16Length", num(text.0.len())),
                        ("sha256Utf16LE", ivy_contracts::hash::digest(bytes).into()),
                    ]);
                    row.insert("value", value);
                }
                row
            })
            .collect(),
    );
    for (row, expected) in compact
        .as_array()
        .unwrap()
        .iter()
        .zip(expected_rows.as_array().unwrap())
    {
        assert_eq!(
            row,
            expected,
            "output {}",
            row.get("name").unwrap().as_str().unwrap()
        );
    }
    assert_eq!(
        console_before,
        ivy_host_runtime::process_output::console_output_code_page()
    );
}
#[test]
fn pending_pipe_read_cancels_without_waiting_for_eof() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut results = Vec::new();
        for page in [None, Some(65001), Some(932)] {
            let (mut reader, writer) = tokio::io::duplex(1024);
            let stop = CancellationToken::new();
            let mut pending = Box::pin(async {
                if let Some(page) = page {
                    read_micro_console_output(&mut reader, &stop, page).await
                } else {
                    read_micro_command_output(&mut reader, &stop).await
                }
            });
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(30), &mut pending)
                    .await
                    .is_err()
            );
            stop.cancel();
            assert!(matches!(
                tokio::time::timeout(std::time::Duration::from_secs(1), pending)
                    .await
                    .unwrap(),
                Err(MicroOutputError::Cancelled)
            ));
            drop(writer);
            results.push(obj([
                ("codePage", page.map_or(J::Null, |n| num(n as usize))),
                ("heldWithoutEof", true.into()),
                ("cancelledWithinOneSecond", true.into()),
            ]));
        }
        trace("pending-comparisons.json", &J::Array(results));
    });
}
