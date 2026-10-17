//! The existing decoded Float32 audio -> mono PCM16 WAV boundary.
//! Opus decoding and the killable worker are separate from these byte semantics.

pub const CODEX_AUDIO_SAMPLE_RATE_HERTZ: u32 = 16_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioError {
    InvalidSourceRate,
    NoSamples,
    InvalidTargetRate,
    OutputTooLarge,
}

pub struct DecodedAudio<'a> {
    pub channels: &'a [&'a [f32]],
    pub sample_rate: f64,
}

impl DecodedAudio<'_> {
    fn frame_count(&self) -> Result<usize, AudioError> {
        if !self.sample_rate.is_finite() || self.sample_rate <= 0.0 {
            return Err(AudioError::InvalidSourceRate);
        }
        self.channels
            .iter()
            .map(|channel| channel.len())
            .min()
            .filter(|count| *count > 0)
            .ok_or(AudioError::NoSamples)
    }

    pub fn duration_seconds(&self) -> Result<f64, AudioError> {
        Ok(self.frame_count()? as f64 / self.sample_rate)
    }

    pub fn encode_mono_pcm16_wav(&self, target_rate: u32) -> Result<Vec<u8>, AudioError> {
        let source_frames = self.frame_count()?;
        if !(8_000..=192_000).contains(&target_rate) {
            return Err(AudioError::InvalidTargetRate);
        }
        // Match JS arithmetic order: multiplication precedes division.
        let output_frames = (source_frames as f64 * f64::from(target_rate) / self.sample_rate)
            .ceil()
            .max(1.0);
        if !output_frames.is_finite() || output_frames > f64::from((u32::MAX - 36) / 2) {
            return Err(AudioError::OutputTooLarge);
        }
        let output_frames = output_frames as usize;
        let data_bytes = output_frames
            .checked_mul(2)
            .ok_or(AudioError::OutputTooLarge)?;
        let total = data_bytes
            .checked_add(44)
            .ok_or(AudioError::OutputTooLarge)?;
        let mut wav = Vec::new();
        wav.try_reserve_exact(total)
            .map_err(|_| AudioError::OutputTooLarge)?;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_bytes as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&target_rate.to_le_bytes());
        wav.extend_from_slice(&(target_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_bytes as u32).to_le_bytes());
        let source_step = self.sample_rate / f64::from(target_rate);
        for index in 0..output_frames {
            let position = (index as f64 * source_step).min((source_frames - 1) as f64);
            let left = position.floor() as usize;
            let right = (left + 1).min(source_frames - 1);
            let fraction = position - left as f64;
            let mut sample = 0.0;
            for channel in self.channels {
                let a = finite_sample(channel[left]);
                let b = finite_sample(channel[right]);
                sample += a + (b - a) * fraction;
            }
            sample = (sample / self.channels.len() as f64).clamp(-1.0, 1.0);
            let scaled = sample * if sample < 0.0 { 32_768.0 } else { 32_767.0 };
            // Math.round chooses +infinity on ties, including negative samples.
            let pcm = (scaled + 0.5).floor() as i16;
            wav.extend_from_slice(&pcm.to_le_bytes());
        }
        Ok(wav)
    }
}

fn finite_sample(value: f32) -> f64 {
    if value.is_finite() {
        f64::from(value)
    } else {
        0.0
    }
}
