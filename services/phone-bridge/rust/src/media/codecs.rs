//! Stateful PCM codecs. Each instance belongs to one call and direction; the
//! media owner resets it when its audio generation or negotiated format changes.
use super::{AudioFormat, Codec};
use nanoaudio_core::codec::{Decoder as _, Encoder as _};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    InvalidFormat,
    InvalidFrame,
    Failed,
}
impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidFormat => "invalid_format",
            Self::InvalidFrame => "invalid_audio_frame",
            Self::Failed => "codec_failed",
        })
    }
}
impl std::error::Error for CodecError {}

pub struct G722Encoder(nanoaudio_g722::G722Encoder);
pub struct G722Decoder(nanoaudio_g722::G722Decoder);
impl G722Encoder {
    pub fn new(format: &AudioFormat) -> Result<Self, CodecError> {
        if format.codec() != Codec::G722 {
            return Err(CodecError::InvalidFormat);
        }
        Ok(Self(nanoaudio_g722::G722Encoder::new()))
    }
    pub fn reset(&mut self) {
        self.0.reset();
    }
    /// 20 ms at 16 kHz PCM, 64 kbit/s. The separate RTP clock advances by160.
    pub fn encode_20ms(&mut self, input: &[i16; 320]) -> Result<[u8; 160], CodecError> {
        let mut output = [0; 160];
        let progress = self
            .0
            .encode(input, &mut output)
            .map_err(|_| CodecError::Failed)?;
        if (progress.consumed, progress.produced) != (320, 160) {
            return Err(CodecError::Failed);
        }
        Ok(output)
    }
}
impl G722Decoder {
    pub fn new(format: &AudioFormat) -> Result<Self, CodecError> {
        if format.codec() != Codec::G722 {
            return Err(CodecError::InvalidFormat);
        }
        Ok(Self(nanoaudio_g722::G722Decoder::new(
            nanoaudio_g722::G722BitRate::Rate64000,
        )))
    }
    pub fn reset(&mut self) {
        self.0.reset();
    }
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, CodecError> {
        // The final QMF result saturates to i16. Preserve its wide history;
        // signed narrowing here would invert overloaded output samples.
        if packet.is_empty() || packet.len() > 960 {
            return Err(CodecError::InvalidFrame);
        }
        let mut output = vec![0; packet.len() * 2];
        let progress = self
            .0
            .decode(packet, &mut output)
            .map_err(|_| CodecError::Failed)?;
        if progress.consumed != packet.len() || progress.produced != output.len() {
            output.fill(0);
            return Err(CodecError::Failed);
        }
        Ok(output)
    }
}

/// libopus owns its native allocation. The binding is Send and deliberately
/// not Sync; all access requires this direction's exclusive mutable owner.
pub struct OpusEncoder(opus::Encoder);
pub struct OpusDecoder(opus::Decoder);
impl OpusEncoder {
    pub fn new(format: &AudioFormat) -> Result<Self, CodecError> {
        if format.codec() != Codec::Opus {
            return Err(CodecError::InvalidFormat);
        }
        let mut encoder = opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Voip)
            .map_err(|_| CodecError::Failed)?;
        // The original mono PCM API is independent of the opus/48000/2 SDP
        // declaration. Match its admitted encoder profile, including default
        // FEC disabled; remote FEC is handled by the independent decoder.
        encoder
            .set_bitrate(opus::Bitrate::Bits(51000))
            .map_err(|_| CodecError::Failed)?;
        encoder.set_complexity(9).map_err(|_| CodecError::Failed)?;
        encoder.set_vbr(true).map_err(|_| CodecError::Failed)?;
        encoder
            .set_vbr_constraint(false)
            .map_err(|_| CodecError::Failed)?;
        encoder
            .set_inband_fec(false)
            .map_err(|_| CodecError::Failed)?;
        encoder
            .set_packet_loss_perc(0)
            .map_err(|_| CodecError::Failed)?;
        encoder.set_dtx(false).map_err(|_| CodecError::Failed)?;
        encoder
            .set_max_bandwidth(opus::Bandwidth::Fullband)
            .map_err(|_| CodecError::Failed)?;
        encoder
            .set_signal(opus::Signal::Auto)
            .map_err(|_| CodecError::Failed)?;
        encoder.set_lsb_depth(16).map_err(|_| CodecError::Failed)?;
        Ok(Self(encoder))
    }
    pub fn reset(&mut self) -> Result<(), CodecError> {
        self.0.reset_state().map_err(|_| CodecError::Failed)
    }
    pub fn encode_20ms(&mut self, input: &[i16; 960]) -> Result<Vec<u8>, CodecError> {
        let mut output = vec![0; 8192];
        let result = self.0.encode(input, &mut output);
        match result {
            Ok(count) if (1..=8192).contains(&count) => {
                output.truncate(count);
                Ok(output)
            }
            _ => {
                output.fill(0);
                Err(CodecError::Failed)
            }
        }
    }
}
impl OpusDecoder {
    pub fn new(format: &AudioFormat) -> Result<Self, CodecError> {
        if format.codec() != Codec::Opus {
            return Err(CodecError::InvalidFormat);
        }
        opus::Decoder::new(48000, opus::Channels::Mono)
            .map(Self)
            .map_err(|_| CodecError::Failed)
    }
    pub fn reset(&mut self) -> Result<(), CodecError> {
        self.0.reset_state().map_err(|_| CodecError::Failed)
    }
    pub fn packet_samples(packet: &[u8]) -> Result<usize, CodecError> {
        if packet.is_empty() || packet.len() > 8192 {
            return Err(CodecError::InvalidFrame);
        }
        let count =
            opus::packet::get_nb_samples(packet, 48000).map_err(|_| CodecError::InvalidFrame)?;
        if !(1..=5760).contains(&count) {
            return Err(CodecError::InvalidFrame);
        }
        Ok(count)
    }
    /// Empty packet requests PLC. A nonempty packet with fec=true asks libopus
    /// to recover its preceding lost frame. libopus falls back to PLC when the
    /// packet contains no FEC for that frame.
    pub fn decode(
        &mut self,
        packet: &[u8],
        maximum_samples: usize,
        fec: bool,
    ) -> Result<Vec<i16>, CodecError> {
        if packet.len() > 8192
            || !(1..=5760).contains(&maximum_samples)
            || (packet.is_empty() && fec)
        {
            return Err(CodecError::InvalidFrame);
        }
        let mut output = vec![0; maximum_samples];
        match self.0.decode(packet, &mut output, fec) {
            Ok(count) if (1..=maximum_samples).contains(&count) => {
                output.truncate(count);
                Ok(output)
            }
            _ => {
                output.fill(0);
                Err(CodecError::InvalidFrame)
            }
        }
    }
}
