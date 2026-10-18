#[path = "../../services/chat-bridge/rust/src/whatsapp/audio_processing.rs"]
mod audio_processing;
use audio_processing::{AudioError, DecodedAudio};

fn samples(wav: &[u8]) -> Vec<i16> {
    wav[44..]
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect()
}

#[test]
fn negative_half_ties_nonfinite_and_clipping_match_js() {
    let channel = [
        -1.0_f32,
        -0.5 / 32768.0,
        -1.5 / 32768.0,
        f32::NAN,
        f32::INFINITY,
        2.0,
    ];
    let audio = DecodedAudio {
        channels: &[&channel],
        sample_rate: 16000.0,
    };
    let wav = audio.encode_mono_pcm16_wav(16000).unwrap();
    assert_eq!(samples(&wav), [-32768, 0, -1, 0, 0, 32767]);
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize,
        wav.len() - 8
    );
}

#[test]
fn shortest_channel_controls_duration_and_linear_resampling() {
    let audio = DecodedAudio {
        channels: &[&[0.0, 1.0, 0.0], &[0.0, 0.0]],
        sample_rate: 8000.0,
    };
    assert_eq!(audio.duration_seconds().unwrap(), 2.0 / 8000.0);
    assert_eq!(
        samples(&audio.encode_mono_pcm16_wav(16000).unwrap()),
        [0, 8192, 16384, 16384]
    );
}

#[test]
fn malformed_decoded_audio_fails_before_allocation() {
    assert_eq!(
        DecodedAudio {
            channels: &[],
            sample_rate: 16000.0
        }
        .duration_seconds(),
        Err(AudioError::NoSamples)
    );
    assert_eq!(
        DecodedAudio {
            channels: &[&[]],
            sample_rate: 16000.0
        }
        .duration_seconds(),
        Err(AudioError::NoSamples)
    );
    assert_eq!(
        DecodedAudio {
            channels: &[&[1.0]],
            sample_rate: f64::NAN
        }
        .duration_seconds(),
        Err(AudioError::InvalidSourceRate)
    );
    let audio = DecodedAudio {
        channels: &[&[1.0]],
        sample_rate: 16000.0,
    };
    assert_eq!(
        audio.encode_mono_pcm16_wav(0),
        Err(AudioError::InvalidTargetRate)
    );
}

#[test]
fn wav_bytes_match_the_existing_typescript_implementation() {
    use std::process::Command;
    let root = option_env!("CARGO_MANIFEST_DIR")
        .map(|path| std::path::Path::new(path).join("../../.."))
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let oracle = root
        .join("services/chat-bridge/src/whatsapp/audio-processing.ts")
        .canonicalize()
        .unwrap();
    let url = format!(
        "file:///{}",
        oracle
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/")
    );
    let channel: Vec<f32> = (0..257).map(|i| (i as f32 * 0.17).sin() * 1.4).collect();
    let other: Vec<f32> = channel.iter().rev().copied().take(253).collect();
    let hex = |values: &[f32]| {
        values
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let code = format!(
        r#"
      import {{ encodeMonoPcm16Wav }} from '{}';
      const channels = ['{}','{}'].map(h => new Float32Array(Uint8Array.from(Buffer.from(h,'hex')).buffer));
      for (const rate of [8000, 44100, 48000, 96000]) {{
        const wav = encodeMonoPcm16Wav({{ channelData:channels, sampleRate:rate }}, 16000);
        const n = Buffer.alloc(4); n.writeUInt32LE(wav.length); process.stdout.write(n); process.stdout.write(wav);
      }}
    "#,
        url,
        hex(&channel),
        hex(&other)
    );
    let output = Command::new("node")
        .args(["--input-type=module", "-e", &code])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut bytes = output.stdout.as_slice();
    for rate in [8000.0, 44100.0, 48000.0, 96000.0] {
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let audio = DecodedAudio {
            channels: &[&channel, &other],
            sample_rate: rate,
        };
        assert_eq!(
            audio
                .encode_mono_pcm16_wav(audio_processing::CODEX_AUDIO_SAMPLE_RATE_HERTZ)
                .unwrap(),
            bytes[4..4 + length]
        );
        bytes = &bytes[4 + length..];
    }
    assert!(bytes.is_empty());
}
