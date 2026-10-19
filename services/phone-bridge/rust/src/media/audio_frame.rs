//! Original audio frame packetization and per-stream RTP clock. SIPSorcery
//! 10.0.15 notice: native/phone-runtime/licenses/SIPSorcery-10.0.15.md.
use super::{RtpHeader, RtpTransportError};

/// The call supplies its original source, sequence and timestamp. Admission,
/// event suppression and the final audio authority guard belong to its owner;
/// this state never owns a socket or authorizes audio publication.
pub struct RtpAudioFrames {
    source: u32,
    sequence: u16,
    timestamp: u32,
}
impl RtpAudioFrames {
    pub const MAXIMUM_PAYLOAD: usize = 1200;
    pub fn new(source: u32, sequence: u16, timestamp: u32) -> Self {
        Self {
            source,
            sequence,
            timestamp,
        }
    }
    pub fn sequence(&self) -> u16 {
        self.sequence
    }
    pub fn timestamp(&self) -> u32 {
        self.timestamp
    }
    /// Dispatch is synchronous, bounded and already admitted by the owner.
    /// Like the original, consume sequence before dispatch and advance the
    /// timestamp afterwards. A caller maps ignored socket-send errors to Ok;
    /// an actual callback error aborts the remaining frame at that boundary.
    pub fn send(
        &mut self,
        duration: u32,
        payload_type: u8,
        payload: &[u8],
        mut dispatch: impl FnMut(RtpHeader, &[u8]) -> Result<(), RtpTransportError>,
    ) -> Result<(), RtpTransportError> {
        if payload_type > 127 || payload.len() > 8192 {
            return Err(RtpTransportError::InvalidPacket);
        }
        let mut increment = 0_u32;
        for part in payload.chunks(Self::MAXIMUM_PAYLOAD) {
            let fraction = part.len() as f64 / payload.len() as f64;
            let packet_duration = (fraction * f64::from(duration)).round_ties_even() as u32;
            let header = RtpHeader {
                marker: false,
                payload_type,
                sequence: self.sequence,
                timestamp: self.timestamp,
                source: self.source,
            };
            self.sequence = self.sequence.wrapping_add(1);
            dispatch(header, part)?;
            self.timestamp = self.timestamp.wrapping_add(packet_duration);
            increment = increment.wrapping_add(packet_duration);
        }
        // This also advances an empty frame, without consuming a sequence.
        self.timestamp = self
            .timestamp
            .wrapping_add(duration.wrapping_sub(increment));
        Ok(())
    }
}
