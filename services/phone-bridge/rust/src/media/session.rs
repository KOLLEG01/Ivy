//! One original SIP media session, its SDP, PCM ports and retained disposal.
use super::session_description::Descriptions;
use super::{
    AudioAttachmentStatus, AudioDesktopIdentity, AudioOperation, AudioSendHealth, AudioSettings,
    CallAudioError, CallAudioPort, CallWaitingAudio, Codec, CodecError, EvsLibrary, MediaCodec,
    NegotiationError, PcmAudioPort, PcmAudioState, PcmSendPort, ReceiveError, RtcpRuntime,
    ScreeningAudio, SdpDirection, SdpDocument, SdpIdentity, SdpType, SipReceiveEvents,
    SipReceiveObservation, SipRtpReceiver,
};
use crate::{
    dtmf::InputSource,
    sip::{DelayedInviteMedia, HostedSipMedia, SipHostError},
};
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SipMediaError {
    InvalidRequest,
    Closed,
    Audio(CallAudioError),
    Receive(ReceiveError),
    Negotiation(NegotiationError),
    CleanupUnconfirmed,
}
impl From<ReceiveError> for SipMediaError {
    fn from(e: ReceiveError) -> Self {
        Self::Receive(e)
    }
}
impl From<CallAudioError> for SipMediaError {
    fn from(e: CallAudioError) -> Self {
        Self::Audio(e)
    }
}
impl From<NegotiationError> for SipMediaError {
    fn from(e: NegotiationError) -> Self {
        Self::Negotiation(e)
    }
}
impl std::fmt::Display for SipMediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SipMediaError {}
type Result<T> = std::result::Result<T, SipMediaError>;

/// Identity comes from the original call admission, not renegotiation. The
/// native owner supplies its random source/sequence/timestamp/session ID once.
pub struct SipMediaIdentity {
    pub session_id: u32,
    pub source: u32,
    pub sequence: u16,
    pub timestamp: u32,
    pub cname: String,
}
pub struct SipMediaSettings {
    pub call_id: String,
    pub interface: SocketAddr,
    /// Original call owner's resolved SDP connection address.
    pub connection_address: IpAddr,
    pub first_port: u16,
    pub last_port: u16,
    pub reorder_ms: u32,
    pub preferences: Vec<Codec>,
    pub identity: SipMediaIdentity,
}
#[derive(Clone)]
pub struct SipMediaObservation {
    pub call_id: String,
    pub closed: bool,
    pub failed: bool,
    pub started: bool,
    pub cleanup_confirmed: bool,
    pub local_direction: SdpDirection,
    pub remote_direction: Option<SdpDirection>,
    pub remote_rtp: Option<SocketAddr>,
    pub remote_rtcp: Option<SocketAddr>,
    pub sdp_version: u64,
    pub audio: AudioAttachmentStatus,
    pub receive: SipReceiveObservation,
    pub send_format: Option<super::AudioFormat>,
    pub transmitted_format: Option<super::AudioFormat>,
    pub sent_frames: u64,
}
struct Completion {
    result: OnceLock<Result<()>>,
    changed: Notify,
}
impl Completion {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: OnceLock::new(),
            changed: Notify::new(),
        })
    }
    fn finish(&self, result: Result<()>) {
        let _ = self.result.set(result);
        self.changed.notify_waiters();
    }
    async fn wait(&self) -> Result<()> {
        loop {
            let n = self.changed.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if let Some(result) = self.result.get() {
                return *result;
            }
            n.await;
        }
    }
}
struct Selected {
    audio: Arc<dyn PcmSendPort>,
    seen: Option<PcmAudioState>,
    generation: u64,
}
struct SendAudio(Mutex<Selected>);
impl SendAudio {
    fn new(audio: Arc<dyn PcmSendPort>) -> Self {
        Self(Mutex::new(Selected {
            audio,
            seen: None,
            generation: 0,
        }))
    }
    fn snapshot(s: &mut Selected) -> (PcmAudioState, PcmAudioState) {
        let raw = s.audio.state();
        if s.seen.is_some_and(|old| old != raw) {
            s.generation = s
                .generation
                .checked_add(1)
                .expect("session audio generation exhausted");
        }
        s.seen = Some(raw);
        (
            PcmAudioState {
                open: raw.open,
                generation: s.generation,
            },
            raw,
        )
    }
    fn select(&self, audio: Arc<dyn PcmSendPort>) {
        let mut s = self.0.lock().unwrap_or_else(|p| p.into_inner());
        s.audio = audio;
        s.seen = None;
        s.generation = s
            .generation
            .checked_add(1)
            .expect("session audio generation exhausted");
    }
}
impl PcmAudioPort for SendAudio {
    fn state(&self) -> PcmAudioState {
        Self::snapshot(&mut self.0.lock().unwrap_or_else(|p| p.into_inner())).0
    }
    fn read_captured(&self, out: &mut [f32], generation: u64) -> usize {
        out.fill(0.0);
        let mut s = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let (admitted, raw) = Self::snapshot(&mut s);
        if !admitted.open || admitted.generation != generation {
            return 0;
        }
        let count = s.audio.read_captured(out, raw.generation);
        if Self::snapshot(&mut s).0 != admitted {
            out.fill(0.0);
            0
        } else {
            count
        }
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        let mut s = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let (admitted, raw) = Self::snapshot(&mut s);
        admitted.open
            && admitted.generation == generation
            && s.audio.write_received(input, raw.generation)
    }
}
impl PcmSendPort for SendAudio {
    fn with_send_state(&self, admitted: PcmAudioState, action: &mut dyn FnMut()) -> bool {
        let mut s = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let (current, raw) = Self::snapshot(&mut s);
        current == admitted && s.audio.with_send_state(raw, action)
    }
    fn send_health(&self) -> AudioSendHealth {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .audio
            .send_health()
    }
}
struct State {
    descriptions: Descriptions,
    started: bool,
    screening: Option<Arc<ScreeningAudio>>,
    disposal: Option<Arc<Completion>>,
}
struct Core {
    settings: SipMediaSettings,
    audio: CallAudioPort,
    waiting: Arc<CallWaitingAudio>,
    send: Arc<SendAudio>,
    receiver: OnceLock<SipRtpReceiver>,
    events: Arc<dyn SipReceiveEvents>,
    state: Mutex<State>,
    closed: AtomicBool,
    failed: AtomicBool,
}
impl Core {
    fn receiver(&self) -> &SipRtpReceiver {
        self.receiver.get().expect("published original receiver")
    }
    fn request_close(&self) {
        self.closed.store(true, Ordering::Release);
        self.audio.revoke();
        if let Some(receiver) = self.receiver.get() {
            receiver.request_close();
        }
    }
    fn fail(&self) {
        if !self.failed.swap(true, Ordering::AcqRel) {
            self.request_close();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.events.failed()));
        }
    }
    fn identity(&self, version: u64) -> Result<SdpIdentity> {
        let mut local = self.receiver().observation().transport.local_rtp;
        local.set_ip(self.settings.connection_address);
        SdpIdentity::new(
            local,
            self.settings.identity.session_id,
            version,
            self.settings.identity.source,
            &self.settings.identity.cname,
        )
        .map_err(|_| SipMediaError::InvalidRequest)
    }
    fn close(self: &Arc<Self>) -> Arc<Completion> {
        self.request_close();
        let completion = Completion::new();
        {
            let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(old) = &s.disposal {
                return old.clone();
            }
            s.disposal = Some(completion.clone());
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            completion.finish(Err(SipMediaError::CleanupUnconfirmed));
            return completion;
        };
        let core = self.clone();
        let original = completion.clone();
        runtime.spawn(async move {
            let work = core.clone();
            let result = tokio::spawn(async move {
                work.receiver().close().await;
                if let Some(screen) = work
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .screening
                    .take()
                {
                    screen.bridge();
                }
                work.audio
                    .close()
                    .wait()
                    .await
                    .map_err(SipMediaError::Audio)
            })
            .await
            .unwrap_or(Err(SipMediaError::CleanupUnconfirmed));
            if result.is_err() {
                core.failed.store(true, Ordering::Release);
            }
            original.finish(result);
        });
        completion
    }
}
struct Events(Weak<Core>);
impl SipReceiveEvents for Events {
    fn digit(&self, digit: char, source: InputSource) {
        if let Some(core) = self.0.upgrade() {
            if !core.closed.load(Ordering::Acquire) {
                core.events.digit(digit, source);
            }
        }
    }
    fn failed(&self) {
        if let Some(core) = self.0.upgrade() {
            core.fail();
        }
    }
    fn rtcp(&self, source: SocketAddr, bytes: &[u8]) {
        if let Some(core) = self.0.upgrade() {
            core.events.rtcp(source, bytes);
        }
    }
    fn report(&self, source: SocketAddr, value: &super::RtcpReceiveObservation) {
        if let Some(core) = self.0.upgrade() {
            core.events.report(source, value);
        }
    }
    fn report_sent(&self, bytes: &[u8]) {
        if let Some(core) = self.0.upgrade() {
            core.events.report_sent(bytes);
        }
    }
    fn rtcp_timeout(&self) {
        if let Some(core) = self.0.upgrade() {
            core.events.rtcp_timeout();
        }
    }
}
struct Handle(Arc<Core>);
impl Drop for Handle {
    fn drop(&mut self) {
        self.0.close();
    }
}
#[derive(Clone)]
pub struct SipMediaSession {
    handle: Arc<Handle>,
}
impl SipMediaSession {
    pub async fn bind(
        settings: SipMediaSettings,
        audio: CallAudioPort,
        evs: Option<Arc<EvsLibrary>>,
        runtime: Arc<dyn RtcpRuntime>,
        events: Arc<dyn SipReceiveEvents>,
    ) -> Result<Self> {
        let prepared = (|| {
            if audio.call_id() != settings.call_id {
                return Err(SipMediaError::InvalidRequest);
            }
            let descriptions = Descriptions::new(&settings.preferences)
                .map_err(|_| SipMediaError::InvalidRequest)?;
            let waiting = Arc::new(
                CallWaitingAudio::new(audio.clone()).map_err(|_| SipMediaError::InvalidRequest)?,
            );
            // Match the original constructor's packaged EVS availability probe.
            if settings.preferences.contains(&Codec::Evs) {
                for format in Codec::Evs.offered_formats() {
                    let _ = MediaCodec::new(format, Arc::new(audio.clone()), evs.clone())
                        .map_err(ReceiveError::Codec)?;
                }
            }
            Ok((descriptions, waiting))
        })();
        let (descriptions, waiting) = match prepared {
            Ok(value) => value,
            Err(error) => {
                audio.close().wait().await?;
                return Err(error);
            }
        };
        let core = Arc::new(Core {
            settings,
            audio,
            send: Arc::new(SendAudio::new(waiting.clone())),
            waiting,
            receiver: OnceLock::new(),
            events,
            state: Mutex::new(State {
                descriptions,
                started: false,
                screening: None,
                disposal: None,
            }),
            closed: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        });
        let receiver = SipRtpReceiver::bind(
            core.settings.interface,
            core.settings.first_port,
            core.settings.last_port,
            core.settings.reorder_ms,
            core.waiting.clone(),
            evs,
            Arc::new(Events(Arc::downgrade(&core))),
        );
        let receiver = match receiver {
            Ok(receiver) => receiver,
            Err(error) => {
                core.audio.close().wait().await?;
                return Err(error.into());
            }
        };
        let _ = core.receiver.set(receiver);
        let identity = &core.settings.identity;
        if core.identity(0).is_err()
            || core
                .receiver()
                .enable_reporting(identity.source, &identity.cname, runtime)
                .is_err()
        {
            core.close().wait().await?;
            return Err(SipMediaError::InvalidRequest);
        }
        if core.closed.load(Ordering::Acquire) {
            core.close().wait().await?;
            return Err(SipMediaError::Closed);
        }
        Ok(Self {
            handle: Arc::new(Handle(core)),
        })
    }
    pub fn call_audio(&self) -> &CallAudioPort {
        &self.handle.0.audio
    }
    pub fn create_offer(&self) -> Option<SdpDocument> {
        let c = &self.handle.0;
        let mut s = c.state.lock().unwrap_or_else(|p| p.into_inner());
        if c.closed.load(Ordering::Acquire) {
            return None;
        }
        let identity = c.identity(s.descriptions.version).ok()?;
        let offer = s.descriptions.offer(&identity);
        c.receiver()
            .description_effects(
                s.descriptions.destinations,
                s.descriptions.can_send(),
                false,
            )
            .ok()?;
        Some(offer)
    }
    pub fn create_answer(&self) -> Option<SdpDocument> {
        let c = &self.handle.0;
        let mut s = c.state.lock().unwrap_or_else(|p| p.into_inner());
        let identity = c.identity(s.descriptions.version).ok()?;
        s.descriptions.answer(
            &identity,
            c.receiver().observation().transport.local_rtp.port(),
        )
    }
    pub fn set_remote_description(&self, kind: SdpType, description: &SdpDocument) -> Result<()> {
        let c = &self.handle.0;
        let (result, report) = {
            let mut s = c.state.lock().unwrap_or_else(|p| p.into_inner());
            if c.closed.load(Ordering::Acquire) {
                return Err(SipMediaError::Closed);
            }
            let result = s.descriptions.apply(kind, description);
            let report = c.receiver().description_effects(
                s.descriptions.destinations,
                s.descriptions.can_send(),
                s.descriptions.close_control,
            )?;
            let result = match result {
                Ok(()) => c
                    .receiver()
                    .configure_description(
                        s.descriptions.negotiation.state().unwrap(),
                        s.descriptions.destinations,
                    )
                    .map_err(Into::into),
                Err(error) => Err(error.into()),
            };
            (result, report)
        };
        if let Some(bytes) = report {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                c.events.report_sent(&bytes)
            }))
            .is_err()
            {
                c.fail();
                return Err(SipMediaError::Receive(ReceiveError::Codec(
                    CodecError::Failed,
                )));
            }
        }
        if matches!(result, Err(SipMediaError::Receive(_))) {
            c.fail();
        }
        result
    }
    pub fn set_stream_status(&self, status: SdpDirection) -> Result<()> {
        let c = &self.handle.0;
        let mut s = c.state.lock().unwrap_or_else(|p| p.into_inner());
        if c.closed.load(Ordering::Acquire) {
            return Err(SipMediaError::Closed);
        }
        s.descriptions.set_status(status);
        c.receiver().description_effects(
            s.descriptions.destinations,
            s.descriptions.can_send(),
            false,
        )?;
        Ok(())
    }
    pub fn challenge_codecs(&self, enabled: bool) {
        self.handle
            .0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .descriptions
            .negotiation
            .challenge_codecs(enabled);
    }
    pub fn prepare_audio(
        &self,
        settings: AudioSettings,
        desktop: Option<AudioDesktopIdentity>,
        cancel: CancellationToken,
    ) -> Result<AudioOperation> {
        let c = &self.handle.0;
        let _s = c.state.lock().unwrap_or_else(|p| p.into_inner());
        if c.closed.load(Ordering::Acquire) || c.failed.load(Ordering::Acquire) {
            return Err(SipMediaError::Closed);
        }
        Ok(c.audio.prepare(settings, desktop, cancel)?)
    }
    pub fn begin_waiting(&self) -> Result<()> {
        Ok(self.handle.0.waiting.begin()?)
    }
    pub fn end_waiting(&self) -> Result<()> {
        Ok(self.handle.0.waiting.end()?)
    }
    pub fn command_feedback(&self, success: bool) -> Result<()> {
        Ok(self.handle.0.waiting.acknowledge(success)?)
    }
    pub fn prepare_screening(
        &self,
        path: &Path,
        hash: &str,
        timeout: u16,
        repeats: u8,
        waiting: bool,
    ) -> Result<()> {
        let c = &self.handle.0;
        let mut s = c.state.lock().unwrap_or_else(|p| p.into_inner());
        if c.closed.load(Ordering::Acquire) || s.screening.is_some() {
            return Err(SipMediaError::Closed);
        }
        let screen = Arc::new(
            ScreeningAudio::new(c.audio.clone(), path, hash, timeout, repeats, waiting)
                .map_err(|_| SipMediaError::InvalidRequest)?,
        );
        c.receiver()
            .change_send_audio(|| c.send.select(screen.clone()))?;
        s.screening = Some(screen);
        Ok(())
    }
    pub fn set_screening_waiting(&self, waiting: bool) {
        if let Some(screen) = &self
            .handle
            .0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .screening
        {
            screen.set_waiting(waiting);
        }
    }
    pub fn bridge_screening(&self) {
        let c = &self.handle.0;
        let s = c.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(screen) = &s.screening {
            screen.bridge();
        }
    }
    pub fn set_inband_needed(&self, needed: bool) {
        self.handle.0.receiver().set_inband_needed(needed);
    }
    pub fn observation(&self) -> SipMediaObservation {
        let c = &self.handle.0;
        let s = c.state.lock().unwrap_or_else(|p| p.into_inner());
        let (transmitted_format, sent_frames) = c.receiver().transmission();
        SipMediaObservation {
            call_id: c.settings.call_id.clone(),
            closed: c.closed.load(Ordering::Acquire),
            failed: c.failed.load(Ordering::Acquire),
            started: s.started,
            cleanup_confirmed: s
                .disposal
                .as_ref()
                .is_some_and(|d| d.result.get().is_some_and(std::result::Result::is_ok)),
            local_direction: s.descriptions.local_direction,
            remote_direction: s.descriptions.remote_direction,
            remote_rtp: s.descriptions.destinations.rtp,
            remote_rtcp: s.descriptions.destinations.rtcp,
            sdp_version: s.descriptions.version,
            audio: c.audio.status(),
            receive: c.receiver().observation(),
            send_format: c.receiver().selected_send_format(),
            transmitted_format,
            sent_frames,
        }
    }
    pub fn request_close(&self) {
        self.handle.0.request_close();
    }
    pub async fn close(&self) -> Result<()> {
        self.handle.0.close().wait().await
    }
}
impl DelayedInviteMedia for SipMediaSession {
    fn is_closed(&self) -> bool {
        self.handle.0.closed.load(Ordering::Acquire)
    }
    fn failed(&self) -> bool {
        self.handle.0.failed.load(Ordering::Acquire)
    }
    fn negotiate_offer(&self, offer: &str) -> Option<String> {
        let description = SdpDocument::parse(offer).ok()?;
        self.set_remote_description(SdpType::Offer, &description)
            .ok()?;
        self.create_answer().map(|d| d.to_sdp())
    }
    fn start(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async move {
            let c = &self.handle.0;
            let mut s = c.state.lock().unwrap_or_else(|p| p.into_inner());
            if c.closed.load(Ordering::Acquire) {
                return false;
            }
            if s.started {
                return true;
            }
            let i = &c.settings.identity;
            let started = (|| {
                if s.descriptions.local_direction != SdpDirection::Inactive {
                    c.receiver().start_reporting()?;
                }
                c.receiver()
                    .start_sending(c.send.clone(), i.source, i.sequence, i.timestamp)?;
                Ok::<_, ReceiveError>(())
            })();
            if started.is_ok() {
                s.started = true;
                true
            } else {
                drop(s);
                c.fail();
                false
            }
        })
    }
}
impl HostedSipMedia for SipMediaSession {
    fn request_close(&self) {
        self.request_close();
    }
    fn close(
        &self,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), SipHostError>> + Send + '_>> {
        Box::pin(async move {
            self.close()
                .await
                .map_err(|_| SipHostError::CleanupUnconfirmed)
        })
    }
}
