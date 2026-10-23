//! Device-rate float output. The native owner holds the same PCM lock for this
//! provider and every buffer/authority operation, including capture callbacks.
use super::{CallAudioError, SAMPLE_RATE, WindowsAudioBuffers};

mod wdl;

pub(crate) struct RenderOutput {
    rate: u32,
    channels: u16,
    generation: i64,
    pipeline: Option<Pipeline>,
    stereo_source: Vec<f32>,
}

struct Pipeline {
    resampler: Option<wdl::Resampler>,
}

impl RenderOutput {
    /// The native factory admits only mono/stereo device mix formats.
    pub fn new(rate: u32, channels: u16) -> Result<Self, CallAudioError> {
        if rate == 0 || !matches!(channels, 1 | 2) {
            return Err(CallAudioError::InvalidRequest);
        }
        Ok(Self {
            rate,
            channels,
            generation: -1,
            pipeline: None,
            stereo_source: Vec::new(),
        })
    }

    /// Always returns a complete device buffer, with silence on authority loss.
    /// Recreating the pipeline on resume retires buffered interpolation/history.
    pub fn read(&mut self, owner: &mut WindowsAudioBuffers, output: &mut [f32]) -> usize {
        if !owner.is_open() {
            self.pipeline = None;
            output.fill(0.0);
            return output.len();
        }
        if self.pipeline.is_none() || self.generation != owner.generation {
            self.pipeline = Some(Pipeline {
                resampler: (self.rate as usize != SAMPLE_RATE)
                    .then(|| wdl::Resampler::new(self.rate)),
            });
            self.generation = owner.generation;
        }
        let pipeline = self.pipeline.as_mut().unwrap();
        let count = if self.channels == 2 {
            let frames = output.len() / 2;
            if self.stereo_source.len() < frames {
                self.stereo_source.resize(frames, 0.0);
            }
            let read = pipeline.read(owner, &mut self.stereo_source[..frames]);
            for (value, pair) in self.stereo_source[..read]
                .iter()
                .zip(output.chunks_exact_mut(2))
            {
                pair.fill(*value);
            }
            read * 2
        } else {
            pipeline.read(owner, output)
        };
        output[count..].fill(0.0);
        if !owner.is_open() {
            self.pipeline = None;
            output.fill(0.0);
        }
        output.len()
    }
}

impl Pipeline {
    fn read(&mut self, owner: &mut WindowsAudioBuffers, output: &mut [f32]) -> usize {
        match self.resampler.as_mut() {
            Some(resampler) => resampler.read(owner, output),
            None => owner.render(output),
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_windows_render.rs"
    ));
}
