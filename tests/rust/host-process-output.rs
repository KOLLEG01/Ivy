#[path = "../../packages/host-runtime/rust/src/process_output.rs"]
mod process_output;
use process_output::{DiagnosticCapture, Utf8Decoder};

#[test]
fn redaction_holds_a_secret_across_every_possible_chunk_boundary() {
    let input = "before secret-🙂-token after";
    for split in 0..=input.len() {
        let mut capture =
            DiagnosticCapture::new(vec!["secret-🙂-token".into()], 65536);
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
    let mut text = String::new();
    for byte in "a🙂é".as_bytes() {
        text.push_str(&decoder.decode_text(&[*byte], false));
    }
    text.push_str(&decoder.decode_text(&[0xff, 0xe2], false));
    text.push_str(&decoder.decode_text(&[], true));
    assert_eq!(text, "a🙂é��");
}

#[test]
fn decoder_consumes_only_the_initial_bom_even_when_fragmented() {
    let mut decoder = Utf8Decoder::default();
    let mut text = String::new();
    for byte in "\u{feff}a\u{feff}b".as_bytes() {
        text.push_str(&decoder.decode_text(&[*byte], false));
    }
    text.push_str(&decoder.decode_text(&[], true));
    assert_eq!(text, "a\u{feff}b");
}

#[cfg(windows)]
#[test]
fn windows_code_pages_keep_native_text_across_every_byte_boundary() {
    use process_output::WindowsCodePageDecoder;
    for (page, bytes, expected) in [
        (1252, &[0x80, 0xe9, 0][..], "€é\0"),
        (437, &[0x82, 0xe1, 0][..], "éß\0"),
        // The first pair's trail byte is also a lead-byte value.
        (932, &[0x81, 0x81, 0x82, 0xa0, 0, 0xa6][..], "＝あ\0ｦ"),
        (65001, "\u{feff}🙂\u{feff}".as_bytes(), "\u{feff}🙂\u{feff}"),
    ] {
        for split in 0..=bytes.len() {
            let mut decoder = WindowsCodePageDecoder::new(page).unwrap();
            assert_eq!(decoder.code_page(), page);
            let mut units = decoder.decode(&bytes[..split], false).unwrap();
            units.extend(decoder.decode(&[], false).unwrap());
            units.extend(decoder.decode(&bytes[split..], false).unwrap());
            units.extend(decoder.decode(&[], true).unwrap());
            assert_eq!(units, expected.encode_utf16().collect::<Vec<_>>(), "page={page}, split={split}");
        }
        let mut decoder = WindowsCodePageDecoder::new(page).unwrap();
        let mut units = Vec::new();
        for byte in bytes { units.extend(decoder.decode(&[*byte], false).unwrap()); }
        units.extend(decoder.decode(&[], true).unwrap());
        assert_eq!(units, expected.encode_utf16().collect::<Vec<_>>(), "page={page}, bytewise");
    }
}

#[cfg(windows)]
#[test]
fn windows_code_page_final_flush_releases_pending_native_replacement() {
    use process_output::WindowsCodePageDecoder;
    // The control converts the final incomplete byte through Windows directly;
    // the streaming path must defer it until flush, then clear its carry.
    let expected = WindowsCodePageDecoder::new(932).unwrap().decode(&[0x82], true).unwrap();
    assert!(!expected.is_empty());
    let mut decoder = WindowsCodePageDecoder::new(932).unwrap();
    assert!(decoder.decode(&[0x82], false).unwrap().is_empty());
    assert!(decoder.decode(&[], false).unwrap().is_empty());
    assert_eq!(decoder.decode(&[], true).unwrap(), expected);
    assert!(decoder.decode(&[], true).unwrap().is_empty());
    assert_eq!(decoder.decode(b"A", true).unwrap(), vec![65]);
    assert_eq!(WindowsCodePageDecoder::new(0).unwrap().code_page(), 0);
    assert!(WindowsCodePageDecoder::new(u32::MAX).is_err());
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
    let mut capture = DiagnosticCapture::new(vec!["aba".into(), "ba".into()], 65536);
    capture.push(b"ababa", false);
    capture.push(&[], true);
    assert_eq!(capture.text(), "[redacted][redacted]");
}

#[test]
fn overlapping_secrets_preserve_order_across_every_split() {
    for (input, secrets, expected) in [
        ("ababa", ["aba", "ba"], "[redacted][redacted]"),
        ("ababa", ["ba", "aba"], "a[redacted][redacted]"),
        ("🙂x🙂x🙂", ["🙂x🙂", "x🙂"], "[redacted][redacted]"),
    ] {
        for split in 0..=input.len() {
            let mut capture =
                DiagnosticCapture::new(secrets.iter().map(|s| (*s).into()).collect(), 65536);
            let emitted = capture.push(&input.as_bytes()[..split], false)
                + &capture.push(&input.as_bytes()[split..], false)
                + &capture.push(&[], true);
            assert_eq!(emitted, expected, "{secrets:?} split {split}");
            assert_eq!(capture.text(), expected);
        }
    }
}

#[test]
fn repeated_overlaps_make_progress_with_bounded_matcher_state() {
    let mut capture = DiagnosticCapture::new(vec!["aba".into()], 65536);
    let mut emitted = 0;
    for _ in 0..10000 {
        capture.push_to(b"abababababababab", false, |text| {
            assert!(text.len() <= 4096);
            emitted += text.len();
        });
        assert!(capture.retained_characters() <= 2);
    }
    assert!(emitted > 160000);
    capture.push_to(b"a", true, |_| {});
    assert_eq!(capture.retained_characters(), 0);
    assert!(capture.truncated());
    assert!(capture.text().len() <= 65536);
}

#[test]
fn replacement_expansion_is_bounded_at_the_sink_and_not_recursive() {
    let mut capture = DiagnosticCapture::new(vec!["e".into(); 10], 65536);
    let mut emitted = 0;
    capture.push_to(b"e", true, |text| {
        assert!(text.len() <= 4096);
        emitted += text.len();
    });
    // Each pass doubles the e's; only downstream passes see inserted text.
    assert_eq!(emitted, 1 + 9 * (1024 - 1));
    assert_eq!(capture.retained_characters(), 0);
}

#[test]
fn literal_matches_keep_unicode_characters_intact_at_emission() {
    let mut capture = DiagnosticCapture::new(vec!["ab".into()], 65536);
    assert_eq!(capture.push("🙂".as_bytes(), false), "🙂");
    assert_eq!(capture.push(&[], true), "");
    let mut capture = DiagnosticCapture::new(vec!["🙂".into()], 65536);
    assert_eq!(capture.push("é🙂🦀".as_bytes(), true), "é[redacted]🦀");
}
