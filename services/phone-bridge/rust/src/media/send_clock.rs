use super::MediaError;
use std::time::Duration;

/// Original 20ms sender cadence with at most the original short catch-up.
/// Advance even without a negotiated codec; an idle sender must keep waiting.
pub struct RtpSendClock {
    next: Duration,
    last: Duration,
}
impl RtpSendClock {
    pub const FRAME: Duration = Duration::from_millis(20);
    pub fn new(now: Duration) -> Result<Self, MediaError> {
        Ok(Self {
            next: now
                .checked_add(Self::FRAME)
                .ok_or(MediaError::ClockOverflow)?,
            last: now,
        })
    }
    pub fn next(&self) -> Duration {
        self.next
    }
    pub fn due(&mut self, now: Duration) -> Result<bool, MediaError> {
        if now < self.last {
            return Err(MediaError::ClockWentBackwards);
        }
        self.last = now;
        Ok(now >= self.next)
    }
    /// Call after the due frame's encode/publication work, including when no
    /// codec is negotiated. This uses the actual completion time, as the
    /// original pump does, rather than moving its suspension check earlier.
    pub fn advance(&mut self, now: Duration) -> Result<(), MediaError> {
        if now < self.last {
            return Err(MediaError::ClockWentBackwards);
        }
        if now < self.next {
            return Err(MediaError::InvalidPacket);
        }
        let mut next = self
            .next
            .checked_add(Self::FRAME)
            .ok_or(MediaError::ClockOverflow)?;
        if now.saturating_sub(next) > Self::FRAME * 2 {
            next = now
                .checked_add(Self::FRAME)
                .ok_or(MediaError::ClockOverflow)?;
        }
        self.last = now;
        self.next = next;
        Ok(())
    }
}
