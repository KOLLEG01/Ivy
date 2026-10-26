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
    // Fixed float32 channels and four WAVs captured from the original encoder.
    let channels: Vec<f32> = include_bytes!("fixtures/chat-wav-input.f32")
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let (channel, other) = channels.split_at(257);
    let mut bytes = include_bytes!("fixtures/chat-wav-reference.bin").as_slice();
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
