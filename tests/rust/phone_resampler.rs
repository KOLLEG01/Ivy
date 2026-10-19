use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_phone_bridge::media::{CodecError, PcmResampler};

const RATES: [(usize, usize); 5] = [
    (48000, 8000),
    (48000, 16000),
    (8000, 48000),
    (16000, 48000),
    (48000, 48000),
];
fn number(value: usize) -> IvyJson {
    IvyJson::Number(value as f64)
}
fn scalar(value: f64) -> IvyJson {
    IvyJson::Number(value)
}
fn hex(values: &[f32]) -> IvyJson {
    values
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        .into()
}
fn pcm(value: &IvyJson) -> Vec<f32> {
    value
        .as_str()
        .unwrap()
        .as_bytes()
        .chunks_exact(8)
        .map(|word| {
            let mut bytes = [0; 4];
            for (i, byte) in bytes.iter_mut().enumerate() {
                *byte =
                    u8::from_str_radix(std::str::from_utf8(&word[2 * i..2 * i + 2]).unwrap(), 16)
                        .unwrap();
            }
            f32::from_le_bytes(bytes)
        })
        .collect()
}
fn waveform(rate: usize, length: usize, frequency: f64) -> Vec<f32> {
    (0..length)
        .map(|i| {
            if frequency == 0.0 {
                0.2
            } else {
                (0.2 * (std::f64::consts::TAU * frequency * i as f64 / rate as f64).sin()) as f32
            }
        })
        .collect()
}
fn convert(
    source: usize,
    target: usize,
    input: &[f32],
    chunks: &[usize],
    resets: &[usize],
) -> Vec<Vec<f32>> {
    let mut filter = PcmResampler::new(source, target).unwrap();
    let mut output = vec![99.0; filter.maximum_output_samples()];
    let mut result = Vec::new();
    let mut position = 0;
    for (index, &length) in chunks.iter().enumerate() {
        if resets.contains(&index) {
            filter.reset();
        }
        let count = filter
            .process(&input[position..position + length], &mut output)
            .unwrap();
        assert!(output[count..].iter().all(|&s| s == 0.0));
        result.push(output[..count].to_vec());
        position += length;
    }
    assert_eq!(position, input.len());
    result
}
fn chunks(length: usize, sizes: &[usize]) -> Vec<usize> {
    let mut result = Vec::new();
    let mut remaining = length;
    while remaining > 0 {
        let count = sizes[result.len() % sizes.len()].min(remaining);
        result.push(count);
        remaining -= count;
    }
    result
}
fn flatten(values: &[Vec<f32>]) -> Vec<f32> {
    values.iter().flatten().copied().collect()
}
fn peak(values: &[f32]) -> usize {
    values
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0
}
fn tone_amplitude(values: &[f32], frequency: f64, rate: usize) -> f64 {
    let skip = rate / 10;
    let values = &values[skip..values.len() - rate / 50];
    if frequency == 0.0 {
        return values.iter().map(|&v| v as f64).sum::<f64>() / values.len() as f64;
    }
    let (sine, cosine) = values
        .iter()
        .enumerate()
        .fold((0.0, 0.0), |(s, c), (i, &v)| {
            let phase = std::f64::consts::TAU * frequency * i as f64 / rate as f64;
            (s + v as f64 * phase.sin(), c + v as f64 * phase.cos())
        });
    2.0 * (sine * sine + cosine * cosine).sqrt() / values.len() as f64
}
fn energy(values: &[f32], rate: usize) -> f64 {
    let values = &values[rate / 10..values.len() - rate / 50];
    (values.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

#[test]
fn pcm_resampler_bounds_identity_and_rejected_input_preserve_history() {
    for pair in [
        (0, 48000),
        (44100, 48000),
        (48000, 44100),
        (8000, 16000),
        (8000, 8000),
        (96000, 48000),
    ] {
        assert!(matches!(
            PcmResampler::new(pair.0, pair.1),
            Err(CodecError::InvalidFormat)
        ));
    }
    for (source, target) in RATES {
        let mut actual = PcmResampler::new(source, target).unwrap();
        let mut expected = PcmResampler::new(source, target).unwrap();
        let input = waveform(source, source / 50, 997.0);
        let mut output = vec![7.0; actual.maximum_output_samples()];
        let mut other = output.clone();
        assert_eq!(actual.maximum_input_samples(), source * 120 / 1000);
        for bad in [
            vec![],
            vec![0.0; source * 120 / 1000 + 1],
            vec![f32::NAN],
            vec![f32::INFINITY],
            vec![f32::NEG_INFINITY],
        ] {
            output.fill(7.0);
            assert_eq!(
                actual.process(&bad, &mut output),
                Err(CodecError::InvalidFrame)
            );
            assert!(output.iter().all(|&s| s == 0.0));
        }
        assert_eq!(
            actual.process(&input, &mut []),
            Err(CodecError::InvalidFrame)
        );
        assert_eq!(
            actual.process(&input, &mut output).unwrap(),
            expected.process(&input, &mut other).unwrap()
        );
        assert_eq!(output, other);
        if source == target {
            assert_eq!(actual.startup_samples(), 0);
            assert_eq!(&output[..input.len()], input);
        } else {
            assert!(actual.startup_samples() > 0);
            assert!(actual.startup_samples() <= target / 200);
        }
    }
}

#[test]
fn pcm_resampler_continuous_clock_arbitrary_splits_and_reset() {
    let mut summaries = Vec::new();
    for (source, target) in RATES {
        let input = waveform(source, source * 4, 997.0);
        let regular = convert(
            source,
            target,
            &input,
            &chunks(input.len(), &[source / 50]),
            &[],
        );
        // Feed single samples as well as every legal receive duration. A split
        // changes neither the stream nor its clock, including fractional phases.
        let sizes = [
            1,
            source / 400,
            source / 200,
            source / 100,
            source / 50,
            source * 40 / 1000,
            source * 60 / 1000,
            source * 120 / 1000,
            3,
            7,
        ];
        let irregular = convert(source, target, &input, &chunks(input.len(), &sizes), &[]);
        let regular_pcm = flatten(&regular);
        let irregular_pcm = flatten(&irregular);
        assert_eq!(regular_pcm.len(), irregular_pcm.len(), "{source}->{target}");
        let difference = regular_pcm
            .iter()
            .zip(&irregular_pcm)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            difference < 2e-5,
            "split difference {source}->{target}: {difference}"
        );
        assert_eq!(
            target * 4 - regular_pcm.len(),
            PcmResampler::new(source, target).unwrap().startup_samples()
        );
        for frame in regular.iter().skip(1) {
            assert_eq!(frame.len(), target / 50);
        }
        let jump = regular_pcm[target / 10..]
            .windows(2)
            .map(|s| (s[1] - s[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            jump < 0.17,
            "boundary continuity {source}->{target}: {jump}"
        );
        let mut reused = PcmResampler::new(source, target).unwrap();
        let mut scratch = vec![0.0; reused.maximum_output_samples()];
        for frame in input.chunks(source / 50) {
            reused.process(frame, &mut scratch).unwrap();
        }
        reused.reset();
        let silence = vec![0.0; source / 50];
        let count = reused.process(&silence, &mut scratch).unwrap();
        assert!(
            scratch.iter().all(|&v| v == 0.0),
            "old generation survived reset"
        );
        let clean = convert(source, target, &input[..source / 50], &[source / 50], &[]);
        reused.reset();
        let fresh_count = reused.process(&input[..source / 50], &mut scratch).unwrap();
        assert_eq!(&scratch[..fresh_count], clean[0]);
        summaries.push(IvyJson::object([
            ("sourceRate", number(source)),
            ("targetRate", number(target)),
            ("samples", number(regular_pcm.len())),
            ("maximumSplitDifference", scalar(difference as f64)),
            ("maximumJump", scalar(jump as f64)),
            ("resetSilenceSamples", number(count)),
        ]));
    }
    trace(
        "IVY_PHONE_RESAMPLE_STREAM_TRACE",
        &IvyJson::Array(summaries),
    );
}

fn trace(variable: &str, value: &IvyJson) {
    if let Some(path) = std::env::var_os(variable) {
        std::fs::write(path, stringify(value, 64_000_000).unwrap()).unwrap();
    }
}
async fn oracle(cases: &[IvyJson]) -> Vec<IvyJson> {
    use std::process::Stdio;
    use tokio::{
        io::AsyncWriteExt,
        process::Command,
        time::{Duration, timeout},
    };
    let mut input = Vec::new();
    for case in cases {
        input.extend(stringify(case, 4_000_000).unwrap().as_bytes());
        input.push(b'\n');
    }
    let mut child = Command::new("pwsh")
        .args([
            "-NoProfile",
            "-File",
            "tests/fixtures/phone-resampler-oracle.ps1",
        ])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let (_, output) = timeout(Duration::from_secs(60), async {
        tokio::join!(
            async move {
                stdin.write_all(&input).await.unwrap();
                stdin.shutdown().await.unwrap();
            },
            child.wait_with_output()
        )
    })
    .await
    .expect("WDL synthetic oracle exceeded its owned deadline");
    let output = output.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values: Vec<_> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| parse(s, 32_000_000).unwrap())
        .collect();
    assert_eq!(values.len(), cases.len());
    values
}

#[tokio::test]
async fn pcm_resampler_original_wdl_delay_gain_alias_and_stream_comparison() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!(
            "Original WDL engine unavailable; differential resampling check was not executed."
        );
        return;
    }
    let mut cases = Vec::new();
    let mut candidates = Vec::new();
    let mut definitions = Vec::new();
    for (source, target) in RATES {
        let nyquist = source.min(target) as f64 / 2.0;
        let mut signals = vec![
            ("dc", 0.0),
            ("tone", 300.0),
            ("tone", 997.0),
            ("tone", nyquist * 0.7),
            ("tone", nyquist * 0.85),
            ("impulse", 0.0),
        ];
        if source > target {
            signals.extend([
                ("alias", nyquist * 1.2),
                ("alias", nyquist * 1.5),
                ("alias", source as f64 * 0.45),
            ]);
        }
        for (kind, frequency) in signals {
            let mut input = waveform(source, source / 2, frequency);
            if kind == "impulse" {
                input.fill(0.0);
                input[source / 25] = 0.2;
            }
            let lengths = chunks(input.len(), &[source / 50]);
            let frames = input.chunks(source / 50).map(hex).collect();
            cases.push(IvyJson::object([
                ("sourceRate", number(source)),
                ("targetRate", number(target)),
                ("frames", IvyJson::Array(frames)),
                ("resets", IvyJson::Array(vec![])),
            ]));
            candidates.push(convert(source, target, &input, &lengths, &[]));
            definitions.push((source, target, kind, frequency));
        }
    }
    let original = oracle(&cases).await;
    trace(
        "IVY_PHONE_RESAMPLE_ORACLE_TRACE",
        &IvyJson::object([
            ("requests", IvyJson::Array(cases.clone())),
            ("original", IvyJson::Array(original.clone())),
            (
                "candidate",
                IvyJson::Array(
                    candidates
                        .iter()
                        .map(|frames| IvyJson::Array(frames.iter().map(|v| hex(v)).collect()))
                        .collect(),
                ),
            ),
        ]),
    );
    let mut reports = Vec::new();
    for (index, ((source, target, kind, frequency), (actual, reply))) in definitions
        .into_iter()
        .zip(candidates.iter().zip(&original))
        .enumerate()
    {
        assert!(
            reply.get("error").is_none(),
            "original WDL case {index}: {reply:?}"
        );
        let reference: Vec<Vec<f32>> = reply
            .get("frames")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(pcm)
            .collect();
        let a = flatten(actual);
        let b = flatten(&reference);
        let mut report = vec![
            ("sourceRate", number(source)),
            ("targetRate", number(target)),
            ("kind", kind.into()),
            ("frequency", scalar(frequency)),
            (
                "candidateCounts",
                IvyJson::Array(actual.iter().map(|v| number(v.len())).collect()),
            ),
            (
                "originalCounts",
                IvyJson::Array(reference.iter().map(|v| number(v.len())).collect()),
            ),
        ];
        if kind == "impulse" {
            let a_delay = peak(&a) as isize - (target / 25) as isize;
            let b_delay = peak(&b) as isize - (target / 25) as isize;
            assert!(
                a_delay >= 0 && a_delay <= (target / 200) as isize,
                "candidate impulse delay {source}->{target}: {a_delay}"
            );
            report.extend([
                ("candidatePeakDelaySamples", scalar(a_delay as f64)),
                ("originalPeakDelaySamples", scalar(b_delay as f64)),
                (
                    "startupHoldbackSamples",
                    number(PcmResampler::new(source, target).unwrap().startup_samples()),
                ),
            ]);
        } else if kind == "alias" {
            let a_db = 20.0
                * (energy(&a, target) / (0.2 / 2.0_f64.sqrt()))
                    .max(1e-12)
                    .log10();
            let b_db = 20.0
                * (energy(&b, target) / (0.2 / 2.0_f64.sqrt()))
                    .max(1e-12)
                    .log10();
            assert!(
                a_db < -60.0,
                "alias rejection {source}->{target} at {frequency}: {a_db}dB"
            );
            report.extend([
                ("candidateAliasDb", scalar(a_db)),
                ("originalAliasDb", scalar(b_db)),
            ]);
        } else {
            let gain = tone_amplitude(&a, frequency, target) / 0.2;
            let original_gain = tone_amplitude(&b, frequency, target) / 0.2;
            assert!(
                (0.94..1.06).contains(&gain),
                "passband gain {source}->{target} at {frequency}: {gain}"
            );
            report.extend([
                ("candidateGain", scalar(gain)),
                ("originalGain", scalar(original_gain)),
            ]);
            if source < target && frequency > 0.0 {
                let image = tone_amplitude(&a, source as f64 - frequency, target);
                let reference_image = tone_amplitude(&b, source as f64 - frequency, target);
                let image_db = 20.0 * (image / 0.2).max(1e-12).log10();
                assert!(
                    image_db < -60.0,
                    "interpolation image {source}->{target}: {image_db}dB"
                );
                report.extend([
                    ("candidateImageDb", scalar(image_db)),
                    (
                        "originalImageDb",
                        scalar(20.0 * (reference_image / 0.2).max(1e-12).log10()),
                    ),
                ]);
            }
        }
        reports.push(IvyJson::object(report));
    }
    trace(
        "IVY_PHONE_RESAMPLE_ORACLE_TRACE",
        &IvyJson::object([
            ("requests", IvyJson::Array(cases)),
            ("original", IvyJson::Array(original)),
            (
                "candidate",
                IvyJson::Array(
                    candidates
                        .iter()
                        .map(|f| IvyJson::Array(f.iter().map(|v| hex(v)).collect()))
                        .collect(),
                ),
            ),
            ("reports", IvyJson::Array(reports)),
        ]),
    );
}
