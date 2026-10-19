use super::{
    AudioFormat, Codec, CodecError, EvsDecoder, EvsEncoder, EvsLibrary, G711Codec, G722Decoder,
    G722Encoder, OpusDecoder, OpusEncoder, PcmResampler, RtpAudioPacket,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmAudioState {
    pub open: bool,
    pub generation: u64,
}

/// The route owns its lock and audio permit. State is one atomic observation.
/// Capture and publication check the supplied generation under that same lock;
/// a stale/closed read leaves output silent, and a stale write is discarded.
pub trait PcmAudioPort: Send + Sync {
    fn state(&self) -> PcmAudioState;
    fn read_captured(&self, output: &mut [f32], generation: u64) -> usize;
    fn write_received(&self, input: &[f32], generation: u64) -> bool;
}

enum Encoder {
    G711(G711Codec),
    G722(G722Encoder),
    Opus(OpusEncoder),
    Evs(EvsEncoder),
}
enum Decoder {
    G711(G711Codec),
    G722(G722Decoder),
    Opus(OpusDecoder),
    Evs(EvsDecoder),
}

/// A call's negotiated format owns independent send/receive codec and filter
/// histories. The RTP sender must also fence the generation at actual send.
pub struct MediaCodec {
    format: AudioFormat,
    audio: Arc<dyn PcmAudioPort>,
    evs: Option<Arc<EvsLibrary>>,
    encoder: Option<Encoder>,
    decoder: Option<Decoder>,
    to_codec: PcmResampler,
    from_codec: PcmResampler,
    observed: PcmAudioState,
    next_receive_timestamp: Option<u32>,
    concealed_samples: u64,
}
impl MediaCodec {
    pub fn new(
        format: AudioFormat,
        audio: Arc<dyn PcmAudioPort>,
        evs: Option<Arc<EvsLibrary>>,
    ) -> Result<Self, CodecError> {
        let rate = if format.codec() == Codec::Evs {
            48000
        } else {
            format.pcm_rate() as usize
        };
        let observed = audio.state();
        let mut owner = Self {
            format,
            audio,
            evs,
            encoder: None,
            decoder: None,
            to_codec: PcmResampler::new(48000, rate)?,
            from_codec: PcmResampler::new(rate, 48000)?,
            observed,
            next_receive_timestamp: None,
            concealed_samples: 0,
        };
        // Fail availability before a caller advertises/starts this format.
        owner.encoder = Some(owner.make_encoder()?);
        owner.decoder = Some(owner.make_decoder()?);
        Ok(owner)
    }
    pub fn format(&self) -> &AudioFormat {
        &self.format
    }
    pub fn rtp_duration(&self) -> u32 {
        self.format.rtp_duration()
    }
    pub fn concealed_samples(&self) -> u64 {
        self.concealed_samples
    }
    fn pcm_rate(&self) -> usize {
        if self.format.codec() == Codec::Evs {
            48000
        } else {
            self.format.pcm_rate() as usize
        }
    }
    fn make_encoder(&self) -> Result<Encoder, CodecError> {
        Ok(match self.format.codec() {
            Codec::Pcma | Codec::Pcmu => {
                Encoder::G711(G711Codec::new(&self.format).map_err(|_| CodecError::InvalidFormat)?)
            }
            Codec::G722 => Encoder::G722(G722Encoder::new(&self.format)?),
            Codec::Opus => Encoder::Opus(OpusEncoder::new(&self.format)?),
            Codec::Evs => Encoder::Evs(EvsEncoder::new(
                self.evs.clone().ok_or(CodecError::Failed)?,
                &self.format,
            )?),
        })
    }
    fn make_decoder(&self) -> Result<Decoder, CodecError> {
        Ok(match self.format.codec() {
            Codec::Pcma | Codec::Pcmu => {
                Decoder::G711(G711Codec::new(&self.format).map_err(|_| CodecError::InvalidFormat)?)
            }
            Codec::G722 => Decoder::G722(G722Decoder::new(&self.format)?),
            Codec::Opus => Decoder::Opus(OpusDecoder::new(&self.format)?),
            Codec::Evs => Decoder::Evs(EvsDecoder::new(
                self.evs.clone().ok_or(CodecError::Failed)?,
                &self.format,
            )?),
        })
    }
    pub fn reset(&mut self) {
        self.encoder = None;
        self.decoder = None;
        self.next_receive_timestamp = None;
        self.to_codec.reset();
        self.from_codec.reset();
    }
    fn current(&mut self) -> bool {
        let current = self.audio.state();
        if current != self.observed {
            self.reset();
            self.observed = current;
        }
        current.open
    }
    fn encode_pcm(&mut self, pcm: &[i16]) -> Result<Vec<u8>, CodecError> {
        if self.encoder.is_none() {
            self.encoder = Some(self.make_encoder()?);
        }
        match self.encoder.as_mut().expect("initialized encoder") {
            Encoder::G711(codec) => Ok(codec
                .encode_20ms(pcm.try_into().map_err(|_| CodecError::InvalidFrame)?)
                .to_vec()),
            Encoder::G722(codec) => Ok(codec
                .encode_20ms(pcm.try_into().map_err(|_| CodecError::InvalidFrame)?)?
                .to_vec()),
            Encoder::Opus(codec) => {
                codec.encode_20ms(pcm.try_into().map_err(|_| CodecError::InvalidFrame)?)
            }
            Encoder::Evs(codec) => {
                codec.encode_20ms(pcm.try_into().map_err(|_| CodecError::InvalidFrame)?)
            }
        }
    }
    pub fn encode(&mut self) -> Result<Vec<u8>, CodecError> {
        let open = self.current();
        let expected = self.observed.generation;
        let mut captured = [0.0; 960];
        let mut mono = [0.0; 960];
        let mut pcm = [0_i16; 960];
        let count = self.pcm_rate() / 50;
        let result = (|| {
            if open {
                self.audio.read_captured(&mut captured, expected);
            }
            self.to_codec.process(&captured, &mut mono[..count])?;
            for (sample, &value) in pcm[..count].iter_mut().zip(&mono[..count]) {
                if !value.is_finite() {
                    return Err(CodecError::InvalidFrame);
                }
                *sample = (value.clamp(-1.0, 1.0) * 32767.0).round_ties_even() as i16;
            }
            let mut packet = self.encode_pcm(&pcm[..count])?;
            if open && self.audio.state() != self.observed {
                packet.fill(0);
                self.reset();
                pcm.fill(0);
                self.observed.open = false;
                packet = self.encode_pcm(&pcm[..count])?;
            }
            Ok(packet)
        })();
        captured.fill(0.0);
        mono.fill(0.0);
        pcm.fill(0);
        result
    }
    pub fn decode(&mut self, packet: &[u8], generation: Option<u64>) -> Result<(), CodecError> {
        if !self.current()
            || generation.is_some_and(|required| required != self.observed.generation)
        {
            return Ok(());
        }
        if packet.is_empty() || packet.len() > 8192 {
            return Err(CodecError::InvalidFrame);
        }
        let expected = self.observed.generation;
        if self.format.codec() == Codec::Opus {
            return self.render_opus(packet, 5760, false, expected);
        }
        if self.decoder.is_none() {
            self.decoder = Some(self.make_decoder()?);
        }
        let mut pcm = match self.decoder.as_mut().expect("initialized decoder") {
            Decoder::G711(codec) => codec.decode(packet).map_err(|_| CodecError::InvalidFrame)?,
            Decoder::G722(codec) => codec.decode(packet)?,
            Decoder::Evs(codec) => codec.decode(packet)?,
            Decoder::Opus(_) => unreachable!("Opus uses its recovery path"),
        };
        let result = self.render(&pcm, expected);
        pcm.fill(0);
        result
    }
    fn render_opus(
        &mut self,
        packet: &[u8],
        length: usize,
        fec: bool,
        generation: u64,
    ) -> Result<(), CodecError> {
        if self.audio.state()
            != (PcmAudioState {
                open: true,
                generation,
            })
        {
            return Ok(());
        }
        if self.decoder.is_none() {
            self.decoder = Some(self.make_decoder()?);
        }
        let Decoder::Opus(decoder) = self.decoder.as_mut().expect("initialized decoder") else {
            return Err(CodecError::InvalidFormat);
        };
        let mut pcm = decoder.decode(packet, length, fec)?;
        let result = self.render(&pcm, generation);
        pcm.fill(0);
        result
    }
    fn render(&mut self, pcm: &[i16], generation: u64) -> Result<(), CodecError> {
        if pcm.is_empty() || pcm.len() > self.pcm_rate() * 120 / 1000 {
            return Err(CodecError::InvalidFrame);
        }
        let mut mono = vec![0.0; pcm.len()];
        let mut rendered = [0.0; 5760];
        let result = (|| {
            for (sample, &value) in mono.iter_mut().zip(pcm) {
                *sample = value as f32 / 32768.0;
            }
            let count = self.from_codec.process(&mono, &mut rendered)?;
            for sample in &mut rendered[..count] {
                if !sample.is_finite() {
                    return Err(CodecError::InvalidFrame);
                }
                *sample = sample.clamp(-1.0, 1.0);
            }
            if self.audio.state()
                != (PcmAudioState {
                    open: true,
                    generation,
                })
                || !self.audio.write_received(&rendered[..count], generation)
            {
                self.reset();
                self.observed.open = false;
            }
            Ok(())
        })();
        mono.fill(0.0);
        rendered.fill(0.0);
        result
    }
    pub fn decode_rtp(&mut self, packet: &RtpAudioPacket) -> Result<(), CodecError> {
        if !self.current() || self.observed.generation != packet.generation {
            return Ok(());
        }
        if self.format.codec() != Codec::Opus {
            return self.decode(&packet.payload, Some(packet.generation));
        }
        let duration = OpusDecoder::packet_samples(&packet.payload)?;
        if let Some(next) = self.next_receive_timestamp {
            let missing = packet.timestamp.wrapping_sub(next) as i32;
            if missing > 0 && missing <= 5760 && missing % 480 == 0 {
                let fec = if duration % 480 == 0 && missing as usize >= duration {
                    duration
                } else {
                    0
                };
                let mut remaining = missing as usize - fec;
                while remaining > 0 {
                    let length = remaining.min(960);
                    self.render_opus(&[], length, false, packet.generation)?;
                    self.concealed_samples = self.concealed_samples.saturating_add(length as u64);
                    remaining -= length;
                }
                if fec > 0 {
                    self.render_opus(&packet.payload, fec, true, packet.generation)?;
                    self.concealed_samples = self.concealed_samples.saturating_add(fec as u64);
                }
            } else if missing != 0 {
                self.reset();
            }
        }
        self.decode(&packet.payload, Some(packet.generation))?;
        if self.audio.state() == self.observed
            && self.observed.open
            && self.observed.generation == packet.generation
        {
            self.next_receive_timestamp = Some(packet.timestamp.wrapping_add(duration as u32));
        }
        Ok(())
    }
}
impl Drop for MediaCodec {
    fn drop(&mut self) {
        self.reset();
    }
}
