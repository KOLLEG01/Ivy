//! Reception/interval behavior retained from SIPSorcery 10.0.15.
//! Copyright (c) 2006–2026 Aaron Clauson. The service's retained
//! native/phone-runtime/licenses/SIPSorcery-10.0.15.md supplies its full notice.
//! Clocks are supplied by the owner; no PCM permission or route state is read.
use super::{MediaError, RtcpReportBlock};
use std::time::Duration;

pub struct RtcpReceptionStatistics {
    source: u32,
    highest: u16,
    cycles: u64,
    base: u32,
    received: u32,
    previous_expected: u64,
    previous_received: u32,
    transit: u32,
    jitter: u32,
    last_sender: Option<(u32, u32)>,
}
impl RtcpReceptionStatistics {
    pub fn new(source: u32) -> Self {
        Self {
            source,
            highest: 0,
            cycles: 0,
            base: 0,
            received: 0,
            previous_expected: 0,
            previous_received: 0,
            transit: 0,
            jitter: 0,
            last_sender: None,
        }
    }
    pub fn source(&self) -> u32 {
        self.source
    }
    /// Arrival uses the same timestamp units as RTP. Keep the original wrap,
    /// first-transit and interval-loss arithmetic, including its diagnostics.
    pub fn record(&mut self, sequence: u16, timestamp: u32, arrival: u32) {
        if self.received == 0 {
            self.base = u32::from(sequence);
        }
        self.received = self.received.wrapping_add(1);
        if u32::from(sequence) == u32::from(self.highest) + 1 {
            self.highest = sequence;
        } else if (sequence == 0 && self.highest == u16::MAX)
            || (sequence < 256 && self.highest > 65280)
        {
            self.highest = sequence;
            self.cycles = self.cycles.wrapping_add(65536);
        } else if sequence > self.highest {
            self.highest = sequence;
        }
        let transit = arrival.wrapping_sub(timestamp);
        let mut change = transit.wrapping_sub(self.transit) as i32;
        self.transit = transit;
        if change < 0 {
            change = change.wrapping_neg();
        }
        self.jitter = self
            .jitter
            .wrapping_add((change as u32).wrapping_sub(self.jitter.wrapping_add(8) >> 4));
    }
    pub fn sender_report(&mut self, sender_ntp: u64, received_ntp32: u32) {
        self.last_sender = Some(((sender_ntp >> 16) as u32, received_ntp32));
    }
    /// Sampling commits interval counters, as in the original GetSample. The
    /// reported highest field intentionally remains the raw sequence number.
    pub fn sample(&mut self, now_ntp32: u32) -> RtcpReportBlock {
        let expected = self
            .cycles
            .wrapping_add(u64::from(self.highest))
            .wrapping_sub(u64::from(self.base))
            .wrapping_add(1);
        let expected_interval = expected.wrapping_sub(self.previous_expected);
        self.previous_expected = expected;
        let received_interval = self.received.wrapping_sub(self.previous_received);
        self.previous_received = self.received;
        let lost = if self.received == 0 {
            0
        } else {
            expected_interval.wrapping_sub(u64::from(received_interval))
        };
        let fraction = if expected_interval == 0 || lost == 0 {
            0
        } else {
            (lost.wrapping_shl(8) / expected_interval) as u8
        };
        let (last, delay) = self
            .last_sender
            .map(|(last, received)| (last, now_ntp32.wrapping_sub(received)))
            .unwrap_or((0, 0));
        RtcpReportBlock::new(
            self.source,
            fraction,
            lost as i32,
            u32::from(self.highest),
            self.jitter >> 4,
            last,
            delay,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtcpTimerEvent {
    pub new_timeout: bool,
}

/// Original report scheduling: every interval, including the first, is a
/// supplied random integer in [2500,7500] ms. Rearm after callback work, not on
/// a periodic catch-up clock. Local-wall milliseconds drive inactivity; the
/// independent monotonic offset drives scheduling. The owner supplies both.
pub struct RtcpReportSchedule {
    created_wall_ticks: i64,
    last_activity_wall_ticks: Option<i64>,
    timeout_ms: i32,
    timed_out: bool,
    started: bool,
    closed: bool,
    in_flight: bool,
    next: Option<Duration>,
    last_clock: Option<Duration>,
    previous_sent: u32,
}
impl RtcpReportSchedule {
    pub const BASE_INTERVAL_MS: u16 = 5000;
    pub const DEFAULT_TIMEOUT_MS: i32 = 30000;
    pub fn new(created_wall_ms: i64) -> Self {
        Self::from_wall_ticks(created_wall_ms.saturating_mul(10_000))
    }
    pub fn from_wall_ticks(created_wall_ticks: i64) -> Self {
        Self {
            created_wall_ticks,
            last_activity_wall_ticks: None,
            timeout_ms: Self::DEFAULT_TIMEOUT_MS,
            timed_out: false,
            started: false,
            closed: false,
            in_flight: false,
            next: None,
            last_clock: None,
            previous_sent: 0,
        }
    }
    fn interval(interval_ms: u16) -> Result<Duration, MediaError> {
        if !(2500..=7500).contains(&interval_ms) {
            return Err(MediaError::InvalidPacket);
        }
        Ok(Duration::from_millis(u64::from(interval_ms)))
    }
    fn observe(&mut self, now: Duration) -> Result<(), MediaError> {
        if self.last_clock.is_some_and(|last| now < last) {
            return Err(MediaError::ClockWentBackwards);
        }
        self.last_clock = Some(now);
        Ok(())
    }
    pub fn start(&mut self, now: Duration, interval_ms: u16) -> Result<bool, MediaError> {
        if self.closed {
            return Err(MediaError::Revoked);
        }
        if self.started {
            return Ok(false);
        }
        let next = now
            .checked_add(Self::interval(interval_ms)?)
            .ok_or(MediaError::ClockOverflow)?;
        self.observe(now)?;
        self.started = true;
        self.next = Some(next);
        Ok(true)
    }
    pub fn next(&self) -> Option<Duration> {
        self.next
    }
    pub fn timed_out(&self) -> bool {
        self.timed_out
    }
    pub fn started(&self) -> bool {
        self.started
    }
    pub fn last_activity_wall_ms(&self) -> Option<i64> {
        self.last_activity_wall_ticks.map(|t| t / 10_000)
    }
    pub fn set_timeout_ms(&mut self, value: i32) {
        self.timeout_ms = value;
    }
    pub fn record_activity(&mut self, wall_ms: i64) {
        self.record_activity_ticks(wall_ms.saturating_mul(10_000));
    }
    pub fn record_activity_ticks(&mut self, wall_ticks: i64) {
        self.last_activity_wall_ticks = Some(wall_ticks);
        self.timed_out = false;
    }
    pub fn sender_report(&self, packets_sent: u32) -> bool {
        packets_sent > self.previous_sent
    }
    pub fn begin_report(
        &mut self,
        now: Duration,
        wall_ms: i64,
    ) -> Result<Option<RtcpTimerEvent>, MediaError> {
        self.begin_report_ticks(now, wall_ms.saturating_mul(10_000))
    }
    pub fn begin_report_ticks(
        &mut self,
        now: Duration,
        wall_ticks: i64,
    ) -> Result<Option<RtcpTimerEvent>, MediaError> {
        self.observe(now)?;
        if self.closed || self.next.is_none_or(|next| now < next) {
            return Ok(None);
        }
        self.next = None;
        self.in_flight = true;
        let last = self
            .last_activity_wall_ticks
            .unwrap_or(self.created_wall_ticks);
        let expired =
            i128::from(wall_ticks) - i128::from(last) > i128::from(self.timeout_ms) * 10_000;
        let new_timeout = expired && !self.timed_out;
        if new_timeout {
            self.timed_out = true;
        }
        Ok(Some(RtcpTimerEvent { new_timeout }))
    }
    /// Call only after the original report callback returns successfully.
    /// A point-in-time report read and Close do not commit this sent count.
    pub fn complete_report(
        &mut self,
        now_after_work: Duration,
        interval_ms: u16,
        packets_sent: u32,
    ) -> Result<(), MediaError> {
        if !self.in_flight {
            return Err(MediaError::InvalidPacket);
        }
        let next = if self.closed {
            None
        } else {
            Some(
                now_after_work
                    .checked_add(Self::interval(interval_ms)?)
                    .ok_or(MediaError::ClockOverflow)?,
            )
        };
        self.observe(now_after_work)?;
        self.in_flight = false;
        self.previous_sent = packets_sent;
        self.next = next;
        Ok(())
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.next = None;
    }
    pub(super) fn commit_report_count(&mut self, packets: u32) -> Result<(), MediaError> {
        if !self.in_flight {
            return Err(MediaError::InvalidPacket);
        }
        self.previous_sent = packets;
        Ok(())
    }
    pub(super) fn stop_timer(&mut self) {
        self.next = None;
        self.in_flight = false;
    }
}
