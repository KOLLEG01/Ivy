use super::{RealtimeError, Result};
use crate::media::{
    AudioFormat, AudioQueue, AudioSendHealth, MediaCodec, PcmAudioPort, PcmAudioState, PcmSendPort,
    RtpAudioPacket, RtpReceiveQueue, RtpReceiveStatus,
};
use ivy_contracts::IvyJson;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

// MediaCodec must not reenter Core's non-reentrant mutex. This bounded working
// buffer exists only during a codec invocation under Core's authority lock.
struct Frame {
    state: PcmAudioState,
    captured: [f32; 960],
    received: Vec<Vec<f32>>,
}
struct CodecPort(Mutex<Frame>);
impl CodecPort {
    fn new() -> Self {
        Self(Mutex::new(Frame {
            state: PcmAudioState {
                open: false,
                generation: 0,
            },
            captured: [0.0; 960],
            received: Vec::with_capacity(8),
        }))
    }
    fn begin(&self, state: PcmAudioState) {
        let mut frame = self.0.lock().unwrap();
        frame.state = state;
        frame.captured.fill(0.0);
        for chunk in &mut frame.received {
            chunk.fill(0.0);
        }
        frame.received.clear();
    }
}
impl PcmAudioPort for CodecPort {
    fn state(&self) -> PcmAudioState {
        self.0.lock().unwrap().state
    }
    fn read_captured(&self, output: &mut [f32], generation: u64) -> usize {
        let frame = self.0.lock().unwrap();
        output.fill(0.0);
        if !frame.state.open || frame.state.generation != generation {
            return 0;
        }
        let count = output.len().min(frame.captured.len());
        output[..count].copy_from_slice(&frame.captured[..count]);
        count
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        let mut frame = self.0.lock().unwrap();
        if !frame.state.open || frame.state.generation != generation {
            return false;
        }
        // Opus recovery is bounded to 120 ms plus the admitted packet. The
        // existing decoder emits at most six PLC blocks, FEC and one packet.
        if frame.received.len() == 8 || input.len() > 5760 {
            return false;
        }
        frame.received.push(input.to_vec());
        true
    }
}
impl Drop for CodecPort {
    fn drop(&mut self) {
        let frame = self.0.get_mut().unwrap_or_else(|p| p.into_inner());
        frame.captured.fill(0.0);
        for chunk in &mut frame.received {
            chunk.fill(0.0);
        }
    }
}

pub(super) struct Core {
    pub queue_ms: u16,
    pub prebuffer_ms: u16,
    permitted: Arc<dyn Fn() -> bool + Send + Sync>,
    received: AudioQueue,
    outgoing: AudioQueue,
    packets: RtpReceiveQueue,
    port: Arc<CodecPort>,
    encoder: Option<MediaCodec>,
    decoder: Option<MediaCodec>,
    format: Option<AudioFormat>,
    pub connected: bool,
    pub failed: bool,
    pub closed: bool,
    resumed: bool,
    had_authority: bool,
    generation: u64,
    pub sent: u64,
    pub received_packets: u64,
    pub now: Duration,
}
impl Core {
    pub fn new(
        queue_ms: u16,
        prebuffer: Option<u16>,
        permitted: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Self> {
        let prebuffer_ms = prebuffer.unwrap_or(60.min(queue_ms));
        if !(20..=200).contains(&queue_ms) || prebuffer_ms > queue_ms {
            return Err(RealtimeError::InvalidRequest);
        }
        let queue = || {
            AudioQueue::new(
                48 * queue_ms as usize,
                48 * prebuffer_ms as usize,
                true,
                true,
            )
            .map_err(|_| RealtimeError::InvalidRequest)
        };
        Ok(Self {
            queue_ms,
            prebuffer_ms,
            permitted,
            received: queue()?,
            outgoing: queue()?,
            packets: RtpReceiveQueue::new(20).map_err(|_| RealtimeError::InvalidRequest)?,
            port: Arc::new(CodecPort::new()),
            encoder: None,
            decoder: None,
            format: None,
            connected: false,
            failed: false,
            closed: false,
            resumed: false,
            had_authority: false,
            generation: 0,
            sent: 0,
            received_packets: 0,
            now: Duration::ZERO,
        })
    }
    pub fn open(&mut self) -> bool {
        let open =
            !self.closed && !self.failed && self.resumed && (self.permitted)() && self.connected;
        if open {
            self.had_authority = true;
        }
        open
    }
    pub fn state(&mut self) -> PcmAudioState {
        PcmAudioState {
            open: self.open(),
            generation: self.generation,
        }
    }
    pub fn clear(&mut self, force: bool) {
        let dirty = self.had_authority
            || self.received.count() != 0
            || self.outgoing.count() != 0
            || self.packets.status().queued_packets != 0;
        self.received.clear();
        self.outgoing.clear();
        self.packets.clear();
        self.had_authority = false;
        if dirty || force {
            self.generation = self.generation.wrapping_add(1);
        }
        self.port.begin(PcmAudioState {
            open: false,
            generation: self.generation,
        });
    }
    pub fn negotiate(&mut self, payload: u8) -> Result<()> {
        let format = AudioFormat::new(
            payload as i32,
            "opus",
            48000,
            48000,
            2,
            Some("useinbandfec=1"),
        )
        .map_err(|_| RealtimeError::Failed)?;
        self.encoder = Some(
            MediaCodec::new(format.clone(), self.port.clone(), None)
                .map_err(|_| RealtimeError::Failed)?,
        );
        self.decoder = Some(
            MediaCodec::new(format.clone(), self.port.clone(), None)
                .map_err(|_| RealtimeError::Failed)?,
        );
        self.format = Some(format);
        Ok(())
    }
    pub fn payload(&self) -> Option<u8> {
        self.format.as_ref().map(|f| f.payload())
    }
    pub fn resume(&mut self) -> Result<()> {
        if self.closed || self.failed {
            return Err(RealtimeError::Failed);
        }
        self.resumed = true;
        Ok(())
    }
    pub fn suspend(&mut self) {
        self.resumed = false;
        self.clear(true);
    }
    pub fn fail(&mut self) {
        if !self.closed {
            self.failed = true;
            self.clear(false);
        }
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.resumed = false;
        self.clear(false);
        self.encoder = None;
        self.decoder = None;
    }
    pub fn receive(&mut self, mut packet: RtpAudioPacket) -> Result<()> {
        if self.closed || self.failed {
            packet.payload.fill(0);
            return Ok(());
        }
        let result = if !self.open() {
            self.packets
                .skip(packet.source, packet.sequence, self.now)
                .map(|()| ())
                .map_err(|_| RealtimeError::Failed)
        } else if self.payload() != Some(packet.payload_type) {
            Ok(())
        } else {
            packet.generation = self.generation;
            self.packets
                .add(&packet, self.now)
                .map_err(|_| RealtimeError::Failed)
                .and_then(|_| self.drain(0))
        };
        packet.payload.fill(0);
        result
    }
    fn drain(&mut self, playout: usize) -> Result<()> {
        while let Some((mut packet, _)) = self
            .packets
            .take(self.now, self.received.count() < playout)
            .map_err(|_| RealtimeError::Failed)?
        {
            let state = self.state();
            self.port.begin(state);
            let result = self
                .decoder
                .as_mut()
                .ok_or(RealtimeError::NotReady)
                .and_then(|decoder| {
                    decoder
                        .decode_rtp(&packet)
                        .map_err(|_| RealtimeError::Failed)
                });
            packet.payload.fill(0);
            let admitted = self.state() == state && state.open;
            let mut frame = self.port.0.lock().unwrap();
            for chunk in &mut frame.received {
                if admitted {
                    self.received.write(chunk);
                }
                chunk.fill(0.0);
            }
            frame.received.clear();
            drop(frame);
            if !admitted {
                self.clear(false);
                return Ok(());
            }
            result?;
            self.received_packets = self.received_packets.wrapping_add(1);
        }
        Ok(())
    }
    pub fn encode(&mut self) -> Result<Option<(PcmAudioState, Vec<u8>)>> {
        if !self.open() || self.encoder.is_none() {
            self.clear(false);
            return Ok(None);
        }
        self.drain(0)?;
        let state = self.state();
        if !state.open {
            self.clear(false);
            return Ok(None);
        }
        self.port.begin(state);
        {
            let mut frame = self.port.0.lock().unwrap();
            self.outgoing.read(&mut frame.captured);
        }
        let result = self.encoder.as_mut().unwrap().encode();
        self.port.0.lock().unwrap().captured.fill(0.0);
        let mut packet = result.map_err(|_| RealtimeError::Failed)?;
        if self.state() != state {
            packet.fill(0);
            self.clear(false);
            return Ok(None);
        }
        Ok(Some((state, packet)))
    }
    pub fn status(&mut self) -> IvyJson {
        let state = if self.closed {
            "closed"
        } else if self.failed {
            "failed"
        } else if self.open() {
            "open"
        } else {
            "suspended"
        };
        IvyJson::object([
            ("state", state.into()),
            ("sourceMode", "webrtc".into()),
            ("sampleRate", IvyJson::Number(48000.0)),
            ("channels", IvyJson::Number(1.0)),
            (
                "captureDroppedSamples",
                IvyJson::Number(self.received.dropped() as i64 as f64),
            ),
            (
                "renderDroppedSamples",
                IvyJson::Number(self.outgoing.dropped() as i64 as f64),
            ),
            (
                "captureQueuedSamples",
                IvyJson::Number(self.received.count() as f64),
            ),
            (
                "renderQueuedSamples",
                IvyJson::Number(self.outgoing.count() as f64),
            ),
            ("renderLatencyMs", IvyJson::Null),
            ("lowLatencyActive", IvyJson::Null),
            ("lowLatencyUnavailableReason", IvyJson::Null),
        ])
    }
    pub fn receive_status(&self) -> RtpReceiveStatus {
        self.packets.status()
    }
    pub fn concealed(&self) -> u64 {
        self.decoder
            .as_ref()
            .map_or(0, MediaCodec::concealed_samples)
    }
    pub fn underruns(&self) -> (u64, u64) {
        (self.received.underruns(), self.outgoing.underruns())
    }
}

#[derive(Clone)]
pub(crate) struct RealtimeAudio(pub(super) Arc<Mutex<Core>>);
impl RealtimeAudio {
    pub fn resume(&self) -> Result<()> {
        self.0.lock().unwrap().resume()
    }
    pub fn suspend(&self) {
        self.0.lock().unwrap().suspend();
    }
    pub fn status(&self) -> IvyJson {
        self.0.lock().unwrap().status()
    }
}
impl PcmAudioPort for RealtimeAudio {
    fn state(&self) -> PcmAudioState {
        self.0.lock().unwrap().state()
    }
    fn read_captured(&self, output: &mut [f32], generation: u64) -> usize {
        let mut core = self.0.lock().unwrap();
        if !core.open() {
            core.clear(false);
            output.fill(0.0);
            return 0;
        }
        if core.generation != generation {
            output.fill(0.0);
            return 0;
        }
        if core.drain(output.len()).is_err() {
            core.fail();
            output.fill(0.0);
            return 0;
        }
        if !core.open() || core.generation != generation {
            core.clear(false);
            output.fill(0.0);
            return 0;
        }
        core.received.read(output)
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        let mut core = self.0.lock().unwrap();
        if !core.open() {
            core.clear(false);
            return false;
        }
        if core.generation != generation {
            return false;
        }
        core.outgoing.write(input);
        true
    }
}
impl PcmSendPort for RealtimeAudio {
    fn with_send_state(&self, admitted: PcmAudioState, action: &mut dyn FnMut()) -> bool {
        let mut core = self.0.lock().unwrap();
        if core.state() != admitted {
            return false;
        }
        action();
        true
    }
    fn send_health(&self) -> AudioSendHealth {
        if self.0.lock().unwrap().failed {
            AudioSendHealth::Failed
        } else {
            AudioSendHealth::Ready
        }
    }
}
