//! Windows call audio composition. PCM authority and callbacks share one lock;
//! native clients retire before mute cleanup, endpoints after it.
use super::{
    AudioDesktopIdentity, AudioMuteGuard, AudioOperation, AudioRoutePermit, AudioSendHealth,
    AudioSettings, CallAudioError, CallAudioFactory, CallAudioRoute, PcmAudioPort, PcmAudioState,
    PcmSendPort,
    windows_audio::WindowsAudioBuffers,
    windows_capture::{CaptureError, worker::Recorder},
    windows_render::{RenderError, status::Status, worker::Player},
};
use crate::desktop::{DesktopOwner, WindowsDesktopProcessOwner};
use ivy_contracts::IvyJson;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, CallAudioError>;
type Task<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;
type Buffers = Arc<Mutex<WindowsAudioBuffers>>;
trait StreamOwner: Send + Sync {
    fn configure(&self) -> Task<()>;
    fn start(&self) -> Result<()>;
    fn retire(&self) -> Task<()>;
    fn close(&self) -> Task<()>;
}
trait RenderOwner: StreamOwner {
    fn status(&self) -> Status;
}
trait MuteOwner: Send + Sync {
    fn close(&self) -> Task<()>;
}
trait Backend: Send + Sync + 'static {
    fn process_supported(&self) -> bool;
    fn desktop(&self, desktop: Option<AudioDesktopIdentity>) -> Task<()>;
    fn player(
        &self,
        settings: AudioSettings,
        desktop: Option<AudioDesktopIdentity>,
        buffers: Buffers,
    ) -> Task<Arc<dyn RenderOwner>>;
    fn recorder(
        &self,
        settings: AudioSettings,
        desktop: Option<AudioDesktopIdentity>,
        buffers: Buffers,
    ) -> Task<Arc<dyn StreamOwner>>;
    fn mute(&self, settings: AudioSettings, buffers: Buffers) -> Task<Arc<dyn MuteOwner>>;
}
fn render_error(error: RenderError) -> CallAudioError {
    match error {
        RenderError::Cleanup => CallAudioError::CleanupUnconfirmed,
        RenderError::Configuration => CallAudioError::InvalidRequest,
        RenderError::Owner => CallAudioError::OwnerChanged,
        RenderError::Unavailable => CallAudioError::NotReady,
        _ => CallAudioError::RouteFailed,
    }
}
fn capture_error(error: CaptureError) -> CallAudioError {
    match error {
        CaptureError::Cleanup => CallAudioError::CleanupUnconfirmed,
        CaptureError::Configuration => CallAudioError::InvalidRequest,
        CaptureError::Owner => CallAudioError::OwnerChanged,
        CaptureError::Unavailable => CallAudioError::NotReady,
        _ => CallAudioError::RouteFailed,
    }
}
impl StreamOwner for Player {
    fn configure(&self) -> Task<()> {
        let work = Player::configure(self);
        Box::pin(async move { work.await.map_err(render_error) })
    }
    fn start(&self) -> Result<()> {
        Player::start(self).map_err(render_error)
    }
    fn retire(&self) -> Task<()> {
        let work = Player::retire(self);
        Box::pin(async move { work.await.map_err(render_error) })
    }
    fn close(&self) -> Task<()> {
        let work = Player::close(self);
        Box::pin(async move { work.await.map_err(render_error) })
    }
}
impl RenderOwner for Player {
    fn status(&self) -> Status {
        Player::status(self)
    }
}
impl StreamOwner for Recorder {
    fn configure(&self) -> Task<()> {
        let work = Recorder::configure(self);
        Box::pin(async move { work.await.map_err(capture_error) })
    }
    fn start(&self) -> Result<()> {
        Recorder::start(self).map_err(capture_error)
    }
    fn retire(&self) -> Task<()> {
        let work = Recorder::retire(self);
        Box::pin(async move { work.await.map_err(capture_error) })
    }
    fn close(&self) -> Task<()> {
        let work = Recorder::close(self);
        Box::pin(async move { work.await.map_err(capture_error) })
    }
}
impl MuteOwner for AudioMuteGuard {
    fn close(&self) -> Task<()> {
        let work = AudioMuteGuard::close(self);
        Box::pin(async move { work.wait().await })
    }
}
struct Native;
impl Backend for Native {
    fn process_supported(&self) -> bool {
        crate::loopback_probe::windows::process_loopback_supported()
    }
    fn desktop(&self, desktop: Option<AudioDesktopIdentity>) -> Task<()> {
        Box::pin(async move {
            let Some(desktop) = desktop else {
                return Ok(());
            };
            tokio::task::spawn_blocking(move || {
                let mut owner = WindowsDesktopProcessOwner::new(
                    desktop.pid(),
                    desktop.creation_ticks(),
                    desktop.image(),
                )
                .map_err(|_| CallAudioError::OwnerChanged)?;
                if owner.is_current().unwrap_or(false) {
                    Ok(())
                } else {
                    Err(CallAudioError::OwnerChanged)
                }
            })
            .await
            .unwrap_or(Err(CallAudioError::CleanupUnconfirmed))
        })
    }
    fn player(
        &self,
        settings: AudioSettings,
        desktop: Option<AudioDesktopIdentity>,
        buffers: Buffers,
    ) -> Task<Arc<dyn RenderOwner>> {
        let work = Player::admit_route(&settings, desktop, buffers);
        Box::pin(
            async move { Ok(Arc::new(work.await.map_err(render_error)?) as Arc<dyn RenderOwner>) },
        )
    }
    fn recorder(
        &self,
        settings: AudioSettings,
        desktop: Option<AudioDesktopIdentity>,
        buffers: Buffers,
    ) -> Task<Arc<dyn StreamOwner>> {
        let work = Recorder::admit_route(&settings, desktop, buffers);
        Box::pin(
            async move { Ok(Arc::new(work.await.map_err(capture_error)?) as Arc<dyn StreamOwner>) },
        )
    }
    fn mute(&self, settings: AudioSettings, buffers: Buffers) -> Task<Arc<dyn MuteOwner>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let guard = AudioMuteGuard::windows(
                    settings.mute.as_ref().unwrap(),
                    &settings.render_endpoint,
                    move || buffers.lock().unwrap_or_else(|p| p.into_inner()).fail(),
                )?;
                Ok(Arc::new(guard) as Arc<dyn MuteOwner>)
            })
            .await
            .unwrap_or(Err(CallAudioError::CleanupUnconfirmed))
        })
    }
}

/// A real native route factory. Construction alone performs no OS operation;
/// only an admitted call's open prepares its explicitly configured endpoints.
#[derive(Default)]
pub struct WindowsAudioFactory;
impl CallAudioFactory for WindowsAudioFactory {
    fn open<'a>(
        &'a self,
        settings: &'a AudioSettings,
        desktop: Option<&'a AudioDesktopIdentity>,
        permit: AudioRoutePermit,
        stop: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<dyn CallAudioRoute>>> + Send + 'a>> {
        begin_open(
            Arc::new(Native),
            settings.clone(),
            desktop.cloned(),
            Arc::new(move || permit.permitted()),
            stop,
        )
    }
}
#[derive(Clone, Default)]
struct Resources {
    recorder: Option<Arc<dyn StreamOwner>>,
    player: Option<Arc<dyn RenderOwner>>,
    mute: Option<Arc<dyn MuteOwner>>,
}
struct Inner {
    settings: AudioSettings,
    buffers: Buffers,
    resources: Mutex<Resources>,
    render: OnceLock<Status>,
    closing: Mutex<Option<AudioOperation>>,
    runtime: Handle,
}
struct Route(Arc<Inner>);
impl Inner {
    fn close(self: &Arc<Self>) -> AudioOperation {
        let mut closing = self.closing.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(work) = &*closing {
            return work.clone();
        }
        self.buffers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .close();
        let work = AudioOperation::new();
        *closing = Some(work.clone());
        let inner = self.clone();
        let completion = work.clone();
        self.runtime.spawn(async move {
            // No callback can be waiting for a PCM lock held across disposal.
            tokio::task::yield_now().await;
            let owners = inner
                .resources
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            let mut result = Ok(());
            if let Some(recorder) = owners.recorder.clone() {
                result = result.and(cleanup(move || recorder.retire()).await);
            }
            if let Some(player) = owners.player.clone() {
                result = result.and(cleanup(move || player.retire()).await);
            }
            if let Some(mute) = owners.mute {
                result = result.and(cleanup(move || mute.close()).await);
            }
            if let Some(recorder) = owners.recorder {
                result = result.and(cleanup(move || recorder.close()).await);
            }
            if let Some(player) = owners.player {
                result = result.and(cleanup(move || player.close()).await);
            }
            completion.complete(result);
        });
        work
    }
}
async fn cleanup(action: impl FnOnce() -> Task<()> + Send + 'static) -> Result<()> {
    // Catch both a synchronous cleanup prefix panic and a future panic while
    // retaining the route and continuing its remaining finally stages.
    tokio::spawn(async move { action().await })
        .await
        .unwrap_or(Err(CallAudioError::CleanupUnconfirmed))
        .map_err(|_| CallAudioError::CleanupUnconfirmed)
}
fn begin_open(
    backend: Arc<dyn Backend>,
    settings: AudioSettings,
    desktop: Option<AudioDesktopIdentity>,
    permitted: Arc<dyn Fn() -> bool + Send + Sync>,
    stop: CancellationToken,
) -> Task<Arc<dyn CallAudioRoute>> {
    let Ok(runtime) = Handle::try_current() else {
        return Box::pin(async { Err(CallAudioError::NotReady) });
    };
    let (result, observer) = tokio::sync::oneshot::channel();
    let owner_runtime = runtime.clone();
    runtime.spawn(async move {
        // These original preflight errors precede route construction and its
        // preparation/release fence.
        let preflight = (|| {
            if matches!(
                settings.source_mode.as_str(),
                "system_excluding_runtime" | "desktop_process"
            ) && !backend.process_supported()
            {
                return Err(CallAudioError::NotReady);
            }
            if settings.source_mode == "desktop_process" && desktop.is_none() {
                return Err(CallAudioError::InvalidRequest);
            }
            settings.validate()
        })();
        if let Err(error) = preflight {
            let _ = result.send(Err(error));
            return;
        }
        let buffers = match WindowsAudioBuffers::new(&settings, permitted) {
            Ok(buffers) => Arc::new(Mutex::new(buffers)),
            Err(error) => {
                let _ = result.send(Err(error));
                return;
            }
        };
        let route = Arc::new(Route(Arc::new(Inner {
            settings,
            buffers,
            resources: Mutex::new(Resources::default()),
            render: OnceLock::new(),
            closing: Mutex::new(None),
            runtime: owner_runtime.clone(),
        })));
        let prepared = route.0.clone();
        let prepared = owner_runtime
            .spawn(async move { prepare(&prepared, backend, desktop, stop).await })
            .await
            .unwrap_or(Err(CallAudioError::CleanupUnconfirmed));
        if let Err(error) = prepared {
            let released = route.0.close().wait().await;
            let error = if released.is_ok() && error != CallAudioError::CleanupUnconfirmed {
                CallAudioError::PreparationReleased
            } else {
                CallAudioError::CleanupUnconfirmed
            };
            let _ = result.send(Err(error));
        } else if result
            .send(Ok(route.clone() as Arc<dyn CallAudioRoute>))
            .is_err()
        {
            route.0.close().wait().await.ok();
        }
    });
    Box::pin(async move {
        observer
            .await
            .unwrap_or(Err(CallAudioError::CleanupUnconfirmed))
    })
}
async fn prepare(
    inner: &Arc<Inner>,
    backend: Arc<dyn Backend>,
    desktop: Option<AudioDesktopIdentity>,
    stop: CancellationToken,
) -> Result<()> {
    if stop.is_cancelled() {
        return Err(CallAudioError::Cancelled);
    }
    backend.desktop(desktop.clone()).await?;
    let player = backend
        .player(
            inner.settings.clone(),
            desktop.clone(),
            inner.buffers.clone(),
        )
        .await?;
    inner
        .resources
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .player = Some(player.clone());
    if inner.settings.mute.is_some() {
        let mute = backend
            .mute(inner.settings.clone(), inner.buffers.clone())
            .await?;
        inner
            .resources
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .mute = Some(mute);
    }
    player.configure().await?;
    let _ = inner.render.set(player.status());
    let recorder = backend
        .recorder(
            inner.settings.clone(),
            desktop.clone(),
            inner.buffers.clone(),
        )
        .await?;
    inner
        .resources
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .recorder = Some(recorder.clone());
    recorder.configure().await?;
    if stop.is_cancelled() {
        return Err(CallAudioError::Cancelled);
    }
    backend.desktop(desktop).await?;
    recorder.start()?;
    player.start()?;
    Ok(())
}
impl Drop for Route {
    fn drop(&mut self) {
        self.0.close();
    }
}
fn pcm_state(buffers: &mut WindowsAudioBuffers) -> PcmAudioState {
    PcmAudioState {
        open: buffers.is_open(),
        generation: buffers.snapshot().generation as u64,
    }
}
impl PcmAudioPort for Route {
    fn state(&self) -> PcmAudioState {
        pcm_state(&mut self.0.buffers.lock().unwrap_or_else(|p| p.into_inner()))
    }
    fn read_captured(&self, output: &mut [f32], generation: u64) -> usize {
        let mut buffers = self.0.buffers.lock().unwrap_or_else(|p| p.into_inner());
        if buffers.snapshot().generation as u64 != generation {
            output.fill(0.0);
            return 0;
        }
        buffers.read_captured(output)
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        let mut buffers = self.0.buffers.lock().unwrap_or_else(|p| p.into_inner());
        if !buffers.is_open() || buffers.snapshot().generation as u64 != generation {
            return false;
        }
        buffers.write_received(input);
        buffers.snapshot().state == "open"
    }
}
impl PcmSendPort for Route {
    fn with_send_state(&self, admitted: PcmAudioState, action: &mut dyn FnMut()) -> bool {
        let mut buffers = self.0.buffers.lock().unwrap_or_else(|p| p.into_inner());
        if pcm_state(&mut buffers) != admitted {
            return false;
        }
        action();
        true
    }
    fn send_health(&self) -> AudioSendHealth {
        if self.failed() {
            AudioSendHealth::Failed
        } else {
            AudioSendHealth::Ready
        }
    }
}
impl CallAudioRoute for Route {
    fn status(&self) -> IvyJson {
        let buffers = self.0.buffers.lock().unwrap_or_else(|p| p.into_inner());
        let snapshot = buffers.snapshot();
        let render = self.0.render.get();
        IvyJson::object([
            ("state", snapshot.state.into()),
            ("sourceMode", self.0.settings.source_mode.as_str().into()),
            ("sampleRate", IvyJson::Number(48_000.0)),
            ("channels", IvyJson::Number(1.0)),
            (
                "captureDroppedSamples",
                IvyJson::Number(snapshot.capture_dropped as i64 as f64),
            ),
            (
                "renderDroppedSamples",
                IvyJson::Number(snapshot.render_dropped as i64 as f64),
            ),
            (
                "captureQueuedSamples",
                IvyJson::Number(snapshot.capture_queued as f64),
            ),
            (
                "renderQueuedSamples",
                IvyJson::Number(snapshot.render_queued as f64),
            ),
            (
                "renderLatencyMs",
                render.map_or(IvyJson::Null, |v| IvyJson::Number(v.latency_ms as f64)),
            ),
            (
                "lowLatencyActive",
                render.map_or(IvyJson::Null, |v| IvyJson::Bool(v.low_latency)),
            ),
            (
                "lowLatencyUnavailableReason",
                render
                    .and_then(|v| v.unavailable.as_deref())
                    .map_or(IvyJson::Null, Into::into),
            ),
        ])
    }
    fn failed(&self) -> bool {
        self.0
            .buffers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot()
            .state
            == "failed"
    }
    fn resume(&self) -> Result<()> {
        self.0
            .buffers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .resume()
    }
    fn suspend(&self) -> Result<()> {
        self.0
            .buffers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .suspend();
        Ok(())
    }
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let work = self.0.close();
        Box::pin(async move { work.wait().await })
    }
}
#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_windows_route.rs"
    ));
}
