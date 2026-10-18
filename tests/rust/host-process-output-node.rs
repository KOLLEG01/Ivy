#[path = "../../packages/host-runtime/rust/src/process_output.rs"]
mod process_output;
use process_output::DiagnosticCapture;
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn streaming_matches_whole_stream_node_replacement() {
    let mut cases = Vec::new();
    let mut actual = Vec::new();
    let mut add = |input: &[u8], secrets: Vec<Vec<u16>>, cuts: &[usize], maximum| {
        let mut capture = DiagnosticCapture::new(secrets.clone(), maximum);
        let mut emitted = String::new();
        let mut chunks = Vec::new();
        let mut start = 0;
        let bound = secrets
            .iter()
            .map(|s| s.len().saturating_sub(1))
            .sum::<usize>()
            + 1;
        for end in cuts.iter().copied().chain(std::iter::once(input.len())) {
            let chunk = &input[start..end];
            chunks.push(hex(chunk));
            capture.push_to(chunk, false, |text| {
                assert!(text.len() <= 4096);
                emitted.push_str(text);
            });
            assert!(capture.retained_units() <= bound);
            start = end;
        }
        capture.push_to(&[], true, |text| {
            assert!(text.len() <= 4096);
            emitted.push_str(text);
        });
        assert_eq!(capture.retained_units(), 0);
        actual.push(format!(
            "{}\t{}\t{}",
            hex(capture.text().as_bytes()),
            capture.truncated(),
            hex(emitted.as_bytes())
        ));
        let secrets = secrets
            .iter()
            .map(|s| {
                s.iter()
                    .map(|u| format!("{u:04x}"))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect::<Vec<_>>()
            .join(";");
        cases.push(format!("{maximum}\t{secrets}\t{}", chunks.join(",")));
    };
    let variants: Vec<Vec<Vec<u16>>> = vec![
        vec![],
        vec![vec![]],
        vec!["aba", "ba"]
            .into_iter()
            .map(|s| s.encode_utf16().collect())
            .collect(),
        vec!["ba", "aba"]
            .into_iter()
            .map(|s| s.encode_utf16().collect())
            .collect(),
        vec!["aba", "aba", "[redacted]", "e"]
            .into_iter()
            .map(|s| s.encode_utf16().collect())
            .collect(),
        vec!["ab🙂", "🙂ab"]
            .into_iter()
            .map(|s| s.encode_utf16().collect())
            .collect(),
        vec![vec![0xd83d], vec![0xde42]],
        vec![vec![0xde42, 97], vec![97, 0xd83d]],
        vec!["[", "e", "e", "d]"]
            .into_iter()
            .map(|s| s.encode_utf16().collect())
            .collect(),
    ];
    for input in [
        b"ababa".as_slice(),
        "🙂ab".as_bytes(),
        &[0xff, 0xe2, 0x82, b'x'],
        b"",
        b"[e]",
    ] {
        for secrets in &variants {
            // Every partition, including a byte boundary inside UTF-8.
            for mask in 0..1usize << input.len().saturating_sub(1) {
                let cuts: Vec<_> = (1..input.len())
                    .filter(|at| mask & (1 << (at - 1)) != 0)
                    .collect();
                add(input, secrets.clone(), &cuts, 31);
            }
        }
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
        add(
            &input,
            variants[random() as usize % variants.len()].clone(),
            &cuts,
            61,
        );
    }
    let long = "ab".repeat(40000) + "a🙂";
    add(
        long.as_bytes(),
        vec![vec![97, 98, 97]],
        &(1..long.len()).step_by(1023).collect::<Vec<_>>(),
        65536,
    );

    let script = option_env!("CARGO_MANIFEST_DIR")
        .map(|p| std::path::Path::new(p).join("../../../tests/fixtures/process-output-oracle.mjs"))
        .unwrap_or_else(|| {
            std::path::Path::new(file!())
                .parent()
                .unwrap()
                .join("../fixtures/process-output-oracle.mjs")
        });
    let mut command = Command::new("node");
    command
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(cases.join("\n").as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = String::from_utf8(output.stdout).unwrap();
    let expected: Vec<_> = expected.lines().collect();
    assert_eq!(expected.len(), actual.len());
    for (at, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual, expected, "case {at}: {}", cases[at]);
    }
}
