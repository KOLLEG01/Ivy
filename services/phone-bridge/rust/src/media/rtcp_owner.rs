//! Application report admission for one original audio stream. Source identity
//! is independent of PCM queue/generation state and survives report sampling.
//! Routing retains SIPSorcery 10.0.15 behavior (Aaron Clauson); the full notice
//! is in native/phone-runtime/licenses/SIPSorcery-10.0.15.md.
use super::{RtcpCompound, RtcpReportBlock};
use std::time::SystemTime;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RtcpReceiveObservation {
    pub reports_received: u64,
    pub reports_sent: u64,
    pub last_report_at: Option<SystemTime>,
    pub peer_reports: Vec<RtcpReportBlock>,
    pub learned_source: Option<u32>,
    pub declared_sources: Vec<u32>,
    pub local_source: Option<u32>,
}

#[derive(Default)]
pub(super) struct RtcpReceiveOwner {
    observation: RtcpReceiveObservation,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RtcpReceiveEffect {
    None,
    Report,
    Bye(u32),
}
impl RtcpReceiveOwner {
    pub fn observation(&self) -> RtcpReceiveObservation {
        self.observation.clone()
    }
    pub fn set_local_source(&mut self, source: u32) -> bool {
        if self
            .observation
            .local_source
            .is_some_and(|current| current != source)
        {
            return false;
        }
        self.observation.local_source = Some(source);
        true
    }
    pub fn configure(&mut self, declared: &[u32]) {
        self.observation.learned_source = Some(0);
        self.observation.declared_sources = declared.to_vec();
    }
    pub fn learn(&mut self, source: u32) {
        if self.observation.learned_source == Some(0) {
            self.observation.learned_source = Some(source);
        }
    }
    pub fn sent(&mut self) {
        self.observation.reports_sent = self.observation.reports_sent.wrapping_add(1);
    }
    fn matches(&self, source: u32) -> bool {
        self.observation.local_source == Some(source)
            || self.observation.learned_source == Some(source)
            || self.observation.declared_sources.contains(&source)
    }
    fn stream(&self, report: RtcpCompound<'_>) -> bool {
        // The original SR branch returns immediately, even on a miss. Only RR
        // falls back to its reception blocks; feedback uses sender, TWCC media.
        if let Some(sender) = report.sender {
            return self.matches(sender.source);
        }
        if let Some(receiver) = report.receiver {
            return self.matches(receiver.source)
                || receiver.blocks.iter().any(|b| self.matches(b.source));
        }
        if let Some(feedback) = report.feedback_packet {
            return self.matches(word(feedback, 4));
        }
        report
            .twcc_packet
            .is_some_and(|packet| self.matches(word(packet, 8)))
    }
    /// Called only after whole-datagram validation and the current peer/closed
    /// boundary, under the receiver lock. BYE never publishes an application
    /// report; only its first source can reset an already matched remote track.
    pub fn receive(
        &mut self,
        report: RtcpCompound<'_>,
        before_report: impl FnOnce(),
    ) -> RtcpReceiveEffect {
        let matched = self.stream(report);
        if let Some(bye) = report.bye_packet {
            if matched && bye[0] & 31 != 0 && self.observation.learned_source == Some(word(bye, 4))
            {
                self.observation.learned_source = Some(0);
                return RtcpReceiveEffect::Bye(word(bye, 4));
            }
            return RtcpReceiveEffect::None;
        }
        if !matched {
            return RtcpReceiveEffect::None;
        }
        before_report();
        self.observation.reports_received = self.observation.reports_received.wrapping_add(1);
        self.observation.last_report_at = Some(SystemTime::now());
        self.observation.peer_reports = report
            .reception_reports()
            .map(|blocks| blocks.iter().take(8).collect())
            .unwrap_or_default();
        RtcpReceiveEffect::Report
    }
}
fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
