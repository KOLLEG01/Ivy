use super::{RealtimeAudio, RealtimeError, Result, pcm::Core};
use crate::media::{PcmAudioState, RtpAudioPacket};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use str0m::{
    Candidate, Event, IceConnectionState, Input, Output, Rtc,
    change::{SdpAnswer, SdpPendingOffer},
    media::{Direction, MediaKind, Mid},
    net::{Protocol, Receive, Transmit},
    rtp::RtpWrite,
};

type SendDatagram<'a> = dyn FnMut(Transmit) -> Result<()> + 'a;
#[derive(Default)]
struct Probation {
    previous: Option<(u32, u16)>,
    ready: bool,
}
impl Probation {
    fn admit(&mut self, ssrc: u32, sequence: u16) -> bool {
        if self.ready {
            return true;
        }
        let consecutive = self.previous.is_some_and(|(source, previous)| {
            source == ssrc && sequence == previous.wrapping_add(1)
        });
        self.previous = Some((ssrc, sequence));
        consecutive
    }
}
/// One mutation owner drives the supplied monotonic clock, candidates and UDP
/// datagrams. All output is drained before another engine mutation. The sender
/// callback is bounded, synchronous and may not reenter this peer or its PCM port.
pub(crate) struct RealtimePeer {
    rtc: Option<Rtc>,
    audio: RealtimeAudio,
    mid: Option<Mid>,
    offer: String,
    pending: Option<SdpPendingOffer>,
    answer_deadline: Option<Instant>,
    timeout: Instant,
    started: Instant,
    last: Instant,
    sequence: u64,
    timestamp: u32,
    pending_audio: Option<PcmAudioState>,
    probation: Probation,
    state: &'static str,
    channel_open: bool,
    closed_result: Option<Result<()>>,
}
impl RealtimePeer {
    pub fn new(
        now: Instant,
        candidates: &[SocketAddr],
        queue_ms: u16,
        prebuffer_ms: Option<u16>,
        permitted: Arc<dyn Fn() -> bool + Send + Sync>,
        send: &mut SendDatagram<'_>,
    ) -> Result<Self> {
        let core = Core::new(queue_ms, prebuffer_ms, permitted)?;
        Self::with_audio(
            now,
            candidates,
            RealtimeAudio(Arc::new(Mutex::new(core))),
            send,
        )
    }
    pub(super) fn with_audio(
        now: Instant,
        candidates: &[SocketAddr],
        audio: RealtimeAudio,
        send: &mut SendDatagram<'_>,
    ) -> Result<Self> {
        let rtc = Rtc::builder()
            .clear_codecs()
            .enable_opus(true, false)
            .set_rtp_mode(true)
            .set_fingerprint_verification(true)
            .build(now);
        let mut peer = Self {
            rtc: Some(rtc),
            audio,
            mid: None,
            offer: String::new(),
            pending: None,
            answer_deadline: None,
            timeout: now,
            started: now,
            last: now,
            sequence: 0,
            timestamp: 0,
            pending_audio: None,
            probation: Probation::default(),
            state: "new",
            channel_open: false,
            closed_result: None,
        };
        peer.drain(send)?;
        for address in candidates {
            let candidate =
                Candidate::host(*address, "udp").map_err(|_| RealtimeError::InvalidRequest)?;
            peer.rtc.as_mut().unwrap().add_local_candidate(candidate);
            peer.drain(send)?;
        }
        let mut change = peer.rtc.as_mut().unwrap().sdp_api();
        let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        change.add_channel("oai-events".into());
        let (offer, pending) = change.apply().ok_or(RealtimeError::Failed)?;
        peer.mid = Some(mid);
        peer.offer = offer.to_sdp_string();
        peer.pending = Some(pending);
        peer.drain(send)?;
        Ok(peer)
    }
    pub fn audio(&self) -> RealtimeAudio {
        self.audio.clone()
    }
    pub fn offer(&self) -> &str {
        &self.offer
    }
    pub fn connection_state(&self) -> &'static str {
        self.state
    }
    pub fn events_open(&self) -> bool {
        self.channel_open
    }
    pub fn next_timeout(&self) -> Instant {
        self.timeout
    }
    fn advance(&mut self, now: Instant) -> Result<()> {
        if now < self.last {
            return Err(RealtimeError::InvalidRequest);
        }
        if self.rtc.is_none() {
            return Err(RealtimeError::Closed);
        }
        if self.audio.0.lock().unwrap().failed {
            return Err(RealtimeError::Failed);
        }
        self.last = now;
        self.audio.0.lock().unwrap().now = now.duration_since(self.started);
        Ok(())
    }
    pub fn answer(&mut self, now: Instant, text: &str, send: &mut SendDatagram<'_>) -> Result<()> {
        if text.trim().is_empty() || text.encode_utf16().count() > 48000 {
            return Err(RealtimeError::InvalidRequest);
        }
        let answer = SdpAnswer::from_sdp_string(text).map_err(|_| RealtimeError::InvalidAnswer)?;
        self.advance(now)?;
        let rtc = self.rtc.as_mut().ok_or(RealtimeError::Closed)?;
        let pending = self.pending.take().ok_or(RealtimeError::NotReady)?;
        let accepted = rtc.sdp_api().accept_answer(pending, answer);
        // Even declined negotiation may have mutated the engine.
        self.drain(send)?;
        accepted.map_err(|_| RealtimeError::InvalidAnswer)?;
        let rtc = self.rtc.as_ref().unwrap();
        let mid = self.mid.unwrap();
        let media = rtc.media(mid).ok_or(RealtimeError::InvalidAnswer)?;
        let payload = rtc
            .codec_config()
            .params()
            .iter()
            .find(|p| {
                media.remote_pts().contains(&p.pt()) && p.spec().codec == str0m::format::Codec::Opus
            })
            .map(|p| *p.pt())
            .ok_or(RealtimeError::InvalidAnswer)?;
        self.audio.0.lock().unwrap().negotiate(payload)?;
        self.answer_deadline = Some(now + Duration::from_secs(15));
        Ok(())
    }
    pub fn connection_result(&self, now: Instant) -> Result<bool> {
        let core = self.audio.0.lock().unwrap();
        if core.closed {
            return Err(RealtimeError::Closed);
        }
        if core.failed {
            return Err(RealtimeError::Failed);
        }
        if core.connected {
            return Ok(true);
        }
        let deadline = self.answer_deadline.ok_or(RealtimeError::NotReady)?;
        if now >= deadline {
            Err(RealtimeError::Timeout)
        } else {
            Ok(false)
        }
    }
    pub fn receive(
        &mut self,
        now: Instant,
        source: SocketAddr,
        destination: SocketAddr,
        bytes: &[u8],
        send: &mut SendDatagram<'_>,
    ) -> Result<bool> {
        self.advance(now)?;
        let rtc = self.rtc.as_mut().ok_or(RealtimeError::Closed)?;
        if let Some((ssrc, sequence)) = rtp_header(bytes) {
            // Never let a pre-DTLS probe seed this peer's SRTP replay window.
            // After DTLS, consecutive packets are required before authentication
            // is attempted; only an authenticated RTP event completes probation.
            if !rtc.is_connected() || !self.probation.admit(ssrc, sequence) {
                return Ok(false);
            }
        }
        let Ok(receive) = Receive::new(Protocol::Udp, source, destination, bytes) else {
            return Ok(false);
        };
        if !rtc.accepts(&Input::Receive(now, receive)) {
            return Ok(false);
        }
        let receive = Receive::new(Protocol::Udp, source, destination, bytes)
            .map_err(|_| RealtimeError::Failed)?;
        let result = rtc.handle_input(Input::Receive(now, receive));
        self.drain(send)?;
        result.map_err(|_| {
            self.audio.0.lock().unwrap().fail();
            self.state = "failed";
            RealtimeError::Failed
        })?;
        Ok(true)
    }
    pub fn timeout(&mut self, now: Instant, send: &mut SendDatagram<'_>) -> Result<()> {
        self.advance(now)?;
        let result = self
            .rtc
            .as_mut()
            .ok_or(RealtimeError::Closed)?
            .handle_input(Input::Timeout(now));
        self.drain(send)?;
        result.map_err(|_| {
            self.audio.0.lock().unwrap().fail();
            self.state = "failed";
            RealtimeError::Failed
        })
    }
    /// Exactly one 20 ms frame. The runtime owner supplies its existing bounded
    /// catch-up clock and Windows priority/timer lease; this component never
    /// creates an independent interval or grants a call's PCM permit.
    pub fn send_frame(&mut self, now: Instant, send: &mut SendDatagram<'_>) -> Result<()> {
        self.advance(now)?;
        let audio = self.audio.clone();
        let mut core = audio.0.lock().unwrap();
        if self.pending_audio.is_some() {
            return Err(RealtimeError::NotReady);
        }
        let (state, mut encoded) = match core.encode() {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(()),
            Err(error) => {
                core.fail();
                self.state = "failed";
                return Err(error);
            }
        };
        let queued: Result<()> = (|| {
            let payload = core.payload().ok_or(RealtimeError::NotReady)?;
            let rtc = self.rtc.as_mut().ok_or(RealtimeError::Closed)?;
            let mut api = rtc.direct_api();
            let stream = api
                .stream_tx_by_mid(self.mid.unwrap(), None)
                .ok_or(RealtimeError::NotReady)?;
            stream.write_rtp(RtpWrite::new(
                payload.into(),
                self.sequence.into(),
                self.timestamp,
                now,
                encoded.as_slice(),
            ));
            Ok(())
        })();
        encoded.fill(0);
        if let Err(error) = queued {
            core.fail();
            self.state = "failed";
            return Err(error);
        }
        core.sent = core.sent.wrapping_add(1);
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(960);
        self.pending_audio = Some(state);
        self.drain_core(&mut core, send)?;
        // write_rtp queues an untimed packet. str0m activates its send queue
        // only when time is supplied through Input, even at the same instant.
        // Keep the admitted PCM state and lock through that activation/output.
        let result = self.rtc.as_mut().unwrap().handle_input(Input::Timeout(now));
        self.drain_core(&mut core, send)?;
        result.map_err(|_| {
            core.fail();
            self.state = "failed";
            RealtimeError::Failed
        })
    }
    fn drain(&mut self, send: &mut SendDatagram<'_>) -> Result<()> {
        let audio = self.audio.clone();
        let mut core = audio.0.lock().unwrap();
        self.drain_core(&mut core, send)
    }
    fn drain_core(&mut self, core: &mut Core, send: &mut SendDatagram<'_>) -> Result<()> {
        let mut failure = None;
        loop {
            let output = match self
                .rtc
                .as_mut()
                .ok_or(RealtimeError::Closed)?
                .poll_output()
            {
                Ok(output) => output,
                Err(_) => {
                    core.fail();
                    self.state = "failed";
                    self.rtc.as_mut().unwrap().disconnect();
                    return Err(RealtimeError::Failed);
                }
            };
            match output {
                Output::Timeout(timeout) => {
                    self.timeout = timeout;
                    break;
                }
                Output::Transmit(transmit) => {
                    if failure.is_some() {
                        continue;
                    }
                    if rtp_header(&transmit.contents).is_some() {
                        let Some(admitted) = self.pending_audio.take() else {
                            continue;
                        };
                        if core.state() != admitted {
                            core.clear(false);
                            continue;
                        }
                    }
                    if let Err(error) = send(transmit) {
                        core.fail();
                        self.state = "failed";
                        failure = Some(error);
                    }
                }
                Output::Event(event) => match event {
                    Event::Connected => {
                        core.connected = true;
                        if failure.is_none() {
                            self.state = "connected";
                        }
                    }
                    Event::IceConnectionStateChange(state) => match state {
                        IceConnectionState::Checking => self.state = "connecting",
                        IceConnectionState::Disconnected => {
                            core.connected = false;
                            self.state = "disconnected";
                        }
                        _ => {}
                    },
                    Event::RtpPacket(packet) => {
                        self.probation.ready = true;
                        if let Err(error) = core.receive(RtpAudioPacket {
                            source: *packet.header.ssrc,
                            sequence: packet.header.sequence_number,
                            timestamp: packet.header.timestamp,
                            payload_type: *packet.header.payload_type,
                            payload: packet.payload.to_vec(),
                            generation: 0,
                        }) {
                            core.fail();
                            self.state = "failed";
                            failure.get_or_insert(error);
                        }
                    }
                    Event::ChannelOpen(_, label) if label == "oai-events" => {
                        self.channel_open = true
                    }
                    Event::Closed => {
                        if !core.closed {
                            core.fail();
                            self.state = "closed";
                        }
                    }
                    _ => {}
                },
            }
        }
        if failure.is_some() && !core.closed {
            self.state = "failed";
        }
        failure.map_or(Ok(()), Err)
    }
    pub fn close(&mut self, send: &mut SendDatagram<'_>) -> Result<()> {
        if let Some(result) = self.closed_result {
            return result;
        }
        self.audio.0.lock().unwrap().close();
        self.state = "closed";
        self.pending_audio = None;
        let Some(rtc) = self.rtc.as_mut() else {
            return Ok(());
        };
        let result = if rtc.is_connected() {
            rtc.close().map_err(|_| RealtimeError::Failed)
        } else {
            // No established DTLS/SCTP session can send a graceful close.
            // Disconnecting and dropping the sans-I/O owner still releases it.
            rtc.disconnect();
            Ok(())
        };
        let drained = self.drain(send);
        self.rtc = None;
        self.state = "closed";
        let result = result.and(drained);
        self.closed_result = Some(result);
        result
    }
}
impl Drop for RealtimePeer {
    fn drop(&mut self) {
        self.audio
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .close();
    }
}
fn rtp_header(bytes: &[u8]) -> Option<(u32, u16)> {
    // RFC5764 demultiplexing keeps RTCP, STUN and DTLS out of RTP probation.
    if bytes.len() < 12 || bytes[0] >> 6 != 2 || (192..=223).contains(&bytes[1]) {
        return None;
    }
    Some((
        u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
        u16::from_be_bytes(bytes[2..4].try_into().unwrap()),
    ))
}
