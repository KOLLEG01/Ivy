//! One retained native thread owns ICE/DTLS/SRTP, sockets and the 20ms clock.
//! Call control only publishes bounded commands or changes the PCM permit.
use super::{
    RealtimeAudio, RealtimeError, RealtimePeer, Result,
    network::{Datagrams, Sockets},
    pcm::Core,
};
use crate::media::{
    AudioOperation, AudioSendHealth, CallAudioError, CallAudioRoute, PcmAudioPort, PcmAudioState,
    PcmSendPort, RtpSendClock,
};
use ivy_contracts::{IvyJson, json::stringify};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct RealtimeOperation(watch::Receiver<Option<Result<()>>>);
impl RealtimeOperation {
    pub(crate) fn channel() -> (watch::Sender<Option<Result<()>>>, Self) {
        let (sender, receiver) = watch::channel(None);
        (sender, Self(receiver))
    }
    pub async fn wait(mut self) -> Result<()> {
        loop {
            if let Some(result) = *self.0.borrow() {
                return result;
            }
            if self.0.changed().await.is_err() {
                return Err(RealtimeError::CleanupUnconfirmed);
            }
        }
    }
}
struct Answer {
    sdp: String,
    result: watch::Sender<Option<Result<()>>>,
}
#[derive(Default)]
struct Control {
    closing: bool,
    answer: Option<Answer>,
}
struct Inner {
    audio: RealtimeAudio,
    control: Mutex<Control>,
    wake: Condvar,
    state: Mutex<&'static str>,
    closed: AudioOperation,
    stop: CancellationToken,
}
impl Inner {
    fn close(&self) {
        // Close PCM synchronously, before waiting for the I/O thread. An old
        // reference can never keep audio authority while cleanup is pending.
        self.audio
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .close();
        self.control
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .closing = true;
        self.wake.notify_all();
    }
    fn closing(&self) -> bool {
        self.stop.is_cancelled()
            || self
                .control
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .closing
    }
    fn state(&self, value: &'static str) {
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = value;
    }
    fn wait(&self, next: Instant) {
        loop {
            let control = self.control.lock().unwrap_or_else(|p| p.into_inner());
            if control.closing || control.answer.is_some() || self.stop.is_cancelled() {
                return;
            }
            let remaining = next.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            let milliseconds = remaining.as_millis().min(10) as u64;
            if milliseconds > 0 {
                drop(
                    self.wake
                        .wait_timeout(control, Duration::from_millis(milliseconds))
                        .unwrap_or_else(|p| p.into_inner()),
                );
                return; // Poll sockets after each interruptible bounded wait.
            }
            drop(control);
            for _ in 0..32 {
                std::hint::spin_loop();
            }
        }
    }
}
pub(crate) type RouteFuture = Pin<Box<dyn Future<Output = Result<Arc<RealtimeRoute>>> + Send>>;
pub(crate) type RouteFactory = dyn Fn(u16, Option<u16>, Arc<dyn Fn() -> bool + Send + Sync>, CancellationToken) -> RouteFuture
    + Send
    + Sync;

/// Starts suspended. Construction is invoked only after original-call admission;
/// dropping any observer cannot cancel preparation, acceptance or thread join.
pub(crate) struct RealtimeRoute {
    inner: Arc<Inner>,
    offer: String,
}
impl RealtimeRoute {
    pub fn open(
        queue_ms: u16,
        prebuffer_ms: Option<u16>,
        permitted: Arc<dyn Fn() -> bool + Send + Sync>,
        stop: CancellationToken,
    ) -> RouteFuture {
        open_with(queue_ms, prebuffer_ms, permitted, stop, Sockets::system)
    }
    pub fn offer(&self) -> &str {
        &self.offer
    }
    pub fn answer(&self, sdp: String) -> Result<RealtimeOperation> {
        if sdp.trim().is_empty() || sdp.encode_utf16().count() > 48_000 {
            return Err(RealtimeError::InvalidRequest);
        }
        if self.failed() {
            return Err(RealtimeError::Failed);
        }
        let (result, operation) = watch::channel(None);
        let mut control = self.inner.control.lock().unwrap_or_else(|p| p.into_inner());
        if control.closing || self.inner.stop.is_cancelled() {
            return Err(RealtimeError::Closed);
        }
        if control.answer.is_some() {
            return Err(RealtimeError::NotReady);
        }
        control.answer = Some(Answer { sdp, result });
        self.inner.wake.notify_all();
        Ok(RealtimeOperation(operation))
    }
    pub fn observation(&self) -> Result<String> {
        let state = *self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut core = self.inner.audio.0.lock().unwrap_or_else(|p| p.into_inner());
        let audio = stringify(&core.status(), crate::ipc::FRAME_BYTES)
            .map_err(|_| RealtimeError::Failed)?;
        let receive = crate::media::native_view::receive_json(&core.receive_status())
            .map_err(|_| RealtimeError::Failed)?;
        let (capture, render) = core.underruns();
        Ok(format!(
            "{{\"state\":\"{}\",\"sentPackets\":{},\"receivedPackets\":{},\"queueMs\":{},\"playbackPrebufferMs\":{},\"receive\":{},\"concealedSamples\":{},\"captureUnderruns\":{},\"renderUnderruns\":{},\"audio\":{}}}",
            state,
            core.sent as i64,
            core.received_packets as i64,
            core.queue_ms,
            core.prebuffer_ms,
            receive,
            core.concealed() as i64,
            capture as i64,
            render as i64,
            audio
        ))
    }
}
impl Drop for RealtimeRoute {
    fn drop(&mut self) {
        self.inner.close();
    }
}
pub(super) fn open_with(
    queue_ms: u16,
    prebuffer_ms: Option<u16>,
    permitted: Arc<dyn Fn() -> bool + Send + Sync>,
    stop: CancellationToken,
    network: impl FnOnce() -> Result<Box<dyn Datagrams>> + Send + 'static,
) -> RouteFuture {
    let core = match Core::new(queue_ms, prebuffer_ms, permitted) {
        Ok(core) => core,
        Err(error) => return Box::pin(async move { Err(error) }),
    };
    let runtime = match tokio::runtime::Handle::try_current() {
        Ok(runtime) => runtime,
        Err(_) => return Box::pin(async { Err(RealtimeError::NotReady) }),
    };
    let inner = Arc::new(Inner {
        audio: RealtimeAudio(Arc::new(Mutex::new(core))),
        control: Mutex::new(Control::default()),
        wake: Condvar::new(),
        state: Mutex::new("new"),
        closed: AudioOperation::new(),
        stop,
    });
    let (ready, observer) = oneshot::channel();
    let worker = inner.clone();
    let thread = std::thread::Builder::new()
        .name("Ivy WebRTC audio".into())
        .spawn(move || {
            let mut ready = Some(ready);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&worker, &mut ready, network)
            }))
            .unwrap_or(Err(RealtimeError::CleanupUnconfirmed));
            if let Some(ready) = ready {
                let _ = ready.send(Err(result.err().unwrap_or(RealtimeError::Failed)));
            }
            worker.close();
            // Queued commands and their original observers always get a result,
            // including a preparation failure or a panic before driver admission.
            if let Some(answer) = worker
                .control
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .answer
                .take()
            {
                answer.result.send_replace(Some(Err(RealtimeError::Closed)));
            }
            result
        });
    match thread {
        Err(_) => {
            inner.close();
            inner.closed.complete(Ok(()));
        }
        Ok(thread) => {
            let completion = inner.clone();
            // Completion means the actual native thread has joined, after all
            // socket/codec/timer destructors. It is independent of every waiter.
            runtime.spawn(async move {
                let result = tokio::task::spawn_blocking(move || thread.join()).await;
                let released = match result {
                    Ok(Ok(Ok(()))) => Ok(()),
                    _ => Err(CallAudioError::CleanupUnconfirmed),
                };
                completion.closed.complete(released);
            });
        }
    }
    Box::pin(async move {
        match observer.await {
            Ok(Ok(route)) => Ok(route),
            outcome => {
                inner.close();
                inner
                    .closed
                    .wait()
                    .await
                    .map_err(|_| RealtimeError::CleanupUnconfirmed)?;
                outcome.unwrap_or(Err(RealtimeError::Failed))
            }
        }
    })
}
fn run(
    inner: &Arc<Inner>,
    ready: &mut Option<oneshot::Sender<Result<Arc<RealtimeRoute>>>>,
    network: impl FnOnce() -> Result<Box<dyn Datagrams>>,
) -> Result<()> {
    let prepared = (|| {
        if inner.closing() {
            return Err(RealtimeError::Cancelled);
        }
        let mut network = network()?;
        let candidates = network.candidates();
        let peer =
            RealtimePeer::with_audio(Instant::now(), &candidates, inner.audio.clone(), &mut |p| {
                network.send(p)
            })?;
        Ok((network, peer))
    })();
    let (mut network, mut peer) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            // All preparation locals have been released before publishing the
            // error; join still confirms the actual thread/timer retirement.
            let _ = ready.take().unwrap().send(Err(error));
            return Ok(());
        }
    };
    let route = Arc::new(RealtimeRoute {
        inner: inner.clone(),
        offer: peer.offer().into(),
    });
    if ready.take().unwrap().send(Ok(route)).is_err() {
        inner.close();
    }
    let driven = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drive(inner, &mut peer, network.as_mut())
    }));
    if !matches!(driven, Ok(Ok(()))) {
        inner
            .audio
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .fail();
        inner.state("failed");
        // Preserve a failed route and its cleanup authority until the original
        // call disposes it. Failure is not successful/implicit route replacement.
        while !inner.closing() {
            if let Some(answer) = inner
                .control
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .answer
                .take()
            {
                answer.result.send_replace(Some(Err(RealtimeError::Failed)));
            }
            inner.wait(Instant::now() + Duration::from_millis(10));
        }
    }
    // A runtime/transport failure ends PCM but does not itself assert a resource
    // leak. Cleanup's own outcome, including graceful close, owns that fence.
    let closed = peer.close(&mut |p| network.send(p));
    inner.state("closed");
    closed
}
fn drive(inner: &Arc<Inner>, peer: &mut RealtimePeer, network: &mut dyn Datagrams) -> Result<()> {
    let _timer = crate::media::sender::platform::enter().map_err(|_| RealtimeError::Failed)?;
    let origin = Instant::now();
    let mut clock = RtpSendClock::new(Duration::ZERO).map_err(|_| RealtimeError::Failed)?;
    let mut answer: Option<Answer> = None;
    // Stack storage has no unbounded receive queue; plaintext is zeroed after
    // every engine input and on every exit, including unwinding.
    struct Buffer([u8; 65_536]);
    impl Drop for Buffer {
        fn drop(&mut self) {
            self.0.fill(0);
        }
    }
    let mut buffer = Buffer([0; 65_536]);
    let result = (|| {
        while !inner.closing() {
            let command = inner
                .control
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .answer
                .take();
            if let Some(command) = command {
                if answer.is_some() {
                    command
                        .result
                        .send_replace(Some(Err(RealtimeError::NotReady)));
                } else {
                    match peer.answer(Instant::now(), &command.sdp, &mut |p| network.send(p)) {
                        Ok(()) => answer = Some(command),
                        Err(error) => {
                            command.result.send_replace(Some(Err(error)));
                        }
                    }
                }
            }
            for _ in 0..32 {
                if inner.closing() {
                    break;
                }
                let Some((length, source, destination)) = network.receive(&mut buffer.0)? else {
                    break;
                };
                let received = peer.receive(
                    Instant::now(),
                    source,
                    destination,
                    &buffer.0[..length],
                    &mut |p| network.send(p),
                );
                buffer.0[..length].fill(0);
                received?;
            }
            let now = Instant::now();
            if now >= peer.next_timeout() {
                peer.timeout(now, &mut |p| network.send(p))?;
            }
            if clock
                .due(origin.elapsed())
                .map_err(|_| RealtimeError::Failed)?
            {
                peer.send_frame(Instant::now(), &mut |p| network.send(p))?;
                clock
                    .advance(origin.elapsed())
                    .map_err(|_| RealtimeError::Failed)?;
            }
            inner.state(peer.connection_state());
            if let Some(pending) = &answer {
                match peer.connection_result(Instant::now()) {
                    Ok(false) => {}
                    result => {
                        pending.result.send_replace(Some(result.map(|_| ())));
                        answer = None;
                    }
                }
            }
            let next = (origin + clock.next()).min(peer.next_timeout());
            inner.wait(next);
        }
        Ok(())
    })();
    if let Some(answer) = answer {
        answer
            .result
            .send_replace(Some(Err(result.err().unwrap_or(RealtimeError::Closed))));
    }
    result
}
fn route_error(error: RealtimeError) -> CallAudioError {
    match error {
        RealtimeError::InvalidRequest => CallAudioError::InvalidRequest,
        RealtimeError::CleanupUnconfirmed => CallAudioError::CleanupUnconfirmed,
        _ => CallAudioError::RouteFailed,
    }
}
impl PcmAudioPort for RealtimeRoute {
    fn state(&self) -> PcmAudioState {
        self.inner.audio.state()
    }
    fn read_captured(&self, output: &mut [f32], generation: u64) -> usize {
        self.inner.audio.read_captured(output, generation)
    }
    fn write_received(&self, input: &[f32], generation: u64) -> bool {
        self.inner.audio.write_received(input, generation)
    }
}
impl PcmSendPort for RealtimeRoute {
    fn with_send_state(&self, state: PcmAudioState, action: &mut dyn FnMut()) -> bool {
        self.inner.audio.with_send_state(state, action)
    }
    fn send_health(&self) -> AudioSendHealth {
        self.inner.audio.send_health()
    }
}
impl CallAudioRoute for RealtimeRoute {
    fn status(&self) -> IvyJson {
        self.inner.audio.status()
    }
    fn failed(&self) -> bool {
        self.inner
            .audio
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .failed
    }
    fn resume(&self) -> std::result::Result<(), CallAudioError> {
        self.inner.audio.resume().map_err(route_error)
    }
    fn suspend(&self) -> std::result::Result<(), CallAudioError> {
        self.inner.audio.suspend();
        Ok(())
    }
    fn close(
        &self,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), CallAudioError>> + Send + '_>> {
        self.inner.close();
        let work = self.inner.closed.clone();
        Box::pin(async move { work.wait().await })
    }
}

#[cfg(test)]
pub(super) fn bound(
    queue_ms: u16,
    prebuffer: Option<u16>,
    permitted: Arc<dyn Fn() -> bool + Send + Sync>,
    stop: CancellationToken,
    addresses: Vec<std::net::SocketAddr>,
) -> RouteFuture {
    open_with(queue_ms, prebuffer, permitted, stop, move || {
        Sockets::bind(addresses).map(|s| Box::new(s) as Box<dyn Datagrams>)
    })
}
