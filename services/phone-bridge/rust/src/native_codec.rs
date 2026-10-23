//! Bounded synthetic diagnostics through the production codec/resampler owners.
//! Package loading remains with the verified native executable's library owner.
use crate::media::{Codec, EvsLibrary, MediaCodec, PcmAudioPort, PcmAudioState};
use ivy_contracts::IvyJson;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecLibraryError {
    Missing,
    Incompatible,
    Failed,
}
impl CodecLibraryError {
    fn code(self) -> &'static str {
        match self {
            Self::Missing => "codec_library_missing",
            Self::Incompatible => "codec_library_incompatible",
            Self::Failed => "codec_roundtrip_failed",
        }
    }
}
#[derive(Default)]
struct Samples {
    captured: u64,
    received: u64,
    energy: f64,
    invalid: bool,
}
#[derive(Default)]
struct SyntheticPort(Mutex<Samples>);
impl SyntheticPort {
    fn state(&self) -> std::sync::MutexGuard<'_, Samples> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn rms(&self) -> f64 {
        let state = self.state();
        if state.received == 0 {
            0.0
        } else {
            (state.energy / state.received as f64).sqrt()
        }
    }
}
impl PcmAudioPort for SyntheticPort {
    fn state(&self) -> PcmAudioState {
        PcmAudioState {
            open: true,
            generation: 1,
        }
    }
    fn read_captured(&self, output: &mut [f32], _: u64) -> usize {
        let mut state = self.state();
        for sample in output.iter_mut() {
            *sample = (0.4
                * (2.0 * std::f64::consts::PI * 440.0 * state.captured as f64 / 48000.0).sin())
                as f32;
            state.captured += 1;
        }
        output.len()
    }
    fn write_received(&self, input: &[f32], _: u64) -> bool {
        let mut state = self.state();
        for &sample in input {
            if !sample.is_finite() || sample.abs() > 1.0 {
                state.invalid = true;
                return false;
            }
            // Original C# multiplies floats before adding to double energy.
            state.energy += (sample * sample) as f64;
        }
        state.received += input.len() as u64;
        true
    }
}
/// Uses the first offered format, 25 frames and the original 48 kHz tone/bounds.
/// The caller supplies an authenticated ABI2 library or its observed load class.
pub fn codec_test(evs: Result<Arc<EvsLibrary>, CodecLibraryError>) -> IvyJson {
    let mut rows = Vec::new();
    let mut passed = true;
    for codec in [
        Codec::G722,
        Codec::Pcma,
        Codec::Pcmu,
        Codec::Opus,
        Codec::Evs,
    ] {
        let source = Arc::new(SyntheticPort::default());
        let sink = Arc::new(SyntheticPort::default());
        let mut encoded_bytes = 0_u64;
        let mut frames = 0_u32;
        let result = (|| -> Result<(), &'static str> {
            let evs = if codec == Codec::Evs {
                Some(evs.as_ref().map_err(|e| e.code())?.clone())
            } else {
                None
            };
            let format = codec.offered_formats().remove(0);
            let mut encoder = MediaCodec::new(format.clone(), source, evs.clone())
                .map_err(|_| "codec_roundtrip_failed")?;
            let mut decoder =
                MediaCodec::new(format, sink.clone(), evs).map_err(|_| "codec_roundtrip_failed")?;
            for _ in 0..25 {
                let mut packet = encoder.encode().map_err(|_| "codec_roundtrip_failed")?;
                let decode = if packet.is_empty() || packet.len() > 8192 {
                    Err("codec_roundtrip_failed")
                } else {
                    encoded_bytes += packet.len() as u64;
                    decoder
                        .decode(&packet, None)
                        .map_err(|_| "codec_roundtrip_failed")
                };
                packet.fill(0);
                decode?;
                if sink.state().invalid {
                    return Err("codec_roundtrip_failed");
                }
                frames += 1;
            }
            let samples = sink.state().received;
            let rms = sink.rms();
            if !(23000..=24000).contains(&samples)
                || !(0.1..=0.55).contains(&rms)
                || !rms.is_finite()
            {
                return Err("roundtrip_invalid");
            }
            Ok(())
        })();
        passed &= result.is_ok();
        let decoded_samples = sink.state().received;
        let rms = sink.rms();
        rows.push(IvyJson::object([
            ("name", codec.name().into()),
            ("passed", result.is_ok().into()),
            ("frames", IvyJson::Number(frames as f64)),
            ("encodedBytes", IvyJson::Number(encoded_bytes as f64)),
            ("decodedSamples", IvyJson::Number(decoded_samples as f64)),
            ("rms", IvyJson::Number(rms)),
            ("error", result.err().map_or(IvyJson::Null, Into::into)),
        ]));
    }
    IvyJson::object([("passed", passed.into()), ("codecs", IvyJson::Array(rows))])
}
