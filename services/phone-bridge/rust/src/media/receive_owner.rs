//! SIP receive composition. The call retains SDP, sender, RTCP scheduling and
//! owned audio disposal; this owner retains its actual RTP pair and gap pump.
use super::receive_analysis::ReceiveAnalysis;
use super::{
    AudioFormat, AudioNegotiation, CodecError, EvsLibrary, MediaCodec, MediaError, PcmAudioPort,
    PcmAudioState, RtpAudioPacket, RtpDatagramSink, RtpPacket, RtpPeer, RtpReceiveQueue,
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
    /// call's report owner parses and counts this compound before publication.
    fn rtcp(&self, source: SocketAddr, compound: &[u8]);
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
}
struct State {
    peer: Option<RtpPeer>,
    formats: Vec<AudioFormat>,
    event_payloads: Vec<u8>,
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
struct Inner {
    state: Mutex<State>,
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
}
impl Inner {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
    fn request_close(&self) {
        self.closed.store(true, Ordering::Release);
        self.stop.cancel();
        if let Some(transport) = self.transport.get() {
            transport.request_close();
        }
    }
    fn fail(&self, error: ReceiveError) {
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
        if !state.peer.is_some_and(|peer| {
            peer.rtp().port() == source.port() && crate::sip::same_ip(peer.rtp(), source)
        }) {
            state.queue.reject_peer();
            return Ok(Vec::new());
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
            let accepted = {
                let state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
                !inner.closed.load(Ordering::Acquire)
                    && state.peer.is_some_and(|peer| {
                        peer.rtcp().port() == source.port()
                            && crate::sip::same_ip(peer.rtcp(), source)
                    })
            };
            if accepted
                && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
/// wakeups. It does not own the call's audio route, sender, SIP dialog or RTCP
/// report state. Final call disposal must additionally revoke/close that route.
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
                peer: None,
                formats: Vec::new(),
                event_payloads: Vec::new(),
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
            evs,
            events,
            transport: OnceLock::new(),
            origin: Instant::now(),
            closed: AtomicBool::new(false),
            stop: CancellationToken::new(),
            pending: Notify::new(),
            completed: OnceLock::new(),
            changed: Notify::new(),
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
        inner
            .transport
            .get()
            .unwrap()
            .set_peer(Some(peer))
            .map_err(ReceiveError::Transport)?;
        state.queue.clear();
        state.receiver = None;
        state.observed = None;
        state.peer = Some(peer);
        state.formats = plan.receive_formats().to_vec();
        state.event_payloads = plan.offered_event_payloads().to_vec();
        Ok(())
    }
    pub fn observation(&self) -> SipReceiveObservation {
        let inner = &self.handle.0;
        let state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
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
