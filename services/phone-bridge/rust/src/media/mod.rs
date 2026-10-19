mod evs;
mod codecs;
mod format;
mod g711;
mod negotiation;
mod packet;
mod permit;
mod queue;
mod resample;
mod rtp;
mod sdp;
mod startup;
mod transport;

pub use permit::AudioPermit;
pub use queue::AudioQueue;
pub use resample::PcmResampler;
pub use rtp::{RtpAudioPacket, RtpReceiveQueue, RtpReceiveStatus};
pub use sdp::{
    SDP_UNITS, SdpConnection, SdpDirection, SdpDocument, SdpError, SdpFormat, SdpMedia,
    SdpMediaKind, SdpOrigin, bounded_sdp, valid_sdp,
};
pub use startup::RtpStartupGate;

pub const SAMPLE_RATE: usize = 48_000;
pub const FRAME_SAMPLES: usize = SAMPLE_RATE / 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    InvalidCapacity,
    InvalidPrebuffer,
    InvalidReorderDelay,
    InvalidPacket,
    ClockWentBackwards,
    InvalidPermitAge,
    StaleObservation,
    Revoked,
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MediaError {}
pub use codecs::{CodecError, G722Decoder, G722Encoder, OpusDecoder, OpusEncoder};
pub use evs::{
    EVS_OFFER_PARAMETERS, EvsProfile, evs_channels, evs_header_full, evs_offer_parameters,
    evs_select, evs_sender,
};
pub use format::{AudioFormat, Codec, FormatError};
pub use g711::G711Codec;
pub use negotiation::{
    AudioNegotiation, AudioNegotiator, NegotiationError, SdpIdentity, SdpType, TelephoneEventFormat,
};
pub use packet::{RtpExtension, RtpHeader, RtpPacket};
pub use transport::{
    RtpDatagramSink, RtpPeer, RtpTransport, RtpTransportCounters, RtpTransportError,
    RtpTransportObservation,
};
