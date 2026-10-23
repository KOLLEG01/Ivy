use ivy_phone_bridge::dtmf_detector::InBandDetector;
use std::f64::consts::PI;

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
fn fragmented_pcm_detects_all_sixteen_symbols_at_supported_sample_rates() {
    for rate in [4000, 8000, 16000, 48000] {
        let mut pcm = Vec::new();
        for low in [697.0, 770.0, 852.0, 941.0] {
            for high in [1209.0, 1336.0, 1477.0, 1633.0] {
                pcm.extend(tone(rate, 90, low, high, 4000.0, 4000.0, 900.0));
                pcm.extend(vec![0; rate as usize * 70 / 1000]);
            }
        }
        let mut detector = InBandDetector::default();
        let mut offset = 0;
        let mut found = String::new();
        for length in [1, 79, 160, 37, 333].into_iter().cycle() {
            let end = (offset + length).min(pcm.len());
            for detection in detector.process(&pcm[offset..end], rate).unwrap() {
                assert_eq!(detection.sample_rate, rate);
                assert!(detection.confirmed_duration_ms >= 50);
                assert!(detection.rms.is_finite() && detection.rms > 120.0);
                assert!(
                    detection.tone_energy_ratio.is_finite() && detection.tone_energy_ratio > 0.5
                );
                found.push(detection.digit);
            }
            offset = end;
            if offset == pcm.len() {
                break;
            }
        }
        assert_eq!(found, "123A456B789C*0#D", "{rate}");
        let quality = detector.diagnostics();
        assert!(quality.analyzed_windows >= quality.audible_windows);
        assert!(quality.audible_windows >= quality.candidate_tone_windows);
    }
}
#[test]
fn silent_single_frequency_short_and_noise_blocks_cannot_confirm_a_key() {
    let mut seed = 0x193u32;
    let noise = (0..8000)
        .map(|_| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 16) as i16 / 2
        })
        .collect::<Vec<_>>();
    for pcm in [
        vec![0; 8000],
        vec![10000; 8000],
        tone(8000, 100, 697.0, 1209.0, 4000.0, 0.0, 0.0),
        tone(8000, 30, 697.0, 1209.0, 4000.0, 4000.0, 0.0),
        tone(8000, 100, 697.0, 1209.0, 50.0, 50.0, 0.0),
        noise,
    ] {
        let mut detector = InBandDetector::default();
        assert!(detector.process(&pcm, 8000).unwrap().is_empty());
    }
}
