use ivy_phone_bridge::dtmf_detector::InBandDetector;
use std::{
    f64::consts::PI,
    io::Write,
    process::{Command, Stdio},
};

fn tone(
    rate: i32,
    ms: usize,
    low: f64,
    high: f64,
    low_amp: f64,
    high_amp: f64,
    dc: f64,
) -> Vec<i16> {
    (0..rate as usize * ms / 1_000)
        .map(|i| {
            let time = i as f64 / f64::from(rate);
            (dc + low_amp * (2.0 * PI * low * time).sin()
                + high_amp * (2.0 * PI * high * time).sin())
            .round()
            .clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

#[test]
fn detector_confirms_once_rearms_after_silence_and_keeps_call_counters() {
    let samples = tone(8_000, 200, 697.0, 1209.0, 4_000.0, 4_000.0, 0.0);
    let mut detector = InBandDetector::default();
    assert!(detector.process(&samples[..320], 8_000).unwrap().is_empty());
    // Empty and unsupported-rate blocks do not reset a partly confirmed tone.
    assert!(detector.process(&[], 48_000).unwrap().is_empty());
    assert!(detector.process(&[0; 80], 3_999).unwrap().is_empty());
    let detected = detector.process(&samples[320..400], 8_000).unwrap();
    assert_eq!(detected.len(), 1);
    assert_eq!(
        (detected[0].digit, detected[0].confirmed_duration_ms),
        ('1', 50)
    );
    assert!(detector.process(&samples[400..], 8_000).unwrap().is_empty());
    assert!(detector.process(&[0; 640], 8_000).unwrap().is_empty());
    assert_eq!(detector.process(&samples[..400], 8_000).unwrap().len(), 1);
    let before = detector.diagnostics();
    let changed = tone(16_000, 50, 697.0, 1209.0, 4_000.0, 4_000.0, 0.0);
    let detected = detector.process(&changed, 16_000).unwrap();
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].sample_rate, 16_000);
    assert_eq!(
        detector.diagnostics().analyzed_windows,
        before.analyzed_windows + 2
    );
}

#[test]
fn pcm_detections_and_signal_diagnostics_match_original_csharp() {
    type Block = (i32, Vec<i16>);
    let mut scenarios: Vec<Vec<Block>> = Vec::new();
    // Original native loopback tone frequencies, all sixteen keypad symbols.
    // Fragment across both analysis windows and hops, including single samples.
    for rate in [4_000, 8_000, 16_000, 48_000] {
        let mut pcm = Vec::new();
        for low in [697.0, 770.0, 852.0, 941.0] {
            for high in [1209.0, 1336.0, 1477.0, 1633.0] {
                pcm.extend(tone(rate, 90, low, high, 4_000.0, 4_000.0, 900.0));
                pcm.extend(vec![0; rate as usize * 70 / 1_000]);
            }
        }
        let mut blocks = Vec::new();
        let mut offset = 0;
        for length in [1, 79, 160, 37, 333].into_iter().cycle() {
            let end = (offset + length).min(pcm.len());
            blocks.push((rate, pcm[offset..end].to_vec()));
            offset = end;
            if offset == pcm.len() {
                break;
            }
        }
        scenarios.push(blocks);
    }
    for amplitude in [0.0, 50.0, 119.0, 120.0, 149.0, 150.0, 151.0, 200.0, 4_000.0] {
        scenarios.push(vec![(
            8_000,
            tone(8_000, 100, 770.0, 1336.0, amplitude, amplitude, 10_000.0),
        )]);
    }
    for twist in [0.1, 0.279, 0.28, 0.281, 1.0, 3.549, 3.55, 3.551, 6.0] {
        scenarios.push(vec![(
            8_000,
            tone(8_000, 100, 852.0, 1477.0, twist * 1_000.0, 1_000.0, 0.0),
        )]);
    }
    for scale in [0.94, 0.97, 0.985, 1.0, 1.015, 1.03, 1.06] {
        scenarios.push(vec![(
            8_000,
            tone(
                8_000,
                100,
                941.0 * scale,
                1633.0 * scale,
                4_000.0,
                4_000.0,
                0.0,
            ),
        )]);
    }
    // Long holds, adjacent different tones, short gaps and release/repress.
    let mut transitions = Vec::new();
    for (low, high, duration) in [
        (697.0, 1209.0, 500),
        (770.0, 1336.0, 100),
        (0.0, 0.0, 10),
        (770.0, 1336.0, 100),
        (0.0, 0.0, 80),
        (770.0, 1336.0, 100),
    ] {
        transitions.extend(tone(8_000, duration, low, high, 4_000.0, 4_000.0, 0.0));
    }
    scenarios.push(
        transitions
            .chunks(80)
            .map(|pcm| (8_000, pcm.to_vec()))
            .collect(),
    );
    scenarios.push(vec![
        (8_000, tone(8_000, 40, 697.0, 1209.0, 4_000.0, 4_000.0, 0.0)),
        (48_000, vec![]),
        (3_999, vec![0; 80]),
        (8_000, tone(8_000, 10, 697.0, 1209.0, 4_000.0, 4_000.0, 0.0)),
        (
            16_000,
            tone(16_000, 50, 770.0, 1336.0, 4_000.0, 4_000.0, 0.0),
        ),
        (8_000, vec![1234; 800]),
    ]);
    let mut seed = 0x193_u32;
    let noise: Vec<i16> = (0..8_000)
        .map(|_| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 16) as i16 / 2
        })
        .collect();
    scenarios.push(vec![(8_000, noise)]);
    // A single group and multiple competing lows must not pass dominance checks.
    scenarios.push(vec![(
        8_000,
        tone(8_000, 100, 697.0, 0.0, 4_000.0, 0.0, 0.0),
    )]);
    let mut competing = tone(8_000, 100, 697.0, 1209.0, 3_000.0, 3_000.0, 0.0);
    for (i, sample) in competing.iter_mut().enumerate() {
        *sample += (3_000.0 * (2.0 * PI * 770.0 * i as f64 / 8_000.0).sin()).round() as i16;
    }
    scenarios.push(vec![(8_000, competing)]);
    // Native unchecked integer arithmetic at the internal sample-rate boundary.
    scenarios.push(vec![(53_687_092, vec![1]), (i32::MAX, vec![1, 2])]);
    let input = format!(
        "[{}]",
        scenarios
            .iter()
            .map(|blocks| format!(
                "[{}]",
                blocks
                    .iter()
                    .map(|(rate, pcm)| {
                        let mut row = format!("[{rate}");
                        for sample in pcm {
                            row.push_str(&format!(",{sample}"));
                        }
                        row.push(']');
                        row
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut expected = Vec::<(String, Vec<f64>)>::new();
    for blocks in &scenarios {
        let mut detector = InBandDetector::default();
        for (rate, pcm) in blocks {
            match detector.process(pcm, *rate) {
                Ok(detections) => {
                    for d in detections {
                        expected.push((
                            "d".into(),
                            vec![
                                d.digit as u32 as f64,
                                f64::from(d.sample_rate),
                                f64::from(d.confirmed_duration_ms),
                                d.rms,
                                d.tone_energy_ratio,
                                d.low_group_amplitude,
                                d.high_group_amplitude,
                            ],
                        ));
                    }
                }
                Err(_) => expected.push(("e".into(), vec![])),
            }
            let d = detector.diagnostics();
            expected.push((
                "s".into(),
                vec![
                    d.analyzed_windows as f64,
                    d.audible_windows as f64,
                    d.paired_tone_windows as f64,
                    d.candidate_tone_windows as f64,
                    d.maximum_paired_tone_energy_ratio,
                ],
            ));
        }
    }
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-dtmf-detector-oracle.ps1");
    let mut child = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(fixture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual = String::from_utf8(output.stdout).unwrap();
    let rows: Vec<_> = actual.lines().collect();
    assert_eq!(rows.len(), expected.len());
    for (index, (line, (kind, values))) in rows.iter().zip(&expected).enumerate() {
        let mut parts = line.split(',');
        assert_eq!(parts.next(), Some(kind.as_str()), "row {index}");
        let numbers: Vec<f64> = parts.map(|part| part.parse().unwrap()).collect();
        assert_eq!(numbers.len(), values.len(), "row {index}");
        for (column, (actual, expected)) in numbers.iter().zip(values).enumerate() {
            if (kind == "d" && column < 3) || (kind == "s" && column < 4) {
                assert_eq!(actual, expected, "row {index}, column {column}");
            } else {
                assert!(
                    (actual - expected).abs() <= 1e-8 * expected.abs().max(1.0),
                    "row {index}, column {column}: C# {actual}, Rust {expected}"
                );
            }
        }
    }
}
