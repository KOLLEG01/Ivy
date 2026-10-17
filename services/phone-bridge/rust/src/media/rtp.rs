use super::MediaError;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtpAudioPacket {
    pub sequence: u16,
    pub timestamp: u32,
    pub source: u32,
    pub payload_type: u8,
    pub payload: Vec<u8>,
    pub generation: u64,
}

#[derive(Default, Debug, Clone, PartialEq)]
pub struct RtpReceiveStatus {
    pub queued_packets: usize,
    pub maximum_packets: usize,
    pub reorder_ms: u32,
    pub duplicate_packets: u64,
    pub late_packets: u64,
    pub overflow_packets: u64,
    pub foreign_packets: u64,
    pub gated_packets: u64,
    pub missing_packets: u64,
    pub maximum_residence_ms: f64,
}

struct Entry {
    packet: RtpAudioPacket,
    arrived: Duration,
}

/// One queue per SSRC/call. Caller holds the receive lock, passes a monotonic time
/// and copies no packet until admission. Work is bounded by eight retained packets.
pub struct RtpReceiveQueue {
    queued: Vec<Entry>,
    expected: Option<u16>,
    source: Option<u32>,
    last_clock: Option<Duration>,
    reset_decoder: bool,
    status: RtpReceiveStatus,
}

impl RtpReceiveQueue {
    pub const CAPACITY: usize = 8;
    pub const MAXIMUM_PAYLOAD: usize = 8192;

    pub fn new(reorder_ms: u32) -> Result<Self, MediaError> {
        if reorder_ms > 60 {
            return Err(MediaError::InvalidReorderDelay);
        }
        Ok(Self {
            queued: Vec::with_capacity(Self::CAPACITY),
            expected: None,
            source: None,
            last_clock: None,
            reset_decoder: false,
            status: RtpReceiveStatus {
                maximum_packets: Self::CAPACITY,
                reorder_ms,
                ..Default::default()
            },
        })
    }

    pub fn status(&self) -> RtpReceiveStatus {
        RtpReceiveStatus {
            queued_packets: self.queued.len(),
            ..self.status.clone()
        }
    }

    fn observe_clock(&mut self, now: Duration) -> Result<(), MediaError> {
        if self.last_clock.is_some_and(|last| now < last) {
            return Err(MediaError::ClockWentBackwards);
        }
        self.last_clock = Some(now);
        Ok(())
    }

    fn distance(&self, sequence: u16) -> i16 {
        sequence.wrapping_sub(self.expected.expect("admitted header")) as i16
    }

    fn header(&mut self, source: u32, sequence: u16) -> bool {
        if self.source.is_some_and(|current| current != source) {
            self.status.foreign_packets += 1;
            return false;
        }
        self.source.get_or_insert(source);
        self.expected.get_or_insert(sequence);
        if self.distance(sequence) < 0 {
            self.status.late_packets += 1;
            return false;
        }
        true
    }

    pub fn reject_peer(&mut self) {
        self.status.foreign_packets += 1;
    }

    pub fn skip(&mut self, source: u32, sequence: u16, now: Duration) -> Result<(), MediaError> {
        self.observe_clock(now)?;
        if self.header(source, sequence) {
            self.clear();
            if self.distance(sequence) >= 0 {
                self.expected = Some(sequence.wrapping_add(1));
            }
            self.status.gated_packets += 1;
            self.reset_decoder = true;
        }
        Ok(())
    }

    pub fn add(&mut self, packet: &RtpAudioPacket, now: Duration) -> Result<bool, MediaError> {
        if packet.payload.is_empty()
            || packet.payload.len() > Self::MAXIMUM_PAYLOAD
            || packet.payload_type > 127
        {
            return Err(MediaError::InvalidPacket);
        }
        self.observe_clock(now)?;
        self.expire_gap(now, false);
        if !self.header(packet.source, packet.sequence) {
            return Ok(false);
        }
        if self
            .queued
            .iter()
            .any(|entry| entry.packet.sequence == packet.sequence)
        {
            self.status.duplicate_packets += 1;
            return Ok(false);
        }
        if self.queued.len() == Self::CAPACITY {
            let farthest = self
                .queued
                .iter()
                .enumerate()
                .max_by_key(|(_, entry)| self.distance(entry.packet.sequence))
                .unwrap()
                .0;
            self.status.overflow_packets += 1;
            if self.distance(packet.sequence)
                >= self.distance(self.queued[farthest].packet.sequence)
            {
                return Ok(false);
            }
            self.queued[farthest].packet.payload.fill(0);
            self.queued.remove(farthest);
        }
        self.queued.push(Entry {
            packet: packet.clone(),
            arrived: now,
        });
        Ok(true)
    }

    pub fn take(
        &mut self,
        now: Duration,
        playout_deadline: bool,
    ) -> Result<Option<(RtpAudioPacket, bool)>, MediaError> {
        self.observe_clock(now)?;
        if self.queued.is_empty() {
            return Ok(None);
        }
        self.expire_gap(now, playout_deadline);
        let Some(index) = self
            .queued
            .iter()
            .position(|entry| Some(entry.packet.sequence) == self.expected)
        else {
            return Ok(None);
        };
        let entry = self.queued.remove(index);
        self.expected = Some(entry.packet.sequence.wrapping_add(1));
        self.status.maximum_residence_ms = self
            .status
            .maximum_residence_ms
            .max((now - entry.arrived).as_secs_f64() * 1000.0);
        let discontinuity = std::mem::take(&mut self.reset_decoder);
        Ok(Some((entry.packet, discontinuity)))
    }

    fn expire_gap(&mut self, now: Duration, playout_deadline: bool) {
        if self.queued.is_empty()
            || self
                .queued
                .iter()
                .any(|entry| Some(entry.packet.sequence) == self.expected)
        {
            return;
        }
        let oldest = self.queued.iter().map(|entry| entry.arrived).min().unwrap();
        if !playout_deadline && now - oldest < Duration::from_millis(self.status.reorder_ms.into())
        {
            return;
        }
        let nearest = self
            .queued
            .iter()
            .min_by_key(|entry| self.distance(entry.packet.sequence))
            .unwrap()
            .packet
            .sequence;
        self.status.missing_packets += self.distance(nearest) as u64;
        self.expected = Some(nearest);
        self.reset_decoder = true;
    }

    /// Retain SSRC and advance beyond all discarded packets. Resume cannot replay backlog.
    pub fn clear(&mut self) {
        if let Some(last) = self
            .queued
            .iter()
            .max_by_key(|entry| self.distance(entry.packet.sequence))
        {
            self.expected = Some(last.packet.sequence.wrapping_add(1));
            for entry in &mut self.queued {
                entry.packet.payload.fill(0);
            }
            self.status.gated_packets += self.queued.len() as u64;
            self.queued.clear();
        }
        self.reset_decoder = true;
    }
}

impl Drop for RtpReceiveQueue {
    fn drop(&mut self) {
        self.clear();
    }
}
