/// One WebRTC peer's native startup probation. Apply before passing RTP to the
/// SRTP receiver. A transport probe must not seed the live rollover index.
/// Audio suspension does not reset this peer's authenticated replay history.
#[derive(Default)]
pub struct RtpStartupGate {
    previous: Option<(u32, u16)>,
    ready: bool,
}

impl RtpStartupGate {
    pub fn may_authenticate(&mut self, dtls_connected: bool, source: u32, sequence: u16) -> bool {
        if !dtls_connected {
            return false;
        }
        if self.ready {
            return true;
        }
        let consecutive = self.previous.is_some_and(|(last_source, last_sequence)| {
            source == last_source && sequence == last_sequence.wrapping_add(1)
        });
        self.previous = Some((source, sequence));
        consecutive
    }

    /// Call only after the admitted packet successfully authenticates. A failed
    /// tag must leave startup probation active, matching the native peer.
    pub fn authenticated(&mut self) {
        self.ready = true;
    }
}
