//! Independent G.711 companding at the negotiated 8 kHz PCM clock.
//! RTP octet packing follows RFC 3551 section 4.5.14; no third-party codec code
//! is linked or transliterated. The complete i16/octet domains are qualified
//! against the original codec, including its negative saturation behavior.
use super::{AudioFormat, Codec, FormatError};

#[derive(Clone, Copy)]
pub struct G711Codec {
    codec: Codec,
}
impl G711Codec {
    pub const PACKET_SAMPLES: usize = 160;
    pub const MAXIMUM_DECODED_SAMPLES: usize = 960;
    pub fn new(format: &AudioFormat) -> Result<Self, FormatError> {
        if !matches!(format.codec(), Codec::Pcma | Codec::Pcmu) {
            return Err(FormatError);
        }
        Ok(Self {
            codec: format.codec(),
        })
    }
    pub fn encode_20ms(&self, pcm: &[i16; Self::PACKET_SAMPLES]) -> [u8; Self::PACKET_SAMPLES] {
        pcm.map(|sample| match self.codec {
            Codec::Pcma => a_encode(sample),
            Codec::Pcmu => mu_encode(sample),
            _ => unreachable!("G711 format"),
        })
    }
    /// The enclosing media owner rechecks generation before rendering these
    /// samples. Resampling and audio authority belong to that owner.
    pub fn decode(&self, packet: &[u8]) -> Result<Vec<i16>, FormatError> {
        if packet.is_empty() || packet.len() > Self::MAXIMUM_DECODED_SAMPLES {
            return Err(FormatError);
        }
        Ok(packet
            .iter()
            .map(|&sample| match self.codec {
                Codec::Pcma => a_decode(sample),
                Codec::Pcmu => mu_decode(sample),
                _ => unreachable!("G711 format"),
            })
            .collect())
    }
}
fn mu_encode(sample: i16) -> u8 {
    let sample = i32::from(sample);
    let sign = if sample < 0 { 0x80 } else { 0 };
    // The original i16 encoder wraps abs(MIN) and emits negative zero there.
    let magnitude = (if sample == i32::from(i16::MIN) {
        0
    } else {
        sample.abs()
    })
    .min(32635)
        + 132;
    let exponent = (31 - magnitude.leading_zeros()).saturating_sub(7);
    let mantissa = (magnitude >> (exponent + 3)) & 15;
    !(sign | (exponent as u8) << 4 | mantissa as u8)
}
fn mu_decode(sample: u8) -> i16 {
    // Preserve the original decoder's negative-zero reconstruction.
    if sample == 0x7f {
        return -1;
    }
    let value = !sample;
    let magnitude = (((i32::from(value & 15) << 3) + 132) << ((value & 0x70) >> 4)) - 132;
    (if value & 0x80 != 0 {
        -magnitude
    } else {
        magnitude
    }) as i16
}
fn a_encode(sample: i16) -> u8 {
    let (magnitude, mask) = if sample >= 0 {
        (i32::from(sample), 0xd5)
    } else {
        // Keep the original negative threshold and i16-MIN behavior. Subtracting
        // one before quantization changes 127 other negative boundary samples.
        (
            if sample == i16::MIN {
                0
            } else {
                -i32::from(sample)
            },
            0x55,
        )
    };
    let magnitude = magnitude >> 3;
    let segment = (31 - magnitude.max(1).leading_zeros()).saturating_sub(4);
    let mantissa = (magnitude >> if segment < 2 { 1 } else { segment }) & 15;
    (((segment as u8) << 4) | mantissa as u8) ^ mask
}
fn a_decode(sample: u8) -> i16 {
    let value = sample ^ 0x55;
    let segment = (value & 0x70) >> 4;
    let base = i32::from(value & 15) << 4;
    let magnitude = match segment {
        0 => base + 8,
        1 => base + 264,
        _ => (base + 264) << (segment - 1),
    };
    (if value & 0x80 != 0 {
        magnitude
    } else {
        -magnitude
    }) as i16
}
