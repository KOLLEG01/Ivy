//! One original RTCP session: a single reception source, retained counters,
//! report sampling and after-callback schedule. See the SIPSorcery10.0.15
//! notice in native/phone-runtime/licenses/SIPSorcery-10.0.15.md.
use super::{
    MediaError, RtcpReceptionStatistics, RtcpReportBuilder, RtcpReportSchedule, RtcpRuntime,
    RtcpTransmitCounters, RtcpWallTime, RtpHeader,
};
use std::{sync::Arc, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtcpSessionObservation {
    pub created_local_ticks: i64,
    pub started_local_ticks: Option<i64>,
    pub last_activity_local_ticks: Option<i64>,
    pub sent: RtcpTransmitCounters,
    pub last_ntp_sent: u64,
    pub received_packets: u32,
    pub received_octets: u32,
    pub reception_source: Option<u32>,
    pub payload_sample_rate: i32,
    pub timed_out: bool,
    pub closed: bool,
    pub next: Option<Duration>,
    pub timer_failure: Option<MediaError>,
}

pub struct RtcpSession {
    builder: RtcpReportBuilder,
    runtime: Arc<dyn RtcpRuntime>,
    created: RtcpWallTime,
    started: Option<i64>,
    activity: Option<i64>,
    schedule: RtcpReportSchedule,
    sent: RtcpTransmitCounters,
    last_ntp_sent: u64,
    received_packets: u32,
    received_octets: u32,
    reception: Option<RtcpReceptionStatistics>,
    payload_sample_rate: i32,
    closed: bool,
    timer_failure: Option<MediaError>,
}
impl RtcpSession {
    pub fn new(
        source: u32,
        cname: &str,
        runtime: Arc<dyn RtcpRuntime>,
    ) -> Result<Self, MediaError> {
        let builder = RtcpReportBuilder::new(source, cname)?;
        let created = runtime.now()?;
        Ok(Self {
            builder,
            runtime,
            created,
            started: None,
            activity: None,
            schedule: RtcpReportSchedule::from_wall_ticks(created.local_ticks()),
            sent: RtcpTransmitCounters::default(),
            last_ntp_sent: 0,
            received_packets: 0,
            received_octets: 0,
            reception: None,
            payload_sample_rate: 0,
            closed: false,
            timer_failure: None,
        })
    }
    pub fn source(&self) -> u32 {
        self.builder.source()
    }
    pub fn observation(&self) -> RtcpSessionObservation {
        RtcpSessionObservation {
            created_local_ticks: self.created.local_ticks(),
            started_local_ticks: self.started,
            last_activity_local_ticks: self.activity,
            sent: self.sent,
            last_ntp_sent: self.last_ntp_sent,
            received_packets: self.received_packets,
            received_octets: self.received_octets,
            reception_source: self.reception.as_ref().map(RtcpReceptionStatistics::source),
            payload_sample_rate: self.payload_sample_rate,
            timed_out: self.schedule.timed_out(),
            closed: self.closed,
            next: self.schedule.next(),
            timer_failure: self.timer_failure,
        }
    }
    pub fn set_timeout_ms(&mut self, value: i32) {
        self.schedule.set_timeout_ms(value);
    }
    pub fn set_payload_sample_rate(&mut self, value: i32) {
        self.payload_sample_rate = value;
    }
    pub fn start(&mut self, now: impl FnOnce() -> Duration) -> Result<bool, MediaError> {
        if self.started.is_some() {
            return Ok(false);
        }
        // The original marks StartedAt before choosing entropy. An exception
        // there does not make later packet activity restart the broken timer.
        self.started = Some(self.runtime.now()?.local_ticks());
        let interval = self.runtime.interval_ms()?;
        // The original accepts late RTP/report bookkeeping after SDP has
        // closed RTCP. Start still samples time/entropy; its closed timer is inert.
        if self.closed {
            return Ok(true);
        }
        self.schedule.start(now(), interval)
    }
    pub fn received(&mut self, header: RtpHeader, octets: usize) -> Result<(), MediaError> {
        if octets > 65535 - RtpHeader::BYTES {
            return Err(MediaError::InvalidPacket);
        }
        let activity = self.runtime.now()?;
        self.activity = Some(activity.local_ticks());
        self.schedule.record_activity_ticks(activity.local_ticks());
        self.received_packets = self.received_packets.wrapping_add(1);
        self.received_octets = self.received_octets.wrapping_add(octets as u32);
        let reception = self
            .reception
            .get_or_insert_with(|| RtcpReceptionStatistics::new(header.source));
        let arrival = self
            .runtime
            .now()?
            .arrival(self.created, self.payload_sample_rate);
        reception.record(header.sequence, header.timestamp, arrival);
        Ok(())
    }
    /// The send owner calls this after its actual admitted transport send.
    /// RTP receive activity alone never starts the original report timer.
    pub fn sent(
        &mut self,
        header: RtpHeader,
        octets: usize,
        now: impl FnOnce() -> Duration,
    ) -> Result<(), MediaError> {
        if self.started.is_none() {
            self.start(now)?;
        }
        self.sent
            .record(header.sequence, header.timestamp, octets)?;
        self.last_ntp_sent = self.runtime.now()?.ntp();
        Ok(())
    }
    pub fn report_received(
        &mut self,
        sender_ntp: Option<u64>,
        now: impl FnOnce() -> Duration,
    ) -> Result<(), MediaError> {
        if self.started.is_none() {
            self.start(now)?;
        }
        let activity = self.runtime.now()?;
        self.activity = Some(activity.local_ticks());
        self.schedule.record_activity_ticks(activity.local_ticks());
        if let (Some(reception), Some(ntp)) = (self.reception.as_mut(), sender_ntp) {
            reception.sender_report(ntp, self.runtime.now()?.ntp32());
        }
        Ok(())
    }
    pub fn remove_reception(&mut self, source: u32) {
        if self
            .reception
            .as_ref()
            .is_some_and(|r| r.source() == source)
        {
            self.reception = None;
        }
    }
    pub fn report(&mut self) -> Result<Vec<u8>, MediaError> {
        let ntp = self.runtime.now()?.ntp();
        let sample = self
            .reception
            .as_mut()
            .map(|r| r.sample((ntp >> 16) as u32));
        let blocks = sample.as_slice();
        if self.schedule.sender_report(self.sent.packets) {
            self.builder.sender(self.sent.sender_info(ntp), blocks)
        } else {
            self.builder.receiver(blocks)
        }
    }
    pub fn begin_report(&mut self, now: Duration) -> Result<Option<bool>, MediaError> {
        if self.closed || self.schedule.next().is_none_or(|next| now < next) {
            return Ok(None);
        }
        let wall = self.runtime.now()?;
        self.schedule
            .begin_report_ticks(now, wall.local_ticks())
            .map(|event| event.map(|e| e.new_timeout))
    }
    pub fn complete_report(&mut self, now: impl FnOnce() -> Duration) -> Result<(), MediaError> {
        // Callback failure leaves the old count; entropy failure occurs after
        // the original has already committed it. Keep those failures distinct.
        self.schedule.commit_report_count(self.sent.packets)?;
        let interval = self.runtime.interval_ms()?;
        self.schedule
            .complete_report(now(), interval, self.sent.packets)
    }
    pub fn stop_timer(&mut self, error: MediaError) {
        self.timer_failure = Some(error);
        self.schedule.stop_timer();
    }
    pub fn close(&mut self) -> Result<Option<Vec<u8>>, MediaError> {
        self.close_with_reason("phone_media_closed")
    }
    pub(super) fn close_with_reason(
        &mut self,
        reason: &str,
    ) -> Result<Option<Vec<u8>>, MediaError> {
        if self.closed {
            return Ok(None);
        }
        self.closed = true;
        self.schedule.close();
        let mut report = self.report()?;
        report.extend_from_slice(&self.builder.bye_packet(reason));
        Ok(Some(report))
    }
}
