use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_phone_bridge::media::{
    AudioFormat, Codec, CodecError, G722Decoder, G722Encoder, OpusDecoder, OpusEncoder,
};

fn number(value: usize) -> IvyJson {
    IvyJson::Number(value as f64)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn bytes(value: &IvyJson) -> Vec<u8> {
    let value = value.as_str().unwrap();
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
fn pcm_hex(pcm: &[i16]) -> IvyJson {
    hex(&pcm
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect::<Vec<_>>())
    .into()
}
fn pcm(value: &IvyJson) -> Vec<i16> {
    bytes(value)
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}
fn packets(values: &[Vec<u8>]) -> Vec<IvyJson> {
    values.iter().map(|value| hex(value).into()).collect()
}
fn format(codec: Codec) -> AudioFormat {
    codec.offered_formats().remove(0)
}
fn request(codec: &str, op: &str, frames: Vec<IvyJson>, resets: &[usize]) -> IvyJson {
    IvyJson::object([
        ("codec", codec.into()),
        ("op", op.into()),
        ("frames", IvyJson::Array(frames)),
        (
            "resets",
            IvyJson::Array(resets.iter().map(|&v| number(v)).collect()),
        ),
    ])
}
fn decode_frame(packet: &[u8], samples: usize, fec: bool) -> IvyJson {
    IvyJson::object([
        ("packet", hex(packet).into()),
        ("samples", number(samples)),
        ("fec", IvyJson::Bool(fec)),
    ])
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
fn trace(value: &IvyJson) {
    if let Some(path) = std::env::var_os("IVY_PHONE_PCM_CODEC_TRACE") {
        std::fs::write(path, stringify(value, 32_000_000).unwrap()).unwrap();
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
            "tests/fixtures/phone-pcm-codec-oracle.ps1",
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
    .expect("Original synthetic PCM codec oracle exceeded its owned deadline");
    let output = output.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let results: Vec<_> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter(|s| !s.is_empty())
        .map(|line| parse(line, 8_000_000).unwrap())
        .collect();
    assert_eq!(results.len(), cases.len());
    results
}
fn frames(reply: &IvyJson) -> &[IvyJson] {
    reply
        .get("frames")
        .unwrap_or_else(|| panic!("Original oracle failed: {reply:?}"))
        .as_array()
        .unwrap()
}
fn comparison(actual: &[Vec<i16>], original: &[IvyJson]) -> IvyJson {
    assert_eq!(actual.len(), original.len());
    let mut maximum = 0_i32;
    let mut squared = 0_f64;
    let mut count = 0_usize;
    for (left, right) in actual.iter().zip(original) {
        let right = pcm(right);
        assert_eq!(left.len(), right.len());
        for (&left, right) in left.iter().zip(right) {
            let error = i32::from(left) - i32::from(right);
            maximum = maximum.max(error.abs());
            squared += f64::from(error).powi(2);
            count += 1;
        }
    }
    IvyJson::object([
        ("samples", number(count)),
        ("maximumAbsoluteError", number(maximum as usize)),
        ("rmsError", IvyJson::Number((squared / count as f64).sqrt())),
    ])
}

#[test]
fn codec_owners_have_independent_state_reset_and_bounded_packet_contracts() {
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

#[tokio::test]
async fn original_g722_bitstreams_and_opus_native_interoperability() {
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original managed PCM codec oracle unavailable; differential was not executed.");
        return;
    }
    let mut g_input: Vec<Vec<i16>> = (0..206)
        .map(|frame| {
            (0..320)
                .map(|sample| ((frame * 320 + sample).min(65535) as i32 - 32768) as i16)
                .collect()
        })
        .collect();
    g_input.extend((0..24).map(|frame| samples(320, frame * 320, 16000)));
    g_input.extend([
        vec![0; 320],
        vec![i16::MIN; 320],
        vec![i16::MAX; 320],
        (0..320)
            .map(|i| if i % 2 == 0 { i16::MIN } else { i16::MAX })
            .collect(),
    ]);
    let g_resets = [0, 47, 206, 231];
    let o_resets = [0, 37, 65];
    let mut g_encoder = G722Encoder::new(&format(Codec::G722)).unwrap();
    let g_packets: Vec<Vec<u8>> = g_input
        .iter()
        .enumerate()
        .map(|(i, input)| {
            if g_resets.contains(&i) {
                g_encoder.reset();
            }
            g_encoder
                .encode_20ms(input.as_slice().try_into().unwrap())
                .unwrap()
                .to_vec()
        })
        .collect();
    let mut arbitrary = Vec::new();
    let mut seed = 0x8f24_5ad1_u32;
    for size in [1, 2, 3, 17, 159, 160, 161, 255, 256, 319, 320, 959, 960] {
        arbitrary.push(
            (0..size)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect::<Vec<_>>(),
        );
    }
    arbitrary.push((0..=255).collect());
    arbitrary.push(vec![0; 960]);
    arbitrary.push(vec![255; 960]);
    let o_input: Vec<_> = (0..80)
        .map(|frame| samples(960, frame * 960, 48000))
        .collect();
    let mut o_encoder = OpusEncoder::new(&format(Codec::Opus)).unwrap();
    let o_packets: Vec<_> = o_input
        .iter()
        .enumerate()
        .map(|(i, input)| {
            if o_resets.contains(&i) {
                o_encoder.reset().unwrap();
            }
            o_encoder
                .encode_20ms(input.as_slice().try_into().unwrap())
                .unwrap()
        })
        .collect();
    let cases = vec![
        request("OPUS", "profile", vec![], &[]),
        request(
            "G722",
            "encode",
            g_input.iter().map(|p| pcm_hex(p)).collect(),
            &g_resets,
        ),
        request("G722", "decode", packets(&g_packets), &g_resets),
        request("G722", "decode", packets(&arbitrary), &[0, 8]),
        request(
            "OPUS",
            "encode",
            o_input.iter().map(|p| pcm_hex(p)).collect(),
            &o_resets,
        ),
        request(
            "OPUS",
            "decode",
            o_packets
                .iter()
                .map(|p| decode_frame(p, 5760, false))
                .collect(),
            &o_resets,
        ),
    ];
    let expected = oracle(&cases).await;
    let mut g_decoder = G722Decoder::new(&format(Codec::G722)).unwrap();
    let g_actual: Vec<_> = g_packets
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if g_resets.contains(&i) {
                g_decoder.reset();
            }
            pcm_hex(&g_decoder.decode(p).unwrap())
        })
        .collect();
    let mut g_decoder = G722Decoder::new(&format(Codec::G722)).unwrap();
    let a_actual: Vec<_> = arbitrary
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if [0, 8].contains(&i) {
                g_decoder.reset();
            }
            pcm_hex(&g_decoder.decode(p).unwrap())
        })
        .collect();
    let mut o_decoder = OpusDecoder::new(&format(Codec::Opus)).unwrap();
    let o_actual: Vec<_> = o_packets
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if o_resets.contains(&i) {
                o_decoder.reset().unwrap();
            }
            o_decoder.decode(p, 5760, false).unwrap()
        })
        .collect();
    let original_packets: Vec<_> = frames(&expected[4]).iter().map(bytes).collect();
    let reverse_case = request(
        "OPUS",
        "decode",
        original_packets
            .iter()
            .map(|p| decode_frame(p, 5760, false))
            .collect(),
        &o_resets,
    );
    let reverse_expected = oracle(std::slice::from_ref(&reverse_case)).await;
    let mut o_decoder = OpusDecoder::new(&format(Codec::Opus)).unwrap();
    let reverse_actual: Vec<_> = original_packets
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if o_resets.contains(&i) {
                o_decoder.reset().unwrap();
            }
            assert_eq!(OpusDecoder::packet_samples(p).unwrap(), 960);
            o_decoder.decode(p, 5760, false).unwrap()
        })
        .collect();
    let metrics = [
        comparison(&o_actual, frames(&expected[5])),
        comparison(&reverse_actual, frames(&reverse_expected[0])),
    ];
    let records: Vec<_> = cases
        .iter()
        .zip(&expected)
        .map(|(input, original)| {
            IvyJson::object([("input", input.clone()), ("original", original.clone())])
        })
        .chain(std::iter::once(IvyJson::object([
            ("input", reverse_case),
            ("original", reverse_expected[0].clone()),
        ])))
        .collect();
    trace(&IvyJson::object([
        ("libopus", opus::version().into()),
        ("records", IvyJson::Array(records)),
        ("g722Encoded", IvyJson::Array(packets(&g_packets))),
        ("g722Decoded", IvyJson::Array(g_actual.clone())),
        ("g722ArbitraryDecoded", IvyJson::Array(a_actual.clone())),
        (
            "opusRustPacketsDecoded",
            IvyJson::Array(o_actual.iter().map(|p| pcm_hex(p)).collect()),
        ),
        (
            "opusOriginalPacketsDecoded",
            IvyJson::Array(reverse_actual.iter().map(|p| pcm_hex(p)).collect()),
        ),
        ("opusDecodeMetrics", IvyJson::Array(metrics.to_vec())),
    ]));
    assert_eq!(
        expected[0],
        IvyJson::object([
            ("bitrate", number(51000)),
            ("complexity", number(9)),
            ("channels", number(1)),
            ("sampleRate", number(48000)),
            ("fec", IvyJson::Bool(false)),
            ("loss", number(0)),
            ("vbr", IvyJson::Bool(true)),
            ("dtx", IvyJson::Bool(false))
        ])
    );
    assert_eq!(
        packets(&g_packets),
        frames(&expected[1]),
        "G722 encoded bitstreams"
    );
    assert_eq!(
        g_actual
            .iter()
            .zip(frames(&expected[2]))
            .filter(|(a, b)| a != b)
            .count(),
        0,
        "G722 decoded frame mismatches"
    );
    assert_eq!(
        a_actual
            .iter()
            .zip(frames(&expected[3]))
            .filter(|(a, b)| a != b)
            .count(),
        0,
        "G722 arbitrary decoded frame mismatches"
    );
    // Compare the two decoders on identical packets, not the lossy output to its
    // source or bitstreams generated by different encoder versions.
    for metric in &metrics {
        assert!(
            metric
                .get("maximumAbsoluteError")
                .unwrap()
                .as_f64()
                .unwrap()
                <= 128.0,
            "{metric:?}"
        );
        assert!(
            metric.get("rmsError").unwrap().as_f64().unwrap() <= 16.0,
            "{metric:?}"
        );
    }
}

#[tokio::test]
async fn opus_packet_durations_real_fec_and_plc_interoperate() {
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
    let mut events = Vec::new();
    let mut output = Vec::new();
    let mut recovered = 0;
    let mut normal = 0;
    let mut index = 0;
    while index < packets.len() {
        if index > 0 && index % 4 == 1 && index + 1 < packets.len() {
            // Compare FEC and PLC from identical complete decoder histories.
            // Prior recovery differences must not inflate this evidence count.
            let mut check_fec = OpusDecoder::new(&format).unwrap();
            let mut check_plc = OpusDecoder::new(&format).unwrap();
            for previous in &packets[..index] {
                check_fec.decode(previous, 5760, false).unwrap();
                check_plc.decode(previous, 5760, false).unwrap();
            }
            if check_fec.decode(&packets[index + 1], 960, true).unwrap()
                != check_plc.decode(&[], 960, false).unwrap()
            {
                recovered += 1;
            }
            let recovered_frame = fec.decode(&packets[index + 1], 960, true).unwrap();
            events.push(decode_frame(&packets[index + 1], 960, true));
            output.push(recovered_frame);
            events.push(decode_frame(&packets[index + 1], 5760, false));
            output.push(fec.decode(&packets[index + 1], 5760, false).unwrap());
            index += 2;
        } else {
            events.push(decode_frame(&packets[index], 5760, false));
            output.push(fec.decode(&packets[index], 5760, false).unwrap());
            normal += 1;
            index += 1;
        }
    }
    // Explicit PLC after a real stream is a separate decoder operation.
    for length in [480, 960, 1920, 5760] {
        events.push(decode_frame(&[], length, false));
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
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none() {
        eprintln!("Original Opus oracle unavailable; duration/FEC/PLC differential not executed.");
        return;
    }
    let cases = vec![
        request(
            "OPUS",
            "decode",
            durations
                .iter()
                .map(|p| decode_frame(p, 5760, false))
                .collect(),
            &(0..durations.len()).collect::<Vec<_>>(),
        ),
        request("OPUS", "decode", events, &[]),
    ];
    let expected = oracle(&cases).await;
    let metrics = [
        comparison(&duration_output, frames(&expected[0])),
        comparison(&output, frames(&expected[1])),
    ];
    if let Some(path) = std::env::var_os("IVY_PHONE_PCM_RECOVERY_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("libopus", opus::version().into()),
                    ("fecRecoveriesDifferentFromPlc", number(recovered)),
                    ("normalFrames", number(normal)),
                    ("metrics", IvyJson::Array(metrics.to_vec())),
                    ("cases", IvyJson::Array(cases)),
                    ("original", IvyJson::Array(expected)),
                    (
                        "actualDurations",
                        IvyJson::Array(duration_output.iter().map(|p| pcm_hex(p)).collect()),
                    ),
                    (
                        "actualRecovery",
                        IvyJson::Array(output.iter().map(|p| pcm_hex(p)).collect()),
                    ),
                ]),
                16_000_000,
            )
            .unwrap(),
        )
        .unwrap();
    }
    for metric in &metrics {
        assert!(
            metric
                .get("maximumAbsoluteError")
                .unwrap()
                .as_f64()
                .unwrap()
                <= 128.0,
            "{metric:?}"
        );
        assert!(
            metric.get("rmsError").unwrap().as_f64().unwrap() <= 16.0,
            "{metric:?}"
        );
    }
    assert!(
        recovered >= 10,
        "Only {recovered} actual FEC recoveries differed from PLC"
    );
}
