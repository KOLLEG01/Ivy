#[path = "../../packages/host-runtime/rust/src/process_output.rs"]
mod process_output;
use process_output::{DiagnosticCapture, Utf8Decoder};

#[test]
fn redaction_holds_a_secret_across_every_possible_chunk_boundary() {
    let input = "before secret-🙂-token after";
    for split in 0..=input.len() {
        let mut capture =
            DiagnosticCapture::new(vec!["secret-🙂-token".encode_utf16().collect()], 65536);
        let first = capture.push(&input.as_bytes()[..split], false);
        let second = capture.push(&input.as_bytes()[split..], false);
        let last = capture.push(&[], true);
        assert_eq!(
            first + &second + &last,
            "before [redacted] after",
            "split {split}"
        );
        assert_eq!(capture.text(), "before [redacted] after");
    }
}

#[test]
fn streaming_decoder_preserves_multibyte_text_and_replaces_invalid_bytes() {
    let mut decoder = Utf8Decoder::default();
    let mut units = Vec::new();
    for byte in "a🙂é".as_bytes() {
        units.extend(decoder.decode(&[*byte], false));
    }
    units.extend(decoder.decode(&[0xff, 0xe2], false));
    units.extend(decoder.decode(&[], true));
    assert_eq!(String::from_utf16_lossy(&units), "a🙂é��");
}

#[test]
fn decoder_consumes_only_the_initial_bom_even_when_fragmented() {
    let mut decoder = Utf8Decoder::default();
    let mut units = Vec::new();
    for byte in "\u{feff}a\u{feff}b".as_bytes() {
        units.extend(decoder.decode(&[*byte], false));
    }
    units.extend(decoder.decode(&[], true));
    assert_eq!(String::from_utf16_lossy(&units), "a\u{feff}b");
}

#[test]
fn diagnostic_capture_keeps_only_the_bounded_tail() {
    let mut capture = DiagnosticCapture::new(vec![], 65536);
    capture.push(&vec![b'a'; 65536], false);
    capture.push(b"new", true);
    assert!(capture.truncated());
    assert_eq!(capture.text().len(), 65536);
    assert!(capture.text().ends_with("new"));
}

#[test]
fn overlapping_secrets_follow_configured_replacement_order() {
    let mut capture = DiagnosticCapture::new(
        vec![
            "aba".encode_utf16().collect(),
            "ba".encode_utf16().collect(),
        ],
        65536,
    );
    capture.push(b"ababa", false);
    capture.push(&[], true);
    assert_eq!(capture.text(), "[redacted][redacted]");
}

#[test]
fn crossing_secrets_revisit_earlier_matches_before_emitting() {
    for (input, secrets, expected) in [
        ("ababa", ["aba", "ba"], "[redacted][redacted]"),
        ("ababa", ["ba", "aba"], "a[redacted][redacted]"),
        ("🙂x🙂x🙂", ["🙂x🙂", "x🙂"], "[redacted][redacted]"),
    ] {
        for split in 0..=input.len() {
            let mut capture = DiagnosticCapture::new(
                secrets.iter().map(|s| s.encode_utf16().collect()).collect(),
                65536,
            );
            let emitted = capture.push(&input.as_bytes()[..split], false)
                + &capture.push(&input.as_bytes()[split..], false)
                + &capture.push(&[], true);
            assert_eq!(emitted, expected, "{secrets:?} split {split}");
            assert_eq!(capture.text(), expected);
        }
    }
}
