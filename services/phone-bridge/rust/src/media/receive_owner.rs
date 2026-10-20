//! SIP media transport composition. The call retains SDP and route disposal;
//! this owner retains its one RTP pair, RTCP, gap pump and optional PCM sender.
use super::receive_analysis::ReceiveAnalysis;
use super::rtcp_owner::{RtcpReceiveEffect, RtcpReceiveOwner};
use super::sender::{self, Sender};
use super::transport::MediaDestinations;
use super::{
    AudioFormat, AudioNegotiation, CodecError, EvsLibrary, MediaCodec, MediaError, PcmAudioPort,
    PcmAudioState, PcmSendPort, RtcpCompound, RtcpReceiveObservation, RtcpRuntime, RtcpSession,
    RtcpSessionObservation, RtpAudioPacket, RtpDatagramSink, RtpPacket, RtpPeer, RtpReceiveQueue,
    RtpReceiveStatus, RtpTransport, RtpTransportError, RtpTransportObservation,
};
use crate::dtmf::{InputSource, RtpEventTracker, rtp_event_digit};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveError {
    Closed,
    InvalidPlan,
    Media(MediaError),
    Codec(CodecError),
    Transport(RtpTransportError),
}
impl From<CodecError> for ReceiveError {
    fn from(value: CodecError) -> Self {
        Self::Codec(value)
    }
}
impl From<MediaError> for ReceiveError {
    fn from(value: MediaError) -> Self {
        Self::Media(value)
    }
}
impl std::fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ReceiveError {}

/// A bounded synchronous notification, invoked after releasing the receive
/// lock. The call owns digit policy and any cross-transport deduplication.
pub trait SipReceiveEvents: Send + Sync + 'static {
    fn digit(&self, digit: char, source: InputSource);
    fn failed(&self);
    /// Original negotiated RTCP channel, independent of the PCM permit. The
    /// whole datagram has validated. Raw control is distinct from an admitted
    /// application report; BYE or unknown-only controls may publish only here.
    fn rtcp(&self, source: SocketAddr, compound: &[u8]);
    fn report(&self, _source: SocketAddr, _report: &RtcpReceiveObservation) {}
    fn report_sent(&self, _report: &[u8]) {}
    fn rtcp_timeout(&self) {}
}

#[derive(Clone, Debug, PartialEq)]
pub struct SipReceiveObservation {
    pub transport: RtpTransportObservation,
    pub receive: RtpReceiveStatus,
    pub formats: Vec<AudioFormat>,
    pub event_payloads: Vec<u8>,
    pub received_packets: u64,
    pub unsupported_payload_packets: u64,
    pub event_packets: u64,
    pub detected_events: u64,
    pub inband_analyzed_packets: u64,
    pub inband_detected_digits: u64,
    pub inband: Option<crate::dtmf_detector::Diagnostics>,
    pub failed: bool,
    pub failure: Option<ReceiveError>,
    pub cleanup_confirmed: bool,
    pub rtcp: RtcpReceiveObservation,
    pub rtcp_session: Option<RtcpSessionObservation>,
}
struct State {
    destinations: MediaDestinations,
    formats: Vec<AudioFormat>,
    event_payloads: Vec<u8>,
    selected_event: u8,
    queue: RtpReceiveQueue,
    receiver: Option<MediaCodec>,
    observed: Option<PcmAudioState>,
    tones: RtpEventTracker,
    needs_inband: bool,
    analysis: Option<ReceiveAnalysis>,
    inband_analyzed_packets: u64,
    inband_detected_digits: u64,
    received_packets: u64,
    unsupported_payload_packets: u64,
    event_packets: u64,
    detected_events: u64,
    failure: Option<ReceiveError>,
}
#[derive(Default)]
struct Reports {
    admission: RtcpReceiveOwner,
    session: Option<RtcpSession>,
    closed: bool,
}
pub(super) struct Inner {
    state: Mutex<State>,
    reports: Mutex<Reports>,
    audio: Arc<dyn PcmAudioPort>,
    evs: Option<Arc<EvsLibrary>>,
    events: Arc<dyn SipReceiveEvents>,
    transport: OnceLock<RtpTransport>,
    origin: Instant,
    closed: AtomicBool,
    stop: CancellationToken,
    pending: Notify,
    completed: OnceLock<()>,
    changed: Notify,
    report_pending: Notify,
    pub(super) sender: Sender,
}
impl Inner {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
    fn request_close(&self) {
        // The same short transmit guard encloses final admission, all chunks
        // and bookkeeping. It is never held while waiting for capture/encode.
        self.sender.request_close();
        self.closed.store(true, Ordering::Release);
        // Separate from PCM decoding and all notifications. Concurrent close
        // callers serialize this one BYE before closing the original socket.
        {
            let mut reports = self.reports.lock().unwrap_or_else(|p| p.into_inner());
            if !reports.closed {
                reports.closed = true;
                if let Some(session) = reports.session.as_mut() {
                    if let Ok(Ok(Some(report))) =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.close()))
                    {
                        if let Some(transport) = self.transport.get() {
                            let _ = transport.send_rtcp(&report);
                        }
                    }
                }
            }
        }
        self.stop.cancel();
        if let Some(transport) = self.transport.get() {
            transport.request_close();
        }
    }
    pub(super) fn fail(&self, error: ReceiveError) {
        let first = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.failure.is_some() {
                false
            } else {
                state.failure = Some(error);
                true
            }
        };
        self.request_close();
        if first {
            // A notification failure must not abandon the actual cleanup.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.events.failed()));
        }
    }
    fn current(&self, state: &mut State) -> bool {
        let observed = self.audio.state();
        if state.observed != Some(observed) {
            state.queue.clear();
            state.receiver = None;
            state.observed = Some(observed);
        }
        observed.open && !self.closed.load(Ordering::Acquire)
    }
    fn drain(&self, state: &mut State, now: Duration) -> Result<(), ReceiveError> {
        if !self.current(state) {
            return Ok(());
        }
        while let Some((mut packet, discontinuity)) = state.queue.take(now, false)? {
            let result = (|| {
                if !self.current(state) || state.observed.unwrap().generation != packet.generation {
                    return Ok(());
                }
                let Some(format) = state
                    .formats
                    .iter()
                    .find(|f| f.payload() == packet.payload_type)
                else {
                    return Err(ReceiveError::InvalidPlan);
                };
                if discontinuity
                    || state
                        .receiver
                        .as_ref()
                        .is_none_or(|codec| codec.format() != format)
                {
                    state.receiver = Some(MediaCodec::new(
                        format.clone(),
                        self.audio.clone(),
                        self.evs.clone(),
                    )?);
                }
                // SipMediaSession.DrainReceive uses Decode, not DecodeRtp.
                // Preserve that policy until a separate recovery change is agreed.
                state
                    .receiver
                    .as_mut()
                    .unwrap()
                    .decode(&packet.payload, Some(packet.generation))?;
                state.received_packets += 1;
                Ok(())
            })();
            packet.payload.fill(0);
            result?;
        }
        Ok(())
    }
    fn packet(
        &self,
        source: SocketAddr,
        packet: RtpPacket<'_>,
    ) -> Result<Vec<(char, InputSource)>, ReceiveError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if self.closed.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }
        // Keep the peer boundary in the media owner as well as the socket:
        // a renegotiation may have replaced it after transport admission.
        if !state
            .destinations
            .rtp
            .is_some_and(|peer| peer.port() == source.port() && crate::sip::same_ip(peer, source))
        {
            state.queue.reject_peer();
            return Ok(Vec::new());
        }
        // The original media channel ignores exactly the bare twelve-byte
        // header. Keep framing acceptance separate from application admission.
        if packet.payload.is_empty()
            && packet.contributing_sources.is_empty()
            && packet.extension.is_none()
            && packet.padding == 0
        {
            return Ok(Vec::new());
        }
        if state.selected_event == 0 || packet.header.payload_type != state.selected_event {
            let mut reports = self.reports.lock().unwrap_or_else(|p| p.into_inner());
            if reports.closed {
                return Ok(Vec::new());
            }
            reports.admission.learn(packet.header.source);
        }
        if state.event_payloads.contains(&packet.header.payload_type) {
            state.event_packets += 1;
            if let Some(digit) = packet
                .payload
                .first()
                .filter(|_| packet.payload.len() >= 4)
                .and_then(|id| rtp_event_digit(*id))
            {
                if state.tones.is_new(
                    packet.header.source,
                    packet.header.timestamp,
                    packet.payload[0],
                ) {
                    state.detected_events += 1;
                    return Ok(vec![(digit, InputSource::Rfc4733)]);
                }
            }
            return Ok(Vec::new());
        }
        if !state
            .formats
            .iter()
            .any(|format| format.payload() == packet.header.payload_type)
        {
            state.unsupported_payload_packets += 1;
            return Ok(Vec::new());
        }
        let mut digits = Vec::new();
        // Analysis precedes the PCM route gate, as in SipMediaSession. This
        // decoder writes only into a private detector with no capture/device.
        if state.needs_inband {
            state.inband_analyzed_packets += 1;
            let format = state
                .formats
                .iter()
                .find(|f| f.payload() == packet.header.payload_type)
                .unwrap()
                .clone();
            let detected = state
                .analysis
                .get_or_insert_with(ReceiveAnalysis::default)
                .receive(&format, packet.payload, self.evs.clone())?;
            state.inband_detected_digits += detected.len() as u64;
            digits.extend(
                detected
                    .into_iter()
                    .map(|digit| (digit, InputSource::InBandAudio)),
            );
        } else {
            state.analysis = None;
        }
        let now = self.now();
        if !self.current(&mut state) {
            state
                .queue
                .skip(packet.header.source, packet.header.sequence, now)?;
            return Ok(digits);
        }
        let mut owned = RtpAudioPacket {
            sequence: packet.header.sequence,
            timestamp: packet.header.timestamp,
            source: packet.header.source,
            payload_type: packet.header.payload_type,
            payload: packet.payload.to_vec(),
            generation: state.observed.unwrap().generation,
        };
        let admitted = state.queue.add(&owned, now);
        owned.payload.fill(0);
        admitted?;
        self.drain(&mut state, now)?;
        if state.queue.status().queued_packets > 0 {
            self.pending.notify_one();
        }
        Ok(digits)
    }
    fn record_received(&self, source: SocketAddr, packet: RtpPacket<'_>) {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if self.closed.load(Ordering::Acquire)
            || !state
                .destinations
                .rtp
                .is_some_and(|p| p.port() == source.port() && crate::sip::same_ip(p, source))
            || state.event_payloads.contains(&packet.header.payload_type)
            || !state
                .formats
                .iter()
                .any(|f| f.payload() == packet.header.payload_type)
            || (packet.payload.is_empty()
                && packet.contributing_sources.is_empty()
                && packet.extension.is_none()
                && packet.padding == 0)
        {
            return;
        }
        let mut reports = self.reports.lock().unwrap_or_else(|p| p.into_inner());
        if reports.closed {
            return;
        }
        if let Some(session) = reports.session.as_mut() {
            if let Err(error) = session.received(packet.header, packet.payload.len()) {
                session.stop_timer(error);
            }
        }
    }
    pub(super) fn send_frame(
        &self,
        frames: &mut super::RtpAudioFrames,
        format: &AudioFormat,
        packet: &[u8],
        #[cfg(test)] hook: Option<&sender::SendHook>,
        admitted: PcmAudioState,
    ) -> Result<(), ReceiveError> {
        let mut reports = self.reports.lock().unwrap_or_else(|p| p.into_inner());
        let mut report_error = None;
        let result = frames.send(
            format.rtp_duration(),
            format.payload(),
            packet,
            |header, part| {
                // Original socket-send errors are ignored; missing destination and
                // callback failures keep their separate packetization boundaries.
                match self.transport.get().unwrap().send_rtp(header, part) {
                    Ok(()) | Err(RtpTransportError::Io | RtpTransportError::WouldBlock) => (),
                    Err(error) => return Err(error),
                }
                if let Some(session) = reports.session.as_mut() {
                    if let Err(error) = session.sent(header, part.len(), || self.now()) {
                        report_error = Some(error);
                        return Err(RtpTransportError::Io);
                    }
                    self.report_pending.notify_one();
                }
                #[cfg(test)]
                if let Some(hook) = hook {
                    hook(sender::SendPoint::Chunk, admitted, &mut Vec::new());
                }
                Ok(())
            },
        );
        #[cfg(not(test))]
        let _ = admitted;
        if let Some(error) = report_error {
            return Err(ReceiveError::Media(error));
        }
        result.map_err(ReceiveError::Transport)
    }
}
struct Sink(Weak<Inner>);
impl RtpDatagramSink for Sink {
    fn rtp(&self, source: SocketAddr, packet: RtpPacket<'_>) {
        if let Some(inner) = self.0.upgrade() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match inner.packet(source, packet) {
                    Ok(digits) => {
                        for (digit, source) in digits {
                            if inner.closed.load(Ordering::Acquire) {
                                break;
                            }
                            inner.events.digit(digit, source);
                        }
                        inner.record_received(source, packet);
                    }
                    Err(error) => inner.fail(error),
                }
            }));
            if result.is_err() {
                inner.fail(ReceiveError::Codec(CodecError::Failed));
            }
        }
    }
    fn rtcp(&self, source: SocketAddr, compound: &[u8]) {
        if let Some(inner) = self.0.upgrade() {
            let Ok(report) = RtcpCompound::parse(compound) else {
                return;
            };
            let notification = {
                let state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                let accepted = !inner.closed.load(Ordering::Acquire)
                    && state.destinations.rtcp.is_some_and(|peer| {
                        peer.port() == source.port() && crate::sip::same_ip(peer, source)
                    });
                if !accepted {
                    return;
                }
                let mut reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
                if reports.closed || inner.closed.load(Ordering::Acquire) {
                    return;
                }
                let effect = if compound.len() > 12 {
                    let Reports {
                        admission, session, ..
                    } = &mut *reports;
                    admission.receive(report, || {
                        if let Some(session) = session.as_mut() {
                            if let Err(error) = session.report_received(
                                report.sender.map(|r| r.info.ntp_timestamp),
                                || inner.now(),
                            ) {
                                session.stop_timer(error);
                            }
                        }
                    })
                } else {
                    RtcpReceiveEffect::None
                };
                match effect {
                    RtcpReceiveEffect::Report => {
                        inner.report_pending.notify_one();
                        Some(reports.admission.observation())
                    }
                    RtcpReceiveEffect::Bye(source) => {
                        if let Some(session) = reports.session.as_mut() {
                            session.remove_reception(source);
                        }
                        None
                    }
                    RtcpReceiveEffect::None => None,
                }
            };
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(report) = notification {
                    inner.events.report(source, &report);
                }
                inner.events.rtcp(source, compound)
            }))
            .is_err()
            {
                inner.fail(ReceiveError::Codec(CodecError::Failed));
            }
        }
    }
}
struct Handle(Arc<Inner>);
impl Drop for Handle {
    fn drop(&mut self) {
        self.0.request_close();
    }
}

/// A running SIP audio receiver with one actual socket pair and bounded gap
/// wakeups, opt-in RTCP reporting and a dedicated sender. It does not own the
/// call's audio route or SIP dialog. Final disposal additionally closes the route.
#[derive(Clone)]
pub struct SipRtpReceiver {
    handle: Arc<Handle>,
}
impl SipRtpReceiver {
    pub fn bind(
        interface: SocketAddr,
        first: u16,
        last: u16,
        reorder_ms: u32,
        audio: Arc<dyn PcmAudioPort>,
        evs: Option<Arc<EvsLibrary>>,
        events: Arc<dyn SipReceiveEvents>,
    ) -> Result<Self, ReceiveError> {
        let queue = RtpReceiveQueue::new(reorder_ms)?;
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                destinations: MediaDestinations::default(),
                formats: Vec::new(),
                event_payloads: Vec::new(),
                selected_event: 101,
                queue,
                receiver: None,
                observed: None,
                tones: RtpEventTracker::default(),
                needs_inband: false,
                analysis: None,
                inband_analyzed_packets: 0,
                inband_detected_digits: 0,
                received_packets: 0,
                unsupported_payload_packets: 0,
                event_packets: 0,
                detected_events: 0,
                failure: None,
            }),
            audio,
            reports: Mutex::new(Reports::default()),
            evs,
            events,
            transport: OnceLock::new(),
            origin: Instant::now(),
            closed: AtomicBool::new(false),
            stop: CancellationToken::new(),
            pending: Notify::new(),
            completed: OnceLock::new(),
            changed: Notify::new(),
            report_pending: Notify::new(),
            sender: Sender::default(),
        });
        let transport = RtpTransport::bind(
            interface,
            first,
            last,
            Arc::new(Sink(Arc::downgrade(&inner))),
        )
        .map_err(ReceiveError::Transport)?;
        let _ = inner.transport.set(transport);
        let worker_inner = inner.clone();
        let mut worker = tokio::spawn(async move { pump(worker_inner).await });
        let report_inner = inner.clone();
        let reports = tokio::spawn(async move { report_pump(report_inner).await });
        let completion = inner.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = &mut worker => {
                    if result.is_err() { completion.fail(ReceiveError::Codec(CodecError::Failed)); }
                },
                _ = completion.transport.get().unwrap().wait_closed() => {
                    if !completion.closed.load(Ordering::Acquire) {
                        completion.fail(ReceiveError::Transport(RtpTransportError::Io));
                    }
                    let _ = worker.await;
                },
            }
            completion.request_close();
            completion.sender.wait_closed().await;
            let _ = reports.await;
            completion.transport.get().unwrap().close().await;
            {
                let mut state = completion.state.lock().unwrap_or_else(|p| p.into_inner());
                state.queue.clear();
                state.receiver = None;
                state.analysis = None;
            }
            let _ = completion.completed.set(());
            completion.changed.notify_waiters();
        });
        Ok(Self {
            handle: Arc::new(Handle(inner)),
        })
    }
    /// Apply only an accepted SDP negotiation, under the same receive lock as
    /// queue admission. Probe real codecs before committing the new peer/plan.
    pub fn configure(&self, plan: &AudioNegotiation, peer: RtpPeer) -> Result<(), ReceiveError> {
        self.configure_description(
            plan,
            MediaDestinations {
                rtp: Some(peer.rtp()),
                rtcp: Some(peer.rtcp()),
            },
        )
    }
    pub(super) fn configure_description(
        &self,
        plan: &AudioNegotiation,
        destinations: MediaDestinations,
    ) -> Result<(), ReceiveError> {
        let inner = &self.handle.0;
        if inner.closed.load(Ordering::Acquire) {
            return Err(ReceiveError::Closed);
        }
        for format in plan.receive_formats() {
            let _ = MediaCodec::new(format.clone(), inner.audio.clone(), inner.evs.clone())?;
        }
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if inner.closed.load(Ordering::Acquire) {
            return Err(ReceiveError::Closed);
        }
        let mut sender = inner.sender.state.lock().unwrap_or_else(|p| p.into_inner());
        if sender.closed {
            return Err(ReceiveError::Closed);
        }
        let send_audio: Arc<dyn PcmAudioPort> = match &sender.audio {
            Some(audio) => audio.clone(),
            None => inner.audio.clone(),
        };
        let codec = MediaCodec::new(plan.sender().clone(), send_audio, inner.evs.clone())?;
        let mut reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
        if reports.closed || inner.closed.load(Ordering::Acquire) {
            return Err(ReceiveError::Closed);
        }
        inner
            .transport
            .get()
            .unwrap()
            .set_destinations(destinations.rtp, destinations.rtcp)
            .map_err(ReceiveError::Transport)?;
        state.queue.clear();
        state.receiver = None;
        state.observed = None;
        state.destinations = destinations;
        state.formats = plan.receive_formats().to_vec();
        state.event_payloads = plan.offered_event_payloads().to_vec();
        state.selected_event = plan.selected_event_payload();
        reports.admission.configure(plan.remote_sources());
        sender.revision += 1;
        sender.format = Some(plan.sender().clone());
        sender.codec = Some(codec);
        Ok(())
    }
    /// Endpoint/direction effects precede codec compatibility in the original
    /// SDP stack, even for a rejected offer. Inactive closes RTCP against its
    /// previous control destination; the original pair and counters survive.
    pub(super) fn description_effects(
        &self,
        destinations: MediaDestinations,
        enabled: bool,
        inactive: bool,
    ) -> Result<Option<Vec<u8>>, ReceiveError> {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut sender = inner.sender.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
        if sender.closed || reports.closed || inner.closed.load(Ordering::Acquire) {
            return Err(ReceiveError::Closed);
        }
        let report = if inactive {
            reports
                .session
                .as_mut()
                .map(|s| s.close_with_reason(""))
                .transpose()?
                .flatten()
        } else {
            None
        };
        if let Some(bytes) = &report {
            let _ = inner.transport.get().unwrap().send_rtcp(bytes);
            reports.admission.sent();
            inner.report_pending.notify_one();
        }
        inner
            .transport
            .get()
            .unwrap()
            .set_destinations(destinations.rtp, destinations.rtcp)
            .map_err(ReceiveError::Transport)?;
        state.destinations = destinations;
        sender.rtp_enabled = enabled;
        Ok(report)
    }
    /// Only the session can switch from its waiting port to its original
    /// screening port. Keep the sender/thread/socket and invalidate old work.
    pub(super) fn change_send_audio(&self, change: impl FnOnce()) -> Result<(), ReceiveError> {
        let inner = &self.handle.0;
        let mut sender = inner.sender.state.lock().unwrap_or_else(|p| p.into_inner());
        if sender.closed {
            return Err(ReceiveError::Closed);
        }
        let audio: Arc<dyn PcmAudioPort> = sender
            .audio
            .as_ref()
            .map(|a| a.clone() as Arc<dyn PcmAudioPort>)
            .unwrap_or_else(|| inner.audio.clone());
        let codec = sender
            .format
            .as_ref()
            .map(|format| MediaCodec::new(format.clone(), audio, inner.evs.clone()))
            .transpose()?;
        change();
        sender.revision += 1;
        sender.codec = codec;
        Ok(())
    }
    /// Start once on the original call/audio owner. SDP configuration replaces
    /// its codec under the transmit guard; it never starts a second sender.
    /// Route disposal/revocation remains the call's responsibility. Close waits
    /// for the dedicated thread as well as its existing receive/report owners.
    pub fn start_sending(
        &self,
        audio: Arc<dyn PcmSendPort>,
        source: u32,
        sequence: u16,
        timestamp: u32,
    ) -> Result<bool, ReceiveError> {
        let inner = &self.handle.0;
        self.set_local_source(source)?;
        sender::start(
            inner.clone(),
            audio,
            source,
            sequence,
            timestamp,
            inner.evs.clone(),
        )
    }
    pub fn transmission(&self) -> (Option<AudioFormat>, u64) {
        let state = self
            .handle
            .0
            .sender
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        (state.transmitted.clone(), state.sent)
    }
    pub(super) fn selected_send_format(&self) -> Option<AudioFormat> {
        self.handle
            .0
            .sender
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .format
            .clone()
    }
    #[cfg(test)]
    pub(super) fn sender_state(&self) -> &Sender {
        &self.handle.0.sender
    }
    /// Construct once, using the call's SDP/sender identity. Enabling does not
    /// start the timer; explicit Start, an admitted report or RTP send does.
    pub fn enable_reporting(
        &self,
        source: u32,
        cname: &str,
        runtime: Arc<dyn RtcpRuntime>,
    ) -> Result<(), ReceiveError> {
        let session = RtcpSession::new(source, cname, runtime)?;
        let inner = &self.handle.0;
        let mut reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
        if reports.closed || inner.closed.load(Ordering::Acquire) {
            return Err(ReceiveError::Closed);
        }
        if reports.session.is_some() || !reports.admission.set_local_source(source) {
            return Err(ReceiveError::InvalidPlan);
        }
        reports.session = Some(session);
        Ok(())
    }
    pub fn start_reporting(&self) -> Result<bool, ReceiveError> {
        let inner = &self.handle.0;
        let mut reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
        if reports.closed || inner.closed.load(Ordering::Acquire) {
            return Err(ReceiveError::Closed);
        }
        let session = reports.session.as_mut().ok_or(ReceiveError::InvalidPlan)?;
        let result = session.start(|| inner.now());
        if let Err(error) = result {
            session.stop_timer(error);
        }
        inner.report_pending.notify_one();
        result.map_err(ReceiveError::Media)
    }
    /// Like the original GetRtcpReport, reading commits reception intervals,
    /// but does not dispatch or commit the timer's previous sent-packet count.
    pub fn read_report(&self) -> Result<Vec<u8>, ReceiveError> {
        let mut reports = self
            .handle
            .0
            .reports
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        reports
            .session
            .as_mut()
            .ok_or(ReceiveError::InvalidPlan)?
            .report()
            .map_err(ReceiveError::Media)
    }
    /// The call supplies the same immutable local source as its SDP/sender.
    pub fn set_local_source(&self, source: u32) -> Result<(), ReceiveError> {
        let inner = &self.handle.0;
        let mut reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
        if inner.closed.load(Ordering::Acquire) {
            return Err(ReceiveError::Closed);
        }
        if !reports.admission.set_local_source(source) {
            return Err(ReceiveError::InvalidPlan);
        }
        Ok(())
    }
    pub fn observation(&self) -> SipReceiveObservation {
        let inner = &self.handle.0;
        let state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        let reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
        SipReceiveObservation {
            transport: inner.transport.get().unwrap().observation(),
            receive: state.queue.status(),
            formats: state.formats.clone(),
            event_payloads: state.event_payloads.clone(),
            received_packets: state.received_packets,
            unsupported_payload_packets: state.unsupported_payload_packets,
            event_packets: state.event_packets,
            detected_events: state.detected_events,
            inband_analyzed_packets: state.inband_analyzed_packets,
            inband_detected_digits: state.inband_detected_digits,
            inband: state.analysis.as_ref().map(ReceiveAnalysis::diagnostics),
            failed: state.failure.is_some(),
            failure: state.failure,
            cleanup_confirmed: inner.completed.get().is_some(),
            rtcp: reports.admission.observation(),
            rtcp_session: reports.session.as_ref().map(RtcpSession::observation),
        }
    }
    /// The call updates this when its access/command policy changes. Analysis
    /// remains independent of route permission and is disposed when disabled.
    pub fn set_inband_needed(&self, needed: bool) {
        let mut state = self
            .handle
            .0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if !self.handle.0.closed.load(Ordering::Acquire) {
            state.needs_inband = needed;
            if !needed {
                state.analysis = None;
            }
        }
    }
    pub fn request_close(&self) {
        self.handle.0.request_close();
    }
    /// The cleanup owner outlives all observers. Cancelling one close waiter
    /// cannot retain the original bind or abandon queued PCM/codec disposal.
    pub async fn close(&self) {
        self.request_close();
        let inner = &self.handle.0;
        loop {
            let changed = inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if inner.completed.get().is_some() {
                return;
            }
            changed.await;
        }
    }
}
async fn pump(inner: Arc<Inner>) {
    loop {
        tokio::select! {
            _ = inner.stop.cancelled() => break,
            _ = inner.pending.notified() => (),
        }
        // Only a sequence gap waits. Arrival of the missing packet still
        // drains immediately under the receive lock while this timer runs.
        let delay = inner
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .queue
            .status()
            .reorder_ms
            .max(1);
        tokio::select! {
            _ = inner.stop.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(delay.into())) => (),
        }
        let result = {
            let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            let result = inner.drain(&mut state, inner.now());
            if state.queue.status().queued_packets > 0 {
                inner.pending.notify_one();
            }
            result
        };
        if let Err(error) = result {
            inner.fail(error);
            break;
        }
    }
}

async fn report_pump(inner: Arc<Inner>) {
    loop {
        let pending = inner.report_pending.notified();
        tokio::pin!(pending);
        pending.as_mut().enable();
        let next = {
            let reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
            reports.session.as_ref().and_then(|s| s.observation().next)
        };
        let Some(next) = next else {
            tokio::select! {_=inner.stop.cancelled()=>return,_=pending=>continue}
        };
        let Some(deadline) = inner.origin.checked_add(next) else {
            stop_reports(&inner, MediaError::ClockOverflow);
            continue;
        };
        tokio::select! {_=inner.stop.cancelled()=>return,_=pending=>continue,_=tokio::time::sleep_until(deadline.into())=>()}
        let begin = {
            let mut reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
            if reports.closed {
                return;
            }
            match reports.session.as_mut().unwrap().begin_report(inner.now()) {
                Ok(event) => event,
                Err(error) => {
                    reports.session.as_mut().unwrap().stop_timer(error);
                    None
                }
            }
        };
        let Some(timeout) = begin else {
            continue;
        };
        if timeout
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                inner.events.rtcp_timeout()
            }))
            .is_err()
        {
            stop_reports(&inner, MediaError::ReportCallbackFailed);
            continue;
        }
        let report = {
            let mut reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
            if reports.closed {
                None
            } else {
                match reports.session.as_mut().unwrap().report() {
                    Ok(bytes) => {
                        // The original wrapper reports an attempted dispatch
                        // even with a missing destination or socket send error.
                        let _ = inner.transport.get().unwrap().send_rtcp(&bytes);
                        reports.admission.sent();
                        Some(bytes)
                    }
                    Err(error) => {
                        reports.session.as_mut().unwrap().stop_timer(error);
                        continue;
                    }
                }
            }
        };
        if let Some(report) = report {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                inner.events.report_sent(&report)
            }))
            .is_err()
            {
                stop_reports(&inner, MediaError::ReportCallbackFailed);
                continue;
            }
        }
        // Completion may follow a reentrant Close. Commit the original sent
        // count, but the closed schedule never rearms or dispatches a packet.
        let mut reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
        let session = reports.session.as_mut().unwrap();
        if let Err(error) = session.complete_report(|| inner.now()) {
            session.stop_timer(error);
        }
    }
}
fn stop_reports(inner: &Inner, error: MediaError) {
    let mut reports = inner.reports.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(session) = reports.session.as_mut() {
        session.stop_timer(error);
    }
}
#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_rtcp_runtime_private.rs"]
mod report_tests;
