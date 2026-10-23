//! Digital call audio, its sans-I/O engine and retained native route owner.
mod network;
mod pcm;
mod peer;
pub(crate) mod route;

pub(crate) use pcm::RealtimeAudio;
pub(crate) use peer::RealtimePeer;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RealtimeError {
    InvalidRequest,
    InvalidAnswer,
    NotReady,
    Failed,
    Timeout,
    Closed,
    Cancelled,
    CleanupUnconfirmed,
    PreparationFailed,
}
impl RealtimeError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidAnswer => "phone_webrtc_sdp_invalid",
            Self::NotReady | Self::Closed => "runtime_not_ready",
            Self::Failed => "phone_webrtc_failed",
            _ => "operation_outcome_unknown",
        }
    }
}
pub(crate) type Result<T> = std::result::Result<T, RealtimeError>;

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_realtime.rs"
    ));
}
