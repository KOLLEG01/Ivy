use ivy_contracts::{IvyJson, json::stringify};
use ivy_phone_bridge::media::{
    AudioFormat, Codec, CodecError, G722Decoder, G722Encoder, OpusDecoder, OpusEncoder,
};

fn number(value: usize) -> IvyJson {
    IvyJson::Number(value as f64)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn format(codec: Codec) -> AudioFormat {
    codec.offered_formats().remove(0)
}
fn samples(length: usize, offset: usize, rate: usize) -> Vec<i16> {
    (offset..offset + length)
        .map(|index| {
            // Deterministic changing harmonics, quiet intervals, impulses and noise.
            let t = index as f64 / rate as f64;
            let pitch = 170.0 + 18.0 * (t * 7.0).sin();
            let voice = (t * std::f64::consts::TAU * pitch).sin() * 11000.0
                + (t * std::f64::consts::TAU * 3.0 * pitch).sin() * 2400.0;
            let noise = ((index as u32)
                .wrapping_mul(1664525)
                .wrapping_add(1013904223)
                >> 20) as f64
                - 2048.0;
            if index % (rate / 2) < rate / 30 {
                0
            } else if index % 733 == 0 {
                i16::MAX
            } else {
                (voice + noise / 3.0) as i16
            }
        })
        .collect()
}
#[tokio::test]
async fn published_opus_rfc8251_mono_decoder_vectors() {
    use std::{io::Write, process::Stdio};
    use tokio::{
        process::Command,
        time::{Duration, timeout},
    };
    let (Some(directory), Some(compare), Some(outputs)) = (
        std::env::var_os("IVY_PHONE_OPUS_VECTOR_DIR"),
        std::env::var_os("IVY_PHONE_OPUS_COMPARE"),
        std::env::var_os("IVY_PHONE_PCM_CODEC_OUTPUT_DIR"),
    ) else {
        eprintln!(
            "Published Opus vectors/reference tool unavailable; conformance check was not executed."
        );
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let outputs = std::path::PathBuf::from(outputs);
    let mut reports = Vec::new();
    for index in 1..=12 {
        let name = format!("testvector{index:02}");
        let input = std::fs::read(directory.join(format!("{name}.bit"))).unwrap();
        let path = outputs.join(format!("{name}-mono.pcm"));
        let mut file = std::fs::File::create(&path).unwrap();
        let mut decoder = OpusDecoder::new(&format(Codec::Opus)).unwrap();
        let mut reference = opus::Decoder::new(48000, opus::Channels::Mono).unwrap();
        let mut offset = 0;
        let mut packets = 0;
        let mut samples = 0;
        while offset < input.len() {
            assert!(input.len() - offset >= 8);
            let size = u32::from_be_bytes(input[offset..offset + 4].try_into().unwrap()) as usize;
            let range = u32::from_be_bytes(input[offset + 4..offset + 8].try_into().unwrap());
            offset += 8;
            assert!((1..=8192).contains(&size) && size <= input.len() - offset);
            let packet = &input[offset..offset + size];
            offset += size;
            let expected_samples = OpusDecoder::packet_samples(packet).unwrap();
            let output = decoder.decode(packet, 5760, false).unwrap();
            assert_eq!(output.len(), expected_samples);
            let mut direct = [0; 5760];
            let count = reference.decode(packet, &mut direct, false).unwrap();
            assert_eq!(
                output,
                direct[..count],
                "Wrapper PCM differs from the same real libopus decoder"
            );
            assert_eq!(
                reference.get_final_range().unwrap(),
                range,
                "{name}: packet{packets} final entropy range"
            );
            for sample in &output {
                file.write_all(&sample.to_le_bytes()).unwrap();
            }
            packets += 1;
            samples += output.len();
        }
        drop(file);
        let mut comparisons = Vec::new();
        let mut accepted = false;
        // RFC8251 permits either reference set (CELT phase inversion freedom).
        for suffix in ["", "m"] {
            let output = timeout(
                Duration::from_secs(60),
                Command::new(&compare)
                    .args(["-r", "48000"])
                    .arg(directory.join(format!("{name}{suffix}.dec")))
                    .arg(&path)
                    .kill_on_drop(true)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output(),
            )
            .await
            .expect("Published Opus comparison exceeded deadline")
            .unwrap();
            let log = String::from_utf8(output.stderr).unwrap();
            let quality = log
                .lines()
                .find_map(|line| line.strip_prefix("Opus quality metric: "))
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse::<f64>().ok());
            let passes = output.status.success() && quality.is_some_and(|q| q >= 90.0);
            accepted |= passes;
            comparisons.push(IvyJson::object([
                ("referenceSuffix", suffix.into()),
                ("passed", IvyJson::Bool(passes)),
                ("log", log.into()),
            ]));
        }
        reports.push(IvyJson::object([
            ("vector", name.into()),
            ("packets", number(packets)),
            ("samples", number(samples)),
            ("allFinalRangesMatch", IvyJson::Bool(true)),
            ("comparisons", IvyJson::Array(comparisons)),
        ]));
        std::fs::write(
            outputs.join("opus-vector-comparisons.json"),
            stringify(&IvyJson::Array(reports.clone()), 1_000_000).unwrap(),
        )
        .unwrap();
        assert!(
            accepted,
            "Published vector{index} fails reference Opus metric"
        );
    }
}
#[test]
fn pcm_native_codec_owners_have_independent_state_reset_and_bounded_packet_contracts() {
    let g = format(Codec::G722);
    let o = format(Codec::Opus);
    let wrong = format(Codec::Pcma);
    assert!(matches!(
        G722Encoder::new(&wrong),
        Err(CodecError::InvalidFormat)
    ));
    assert!(matches!(
        G722Decoder::new(&o),
        Err(CodecError::InvalidFormat)
    ));
    assert!(matches!(
        OpusEncoder::new(&g),
        Err(CodecError::InvalidFormat)
    ));
    assert!(matches!(
        OpusDecoder::new(&wrong),
        Err(CodecError::InvalidFormat)
    ));
    let input: [i16; 320] = samples(320, 2000, 16000).try_into().unwrap();
    let mut encoder = G722Encoder::new(&g).unwrap();
    let first = encoder.encode_20ms(&input).unwrap();
    assert_ne!(first, encoder.encode_20ms(&input).unwrap());
    encoder.reset();
    assert_eq!(first, encoder.encode_20ms(&input).unwrap());
    assert_eq!(
        first,
        G722Encoder::new(&g).unwrap().encode_20ms(&input).unwrap()
    );
    let mut decoder = G722Decoder::new(&g).unwrap();
    let decoded = decoder.decode(&first).unwrap();
    assert_ne!(decoded, decoder.decode(&first).unwrap());
    decoder.reset();
    assert_eq!(decoded, decoder.decode(&first).unwrap());
    assert!(decoder.decode(&[]).is_err());
    assert!(decoder.decode(&[0; 961]).is_err());
    assert_eq!(decoder.decode(&[0; 960]).unwrap().len(), 1920);
    let input: [i16; 960] = samples(960, 4800, 48000).try_into().unwrap();
    let mut encoder = OpusEncoder::new(&o).unwrap();
    let first = encoder.encode_20ms(&input).unwrap();
    assert_ne!(first, encoder.encode_20ms(&input).unwrap());
    encoder.reset().unwrap();
    assert_eq!(first, encoder.encode_20ms(&input).unwrap());
    assert_eq!(
        opus::packet::get_nb_channels(&first).unwrap(),
        opus::Channels::Mono
    );
    let mono = AudioFormat::new(112, "OPUS", 48000, 48000, 1, None).unwrap();
    assert_eq!(
        first,
        OpusEncoder::new(&mono)
            .unwrap()
            .encode_20ms(&input)
            .unwrap()
    );
    assert_eq!(OpusDecoder::packet_samples(&first).unwrap(), 960);
    let mut decoder = OpusDecoder::new(&o).unwrap();
    let decoded = decoder.decode(&first, 5760, false).unwrap();
    decoder.decode(&first, 5760, false).unwrap();
    decoder.reset().unwrap();
    assert_eq!(decoded, decoder.decode(&first, 5760, false).unwrap());
    assert!(decoder.decode(&first, 959, false).is_err());
    assert!(decoder.decode(&first, 0, false).is_err());
    assert!(decoder.decode(&first, 5761, false).is_err());
    assert!(decoder.decode(&[0; 8193], 960, false).is_err());
    assert!(decoder.decode(&[], 960, true).is_err());
    assert!(OpusDecoder::packet_samples(&[]).is_err());
    assert!(OpusDecoder::packet_samples(&[0; 8193]).is_err());
    assert_eq!(decoder.decode(&[], 960, false).unwrap().len(), 960);
}

#[test]
fn published_itu_g722_speech_corpus_matches_every_codeword_and_sample() {
    let Some(path) = std::env::var_os("IVY_PHONE_G722_VECTOR_DIR") else {
        eprintln!("Published ITU G722 corpus unavailable; vector check was not executed.");
        return;
    };
    // The publisher's CMake tests use codspw.cod, one code byte per LE16
    // word. Its byte-only codsp.cod has swapped adjacent code bytes. Input
    // and output PCM in this pinned corpus are LE16, unlike its old README.
    let path = std::path::PathBuf::from(path);
    let input = std::fs::read(path.join("inpsp.bin")).unwrap();
    let codewords = std::fs::read(path.join("codspw.cod")).unwrap();
    let expected = std::fs::read(path.join("outsp1.bin")).unwrap();
    assert_eq!(
        (input.len(), codewords.len(), expected.len()),
        (195072, 97536, 195072)
    );
    let input: Vec<_> = input
        .chunks_exact(2)
        .map(|p| i16::from_le_bytes([p[0], p[1]]))
        .collect();
    let expected: Vec<_> = expected
        .chunks_exact(2)
        .map(|p| i16::from_le_bytes([p[0], p[1]]))
        .collect();
    let codewords: Vec<_> = codewords
        .chunks_exact(2)
        .map(|p| {
            assert_eq!(p[1], 0);
            p[0]
        })
        .collect();
    let mut encoder = G722Encoder::new(&format(Codec::G722)).unwrap();
    let mut actual = Vec::new();
    for chunk in input.chunks(320) {
        let mut frame = [0; 320];
        frame[..chunk.len()].copy_from_slice(chunk);
        actual.extend(encoder.encode_20ms(&frame).unwrap());
    }
    assert_eq!(
        actual[..codewords.len()]
            .iter()
            .zip(&codewords)
            .filter(|(a, b)| a != b)
            .count(),
        0,
        "ITU encoder codeword mismatches"
    );
    let mut decoder = G722Decoder::new(&format(Codec::G722)).unwrap();
    let actual: Vec<_> = codewords
        .chunks(160)
        .flat_map(|p| decoder.decode(p).unwrap())
        .collect();
    assert_eq!(actual.len(), expected.len());
    assert_eq!(
        actual.iter().zip(&expected).filter(|(a, b)| a != b).count(),
        0,
        "ITU64k decoder PCM mismatches"
    );
}

#[test]
fn pcm_native_opus_packet_durations_real_fec_and_plc_interoperate() {
    let format = format(Codec::Opus);
    let mut durations = Vec::new();
    let mut duration_output = Vec::new();
    for length in [120, 240, 480, 960, 1920, 2880] {
        let mut encoder =
            opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Audio).unwrap();
        let mut packet = vec![0; 8192];
        let count = encoder
            .encode(&samples(length, 4800, 48000), &mut packet)
            .unwrap();
        packet.truncate(count);
        assert_eq!(OpusDecoder::packet_samples(&packet).unwrap(), length);
        let mut decoder = OpusDecoder::new(&format).unwrap();
        duration_output.push(decoder.decode(&packet, 5760, false).unwrap());
        durations.push(packet);
    }
    let mut encoder =
        opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::LowDelay).unwrap();
    let mut parts = Vec::new();
    for frame in 0..6 {
        let mut p = vec![0; 8192];
        let count = encoder
            .encode(&samples(960, 4800 + frame * 960, 48000), &mut p)
            .unwrap();
        p.truncate(count);
        parts.push(p);
    }
    let mut packet = vec![0; 8192];
    let count = opus::Repacketizer::new()
        .unwrap()
        .combine(
            &parts.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            &mut packet,
        )
        .unwrap();
    packet.truncate(count);
    assert_eq!(OpusDecoder::packet_samples(&packet).unwrap(), 5760);
    duration_output.push(
        OpusDecoder::new(&format)
            .unwrap()
            .decode(&packet, 5760, false)
            .unwrap(),
    );
    durations.push(packet);
    // This synthetic sender differs deliberately from the original/default
    // product encoder: SILK wideband with loss/FEC enabled produces actual LBRR.
    let mut sender =
        opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Voip).unwrap();
    sender.set_bitrate(opus::Bitrate::Bits(24000)).unwrap();
    sender.set_vbr(false).unwrap();
    sender.set_inband_fec(true).unwrap();
    sender.set_packet_loss_perc(30).unwrap();
    sender.set_max_bandwidth(opus::Bandwidth::Wideband).unwrap();
    sender.set_bandwidth(opus::Bandwidth::Wideband).unwrap();
    sender.set_signal(opus::Signal::Voice).unwrap();
    let mut packets = Vec::new();
    for frame in 0..80 {
        let mut p = vec![0; 8192];
        let count = sender
            .encode(&samples(960, 4800 + frame * 960, 48000), &mut p)
            .unwrap();
        p.truncate(count);
        packets.push(p);
    }
    let mut fec = OpusDecoder::new(&format).unwrap();
    let mut output = Vec::new();
    let mut recovered = 0;
    let mut fec_error = 0_f64;
    let mut plc_error = 0_f64;
    let mut improvements = 0;
    let mut normal = 0;
    let mut index = 0;
    while index < packets.len() {
        if index > 0 && index % 4 == 1 && index + 1 < packets.len() {
            // Compare FEC and PLC from identical complete decoder histories.
            // Prior recovery differences must not inflate this evidence count.
            let mut check_fec = OpusDecoder::new(&format).unwrap();
            let mut check_plc = OpusDecoder::new(&format).unwrap();
            let mut check_ideal = OpusDecoder::new(&format).unwrap();
            for previous in &packets[..index] {
                check_fec.decode(previous, 5760, false).unwrap();
                check_plc.decode(previous, 5760, false).unwrap();
                check_ideal.decode(previous, 5760, false).unwrap();
            }
            let recovered_pcm = check_fec.decode(&packets[index + 1], 960, true).unwrap();
            let concealed_pcm = check_plc.decode(&[], 960, false).unwrap();
            if recovered_pcm != concealed_pcm {
                recovered += 1;
                let ideal = check_ideal.decode(&packets[index], 5760, false).unwrap();
                let error = |output: &[i16]| {
                    output
                        .iter()
                        .zip(&ideal)
                        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
                        .sum::<f64>()
                };
                let f = error(&recovered_pcm);
                let p = error(&concealed_pcm);
                fec_error += f;
                plc_error += p;
                if f < p {
                    improvements += 1;
                }
            }
            let recovered_frame = fec.decode(&packets[index + 1], 960, true).unwrap();

            output.push(recovered_frame);

            output.push(fec.decode(&packets[index + 1], 5760, false).unwrap());
            index += 2;
        } else {
            output.push(fec.decode(&packets[index], 5760, false).unwrap());
            normal += 1;
            index += 1;
        }
    }
    // Explicit PLC after a real stream is a separate decoder operation.
    for length in [480, 960, 1920, 5760] {
        output.push(fec.decode(&[], length, false).unwrap());
    }
    if let Some(path) = std::env::var_os("IVY_PHONE_PCM_RECOVERY_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("libopus", opus::version().into()),
                    ("fecRecoveriesDifferentFromPlc", number(recovered)),
                    (
                        "packets",
                        IvyJson::Array(packets.iter().map(|p| hex(p).into()).collect()),
                    ),
                    (
                        "durationPackets",
                        IvyJson::Array(durations.iter().map(|p| hex(p).into()).collect()),
                    ),
                    (
                        "configuredFec",
                        IvyJson::Bool(sender.get_inband_fec().unwrap()),
                    ),
                    (
                        "configuredLoss",
                        number(sender.get_packet_loss_perc().unwrap() as usize),
                    ),
                ]),
                1_000_000,
            )
            .unwrap(),
        )
        .unwrap();
    }
    for (frame, length) in duration_output
        .iter()
        .zip([120, 240, 480, 960, 1920, 2880, 5760])
    {
        assert_eq!(frame.len(), length);
    }
    assert_eq!(output.len(), packets.len() + 4);
    assert!(
        output[..packets.len()]
            .iter()
            .all(|frame| frame.len() == 960)
    );
    for (frame, length) in output[packets.len()..].iter().zip([480, 960, 1920, 5760]) {
        assert_eq!(frame.len(), length);
    }
    assert!(normal > 0);
    assert!(
        recovered >= 10,
        "Only {recovered} actual FEC recoveries differed from PLC"
    );
    assert!(
        improvements >= 6 && fec_error < plc_error,
        "Actual FEC must improve on PLC from the same history"
    );
}
