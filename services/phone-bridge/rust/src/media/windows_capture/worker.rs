//! The call recorder's one native thread and retained close. Observers own no
//! COM interfaces; losing an open/close future never cancels its cleanup.
use super::{CaptureError, CaptureStream, Result, device};
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
type Factory = Box<dyn FnOnce() -> Result<Box<dyn CaptureStream>> + Send>;
type AdmissionFactory = Box<dyn FnOnce() -> Result<Box<dyn Admission>> + Send>;
type GuardFactory = Box<dyn FnOnce() -> Result<Guard> + Send>;
pub(in crate::media) type Completion = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
pub(in crate::media) type Opening = Pin<Box<dyn Future<Output = Result<Recorder>> + Send>>;
trait Admission {
    fn configure(&mut self) -> Result<Box<dyn CaptureStream>>;
    fn retire(&mut self) {}
}
struct Ready(Option<Box<dyn CaptureStream>>);
impl Admission for Ready {
    fn configure(&mut self) -> Result<Box<dyn CaptureStream>> {
        self.0.take().ok_or(CaptureError::Start)
    }
}
impl Admission for device::Admission {
    fn configure(&mut self) -> Result<Box<dyn CaptureStream>> {
        device::Admission::configure(self)
    }
    fn retire(&mut self) {
        device::Admission::retire(self);
    }
}
struct State {
    stop: AtomicBool,
    closed: AtomicBool,
    outcome: OnceLock<Result<()>>,
    joined: OnceLock<Result<()>>,
    capture: OnceLock<Result<()>>,
    configuration: OnceLock<Result<()>>,
    retired: OnceLock<Result<()>>,
    changed: Notify,
    buffers: Buffers,
}
impl State {
    async fn wait_configuration(&self) -> Result<()> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.closed.load(Ordering::Acquire) {
                return Err(CaptureError::Start);
            }
            if let Some(result) = self.configuration.get() {
                return *result;
            }
            if self.outcome.get().is_some() {
                return Err(CaptureError::Cleanup);
            }
            changed.await;
        }
    }
    async fn wait_retired(&self) -> Result<()> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.retired.get() {
                return *result;
            }
            if self.outcome.get().is_some() {
                return Err(CaptureError::Cleanup);
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
        let _ = self.capture.set(result);
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
            .map_err(|_| CaptureError::Cleanup)
            .and_then(|()| {
                state
                    .outcome
                    .get()
                    .copied()
                    .unwrap_or(Err(CaptureError::Cleanup))
            });
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
pub(in crate::media) struct Recorder {
    state: Arc<State>,
    control: Mutex<Control>,
    runtime: tokio::runtime::Handle,
}
impl Recorder {
    pub fn admit_route(
        settings: &AudioSettings,
        desktop: Option<AudioDesktopIdentity>,
        buffers: Buffers,
    ) -> Opening {
        let request = match device::Request::new(settings, desktop.as_ref()) {
            Ok(request) => request,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Self::open_admitted_with(
            Box::new(move || Ok(Box::new(device::admit(&request)?) as Box<dyn Admission>)),
            desktop_guard(desktop),
            buffers,
            false,
        )
    }
    /// The source and desktop checks run on the future recorder thread. The
    /// full route must validate its render/capture pair and own the same buffer
    /// lock before calling this capture-only constructor.
    pub(super) fn open(request: device::Request, buffers: Buffers) -> Opening {
        let desktop = request.desktop.clone();
        let guard = desktop_guard(desktop);
        Self::open_with(Box::new(move || device::open(&request)), guard, buffers)
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
            return Box::pin(async { Err(CaptureError::Unavailable) });
        };
        let state = Arc::new(State {
            stop: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            outcome: OnceLock::new(),
            joined: OnceLock::new(),
            capture: OnceLock::new(),
            configuration: OnceLock::new(),
            retired: OnceLock::new(),
            changed: Notify::new(),
            buffers,
        });
        let (commands, receiver) = mpsc::channel();
        let (ready, opening) = oneshot::channel();
        let worker_state = state.clone();
        let thread = std::thread::Builder::new()
            .name("phone-call-capture".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    worker(factory, guard, &worker_state, receiver, ready, immediate)
                }))
                .unwrap_or(Err(CaptureError::Cleanup));
                let _ = worker_state.outcome.set(result);
                worker_state.changed.notify_waiters();
            });
        let recorder = thread.map(|thread| Self {
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
        // Own the recorder before constructing the observer. An unpolled or
        // abandoned open drops it, whose prefix signals close and starts join.
        Box::pin(async move {
            let recorder = recorder.map_err(|_| CaptureError::Unavailable)?;
            if let Err(error) = opening.await.unwrap_or(Err(CaptureError::Cleanup)) {
                recorder.close().await?;
                return Err(error);
            }
            Ok(recorder)
        })
    }
    pub fn configure(&self) -> Completion {
        let mut control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if self.state.closed.load(Ordering::Acquire) || self.state.stop.load(Ordering::Acquire) {
            return Box::pin(async { Err(CaptureError::Start) });
        }
        if !control.configuring {
            control.configuring = true;
            if control.commands.send(Command::Configure).is_err() {
                let _ = self.state.configuration.set(Err(CaptureError::Cleanup));
                self.state.changed.notify_waiters();
            }
        }
        let state = self.state.clone();
        Box::pin(async move { state.wait_configuration().await })
    }
    pub fn start(&self) -> Result<()> {
        let mut control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if control.started
            || self.state.configuration.get() != Some(&Ok(()))
            || self.state.closed.load(Ordering::Acquire)
            || self.state.stop.load(Ordering::Acquire)
        {
            return Err(CaptureError::Start);
        }
        control.started = true;
        let (tx, rx) = mpsc::sync_channel(1);
        control
            .commands
            .send(Command::Start(tx))
            .map_err(|_| CaptureError::Start)?;
        rx.recv().map_err(|_| CaptureError::Start)?
    }
    pub fn stop(&self) {
        self.state.stop.store(true, Ordering::Release);
    }
    pub fn retire(&self) -> Completion {
        self.stop();
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
        self.stop();
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
impl Drop for Recorder {
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
            return Err(CaptureError::Owner);
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
                        return Err(CaptureError::Owner);
                    }
                    Ok(candidate)
                }))
                .unwrap_or(Err(CaptureError::Cleanup));
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
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        stream.run(&state.stop, &mut |bytes, flags, _, _| {
                            state
                                .buffers
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .captured(bytes, flags);
                            Ok(())
                        })
                    }))
                    .unwrap_or(Err(CaptureError::Callback));
                    state.stopped(result);
                } else {
                    let _ = reply.send(Err(CaptureError::Start));
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
    stream: &mut Option<Box<dyn CaptureStream>>,
    admission: &mut dyn Admission,
    state: &State,
) {
    if state.retired.get().is_some() {
        return;
    }
    // A failing stream destructor must not prevent retiring partial clients or
    // discard the endpoint owner before the route has attempted mute cleanup.
    let stream_result =
        catch_unwind(AssertUnwindSafe(|| drop(stream.take()))).map_err(|_| CaptureError::Cleanup);
    let partial_result =
        catch_unwind(AssertUnwindSafe(|| admission.retire())).map_err(|_| CaptureError::Cleanup);
    let result = stream_result.and(partial_result).and(
        if state.configuration.get() == Some(&Err(CaptureError::Cleanup)) {
            Err(CaptureError::Cleanup)
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
            .map_err(|_| CaptureError::Owner)?;
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
        "/../../../tests/rust/phone_windows_capture_owner.rs"
    ));
}
