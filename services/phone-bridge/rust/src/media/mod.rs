mod evs;
mod format;
mod permit;
mod queue;
mod rtp;
mod startup;

pub use permit::AudioPermit;
pub use queue::AudioQueue;
pub use rtp::{RtpAudioPacket, RtpReceiveQueue, RtpReceiveStatus};
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
pub use evs::{
    EVS_OFFER_PARAMETERS, EvsProfile, evs_channels, evs_header_full, evs_offer_parameters,
    evs_select, evs_sender,
};
pub use format::{AudioFormat, Codec, FormatError};
