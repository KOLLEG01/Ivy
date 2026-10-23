//! The Phone source already has the mix rate/channel count. Low-latency setup
//! may change its sample representation, but never introduces another resampler.
use crate::media::{
    CallAudioError,
    windows_audio::{WindowsAudioBuffers, render::RenderOutput},
};

pub(super) const PCM_SUBTYPE: u128 = 0x00000001_0000_0010_8000_00aa00389b71;
pub(super) const FLOAT_SUBTYPE: u128 = 0x00000003_0000_0010_8000_00aa00389b71;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Encoding {
    Float32,
    Pcm16,
    Pcm24,
}

#[derive(Clone, Copy)]
pub(super) struct MixFormat {
    pub rate: u32,
    pub tag: u16,
    pub bits: u16,
    pub subtype: u128,
}
impl MixFormat {
    /// Channel admission belongs to RenderOutput. The source uses those same
    /// channels. Valid-bits and speaker-mask metadata do not change byte layout.
    /// None means the engine must use the original standard shared-mode path.
    pub fn adaptation(self, source_rate: u32) -> Option<Encoding> {
        if self.rate != source_rate {
            return None;
        }
        let tag = match (self.tag, self.subtype, self.bits) {
            (0xfffe, PCM_SUBTYPE, _) => 1,
            (0xfffe, FLOAT_SUBTYPE, 32) => 3,
            (tag, _, _) => tag,
        };
        match (tag, self.bits) {
            (3, 32) => Some(Encoding::Float32),
            (1, 16) => Some(Encoding::Pcm16),
            (1, 24) => Some(Encoding::Pcm24),
            _ => None,
        }
    }
}
impl Encoding {
    pub fn width(self) -> usize {
        match self {
            Self::Float32 => 4,
            Self::Pcm16 => 2,
            Self::Pcm24 => 3,
        }
    }
    pub fn encode(self, input: &[f32], output: &mut [u8]) -> usize {
        let count = input.len().min(output.len() / self.width());
        for (value, word) in input[..count]
            .iter()
            .zip(output.chunks_exact_mut(self.width()))
        {
            match self {
                Self::Float32 => word.copy_from_slice(&value.to_le_bytes()),
                Self::Pcm16 => word.copy_from_slice(
                    &((value.clamp(-1.0, 1.0) * 32767.0_f32) as i16).to_le_bytes(),
                ),
                Self::Pcm24 => word.copy_from_slice(
                    &((f64::from(value.clamp(-1.0, 1.0)) * 8388607.0_f64) as i32).to_le_bytes()
                        [..3],
                ),
            }
        }
        count * self.width()
    }
}

pub(super) struct RenderBytes {
    samples: RenderOutput,
    encoding: Encoding,
    scratch: Vec<f32>,
}
impl RenderBytes {
    pub fn new(rate: u32, channels: u16, encoding: Encoding) -> Result<Self, CallAudioError> {
        Ok(Self {
            samples: RenderOutput::new(rate, channels)?,
            encoding,
            scratch: vec![],
        })
    }
    /// Like the published sample providers, a trailing incomplete sample is
    /// untouched. Native frame-sized requests have no such trailing bytes.
    pub fn read(&mut self, owner: &mut WindowsAudioBuffers, output: &mut [u8]) -> usize {
        let count = output.len() / self.encoding.width();
        if self.encoding == Encoding::Float32 && output.as_mut_ptr().cast::<f32>().is_aligned() {
            // Every f32 bit pattern is valid. The exclusive byte borrow and
            // alignment/length checks permit this bounded in-place view.
            let samples =
                unsafe { std::slice::from_raw_parts_mut(output.as_mut_ptr().cast::<f32>(), count) };
            return self.samples.read(owner, samples) * 4;
        }
        if self.scratch.len() < count {
            self.scratch.resize(count, 0.0);
        }
        let read = self.samples.read(owner, &mut self.scratch[..count]);
        self.encoding.encode(&self.scratch[..read], output)
    }
}

#[derive(Clone, Copy)]
pub(super) struct EnginePeriod {
    pub default: u32,
    pub fundamental: u32,
    pub minimum: u32,
    pub maximum: u32,
}
impl EnginePeriod {
    /// Preserve the published uint arithmetic and clamp placement, including
    /// malformed driver tuples. Native initialization decides whether to accept.
    pub fn choose(self) -> u32 {
        let mut period = self.minimum;
        if self.fundamental > 0 && !period.is_multiple_of(self.fundamental) {
            period = (period / self.fundamental)
                .wrapping_add(1)
                .wrapping_mul(self.fundamental);
            if period > self.maximum {
                period = self.maximum;
            }
        }
        period
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_windows_render_format.rs"
    ));
}
