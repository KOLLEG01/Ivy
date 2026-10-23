//! Passive process-excluding probe policy. The factory owns its native recorder;
//! only bounded level/count telemetry survives each callback.
use ivy_contracts::IvyJson;
use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{sync::Notify, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

#[cfg(all(windows, target_pointer_width = "64"))]
pub mod windows;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeError {
    Unsupported,
    Unavailable,
    Cancelled,
    SampleBound,
    InvalidPcm,
    Capture,
    StoppedEarly,
    StopTimeout,
    Cleanup,
}
type Result<T> = std::result::Result<T, ProbeError>;
pub type ProbeFuture<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;
#[derive(Clone, Debug, PartialEq)]
pub struct LoopbackProbeResult {
    pub state: String,
    pub detected: bool,
    pub peak: f32,
    pub samples: u64,
    pub excluded_process_id: u32,
}
impl LoopbackProbeResult {
    pub fn json(&self) -> IvyJson {
        // Preserve the original single-precision JSON spelling before the
        // common JSON value represents the number as a double.
        let peak: f64 = serde_json::to_string(&self.peak).unwrap().parse().unwrap();
        IvyJson::object([
            ("state", self.state.clone().into()),
            ("detected", self.detected.into()),
            ("peak", IvyJson::Number(peak)),
            ("samples", IvyJson::Number(self.samples as f64)),
            ("sampleRate", IvyJson::Number(48000.0)),
            ("channels", IvyJson::Number(2.0)),
            (
                "excludedProcessId",
                IvyJson::Number(self.excluded_process_id as f64),
            ),
        ])
    }
}
#[derive(Default)]
struct Levels {
    peak: f32,
    samples: u64,
}
#[derive(Default)]
pub struct LoopbackProbeMeter(Mutex<Levels>);
impl LoopbackProbeMeter {
    fn add_silence(&self, count: usize) -> Result<()> {
        let mut levels = self.0.lock().unwrap();
        if count > 192000 || levels.samples + count as u64 > 960000 {
            return Err(ProbeError::SampleBound);
        }
        levels.samples += count as u64;
        Ok(())
    }
    /// Like MemoryMarshal.Cast<byte,float>, ignore an incomplete final value.
    pub fn add(&self, bytes: &[u8], silent: bool) -> Result<()> {
        let mut levels = self.0.lock().unwrap();
        let count = bytes.len() / 4;
        if count > 192000 || levels.samples + count as u64 > 960000 {
            return Err(ProbeError::SampleBound);
        }
        for value in bytes.chunks_exact(4) {
            let value = f32::from_le_bytes(value.try_into().unwrap());
            if !silent {
                if !value.is_finite() {
                    return Err(ProbeError::InvalidPcm);
                }
                levels.peak = levels.peak.max(value.abs().min(1.0));
            }
        }
        levels.samples += count as u64;
        Ok(())
    }
    pub fn result(&self, state: &str) -> LoopbackProbeResult {
        let levels = self.0.lock().unwrap();
        LoopbackProbeResult {
            state: state.into(),
            detected: state == "completed" && levels.peak > 0.0001,
            peak: levels.peak,
            samples: levels.samples,
            excluded_process_id: std::process::id(),
        }
    }
}
struct Events {
    meter: LoopbackProbeMeter,
    stopped: OnceLock<Result<()>>,
    changed: Notify,
}
#[derive(Clone)]
pub struct ProbeEvents(Arc<Events>);
impl ProbeEvents {
    fn silence(&self, count: usize) {
        if let Err(error) = self.0.meter.add_silence(count) {
            self.stopped(Err(error));
        }
    }
    fn new() -> Self {
        Self(Arc::new(Events {
            meter: LoopbackProbeMeter::default(),
            stopped: OnceLock::new(),
            changed: Notify::new(),
        }))
    }
    pub fn data(&self, bytes: &[u8], silent: bool) {
        if let Err(error) = self.0.meter.add(bytes, silent) {
            self.stopped(Err(error));
        }
    }
    pub fn stopped(&self, result: Result<()>) {
        let _ = self.0.stopped.set(result);
        self.0.changed.notify_waiters();
    }
    fn finished(&self) -> bool {
        self.0.stopped.get().is_some()
    }
    async fn wait(&self) -> Result<()> {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.0.stopped.get() {
                return *result;
            }
            changed.await;
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeCapture {
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_ms: u16,
    pub excluded_process_id: u32,
}
pub trait ProbeRecorder: Send {
    fn subscribe(&mut self, events: ProbeEvents) -> Result<()>;
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn close(&mut self) -> ProbeFuture<()>;
}
pub trait ProbeFactory: Send + Sync + 'static {
    fn supported(&self) -> bool;
    fn open(&self, capture: ProbeCapture) -> ProbeFuture<Box<dyn ProbeRecorder>>;
}
struct Completion {
    result: OnceLock<Result<LoopbackProbeResult>>,
    changed: Notify,
}
#[derive(Clone)]
pub struct ProbeOperation(Arc<Completion>);
impl ProbeOperation {
    pub fn finished(&self) -> bool {
        self.0.result.get().is_some()
    }
    pub async fn wait(&self) -> Result<LoopbackProbeResult> {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.0.result.get() {
                return result.clone();
            }
            changed.await;
        }
    }
    fn complete(&self, result: Result<LoopbackProbeResult>) {
        let _ = self.0.result.set(result);
        self.0.changed.notify_waiters();
    }
}
struct Deadline {
    token: CancellationToken,
    timer: JoinHandle<()>,
}
impl Deadline {
    fn new(parent: CancellationToken) -> Self {
        let token = parent.child_token();
        let cancel = token.clone();
        let wait = tokio::time::sleep(Duration::from_secs(12));
        let timer = tokio::spawn(async move {
            tokio::select! {_ = wait=>cancel.cancel(),_ = cancel.cancelled()=>()}
        });
        Self { token, timer }
    }
    fn check(&self) -> Result<()> {
        if self.token.is_cancelled() {
            Err(ProbeError::Cancelled)
        } else {
            Ok(())
        }
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        self.timer.abort();
    }
}
pub struct PhoneLoopbackProbe;
impl PhoneLoopbackProbe {
    /// The operation owns open/run/close. Dropping every observer cancels none
    /// of that work; callers explicitly cancel through the original stop token.
    pub fn start(
        factory: Arc<dyn ProbeFactory>,
        idle: impl Fn() -> bool + Send + Sync + 'static,
        stop: CancellationToken,
    ) -> ProbeOperation {
        let operation = ProbeOperation(Arc::new(Completion {
            result: OnceLock::new(),
            changed: Notify::new(),
        }));
        let beginning = catch_unwind(AssertUnwindSafe(|| {
            if !factory.supported() {
                return Err(ProbeError::Unsupported);
            }
            tokio::runtime::Handle::try_current().map_err(|_| ProbeError::Unavailable)?;
            let deadline = Deadline::new(stop);
            let opening = factory.open(ProbeCapture {
                sample_rate: 48000,
                channels: 2,
                buffer_ms: 40,
                excluded_process_id: std::process::id(),
            });
            Ok((deadline, opening))
        }))
        .unwrap_or(Err(ProbeError::Unavailable));
        match beginning {
            Err(error) => operation.complete(Err(error)),
            Ok((deadline, opening)) => {
                let completed = operation.clone();
                tokio::spawn(async move {
                    let result = tokio::spawn(run(deadline, opening, idle))
                        .await
                        .unwrap_or(Err(ProbeError::Unavailable));
                    completed.complete(result);
                });
            }
        }
        operation
    }
}
async fn run(
    deadline: Deadline,
    opening: ProbeFuture<Box<dyn ProbeRecorder>>,
    idle: impl Fn() -> bool + Send + 'static,
) -> Result<LoopbackProbeResult> {
    // Opening deliberately receives no cancellation token: the original waits
    // for its builder, then checks cancellation and disposes any late recorder.
    let mut recorder = protected_future(opening, ProbeError::Capture).await?;
    let events = ProbeEvents::new();
    let result=async {
        deadline.check()?;
        if !protect(ProbeError::Capture,&idle)?{return Ok(events.0.meter.result("interrupted"));}
        protect(ProbeError::Capture,||recorder.subscribe(events.clone()))??;
        protect(ProbeError::Capture,||recorder.start())??;
        let capture=async {
            let end=Instant::now()+Duration::from_secs(5);let mut state="completed";
            while Instant::now()<end {
                deadline.check()?;
                if !protect(ProbeError::Capture,&idle)?{state="interrupted";break;}
                if events.finished(){events.wait().await?;return Err(ProbeError::StoppedEarly);}
                tokio::select!{biased;_ = deadline.token.cancelled()=>return Err(ProbeError::Cancelled),_ = tokio::time::sleep(Duration::from_millis(50))=>()}
            }
            Ok(state)
        }.await;
        // Stop is the original finally: its error overrides capture failure.
        protect(ProbeError::Capture,||recorder.stop())??;let state=capture?;
        // Task.WaitAsync returns an already completed task before considering
        // cancellation. Preserve that ordering after the stop callback.
        if events.finished(){events.wait().await?;}else{
            deadline.check()?;
            tokio::select!{biased;_ = deadline.token.cancelled()=>return Err(ProbeError::Cancelled),value = tokio::time::timeout(Duration::from_secs(3),events.wait())=>value.map_err(|_|ProbeError::StopTimeout)??}
        }
        Ok(events.0.meter.result(state))
    }.await;
    // Awaited disposal also overrides an earlier result or failure.
    let closing = protect(ProbeError::Cleanup, || recorder.close())?;
    protected_future(closing, ProbeError::Cleanup).await?;
    result
}
fn protect<T>(error: ProbeError, work: impl FnOnce() -> T) -> Result<T> {
    catch_unwind(AssertUnwindSafe(work)).map_err(|_| error)
}
async fn protected_future<T>(mut work: ProbeFuture<T>, error: ProbeError) -> Result<T> {
    std::future::poll_fn(move |cx| {
        protect(error, || work.as_mut().poll(cx)).unwrap_or(std::task::Poll::Ready(Err(error)))
    })
    .await
}
