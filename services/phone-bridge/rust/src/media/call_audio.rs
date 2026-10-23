//! One original call's audio preparation, authority and retained cleanup.
use super::{AudioPermit, AudioSendHealth, MediaError, PcmAudioPort, PcmAudioState, PcmSendPort};
use ivy_contracts::IvyJson;
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
mod realtime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallAudioError {
    InvalidRequest,
    NotReady,
    OwnerChanged,
    AudioNotReady,
    Cancelled,
    PreparationReleased,
    CleanupUnconfirmed,
    RouteFailed,
    Permit(MediaError),
}
impl From<MediaError> for CallAudioError {
    fn from(value: MediaError) -> Self {
        Self::Permit(value)
    }
}
impl std::fmt::Display for CallAudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CallAudioError {}
type Result<T> = std::result::Result<T, CallAudioError>;

#[derive(Clone, PartialEq, Eq)]
pub struct AudioMutePolicy {
    pub muted_render_endpoint: Option<String>,
    pub unmute_receive: bool,
    pub interval_seconds: u16,
}
impl AudioMutePolicy {
    pub fn validate(&self, receive: &str) -> Result<()> {
        if !(1..=3600).contains(&self.interval_seconds)
            || self
                .muted_render_endpoint
                .as_ref()
                .is_some_and(|id| !endpoint(id) || id == receive)
        {
            return Err(CallAudioError::InvalidRequest);
        }
        Ok(())
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct AudioSettings {
    pub capture_endpoint: String,
    pub render_endpoint: String,
    pub source_mode: String,
    pub capture_buffer_ms: u16,
    pub render_latency_ms: u16,
    pub queue_ms: u16,
    pub mute: Option<AudioMutePolicy>,
    pub playback_prebuffer_ms: u16,
    pub enable_wasapi_low_latency: bool,
    pub loopback_render_endpoint: Option<String>,
}
fn endpoint(value: &str) -> bool {
    !value.trim().is_empty() && value.encode_utf16().count() <= 32767 && !value.contains('\0')
}
impl AudioSettings {
    pub fn validate(&self) -> Result<()> {
        if !endpoint(&self.capture_endpoint)
            || !endpoint(&self.render_endpoint)
            || self.capture_endpoint == self.render_endpoint
            || !matches!(
                self.source_mode.as_str(),
                "system_excluding_runtime" | "desktop_process" | "microphone" | "virtual_speaker"
            )
            || !(10..=100).contains(&self.capture_buffer_ms)
            || !(2..=100).contains(&self.render_latency_ms)
            || !(20..=200).contains(&self.queue_ms)
            || self.queue_ms < self.capture_buffer_ms
            || self.playback_prebuffer_ms > self.queue_ms
            || self.source_mode == "virtual_speaker"
                && self
                    .loopback_render_endpoint
                    .as_ref()
                    .is_none_or(|id| !endpoint(id) || *id == self.render_endpoint)
            || self
                .mute
                .as_ref()
                .is_some_and(|m| m.validate(&self.render_endpoint).is_err())
        {
            return Err(CallAudioError::InvalidRequest);
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct AudioDesktopIdentity {
    pid: i32,
    creation_ticks: i64,
    image: PathBuf,
    application: String,
}
impl AudioDesktopIdentity {
    pub fn new(pid: i32, creation_ticks: i64, image: &Path, application: &str) -> Result<Self> {
        if pid <= 0
            || creation_ticks <= 0
            || crate::desktop::validate_application(application).is_err()
        {
            return Err(CallAudioError::InvalidRequest);
        }
        Ok(Self {
            pid,
            creation_ticks,
            image: desktop_image(image)?,
            application: application.into(),
        })
    }
    pub fn pid(&self) -> i32 {
        self.pid
    }
    pub fn creation_ticks(&self) -> i64 {
        self.creation_ticks
    }
    pub fn image(&self) -> &Path {
        &self.image
    }
    pub fn application(&self) -> &str {
        &self.application
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.creation_ticks == other.creation_ticks
            && self.application == other.application
            && image_equal(&self.image, &other.image)
    }
}
pub(crate) fn desktop_image(image: &Path) -> Result<PathBuf> {
    // Match DesktopIdentity/ProcessOwner: Windows rooted paths include drive-
    // relative and root-relative forms, resolved once by GetFullPath semantics.
    #[cfg(windows)]
    let rooted = image.has_root()
        || matches!(
            image.components().next(),
            Some(std::path::Component::Prefix(_))
        );
    #[cfg(not(windows))]
    let rooted = image.is_absolute();
    if !rooted {
        return Err(CallAudioError::InvalidRequest);
    }
    std::path::absolute(image).map_err(|_| CallAudioError::InvalidRequest)
}
#[cfg(windows)]
pub(crate) fn image_equal(a: &Path, b: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CompareStringOrdinal(
            a: *const u16,
            an: i32,
            b: *const u16,
            bn: i32,
            ignore_case: i32,
        ) -> i32;
    }
    let a: Vec<_> = a.as_os_str().encode_wide().collect();
    let b: Vec<_> = b.as_os_str().encode_wide().collect();
    // SAFETY: both immutable UTF16 buffers and their explicit lengths live for this call.
    unsafe { CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) == 2 }
}
#[cfg(not(windows))]
pub(crate) fn image_equal(a: &Path, b: &Path) -> bool {
    a == b
        || a.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

struct RoutePermit {
    origin: Instant,
    deadline: AtomicU64,
    revoked: AtomicBool,
}
/// Route callbacks read this without reentering the call/route lock. Device
/// preparation never grants it. Observation permits retain the same500ms bound.
#[derive(Clone)]
pub struct AudioRoutePermit(Arc<RoutePermit>);
impl AudioRoutePermit {
    pub fn permitted(&self) -> bool {
        let deadline = self.0.deadline.load(Ordering::Acquire);
        !self.0.revoked.load(Ordering::Acquire)
            && deadline != 0
            && (deadline == u64::MAX || self.0.origin.elapsed().as_nanos() < u128::from(deadline))
    }
}
/// Route methods are bounded and must not synchronously reenter call control.
/// Their own send guard also serializes asynchronous device state changes.
pub trait CallAudioRoute: PcmSendPort {
    fn status(&self) -> IvyJson;
    fn failed(&self) -> bool;
    fn resume(&self) -> Result<()>;
    fn suspend(&self) -> Result<()>;
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}
pub trait CallAudioFactory: Send + Sync + 'static {
    /// PreparationReleased may be returned only after all opened resources
    /// have actually been disposed. Other failures retain the original fence.
    fn open<'a>(
        &'a self,
        settings: &'a AudioSettings,
        desktop: Option<&'a AudioDesktopIdentity>,
        permit: AudioRoutePermit,
        stop: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<dyn CallAudioRoute>>> + Send + 'a>>;
}
struct Completion {
    result: OnceLock<Result<()>>,
    changed: Notify,
}
/// Clones wait on the same original work; cancelling a waiter cancels no I/O.
#[derive(Clone)]
pub struct AudioOperation(Arc<Completion>);
impl AudioOperation {
    pub(super) fn new() -> Self {
        Self(Arc::new(Completion {
            result: OnceLock::new(),
            changed: Notify::new(),
        }))
    }
    pub(super) fn complete(&self, result: Result<()>) {
        let _ = self.0.result.set(result);
        self.0.changed.notify_waiters();
    }
    pub fn finished(&self) -> bool {
        self.0.result.get().is_some()
    }
    pub async fn wait(&self) -> Result<()> {
        loop {
            let n = self.0.changed.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if let Some(result) = self.0.result.get() {
                return *result;
            }
            n.await;
        }
    }
}
#[derive(Clone)]
pub struct AudioAttachmentStatus {
    pub call_id: String,
    pub state: &'static str,
    pub route: Option<IvyJson>,
}
#[derive(Clone, Copy)]
pub(super) struct CallAudioContext {
    pub live: bool,
    pub pcm: PcmAudioState,
    pub route_state: Option<PcmAudioState>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Signature {
    route_epoch: u64,
    route: Option<PcmAudioState>,
    permit: u64,
    current: bool,
    bound: Option<u64>,
    failed: bool,
    revoked: bool,
}
pub(super) struct CallState {
    permit: AudioPermit,
    expires: Option<Duration>,
    permitted_generation: Option<u64>,
    route: Option<Arc<dyn CallAudioRoute>>,
    route_epoch: u64,
    preparation: Option<AudioOperation>,
    rebind: Option<AudioOperation>,
    disposal: Option<AudioOperation>,
    settings: Option<AudioSettings>,
    desktop: Option<AudioDesktopIdentity>,
    authority_bound: bool,
    recovery_armed: bool,
    rebind_attempted: bool,
    failed: bool,
    closed: bool,
    revocation_started: bool,
    final_voice_failure: bool,
    cleanup_error: Option<CallAudioError>,
    observed: Option<Signature>,
    generation: u64,
    realtime_generation: i32,
    realtime: Option<Arc<super::realtime::route::RealtimeRoute>>,
    realtime_stopping: Option<AudioOperation>,
}
struct Inner {
    call_id: String,
    state: Mutex<CallState>,
    factory: Arc<dyn CallAudioFactory>,
    lease: AudioRoutePermit,
    stop: CancellationToken,
    realtime_factory: Arc<super::realtime::route::RouteFactory>,
}
impl Inner {
    fn now(&self) -> Duration {
        self.lease.0.origin.elapsed()
    }
    fn refresh(&self, state: &mut CallState) -> PcmAudioState {
        if !state.final_voice_failure && state.route.as_ref().is_some_and(|route| route.failed()) {
            state.failed = true;
        }
        let current = state.permit.is_current(self.now());
        let revoked = self.lease.0.revoked.load(Ordering::Acquire);
        let bound = current
            && state.permitted_generation == Some(state.permit.generation())
            && !state.failed
            && !revoked;
        self.lease.0.deadline.store(
            if !bound {
                0
            } else {
                state.expires.map_or(u64::MAX, |d| {
                    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
                })
            },
            Ordering::Release,
        );
        let route = if state.final_voice_failure {
            state.observed.and_then(|old| old.route)
        } else {
            state.route.as_ref().map(|route| route.state())
        };
        let signature = Signature {
            route_epoch: state.route_epoch,
            route,
            permit: state.permit.generation(),
            current,
            bound: state.permitted_generation,
            failed: state.failed,
            revoked,
        };
        if state.observed.is_some_and(|old| old != signature) {
            state.generation = state
                .generation
                .checked_add(1)
                .expect("call generation exhausted");
        }
        state.observed = Some(signature);
        PcmAudioState {
            open: bound && route.is_some_and(|r| r.open),
            generation: state.generation,
        }
    }
    fn activate(&self, state: &mut CallState) -> Result<()> {
        if self.lease.0.revoked.load(Ordering::Acquire)
            || state.failed
            || state.route.is_none()
            || !state.permit.is_current(self.now())
        {
            return Err(CallAudioError::NotReady);
        }
        let generation = state.permit.generation();
        if state.permitted_generation != Some(generation) {
            state.route.as_ref().unwrap().suspend()?;
            state.permitted_generation = Some(generation);
        }
        self.refresh(state);
        state.route.as_ref().unwrap().resume()?;
        self.refresh(state);
        Ok(())
    }
    fn revoke(&self) {
        // Intent is visible even while a bounded capture callback is finishing.
        self.lease.0.revoked.store(true, Ordering::Release);
        self.lease.0.deadline.store(0, Ordering::Release);
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        self.revoke_locked(&mut state);
    }
    fn revoke_locked(&self, state: &mut CallState) {
        if state.revocation_started {
            return;
        }
        state.revocation_started = true;
        state.permit.revoke();
        self.stop.cancel();
        if let Some(route) = &state.route {
            if let Err(error) = route.suspend() {
                state.failed = true;
                state.cleanup_error = Some(error);
            }
        }
        self.refresh(state);
    }
}
struct Handle(Arc<Inner>);
impl Drop for Handle {
    fn drop(&mut self) {
        close_inner(self.0.clone());
    }
}
#[derive(Clone)]
pub struct CallAudioPort {
    handle: Arc<Handle>,
}
impl CallAudioPort {
    pub fn new(call_id: String, factory: Arc<dyn CallAudioFactory>) -> Result<Self> {
        if !crate::native_rpc::guid_d(&call_id) {
            return Err(CallAudioError::InvalidRequest);
        }
        let lease = AudioRoutePermit(Arc::new(RoutePermit {
            origin: Instant::now(),
            deadline: AtomicU64::new(0),
            revoked: AtomicBool::new(false),
        }));
        Ok(Self {
            handle: Arc::new(Handle(Arc::new(Inner {
                call_id,
                factory,
                lease,
                stop: CancellationToken::new(),
                realtime_factory: Arc::new(super::realtime::route::RealtimeRoute::open),
                state: Mutex::new(CallState {
                    permit: AudioPermit::default(),
                    expires: Some(Duration::ZERO),
                    permitted_generation: None,
                    route: None,
                    route_epoch: 0,
                    preparation: None,
                    rebind: None,
                    disposal: None,
                    settings: None,
                    desktop: None,
                    authority_bound: false,
                    recovery_armed: false,
                    rebind_attempted: false,
                    failed: false,
                    closed: false,
                    revocation_started: false,
                    final_voice_failure: false,
                    cleanup_error: None,
                    observed: None,
                    generation: 0,
                    realtime_generation: -1,
                    realtime: None,
                    realtime_stopping: None,
                }),
            }))),
        })
    }
    pub fn call_id(&self) -> &str {
        &self.handle.0.call_id
    }
    pub fn voice_target(&self) -> Result<(AudioDesktopIdentity, String)> {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.desktop.is_none() || state.route.is_none() {
            return Err(CallAudioError::NotReady);
        }
        // VoiceTarget reads Failed, not the PCM snapshot. Preserve its sticky
        // provider failure and exception policy without advancing generations.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.route.as_ref().unwrap().failed()
        }))
        .unwrap_or(true)
        {
            state.failed = true;
        }
        if state.failed || inner.lease.0.revoked.load(Ordering::Acquire) {
            return Err(CallAudioError::NotReady);
        }
        Ok((
            state.desktop.clone().ok_or(CallAudioError::NotReady)?,
            state
                .settings
                .as_ref()
                .ok_or(CallAudioError::NotReady)?
                .capture_endpoint
                .clone(),
        ))
    }
    pub fn now(&self) -> Duration {
        self.handle.0.now()
    }
    pub fn status(&self) -> AudioAttachmentStatus {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        let pcm = inner.refresh(&mut state);
        AudioAttachmentStatus {
            call_id: inner.call_id.clone(),
            state: if state.closed {
                "closed"
            } else if state.failed {
                "failed"
            } else if inner.lease.0.revoked.load(Ordering::Acquire) {
                "revoked"
            } else if state.preparation.is_none() {
                "unprepared"
            } else if state.route.is_none() {
                "preparing"
            } else if pcm.open {
                "open"
            } else {
                "suspended"
            },
            route: state.route.as_ref().map(|r| r.status()),
        }
    }
    pub fn prepare(
        &self,
        settings: AudioSettings,
        desktop: Option<AudioDesktopIdentity>,
        cancel: CancellationToken,
    ) -> Result<AudioOperation> {
        settings.validate()?;
        if settings.source_mode == "desktop_process" && desktop.is_none() {
            return Err(CallAudioError::InvalidRequest);
        }
        let inner = self.handle.0.clone();
        let operation = AudioOperation::new();
        {
            let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if inner.lease.0.revoked.load(Ordering::Acquire) || state.preparation.is_some() {
                return Err(CallAudioError::NotReady);
            }
            state.settings = Some(settings.clone());
            state.desktop = desktop.clone();
            state.preparation = Some(operation.clone());
        }
        let work = inner.clone();
        spawn_owned(inner, operation.clone(), Kind::Prepare, async move {
            let stop = work.stop.child_token();
            let cancellation = stop.clone();
            let observed = cancel.clone();
            if cancel.is_cancelled() {
                stop.cancel();
            }
            let observer = tokio::spawn(async move {
                tokio::select! {_=observed.cancelled()=>cancellation.cancel(),_=cancellation.cancelled()=>()}
            });
            let result = prepare_core(&work, settings, desktop, stop, cancel).await;
            observer.abort();
            result
        });
        Ok(operation)
    }
    pub fn activate(&self) -> Result<()> {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        inner.refresh(&mut state);
        inner.activate(&mut state)
    }
    pub fn suspend(&self) -> Result<()> {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(route) = &state.route {
            route.suspend()?;
        }
        inner.refresh(&mut state);
        Ok(())
    }
    pub fn bind_authority(&self, endpoint: &str) -> Result<()> {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        inner.refresh(&mut state);
        if state.authority_bound
            || state.failed
            || inner.lease.0.revoked.load(Ordering::Acquire)
            || state.route.is_none()
            || state
                .settings
                .as_ref()
                .is_none_or(|s| s.capture_endpoint != endpoint)
        {
            return Err(CallAudioError::NotReady);
        }
        state.authority_bound = true;
        clear(inner, &mut state)
    }
    pub fn grant_authority(&self) -> Result<()> {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        state.permit.grant()?;
        state.expires = None;
        inner.refresh(&mut state);
        inner.activate(&mut state)
    }
    pub fn refresh_authority(&self, observation: Duration) -> Result<()> {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        state
            .permit
            .renew(observation, Duration::from_millis(500), inner.now())?;
        state.expires = observation.checked_add(Duration::from_millis(500));
        inner.refresh(&mut state);
        inner.activate(&mut state)
    }
    pub fn clear_authority(&self) -> Result<()> {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        clear(inner, &mut state)
    }
    /// The agreed final-Voice exception is latched while the original route
    /// gate is still held. Native hangup/disposal cannot retry that provider.
    pub(crate) fn finish_voice_audio(&self, preserve: bool) -> Result<()> {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let inner = &self.handle.0;
        if !preserve {
            inner.lease.0.revoked.store(true, Ordering::Release);
            inner.lease.0.deadline.store(0, Ordering::Release);
        }
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.final_voice_failure {
            return Err(CallAudioError::CleanupUnconfirmed);
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if preserve {
                clear(inner, &mut state)
            } else {
                inner.revoke_locked(&mut state);
                Ok(())
            }
        }))
        .unwrap_or(Err(CallAudioError::CleanupUnconfirmed));
        if outcome.is_err() {
            state.final_voice_failure = true;
            state.revocation_started = true;
            state.failed = true;
            state.cleanup_error = Some(CallAudioError::CleanupUnconfirmed);
            state.permit.revoke();
            inner.lease.0.revoked.store(true, Ordering::Release);
            inner.lease.0.deadline.store(0, Ordering::Release);
        }
        outcome
    }
    pub fn release_voice_authority(&self) -> Result<()> {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        inner.refresh(&mut state);
        if state.failed || state.closed || inner.lease.0.revoked.load(Ordering::Acquire) {
            return Err(CallAudioError::NotReady);
        }
        clear(inner, &mut state)?;
        state.authority_bound = false;
        Ok(())
    }
    pub fn arm_voice_recovery(&self) -> Result<()> {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        inner.refresh(&mut state);
        if !state.authority_bound
            || state.failed
            || state.closed
            || state.route.is_none()
            || inner.lease.0.revoked.load(Ordering::Acquire)
        {
            return Err(CallAudioError::NotReady);
        }
        state.recovery_armed = true;
        Ok(())
    }
    pub fn rebind(
        &self,
        desktop: AudioDesktopIdentity,
        original_capture: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<AudioOperation> {
        if !original_capture() {
            return Err(CallAudioError::OwnerChanged);
        }
        let inner = self.handle.0.clone();
        let operation = AudioOperation::new();
        let (old, settings) = {
            let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            inner.refresh(&mut state);
            if !state.recovery_armed
                || state.rebind_attempted
                || state.closed
                || inner.lease.0.revoked.load(Ordering::Acquire)
                || !state.failed
                || state.route.is_none()
                || state.desktop.as_ref().is_none_or(|old| !old.same(&desktop))
            {
                return Err(CallAudioError::NotReady);
            }
            state.rebind_attempted = true;
            state.rebind = Some(operation.clone());
            state.route_epoch += 1;
            (state.route.take().unwrap(), state.settings.clone().unwrap())
        };
        let work = inner.clone();
        spawn_owned(inner, operation.clone(), Kind::Rebind, async move {
            if let Err(error) = old.close().await {
                work.state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .cleanup_error = Some(error);
                return Err(error);
            }
            if !original_capture()
                || !work
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .permit
                    .is_current(work.now())
                || work.lease.0.revoked.load(Ordering::Acquire)
            {
                return Err(CallAudioError::OwnerChanged);
            }
            let opened = work
                .factory
                .open(
                    &settings,
                    Some(&desktop),
                    work.lease.clone(),
                    work.stop.clone(),
                )
                .await;
            let route = match opened {
                Ok(route) => route,
                Err(error) => {
                    if error != CallAudioError::PreparationReleased {
                        work.state
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .cleanup_error = Some(error);
                    }
                    return Err(error);
                }
            };
            let valid = !route.state().open
                && route.status().get("state") == Some(&IvyJson::from("suspended"))
                && original_capture();
            let adopted = {
                let mut state = work.state.lock().unwrap_or_else(|p| p.into_inner());
                if valid
                    && !work.lease.0.revoked.load(Ordering::Acquire)
                    && !state.closed
                    && state.permit.is_current(work.now())
                {
                    state.route = Some(route.clone());
                    state.route_epoch += 1;
                    state.failed = false;
                    state.permitted_generation = None;
                    let result = work.activate(&mut state);
                    if result.is_ok() && !work.refresh(&mut state).open {
                        Err(CallAudioError::AudioNotReady)
                    } else {
                        result
                    }
                } else {
                    Err(CallAudioError::OwnerChanged)
                }
            };
            if work
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .route
                .as_ref()
                .is_none_or(|r| !Arc::ptr_eq(r, &route))
            {
                if let Err(error) = route.close().await {
                    work.state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .cleanup_error = Some(error);
                    return Err(error);
                }
            }
            adopted
        });
        Ok(operation)
    }
    pub fn revoke(&self) {
        self.handle.0.revoke();
    }
    pub(super) fn revoked(&self) -> bool {
        self.handle.0.lease.0.revoked.load(Ordering::Acquire)
    }
    pub fn close(&self) -> AudioOperation {
        close_inner(self.handle.0.clone())
    }
    /// Progress ports hold this same call lock while serializing their own
    /// state and final dispatch. This never grants normal capture authority.
    pub(super) fn with_call<T>(
        &self,
        action: impl FnOnce(CallAudioContext, Option<&Arc<dyn CallAudioRoute>>) -> T,
    ) -> T {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        let pcm = inner.refresh(&mut state);
        action(
            CallAudioContext {
                live: !inner.lease.0.revoked.load(Ordering::Acquire) && !state.closed,
                pcm,
                route_state: state.observed.unwrap().route,
            },
            state.route.as_ref(),
        )
    }
}
fn clear(inner: &Inner, state: &mut CallState) -> Result<()> {
    if state.final_voice_failure {
        return Err(CallAudioError::CleanupUnconfirmed);
    }
    state.permit.suspend();
    inner.lease.0.deadline.store(0, Ordering::Release);
    if let Some(route) = &state.route {
        route.suspend()?;
    }
    inner.refresh(state);
    Ok(())
}
async fn prepare_core(
    inner: &Arc<Inner>,
    settings: AudioSettings,
    desktop: Option<AudioDesktopIdentity>,
    stop: CancellationToken,
    cancel: CancellationToken,
) -> Result<()> {
    tokio::task::yield_now().await;
    if stop.is_cancelled() || cancel.is_cancelled() || inner.lease.0.revoked.load(Ordering::Acquire)
    {
        return Err(CallAudioError::Cancelled);
    }
    let route = inner
        .factory
        .open(
            &settings,
            desktop.as_ref(),
            inner.lease.clone(),
            stop.clone(),
        )
        .await?;
    let result = {
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if stop.is_cancelled()
            || cancel.is_cancelled()
            || inner.lease.0.revoked.load(Ordering::Acquire)
        {
            Err(CallAudioError::Cancelled)
        } else if route.state().open
            || route.status().get("state") != Some(&IvyJson::from("suspended"))
        {
            Err(CallAudioError::NotReady)
        } else {
            state.route = Some(route.clone());
            state.route_epoch += 1;
            Ok(())
        }
    };
    if result.is_err() {
        route.close().await?;
    }
    result
}
#[derive(Clone, Copy)]
enum Kind {
    Prepare,
    Rebind,
    Close,
}
fn spawn_owned(
    inner: Arc<Inner>,
    operation: AudioOperation,
    kind: Kind,
    work: impl Future<Output = Result<()>> + Send + 'static,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        state.failed = true;
        state.cleanup_error = Some(CallAudioError::CleanupUnconfirmed);
        drop(state);
        operation.complete(Err(CallAudioError::CleanupUnconfirmed));
        return;
    };
    runtime.spawn(async move {
        let joined = tokio::spawn(work).await;
        let panicked = joined.is_err();
        let result = joined.unwrap_or(Err(CallAudioError::CleanupUnconfirmed));
        {
            let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if result.is_err()
                && (!matches!(kind, Kind::Prepare)
                    || !inner.lease.0.revoked.load(Ordering::Acquire))
            {
                state.failed = true;
            }
            if panicked {
                state.cleanup_error = Some(CallAudioError::CleanupUnconfirmed);
            }
            if matches!(kind, Kind::Close) && result.is_ok() {
                state.closed = true;
            }
            inner.refresh(&mut state);
        }
        operation.complete(result);
    });
}
fn close_inner(inner: Arc<Inner>) -> AudioOperation {
    inner.revoke();
    let operation = AudioOperation::new();
    {
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = &state.disposal {
            return existing.clone();
        }
        state.disposal = Some(operation.clone());
        if state.final_voice_failure {
            operation.complete(Err(CallAudioError::CleanupUnconfirmed));
            return operation;
        }
    }
    let work = inner.clone();
    spawn_owned(inner, operation.clone(), Kind::Close, async move {
        tokio::task::yield_now().await;
        let (preparation, rebind, realtime_stopping) = {
            let state = work.state.lock().unwrap_or_else(|p| p.into_inner());
            (
                state.preparation.clone(),
                state.rebind.clone(),
                state.realtime_stopping.clone(),
            )
        };
        let mut failure = None;
        if let Some(preparation) = preparation {
            if let Err(error) = preparation.wait().await {
                if !matches!(
                    error,
                    CallAudioError::PreparationReleased | CallAudioError::Cancelled
                ) {
                    failure = Some(error);
                }
            }
        }
        if let Some(rebind) = rebind {
            let _ = rebind.wait().await;
        }
        if let Some(stopping) = realtime_stopping {
            if stopping.wait().await.is_err() {
                return Err(CallAudioError::CleanupUnconfirmed);
            }
        }
        let route = {
            let mut state = work.state.lock().unwrap_or_else(|p| p.into_inner());
            state.route_epoch += 1;
            state.realtime = None;
            state.route.take()
        };
        if let Some(route) = route {
            route.close().await?;
        }
        let state = work.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(error) = state.cleanup_error.or(failure) {
            return Err(error);
        }
        Ok(())
    });
    operation
}
impl PcmAudioPort for CallAudioPort {
    fn state(&self) -> PcmAudioState {
        self.with_call(|context, _| context.pcm)
    }
    fn read_captured(&self, output: &mut [f32], generation: u64) -> usize {
        output.fill(0.0);
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        let admitted = inner.refresh(&mut state);
        if !inner.lease.permitted() || state.route.is_none() || admitted.generation != generation {
            return 0;
        }
        let route = state.route.as_ref().unwrap().clone();
        let route_state = state.observed.unwrap().route.unwrap();
        let count = route.read_captured(output, route_state.generation);
        if inner.refresh(&mut state) != admitted {
            output.fill(0.0);
            0
        } else {
            count
        }
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        let admitted = inner.refresh(&mut state);
        if !admitted.open || admitted.generation != generation {
            return false;
        }
        let route = state.route.as_ref().unwrap();
        route.write_received(input, state.observed.unwrap().route.unwrap().generation)
    }
}
impl PcmSendPort for CallAudioPort {
    fn with_send_state(&self, admitted: PcmAudioState, action: &mut dyn FnMut()) -> bool {
        self.with_call(|context, route| {
            if !context.live || context.pcm != admitted {
                return false;
            }
            if let Some(route) = route {
                route.with_send_state(context.route_state.unwrap(), action)
            } else {
                action();
                true
            }
        })
    }
    fn send_health(&self) -> AudioSendHealth {
        let inner = &self.handle.0;
        let mut state = inner.state.lock().unwrap_or_else(|p| p.into_inner());
        inner.refresh(&mut state);
        if !state.failed {
            AudioSendHealth::Ready
        } else if state.recovery_armed
            && !state.closed
            && !inner.lease.0.revoked.load(Ordering::Acquire)
            && (!state.rebind_attempted
                || state.rebind.as_ref().is_some_and(|work| !work.finished()))
        {
            AudioSendHealth::RepairPending
        } else {
            AudioSendHealth::Failed
        }
    }
}
