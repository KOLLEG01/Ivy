//! Call-local transport duplicate suppression. The call owner serializes access
//! and discards these trackers when the original call ends.
use std::{
    collections::{HashSet, VecDeque},
    hash::Hash,
    time::Duration,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputSource {
    Rfc4733,
    Rfc4733AlternatePayload,
    SipInfo,
    InBandAudio,
}

/// Suppressed echoes must not extend the window or change the accepted source.
#[derive(Default)]
pub struct InputDeduplicator {
    last: Option<(char, InputSource, Duration)>,
}

impl InputDeduplicator {
    pub fn is_new(&mut self, digit: char, source: InputSource, now: Duration) -> bool {
        if let Some((last_digit, last_source, at)) = self.last {
            if digit == last_digit
                && source != last_source
                && now.saturating_sub(at) <= Duration::from_millis(400)
            {
                return false;
            }
        }
        self.last = Some((digit, source, now));
        true
    }
}

// Both native trackers retain 64 distinct keys in FIFO order. Duplicates do not
// refresh their position. A tracker belongs to one original call.
struct Recent<K> {
    keys: HashSet<K>,
    order: VecDeque<K>,
}

impl<K: Copy + Eq + Hash> Default for Recent<K> {
    fn default() -> Self {
        Self {
            keys: HashSet::with_capacity(64),
            order: VecDeque::with_capacity(64),
        }
    }
}

impl<K: Copy + Eq + Hash> Recent<K> {
    fn is_new(&mut self, key: K) -> bool {
        if !self.keys.insert(key) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > 64 {
            self.keys.remove(&self.order.pop_front().unwrap());
        }
        true
    }
}

#[derive(Default)]
pub struct RtpEventTracker(Recent<(u32, u32, u8)>);

impl RtpEventTracker {
    /// Starts, ends and alternative payload numbers share event identity.
    pub fn is_new(&mut self, ssrc: u32, timestamp: u32, event: u8) -> bool {
        self.0.is_new((ssrc, timestamp, event))
    }
}

#[derive(Default)]
pub struct SipInfoTracker(Recent<i32>);

impl SipInfoTracker {
    pub fn is_new(&mut self, cseq: i32) -> bool {
        self.0.is_new(cseq)
    }
}

pub fn rtp_event_digit(event: u8) -> Option<char> {
    match event {
        0..=9 => Some(char::from(b'0' + event)),
        10 => Some('*'),
        11 => Some('#'),
        12..=15 => Some(char::from(b'A' + event - 12)),
        _ => None,
    }
}
