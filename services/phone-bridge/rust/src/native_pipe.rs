//! Owned native inherited-stdio transport. Pipe observers never own or abort
//! dispatched effects; the original runtime shutdown and process fence do.
use crate::{
    ipc::{FrameError, FrameReader, MAX_RECEIPTS, has_pipe_capacity},
    native_rpc::{
        NativeDispatcher, NativeHandler, NativeInvocation, NativeInvocationStart, NativeOutcome,
        NativeReply, NativeRequest,
    },
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::Mutex as AsyncMutex,
    task::{JoinHandle, JoinSet},
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

pub type NativeShutdown =
    Pin<Box<dyn Future<Output = Result<(), NativeShutdownError>> + Send + 'static>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeShutdownError {
    OutcomeUnknown,
}
pub trait NativeRuntime: NativeHandler {
    /// Return the original owned cleanup. It must acknowledge native resource
    /// quiescence before success; dropping its observer must not release a fence.
    fn shutdown(&self) -> NativeShutdown;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeStopReason {
    Requested,
    Eof,
    InvalidFrame,
    InputFailed,
    OutputFailed,
    HeartbeatLost,
    OwnerFailed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeShutdownStatus {
    Confirmed,
    Failed,
    Unconfirmed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeServerReport {
    pub reason: NativeStopReason,
    pub shutdown: NativeShutdownStatus,
    pub retained_receipts: usize,
}
struct Stop {
    token: CancellationToken,
    reason: Mutex<Option<NativeStopReason>>,
}
impl Stop {
    fn request(&self, reason: NativeStopReason) {
        self.reason.lock().unwrap().get_or_insert(reason);
        self.token.cancel();
    }
}
/// An observer of a separately owned server. A cancelled wait can be resumed;
/// dropping the session requests stop and leaves shutdown running.
pub struct NativePipeSession {
    stop: Arc<Stop>,
    task: Option<JoinHandle<NativeServerReport>>,
    report: Option<NativeServerReport>,
}
impl NativePipeSession {
    pub fn request_stop(&self) {
        self.stop.request(NativeStopReason::Requested);
    }
    pub async fn wait(&mut self) -> NativeServerReport {
        if let Some(report) = self.report {
            return report;
        }
        let report = self
            .task
            .as_mut()
            .unwrap()
            .await
            .unwrap_or(NativeServerReport {
                reason: NativeStopReason::OwnerFailed,
                shutdown: NativeShutdownStatus::Unconfirmed,
                retained_receipts: 0,
            });
        self.task.take();
        self.report = Some(report);
        report
    }
}
impl Drop for NativePipeSession {
    fn drop(&mut self) {
        self.request_stop();
    }
}
struct PipeHandler {
    runtime: Arc<dyn NativeRuntime>,
    heartbeat: Arc<Mutex<Instant>>,
    stop: Arc<Stop>,
}
impl PipeHandler {
    fn control(&self, request: &NativeRequest) -> Option<NativeInvocation> {
        if matches!(request.method(), "heartbeat" | "shutdown") {
            if !request.params().as_object().unwrap().is_empty() {
                return Some(Box::pin(async { NativeOutcome::error("invalid_request") }));
            }
            if request.method() == "shutdown" {
                self.stop.request(NativeStopReason::Requested);
                return Some(Box::pin(async {
                    NativeOutcome::result("{\"stopping\":true}".into())
                }));
            }
            *self.heartbeat.lock().unwrap() = Instant::now();
        }
        None
    }
}
impl NativeHandler for PipeHandler {
    fn start(&self, request: NativeRequest) -> NativeInvocationStart {
        if let Some(action) = self.control(&request) {
            Box::pin(async move { action })
        } else {
            self.runtime.start(request)
        }
    }
    fn cleanup_slots(&self) -> usize {
        self.runtime.cleanup_slots()
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        if let Some(action) = self.control(&request) {
            return action;
        }
        self.runtime.invoke(request)
    }
}

pub struct NativePipeServer;
impl NativePipeServer {
    pub fn start<R, W>(
        epoch: String,
        input: R,
        output: W,
        runtime: Arc<dyn NativeRuntime>,
    ) -> Result<NativePipeSession, FrameError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let stop = Arc::new(Stop {
            token: CancellationToken::new(),
            reason: Mutex::new(None),
        });
        let heartbeat = Arc::new(Mutex::new(Instant::now()));
        let handler = Arc::new(PipeHandler {
            runtime: runtime.clone(),
            heartbeat: heartbeat.clone(),
            stop: stop.clone(),
        });
        let dispatcher = NativeDispatcher::new(epoch, MAX_RECEIPTS, handler)?;
        let owner_stop = stop.clone();
        let task = tokio::spawn(async move {
            run(input, output, runtime, dispatcher, heartbeat, owner_stop).await
        });
        Ok(NativePipeSession {
            stop,
            task: Some(task),
            report: None,
        })
    }
}
async fn write_reply<W: AsyncWrite + Unpin>(
    output: &AsyncMutex<W>,
    reply: NativeReply,
    stop: &Stop,
) -> Result<(), ()> {
    let mut bytes = reply.to_json().map_err(|_| ())?.into_bytes();
    bytes.push(b'\n');
    let result = tokio::select! {
        biased;
        _ = stop.token.cancelled() => Ok(()),
        result = async {
            let mut output = output.lock().await;
            output.write_all(&bytes).await?;
            output.flush().await
        } => result.map_err(|_| ()),
    };
    bytes.fill(0);
    result
}
async fn run<R: AsyncRead + Unpin, W: AsyncWrite + Unpin + Send + 'static>(
    mut input: R,
    output: W,
    runtime: Arc<dyn NativeRuntime>,
    dispatcher: NativeDispatcher,
    heartbeat: Arc<Mutex<Instant>>,
    stop: Arc<Stop>,
) -> NativeServerReport {
    let watch_stop = stop.clone();
    let watchdog = tokio::spawn(async move {
        let mut timer = tokio::time::interval_at(
            Instant::now() + Duration::from_secs(1),
            Duration::from_secs(1),
        );
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = watch_stop.token.cancelled() => return,
                _ = timer.tick() => {
                    let elapsed = Instant::now().checked_duration_since(*heartbeat.lock().unwrap());
                    if elapsed.is_none_or(|elapsed| elapsed >= Duration::from_secs(15)) {
                        watch_stop.request(NativeStopReason::HeartbeatLost);
                        return;
                    }
                }
            }
        }
    });
    let output = Arc::new(AsyncMutex::new(output));
    let mut active = JoinSet::new();
    let mut reader = FrameReader::default();
    let mut buffer = [0; 4096];
    'read: loop {
        let count = tokio::select! {
            biased;
            _ = stop.token.cancelled() => break,
            result = active.join_next(), if !active.is_empty() => {
                if !matches!(result, Some(Ok(Ok(())))) { stop.request(NativeStopReason::OutputFailed); }
                continue;
            }
            result = input.read(&mut buffer) => match result {
                Ok(0) => { stop.request(NativeStopReason::Eof); break; }
                Ok(count) => count,
                Err(_) => { stop.request(NativeStopReason::InputFailed); break; }
            },
        };
        // Admit each preceding valid frame before parsing the next frame in the
        // same OS read, even when that next frame terminates the epoch.
        for part in buffer[..count].split_inclusive(|byte| *byte == b'\n') {
            let mut request = None;
            if reader
                .feed(part, |frame| {
                    request = Some(NativeRequest::parse(frame.as_bytes())?);
                    Ok(())
                })
                .is_err()
            {
                stop.request(NativeStopReason::InvalidFrame);
                break 'read;
            }
            let Some(request) = request else {
                continue;
            };
            while let Some(result) = active.try_join_next() {
                if !matches!(result, Ok(Ok(()))) {
                    stop.request(NativeStopReason::OutputFailed);
                    break 'read;
                }
            }
            if stop.token.is_cancelled() {
                break 'read;
            }
            if !has_pipe_capacity(request.method(), active.len()) {
                let reply = dispatcher.reply(
                    request.request_id(),
                    NativeOutcome::error("capacity_exceeded"),
                );
                if write_reply(&output, reply, &stop).await.is_err() {
                    stop.request(NativeStopReason::OutputFailed);
                    break 'read;
                }
                continue;
            }
            let response = dispatcher.begin(request).await;
            let output = output.clone();
            let stop = stop.clone();
            active.spawn(async move { write_reply(&output, response.await, &stop).await });
        }
        buffer[..count].fill(0);
    }
    dispatcher.stop().await;
    stop.token.cancel();
    let _ = reader.finish(); // Original EOF silently discards a partial frame.
    buffer.fill(0);
    // These are response observers only. Dispatcher invocations keep their own
    // ownership and safe receipt even if their response cannot be written.
    active.abort_all();
    while active.join_next().await.is_some() {}
    let shutdown = tokio::spawn(async move { runtime.shutdown().await });
    let shutdown = match tokio::time::timeout(Duration::from_secs(4), shutdown).await {
        Ok(Ok(Ok(()))) => NativeShutdownStatus::Confirmed,
        Ok(_) => NativeShutdownStatus::Failed,
        Err(_) => NativeShutdownStatus::Unconfirmed, // JoinHandle drop detaches, never aborts cleanup.
    };
    let _ = watchdog.await;
    let reason = stop
        .reason
        .lock()
        .unwrap()
        .unwrap_or(NativeStopReason::Requested);
    NativeServerReport {
        reason,
        shutdown,
        retained_receipts: dispatcher.receipt_count(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeStdioError {
    InvalidEpoch,
    RuntimeUnavailable,
}
/// Entry helper for the dedicated owned native child process only. Its caller
/// owns Windows job/lease validation and process exit. Do not call in a service
/// host: the panic hook is process-wide and intentionally omits payloads.
pub fn run_owned_native_stdio(
    epoch: String,
    runtime: Arc<dyn NativeRuntime>,
) -> Result<NativeServerReport, NativeStdioError> {
    std::panic::set_hook(Box::new(|_| eprintln!("phone_native_panic")));
    run_owned_native_stdio_with(epoch, move || Ok(runtime))
}
pub(crate) fn run_owned_native_stdio_with(
    epoch: String,
    create: impl FnOnce() -> Result<Arc<dyn NativeRuntime>, NativeStdioError>,
) -> Result<NativeServerReport, NativeStdioError> {
    let executor = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| NativeStdioError::RuntimeUnavailable)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        executor.block_on(async {
            let runtime = create()?;
            std::panic::set_hook(Box::new(|_| eprintln!("phone_native_panic")));
            let mut session =
                NativePipeServer::start(epoch, tokio::io::stdin(), tokio::io::stdout(), runtime)
                    .map_err(|_| NativeStdioError::InvalidEpoch)?;
            Ok(session.wait().await)
        })
    }))
    .unwrap_or(Err(NativeStdioError::RuntimeUnavailable));
    // Windows inherited stdin can leave a blocking read pending after lease
    // loss. It must not hold process exit hostage after native cleanup returns.
    executor.shutdown_background();
    result
}
