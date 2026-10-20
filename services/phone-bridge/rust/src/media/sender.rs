//! The call's dedicated 20ms PCM sender, on its retained RTP/RTCP pair.
use super::receive_owner::Inner;
use super::{
    AudioFormat, CodecError, EvsLibrary, MediaCodec, PcmAudioPort, PcmAudioState, ReceiveError,
    RtpAudioFrames, RtpSendClock,
};
use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioSendHealth {
    Ready,
    RepairPending,
    Failed,
}

/// The route must hold the same authority lock used by suspend/revoke/rebind
/// throughout action. Validate both open state and generation, including closed
/// silence. Generations cannot be reused across route/owner replacements. A
/// failed route stays silent while its separately owned repair is pending.
/// The action runs exactly once on success and never on rejection. It is
/// synchronous, bounded UDP work; it must not reenter
/// route/call control. Revoke must never hold this lock while calling the media
/// owner. Separately authorized waiting/screening ports implement this same
/// boundary using their own original authority, never a fresh Voice grant.
pub trait PcmSendPort: PcmAudioPort {
    fn with_send_state(&self, admitted: PcmAudioState, action: &mut dyn FnMut()) -> bool;
    fn send_health(&self) -> AudioSendHealth;
}

pub(super) struct SendState {
    pub closed: bool,
    pub started: bool,
    pub completed: bool,
    pub revision: u64,
    pub audio: Option<Arc<dyn PcmSendPort>>,
    pub format: Option<AudioFormat>,
    pub codec: Option<MediaCodec>,
    pub frames: Option<RtpAudioFrames>,
    pub transmitted: Option<AudioFormat>,
    pub sent: u64,
    #[cfg(test)]
    pub hook: Option<SendHook>,
}
pub(super) struct Sender {
    pub state: Mutex<SendState>,
    wake: Condvar,
    changed: Notify,
}
impl Default for Sender {
    fn default() -> Self {
        Self {
            state: Mutex::new(SendState {
                closed: false,
                started: false,
                completed: false,
                revision: 0,
                audio: None,
                format: None,
                codec: None,
                frames: None,
                transmitted: None,
                sent: 0,
                #[cfg(test)]
                hook: None,
            }),
            wake: Condvar::new(),
            changed: Notify::new(),
        }
    }
}
impl Sender {
    pub fn request_close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.closed = true;
        state.codec = None;
        self.wake.notify_all();
    }
    pub async fn wait_closed(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                if !state.started || state.completed {
                    return;
                }
            }
            changed.await;
        }
    }
    // Match the original <=10ms interruptible waits and final short spin.
    // No asynchronous runtime, timer callback or control work runs this pump.
    fn wait(&self, origin: Instant, next: Duration) -> bool {
        loop {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.closed {
                return false;
            }
            let remaining = next.saturating_sub(origin.elapsed());
            if remaining.is_zero() {
                return true;
            }
            let milliseconds = remaining.as_millis().min(10) as u64;
            if milliseconds > 0 {
                drop(
                    self.wake
                        .wait_timeout(state, Duration::from_millis(milliseconds))
                        .unwrap_or_else(|p| p.into_inner()),
                );
            } else {
                drop(state);
                for _ in 0..32 {
                    std::hint::spin_loop();
                }
            }
        }
    }
}

pub(super) fn start(
    inner: Arc<Inner>,
    audio: Arc<dyn PcmSendPort>,
    source: u32,
    sequence: u16,
    timestamp: u32,
    evs: Option<Arc<EvsLibrary>>,
) -> Result<bool, ReceiveError> {
    {
        let mut state = inner.sender.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed {
            return Err(ReceiveError::Closed);
        }
        if state.started {
            return Ok(false);
        }
        state.codec = state
            .format
            .as_ref()
            .map(|format| MediaCodec::new(format.clone(), audio.clone(), evs))
            .transpose()?;
        state.audio = Some(audio.clone());
        state.frames = Some(RtpAudioFrames::new(source, sequence, timestamp));
        state.started = true;
    }
    let worker = inner.clone();
    let spawned = std::thread::Builder::new()
        .name("Ivy RTP audio".into())
        .spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pump(&worker, audio)));
            match result {
                Ok(Ok(())) => (),
                Ok(Err(error)) => worker.fail(error),
                Err(_) => worker.fail(ReceiveError::Codec(CodecError::Failed)),
            }
            let mut state = worker
                .sender
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            state.codec = None;
            state.completed = true;
            drop(state);
            worker.sender.changed.notify_waiters();
        });
    if spawned.is_err() {
        inner
            .sender
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .completed = true;
        inner.sender.changed.notify_waiters();
        inner.fail(ReceiveError::Codec(CodecError::Failed));
        return Err(ReceiveError::Codec(CodecError::Failed));
    }
    Ok(true)
}

fn pump(inner: &Arc<Inner>, audio: Arc<dyn PcmSendPort>) -> Result<(), ReceiveError> {
    let _timer = platform::enter().map_err(ReceiveError::Codec)?;
    let origin = Instant::now();
    let mut clock = RtpSendClock::new(Duration::ZERO)?;
    let mut audio_failed_at = None;
    while inner.sender.wait(origin, clock.next()) {
        let now = origin.elapsed();
        if !clock.due(now)? {
            continue;
        }
        #[cfg(test)]
        {
            let hook = inner
                .sender
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .hook
                .clone();
            if let Some(hook) = hook {
                hook(SendPoint::Due(clock.next()), audio.state(), &mut Vec::new());
            }
        }
        match audio.send_health() {
            AudioSendHealth::Ready => audio_failed_at = None,
            AudioSendHealth::Failed => return Err(ReceiveError::Codec(CodecError::Failed)),
            AudioSendHealth::RepairPending => {
                let failed_at = *audio_failed_at.get_or_insert(now);
                if now.saturating_sub(failed_at) >= Duration::from_secs(15) {
                    return Err(ReceiveError::Codec(CodecError::Failed));
                }
            }
        }
        let work = {
            let mut state = inner.sender.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.closed {
                return Ok(());
            }
            #[cfg(test)]
            let hook = state.hook.clone();
            #[cfg(test)]
            if let Some(codec) = state.codec.as_mut() {
                codec.encode_hook = hook.map(|hook| {
                    Arc::new(move |point, admitted| {
                        hook(SendPoint::Encode(point), admitted, &mut Vec::new())
                    }) as EncodeHook
                });
            }
            state.codec.take().map(|codec| (state.revision, codec))
        };
        if let Some((revision, mut codec)) = work {
            // No call/route lock covers capture or codec work. Close and format
            // replacement can proceed and invalidate this original admission.
            let (admitted, packet) = codec.encode_admitted()?;
            let mut packet = Packet(packet);
            #[cfg(test)]
            let hook = inner
                .sender
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .hook
                .clone();
            #[cfg(test)]
            if let Some(hook) = &hook {
                hook(SendPoint::BeforeDispatch, admitted, &mut packet.0);
            }
            let mut state = inner.sender.state.lock().unwrap_or_else(|p| p.into_inner());
            let mut result = Ok(());
            let mut published = false;
            if !state.closed && state.revision == revision {
                // Lock order: transmit -> route authority -> reports -> socket.
                // Keep every chunk and ALL bookkeeping inside both guards.
                published = audio.with_send_state(admitted, &mut || {
                    result = inner.send_frame(
                        state.frames.as_mut().unwrap(),
                        codec.format(),
                        &packet.0,
                        #[cfg(test)]
                        hook.as_ref(),
                        admitted,
                    );
                    if result.is_ok() {
                        if state.transmitted.as_ref() != Some(codec.format()) {
                            state.transmitted = Some(codec.format().clone());
                        }
                        state.sent = state.sent.wrapping_add(1);
                    }
                });
            }
            packet.0.fill(0);
            if !published {
                codec.reset();
            }
            if !state.closed && state.revision == revision {
                state.codec = Some(codec);
            }
            drop(state);
            result?;
        }
        // The approved idle correction advances this same clock even without
        // a format, with no capture, codec, packet or send-counter effects.
        clock.advance(origin.elapsed())?;
    }
    Ok(())
}

struct Packet(Vec<u8>);
impl Drop for Packet {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[cfg(windows)]
mod platform {
    use super::CodecError;
    #[link(name = "winmm")]
    unsafe extern "system" {
        fn timeBeginPeriod(period: u32) -> u32;
        fn timeEndPeriod(period: u32) -> u32;
    }
    pub struct Timer(bool);
    pub fn enter() -> Result<Timer, CodecError> {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
        };
        // SAFETY: changes only this owned audio thread; no foreign handle lives
        // beyond this call. Highest matches the original dedicated .NET thread.
        if unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) } == 0 {
            return Err(CodecError::Failed);
        }
        // The original lease treats an unavailable 1ms resolution as inactive.
        Ok(Timer(unsafe { timeBeginPeriod(1) } == 0))
    }
    impl Drop for Timer {
        fn drop(&mut self) {
            if self.0 {
                unsafe {
                    timeEndPeriod(1);
                }
            }
        }
    }
}
#[cfg(not(windows))]
mod platform {
    pub fn enter() -> Result<(), super::CodecError> {
        Ok(())
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EncodePoint {
    Admitted,
    Captured,
    Resampled,
    Encoded,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SendPoint {
    Due(Duration),
    Encode(EncodePoint),
    BeforeDispatch,
    Chunk,
}
#[cfg(test)]
pub(super) type EncodeHook = Arc<dyn Fn(EncodePoint, PcmAudioState) + Send + Sync>;
#[cfg(test)]
pub(super) type SendHook = Arc<dyn Fn(SendPoint, PcmAudioState, &mut Vec<u8>) + Send + Sync>;

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_sender_runtime_private.rs"]
mod tests;
