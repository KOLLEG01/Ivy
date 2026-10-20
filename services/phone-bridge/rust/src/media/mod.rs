mod audio_frame;
mod call_audio;
mod codec_owner;
mod codecs;
mod evs;
mod evs_native;
mod format;
mod g711;
mod identity;
pub mod native_view;
mod negotiation;
mod packet;
mod permit;
mod progress_audio;
mod queue;
mod receive_analysis;
mod receive_owner;
mod resample;
mod rtcp;
mod rtcp_owner;
mod rtcp_receive;
mod rtcp_session;
mod rtcp_statistics;
mod rtcp_time;
mod rtp;
mod sdp;
mod sdp_address;
mod send_clock;
mod sender;
mod session;
mod session_description;
mod startup;
mod transport;

pub use audio_frame::RtpAudioFrames;
pub use call_audio::{
    AudioAttachmentStatus, AudioDesktopIdentity, AudioMutePolicy, AudioOperation, AudioRoutePermit,
    AudioSettings, CallAudioError, CallAudioFactory, CallAudioPort, CallAudioRoute,
};
pub use identity::{SipMediaEntropy, SystemSipMediaEntropy};
pub use permit::AudioPermit;
pub use progress_audio::{
    CallWaitingAudio, ScreeningAudio, WavError, load_announcement, parse_announcement,
};
pub use queue::AudioQueue;
pub use receive_owner::{ReceiveError, SipReceiveEvents, SipReceiveObservation, SipRtpReceiver};
pub use resample::PcmResampler;
pub use rtcp::{RtcpReportBlock, RtcpReportBuilder, RtcpSenderInfo, RtcpTransmitCounters};
pub use rtcp_owner::RtcpReceiveObservation;
pub use rtcp_receive::{RtcpCompound, RtcpReceiverReport, RtcpReportBlocks, RtcpSenderReport};
pub use rtcp_session::{RtcpSession, RtcpSessionObservation};
pub use rtcp_statistics::{RtcpReceptionStatistics, RtcpReportSchedule, RtcpTimerEvent};
#[cfg(windows)]
pub use rtcp_time::WindowsRtcpRuntime;
pub use rtcp_time::{RtcpRuntime, RtcpWallTime, rtcp_interval};
pub use rtp::{RtpAudioPacket, RtpReceiveQueue, RtpReceiveStatus};
pub use sdp::{
    SDP_UNITS, SdpConnection, SdpDirection, SdpDocument, SdpError, SdpFormat, SdpMedia,
    SdpMediaKind, SdpOrigin, bounded_sdp, valid_sdp,
};
pub use sdp_address::{SipSdpRoute, SystemSipSdpRoute};
pub use send_clock::RtpSendClock;
pub use sender::{AudioSendHealth, PcmSendPort};
pub use session::{
    SipMediaError, SipMediaIdentity, SipMediaObservation, SipMediaSession, SipMediaSettings,
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
    ClockOverflow,
    ClockUnavailable,
    EntropyUnavailable,
    AddressUnavailable,
    ReportCallbackFailed,
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
pub use codec_owner::{MediaCodec, PcmAudioPort, PcmAudioState};
pub use codecs::{CodecError, G722Decoder, G722Encoder, OpusDecoder, OpusEncoder};
pub use evs::{
    EVS_OFFER_PARAMETERS, EvsProfile, evs_channels, evs_header_full, evs_offer_parameters,
    evs_select, evs_sender,
};
pub use evs_native::{EvsDecoder, EvsEncoder, EvsLibrary};
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
