//! The render stream's retained native thread. Futures observe opening/closing;
//! they never own COM resources or cancel disposal by being dropped.
use super::{
    RenderError, Result,
    device::{self, Info, StopSignal},
    status::Status,
};
use crate::{
    desktop::{DesktopOwner, WindowsDesktopProcessOwner},
    media::{AudioDesktopIdentity, AudioSettings, windows_audio::WindowsAudioBuffers},
};
use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
use tokio::sync::{Notify, oneshot};
type Buffers = Arc<Mutex<WindowsAudioBuffers>>;
type Guard = Box<dyn FnMut() -> bool>;
type GuardFactory = Box<dyn FnOnce() -> Result<Guard> + Send>;
type Factory = Box<dyn FnOnce() -> Result<Box<dyn Stream>> + Send>;
type AdmissionFactory = Box<dyn FnOnce() -> Result<Box<dyn Admission>> + Send>;
pub(in crate::media) type Completion = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
pub(in crate::media) type Opening = Pin<Box<dyn Future<Output = Result<Player>> + Send>>;
trait Admission {
    fn configure(&mut self) -> Result<Box<dyn Stream>>;
    fn retire(&mut self) {}
}
struct Ready(Option<Box<dyn Stream>>);
impl Admission for Ready {
    fn configure(&mut self) -> Result<Box<dyn Stream>> {
        self.0.take().ok_or(RenderError::Start)
    }
}
impl Admission for device::Admission {
    fn configure(&mut self) -> Result<Box<dyn Stream>> {
        Ok(Box::new(device::Admission::configure(self)?))
    }
    fn retire(&mut self) {
        device::Admission::retire(self);
    }
}
trait Stream {
    fn info(&self) -> &Info;
    fn stop_signal(&self) -> StopSignal;
    fn run(&mut self, stop: &AtomicBool, buffers: &Mutex<WindowsAudioBuffers>) -> Result<()>;
}
impl Stream for device::NativeStream {
    fn info(&self) -> &Info {
        &self.info
    }
    fn stop_signal(&self) -> StopSignal {
        self.stop_signal()
    }
    fn run(&mut self, stop: &AtomicBool, buffers: &Mutex<WindowsAudioBuffers>) -> Result<()> {
        self.run(stop, buffers)
    }
}
struct State {
    stop: AtomicBool,
    closed: AtomicBool,
    outcome: OnceLock<Result<()>>,
    joined: OnceLock<Result<()>>,
    render: OnceLock<Result<()>>,
    signal_error: OnceLock<RenderError>,
    info: OnceLock<Status>,
    configuration: OnceLock<Result<()>>,
    retired: OnceLock<Result<()>>,
    signal: OnceLock<StopSignal>,
    changed: Notify,
    buffers: Buffers,
}
impl State {
    async fn wait_retired(&self) -> Result<()> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.retired.get() {
                return result.and_then(|()| self.signal_error.get().copied().map_or(Ok(()), Err));
            }
            if self.outcome.get().is_some() {
                return Err(RenderError::Cleanup);
            }
            changed.await;
        }
    }
    async fn wait_configuration(&self) -> Result<()> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.closed.load(Ordering::Acquire) {
                return Err(RenderError::Start);
            }
            if let Some(result) = self.configuration.get() {
                return *result;
            }
            if self.outcome.get().is_some() {
                return Err(RenderError::Cleanup);
            }
            changed.await;
        }
    }
    async fn wait(&self) -> Result<()> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.joined.get() {
                return *result;
            }
            changed.await;
        }
    }
    fn stopped(&self, result: Result<()>) {
        let _ = self.render.set(result);
        self.buffers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .fail();
        self.changed.notify_waiters();
    }
}
enum Command {
    Configure,
    Start(mpsc::SyncSender<Result<()>>),
    Retire,
    Close,
}
fn join_worker(
    runtime: &tokio::runtime::Handle,
    thread: std::thread::JoinHandle<()>,
    state: Arc<State>,
) {
    runtime.spawn_blocking(move || {
        let result = thread
            .join()
            .map_err(|_| RenderError::Cleanup)
            .and_then(|()| {
                state
                    .outcome
                    .get()
                    .copied()
                    .unwrap_or(Err(RenderError::Cleanup))
            })
            .and_then(|()| state.signal_error.get().copied().map_or(Ok(()), Err));
        let _ = state.joined.set(result);
        state.changed.notify_waiters();
    });
}
struct Control {
    commands: mpsc::Sender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
    started: bool,
    configuring: bool,
    retiring: bool,
}
pub(in crate::media) struct Player {
    state: Arc<State>,
    control: Mutex<Control>,
    runtime: tokio::runtime::Handle,
}
impl Player {
    pub fn admit_route(
        settings: &AudioSettings,
        desktop: Option<AudioDesktopIdentity>,
        buffers: Buffers,
    ) -> Opening {
        match device::Request::new(settings) {
            Ok(request) => {
                Self::admit(request, settings.capture_endpoint.clone(), desktop, buffers)
            }
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
    /// Admit the configured pair before the route starts its optional mute
    /// guard. Configuration later consumes the same retained native device.
    pub(super) fn admit(
        request: device::Request,
        capture_endpoint: String,
        desktop: Option<AudioDesktopIdentity>,
        buffers: Buffers,
    ) -> Opening {
        Self::open_admitted_with(
            Box::new(move || {
                Ok(Box::new(device::admit(&request, &capture_endpoint)?) as Box<dyn Admission>)
            }),
            desktop_guard(desktop),
            buffers,
            false,
        )
    }
    /// Full route admission still owns endpoint-pair/version/cancellation/mute
    /// checks. Desktop guards bracket this render-only native construction.
    pub(super) fn open(
        request: device::Request,
        desktop: Option<AudioDesktopIdentity>,
        buffers: Buffers,
    ) -> Opening {
        Self::open_with(
            Box::new(move || Ok(Box::new(device::open(&request)?) as Box<dyn Stream>)),
            desktop_guard(desktop),
            buffers,
        )
    }
    fn open_with(factory: Factory, guard: GuardFactory, buffers: Buffers) -> Opening {
        Self::open_admitted_with(
            Box::new(move || Ok(Box::new(Ready(Some(factory()?))) as Box<dyn Admission>)),
            guard,
            buffers,
            true,
        )
    }
    fn open_admitted_with(
        factory: AdmissionFactory,
        guard: GuardFactory,
        buffers: Buffers,
        immediate: bool,
    ) -> Opening {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Box::pin(async { Err(RenderError::Unavailable) });
        };
        let state = Arc::new(State {
            stop: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            outcome: OnceLock::new(),
            joined: OnceLock::new(),
            render: OnceLock::new(),
            signal_error: OnceLock::new(),
            info: OnceLock::new(),
            configuration: OnceLock::new(),
            retired: OnceLock::new(),
            signal: OnceLock::new(),
            changed: Notify::new(),
            buffers,
        });
        let (commands, receiver) = mpsc::channel();
        let (ready, opening) = oneshot::channel();
        let worker_state = state.clone();
        let thread = std::thread::Builder::new()
            .name("phone-call-render".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    worker(factory, guard, &worker_state, receiver, ready, immediate)
                }))
                .unwrap_or(Err(RenderError::Cleanup));
                let _ = worker_state.outcome.set(result);
                worker_state.changed.notify_waiters();
            });
        let player = thread.map(|thread| Self {
            state,
            control: Mutex::new(Control {
                commands,
                thread: Some(thread),
                started: false,
                configuring: immediate,
                retiring: false,
            }),
            runtime,
        });
        // Own Player before returning the observer, including an unpolled open.
        Box::pin(async move {
            let player = player.map_err(|_| RenderError::Unavailable)?;
            match opening.await.unwrap_or(Err(RenderError::Cleanup)) {
                Ok(()) => Ok(player),
                Err(error) => {
                    player.close().await?;
                    Err(error)
                }
            }
        })
    }
    /// Starts configuration once. Losing this observer does not cancel the
    /// native operation; close still joins it and disposes the admitted device.
    pub fn configure(&self) -> Completion {
        let mut control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if self.state.closed.load(Ordering::Acquire) || self.state.stop.load(Ordering::Acquire) {
            return Box::pin(async { Err(RenderError::Start) });
        }
        if !control.configuring {
            control.configuring = true;
            if control.commands.send(Command::Configure).is_err() {
                let _ = self.state.configuration.set(Err(RenderError::Cleanup));
                self.state.changed.notify_waiters();
            }
        }
        let state = self.state.clone();
        Box::pin(async move { state.wait_configuration().await })
    }
    pub fn status(&self) -> Status {
        self.state
            .info
            .get()
            .expect("prepared render status")
            .clone()
    }
    pub fn start(&self) -> Result<()> {
        let mut control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if control.started
            || self.state.configuration.get() != Some(&Ok(()))
            || self.state.closed.load(Ordering::Acquire)
            || self.state.stop.load(Ordering::Acquire)
        {
            return Err(RenderError::Start);
        }
        control.started = true;
        let (tx, rx) = mpsc::sync_channel(1);
        control
            .commands
            .send(Command::Start(tx))
            .map_err(|_| RenderError::Start)?;
        rx.recv().map_err(|_| RenderError::Start)?
    }
    pub fn stop(&self) -> Result<()> {
        self.state.stop.store(true, Ordering::Release);
        let _control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if self.state.closed.load(Ordering::Acquire) {
            return self.state.signal_error.get().copied().map_or(Ok(()), Err);
        }
        // Record signal failure while serialized with taking the join owner.
        // Close cannot publish success before an in-flight stop records failure.
        let result = self.state.signal.get().map_or(Ok(()), |s| s.set());
        if let Err(error) = result {
            let _ = self.state.signal_error.set(error);
        }
        result
    }
    /// Retire clients and events, keeping endpoint ownership on the native
    /// thread until close. This completion is not proof of final thread exit.
    pub fn retire(&self) -> Completion {
        let _ = self.stop();
        self.state
            .buffers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .close();
        let mut control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if !control.retiring && !self.state.closed.load(Ordering::Acquire) {
            control.retiring = true;
            let _ = control.commands.send(Command::Retire);
        }
        let state = self.state.clone();
        Box::pin(async move { state.wait_retired().await })
    }
    fn signal_close(&self) {
        // Failure to signal never discards the native join. The pump also sees
        // stop at timeout; a failed signal remains an explicit close failure.
        let _ = self.stop();
        self.state
            .buffers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .close();
        let mut control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if !self.state.closed.swap(true, Ordering::AcqRel) {
            let _ = control.commands.send(Command::Close);
        }
        if let Some(thread) = control.thread.take() {
            join_worker(&self.runtime, thread, self.state.clone());
        }
    }
    pub fn close(&self) -> Completion {
        self.signal_close();
        let state = self.state.clone();
        Box::pin(async move { state.wait().await })
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        self.signal_close();
    }
}
fn worker(
    factory: AdmissionFactory,
    guard: GuardFactory,
    state: &State,
    commands: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<()>>,
    immediate: bool,
) -> Result<()> {
    let opened = (|| {
        let mut guard = guard()?;
        if !catch_unwind(AssertUnwindSafe(&mut guard)).unwrap_or(false) {
            return Err(RenderError::Owner);
        }
        let admission = factory()?;
        Ok((admission, guard))
    })();
    let (mut admission, mut guard) = match opened {
        Ok(value) => value,
        Err(error) => {
            let _ = ready.send(Err(error));
            return Ok(());
        }
    };
    let mut ready = Some(ready);
    let mut stream = None;
    let mut next = if immediate {
        Some(Command::Configure)
    } else {
        if ready.take().unwrap().send(Ok(())).is_err() {
            return Ok(());
        }
        None
    };
    loop {
        let command = next
            .take()
            .or_else(|| commands.recv().ok())
            .unwrap_or(Command::Close);
        match command {
            Command::Configure => {
                let configured = catch_unwind(AssertUnwindSafe(|| {
                    let candidate = admission.configure()?;
                    if !catch_unwind(AssertUnwindSafe(&mut guard)).unwrap_or(false) {
                        return Err(RenderError::Owner);
                    }
                    let info = Status::new(candidate.info())?;
                    let _ = state.info.set(info);
                    let _ = state.signal.set(candidate.stop_signal());
                    Ok(candidate)
                }))
                .unwrap_or(Err(RenderError::Cleanup));
                let result = match configured {
                    Ok(value) => {
                        stream = Some(value);
                        Ok(())
                    }
                    Err(error) => Err(error),
                };
                let _ = state.configuration.set(result);
                state.changed.notify_waiters();
                if let Some(ready) = ready.take() {
                    if ready.send(result).is_err() {
                        break;
                    }
                }
            }
            Command::Start(reply) => {
                if let Some(stream) = stream.as_mut() {
                    let _ = reply.send(Ok(()));
                    let result =
                        catch_unwind(AssertUnwindSafe(|| stream.run(&state.stop, &state.buffers)))
                            .unwrap_or(Err(RenderError::Callback));
                    state.stopped(result);
                } else {
                    let _ = reply.send(Err(RenderError::Start));
                }
            }
            Command::Retire => retire_stream(&mut stream, admission.as_mut(), state),
            Command::Close => break,
        }
    }
    retire_stream(&mut stream, admission.as_mut(), state);
    *state.retired.get().unwrap()
}
fn retire_stream(
    stream: &mut Option<Box<dyn Stream>>,
    admission: &mut dyn Admission,
    state: &State,
) {
    if state.retired.get().is_some() {
        return;
    }
    // A failing stream destructor must not prevent retiring partial clients or
    // discard the endpoint owner before the route has attempted mute cleanup.
    let stream_result =
        catch_unwind(AssertUnwindSafe(|| drop(stream.take()))).map_err(|_| RenderError::Cleanup);
    let partial_result =
        catch_unwind(AssertUnwindSafe(|| admission.retire())).map_err(|_| RenderError::Cleanup);
    let result = stream_result.and(partial_result).and(
        if state.configuration.get() == Some(&Err(RenderError::Cleanup)) {
            Err(RenderError::Cleanup)
        } else {
            Ok(())
        },
    );
    let _ = state.retired.set(result);
    state.changed.notify_waiters();
}

fn desktop_guard(desktop: Option<AudioDesktopIdentity>) -> GuardFactory {
    Box::new(move || {
        if let Some(desktop) = desktop {
            let mut owner = WindowsDesktopProcessOwner::new(
                desktop.pid(),
                desktop.creation_ticks(),
                desktop.image(),
            )
            .map_err(|_| RenderError::Owner)?;
            Ok(Box::new(move || owner.is_current().unwrap_or(false)) as Guard)
        } else {
            Ok(Box::new(|| true) as Guard)
        }
    })
}
#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_windows_render_owner.rs"
    ));
}
