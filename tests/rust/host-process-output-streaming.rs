#[path = "../../packages/host-runtime/rust/src/process_output.rs"]
mod process_output;
use process_output::DiagnosticCapture;

#[test]
fn streaming_matches_whole_stream_literal_replacement() {
    let check = |input: &[u8], secrets: Vec<String>, cuts: &[usize], maximum| {
        // Independent standard-library reference: decode the entire byte stream,
        // omit its first BOM, then apply configured literals in order.
        let decoded = String::from_utf8_lossy(input);
        let mut expected = decoded
            .strip_prefix('\u{feff}')
            .unwrap_or(&decoded)
            .to_owned();
        for secret in &secrets {
            if !secret.is_empty() {
                expected = expected.replace(secret, "[redacted]");
            }
        }
        let mut capture = DiagnosticCapture::new(secrets.clone(), maximum);
        let mut emitted = String::new();
        let mut start = 0;
        let bound = secrets
            .iter()
            .map(|s| s.chars().count().saturating_sub(1))
            .sum::<usize>();
        for end in cuts.iter().copied().chain(std::iter::once(input.len())) {
            capture.push_to(&input[start..end], false, |text| {
                assert!(text.len() <= 4096);
                emitted.push_str(text);
            });
            assert!(capture.retained_characters() <= bound);
            start = end;
        }
        capture.push_to(&[], true, |text| {
            assert!(text.len() <= 4096);
            emitted.push_str(text);
        });
        assert_eq!(capture.retained_characters(), 0);
        assert_eq!(
            emitted, expected,
            "input={input:?}, secrets={secrets:?}, cuts={cuts:?}"
        );
        let tail = &expected.as_bytes()[expected.len().saturating_sub(maximum)..];
        assert_eq!(capture.text(), String::from_utf8_lossy(tail));
        assert_eq!(capture.truncated(), expected.len() > maximum);
    };
    let variants: Vec<Vec<String>> = [
        vec![],
        vec![""],
        vec!["aba", "ba"],
        vec!["ba", "aba"],
        vec!["aba", "aba", "[redacted]", "e"],
        vec!["ab🙂", "🙂ab"],
        vec!["[", "e", "e", "d]"],
    ]
    .into_iter()
    .map(|secrets| secrets.into_iter().map(str::to_owned).collect())
    .collect();
    for input in [
        b"ababa".as_slice(),
        "🙂ab".as_bytes(),
        &[0xff, 0xe2, 0x82, b'x'],
        b"",
        b"[e]",
        "\u{feff}é🙂".as_bytes(),
    ] {
        for secrets in &variants {
            // Every partition, including byte boundaries inside UTF-8 scalars.
            for mask in 0..1usize << input.len().saturating_sub(1) {
                let cuts: Vec<_> = (1..input.len())
                    .filter(|at| mask & (1 << (at - 1)) != 0)
                    .collect();
                check(input, secrets.clone(), &cuts, 31);
            }
        }
    }
    // Byte-tail caps may cut a character; callback chunks always remain text.
    for maximum in 0..=4 {
        check("é🙂".as_bytes(), vec![], &[1, 3, 4, 5], maximum);
    }
    let mut seed = 0x81da_7604_u64;
    let mut random = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..600 {
        let mut input = Vec::new();
        let tokens: &[&[u8]] = &[
            b"aba",
            b"ba",
            b"[redacted]",
            "🙂".as_bytes(),
            "é".as_bytes(),
            "\u{feff}".as_bytes(),
            &[0xff, 0xe2],
            b"x",
        ];
        for _ in 0..random() % 40 {
            input.extend(tokens[random() as usize % tokens.len()]);
        }
        let mut cuts = Vec::new();
        let mut at = 0;
        while at < input.len() {
            at = (at + 1 + random() as usize % 17).min(input.len());
            cuts.push(at);
        }
        check(
            &input,
            variants[random() as usize % variants.len()].clone(),
            &cuts,
            61,
        );
    }
    let long = "ab".repeat(40000) + "a🙂";
    check(
        long.as_bytes(),
        vec!["aba".into()],
        &(1..long.len()).step_by(1023).collect::<Vec<_>>(),
        65536,
    );
}
