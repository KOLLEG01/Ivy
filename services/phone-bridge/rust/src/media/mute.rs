//! Exact endpoint mute policy. Leases agree across call routes; release never
//! restores an earlier Windows state. The route supplies its endpoint provider.
use super::{AudioMutePolicy, AudioOperation, CallAudioError};
use std::{
    collections::HashMap,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{runtime::Handle, task::JoinHandle};
use tokio_util::sync::CancellationToken;

#[cfg(all(windows, target_pointer_width = "64"))]
#[path = "mute_windows.rs"]
mod windows;
type Result<T> = std::result::Result<T, CallAudioError>;
pub trait AudioMuteEndpoint {
    fn id(&self) -> Result<String>;
    fn active_render(&self) -> Result<bool>;
    fn muted(&self) -> Result<bool>;
    fn set_muted(&mut self, value: bool) -> Result<()>;
    fn close(&mut self) -> Result<()>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioMuteDelayError {
    Cancelled,
    Failed,
}
pub type AudioMuteDelay =
    Pin<Box<dyn Future<Output = std::result::Result<(), AudioMuteDelayError>> + Send>>;
pub trait AudioMuteClock: Send + Sync + 'static {
    fn delay(
        &self,
        seconds: u16,
        stop: CancellationToken,
    ) -> std::result::Result<AudioMuteDelay, AudioMuteDelayError>;
}
pub struct SystemAudioMuteClock;
impl AudioMuteClock for SystemAudioMuteClock {
    fn delay(
        &self,
        seconds: u16,
        stop: CancellationToken,
    ) -> std::result::Result<AudioMuteDelay, AudioMuteDelayError> {
        let wait = tokio::time::sleep(Duration::from_secs(seconds.into()));
        Ok(Box::pin(async move {
            tokio::select! {_ = stop.cancelled()=>Err(AudioMuteDelayError::Cancelled),_ = wait=>Ok(())}
        }))
    }
}
type Open = dyn Fn(&str) -> Result<Box<dyn AudioMuteEndpoint>> + Send + Sync;
type Failed = dyn Fn() + Send + Sync;
static LEASES: OnceLock<Mutex<HashMap<String, (bool, usize)>>> = OnceLock::new();
struct Lease(Vec<(String, bool)>);
impl Lease {
    fn reserve(desired: Vec<(String, bool)>) -> Result<Arc<Self>> {
        let mut leases = LEASES.get_or_init(Default::default).lock().unwrap();
        if desired
            .iter()
            .any(|(id, muted)| leases.get(id).is_some_and(|(old, _)| old != muted))
        {
            return Err(CallAudioError::RouteFailed);
        }
        for (id, muted) in &desired {
            let owners = leases.get(id).map_or(0, |(_, n)| *n);
            leases.insert(id.clone(), (*muted, owners + 1));
        }
        Ok(Arc::new(Self(desired)))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut leases = LEASES.get().unwrap().lock().unwrap();
        for (id, _) in &self.0 {
            let lease = leases.get_mut(id).unwrap();
            if lease.1 == 1 {
                leases.remove(id);
            } else {
                lease.1 -= 1;
            }
        }
    }
}
fn enforce(lease: &Lease, open: &Open) -> Result<()> {
    for (id, muted) in &lease.0 {
        let mut endpoint = catch_unwind(AssertUnwindSafe(|| open(id)))
            .unwrap_or(Err(CallAudioError::RouteFailed))?;
        let result = catch_unwind(AssertUnwindSafe(|| {
            if endpoint.id()? != *id || !endpoint.active_render()? {
                return Err(CallAudioError::RouteFailed);
            }
            if endpoint.muted()? != *muted {
                endpoint.set_muted(*muted)?;
            }
            if endpoint.muted()? != *muted {
                return Err(CallAudioError::RouteFailed);
            }
            Ok(())
        }))
        .unwrap_or(Err(CallAudioError::RouteFailed));
        let closed = catch_unwind(AssertUnwindSafe(|| endpoint.close()))
            .unwrap_or(Err(CallAudioError::RouteFailed));
        closed.and(result)?;
    }
    Ok(())
}
fn delay(
    clock: &dyn AudioMuteClock,
    seconds: u16,
    stop: CancellationToken,
) -> std::result::Result<AudioMuteDelay, AudioMuteDelayError> {
    catch_unwind(AssertUnwindSafe(|| clock.delay(seconds, stop)))
        .unwrap_or(Err(AudioMuteDelayError::Failed))
}
struct State {
    worker: JoinHandle<Result<()>>,
    lease: Arc<Lease>,
}
pub struct AudioMuteGuard {
    stop: CancellationToken,
    state: Mutex<Option<State>>,
    runtime: Handle,
}
impl AudioMuteGuard {
    #[cfg(all(windows, target_pointer_width = "64"))]
    pub fn windows(
        policy: &AudioMutePolicy,
        receive: &str,
        failed: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::start(policy, receive, failed, windows::open)
    }
    pub fn start(
        policy: &AudioMutePolicy,
        receive: &str,
        failed: impl Fn() + Send + Sync + 'static,
        open: impl Fn(&str) -> Result<Box<dyn AudioMuteEndpoint>> + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::with_clock(
            policy,
            receive,
            failed,
            open,
            Arc::new(SystemAudioMuteClock),
        )
    }
    pub fn with_clock(
        policy: &AudioMutePolicy,
        receive: &str,
        failed: impl Fn() + Send + Sync + 'static,
        open: impl Fn(&str) -> Result<Box<dyn AudioMuteEndpoint>> + Send + Sync + 'static,
        clock: Arc<dyn AudioMuteClock>,
    ) -> Result<Self> {
        policy.validate(receive)?;
        let runtime = Handle::try_current().map_err(|_| CallAudioError::RouteFailed)?;
        let mut desired = Vec::new();
        if let Some(id) = &policy.muted_render_endpoint {
            desired.push((id.clone(), true));
        }
        if policy.unmute_receive {
            desired.push((receive.into(), false));
        }
        let lease = Lease::reserve(desired)?;
        let open: Arc<Open> = Arc::new(open);
        let failed: Arc<Failed> = Arc::new(failed);
        enforce(&lease, open.as_ref())?;
        let stop = CancellationToken::new();
        // Like RunAsync, create the first delay before returning the guard.
        let first = delay(clock.as_ref(), policy.interval_seconds, stop.clone());
        let first = match first {
            Ok(wait) => Ok(wait),
            Err(AudioMuteDelayError::Cancelled) if stop.is_cancelled() => Err(Ok(())),
            Err(_) => Err(catch_unwind(AssertUnwindSafe(|| failed()))
                .map_err(|_| CallAudioError::CleanupUnconfirmed)),
        };
        let (worker_lease, worker_stop) = (lease.clone(), stop.clone());
        let seconds = policy.interval_seconds;
        let worker = runtime.spawn(async move {
            let mut wait = match first {
                Ok(wait) => wait,
                Err(result) => return result,
            };
            loop {
                match wait.await {
                    Err(AudioMuteDelayError::Cancelled) if worker_stop.is_cancelled() => {
                        return Ok(());
                    }
                    Err(_) => {
                        failed();
                        return Ok(());
                    }
                    Ok(()) => (),
                }
                let (lease, open) = (worker_lease.clone(), open.clone());
                if !matches!(
                    tokio::task::spawn_blocking(move || enforce(&lease, open.as_ref())).await,
                    Ok(Ok(()))
                ) {
                    failed();
                    return Ok(());
                }
                wait = match delay(clock.as_ref(), seconds, worker_stop.clone()) {
                    Ok(wait) => wait,
                    Err(AudioMuteDelayError::Cancelled) if worker_stop.is_cancelled() => {
                        return Ok(());
                    }
                    Err(_) => {
                        failed();
                        return Ok(());
                    }
                };
            }
        });
        Ok(Self {
            stop,
            state: Mutex::new(Some(State { worker, lease })),
            runtime,
        })
    }
    /// First close retains cancellation/join/release independently of its
    /// observer. Like the original interlocked disposal, later calls return
    /// immediately; the route must retain its first complete disposal result.
    pub fn close(&self) -> AudioOperation {
        let result = AudioOperation::new();
        let Some(state) = self.state.lock().unwrap().take() else {
            result.complete(Ok(()));
            return result;
        };
        self.stop.cancel();
        let completed = result.clone();
        self.runtime.spawn(async move {
            let value = state
                .worker
                .await
                .unwrap_or(Err(CallAudioError::CleanupUnconfirmed));
            drop(state.lease);
            completed.complete(value);
        });
        result
    }
}
impl Drop for AudioMuteGuard {
    fn drop(&mut self) {
        self.close();
    }
}
