//! Private, versioned transport to the pinned official SDK. This layer owns
//! only child/pipe/call lifetimes; SDK and native policy stay in their owners.
use super::{
    error::{AdapterError, Result},
    value::{JsonScalar, decode, field},
};
use ivy_contracts::{
    IvyJson as J,
    json::{stringify_into, stringify_size},
};
use ivy_host_runtime::process::{self, OwnedProcess, owned_process_tree_stopped};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
};

pub const VERSION: f64 = 2.0;
const MAXIMUM_BYTES: usize = 32 * 1024 * 1024;
pub const EVENT_WINDOW: usize = 9;
const DATA_EVENT_BYTES: usize = 48 * 1024 * 1024;
const CONTROL_EVENT_BYTES: usize = 2 * 1024 * 1024;
const REPLY_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_REPLY_BYTES: usize = 48 * 1024 * 1024;
const COMMAND_BYTES: usize = 64 * 1024 * 1024;
const CONTROL_BYTES: usize = 8 * 1024 * 1024;
const CALLS: usize = 64;
const CONTROL_CALLS: usize = 8;

type Reservation = (OwnedSemaphorePermit, Option<OwnedSemaphorePermit>);
fn capacity() -> AdapterError {
    AdapterError::typed(
        "native_capacity",
        "SDK transport count or retained-memory capacity is occupied",
    )
}
fn reserve(
    all: &Arc<Semaphore>,
    data: &Arc<Semaphore>,
    amount: usize,
    control: bool,
) -> Result<Reservation> {
    let amount = u32::try_from(amount).map_err(|_| capacity())?;
    let all = all
        .clone()
        .try_acquire_many_owned(amount)
        .map_err(|_| capacity())?;
    let data = if control {
        None
    } else {
        Some(
            data.clone()
                .try_acquire_many_owned(amount)
                .map_err(|_| capacity())?,
        )
    };
    Ok((all, data))
}
/// Node's module/SDK path parsers require DOS/UNC spelling, including when
/// Rust admitted the same file through a canonical Windows namespace path.
pub(super) fn sdk_path(path: &std::path::Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::{
            os::windows::ffi::{OsStrExt, OsStringExt},
            path::{Component, Prefix},
        };
        let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if let Some(Component::Prefix(prefix)) = path.components().next() {
            match prefix.kind() {
                Prefix::VerbatimDisk(_) => return PathBuf::from(OsString::from_wide(&units[4..])),
                Prefix::VerbatimUNC(_, _) => {
                    return PathBuf::from(OsString::from_wide(
                        &[&[92, 92][..], &units[8..]].concat(),
                    ));
                }
                _ => {}
            }
        }
    }
    path.to_path_buf()
}
pub struct BridgeLaunch {
    pub node_executable: PathBuf,
    pub entry: PathBuf,
    pub cwd: PathBuf,
    /// Complete, already admitted environment; no ambient credential fallback.
    pub environment: BTreeMap<OsString, OsString>,
    pub job_launcher: Option<PathBuf>,
}
struct CommandMessage {
    bytes: Vec<u8>,
    // Charged before serialization; held through the actual pipe write.
    _bytes: Option<Reservation>,
}
struct EventCredit {
    sequence: u64,
    acknowledgements: mpsc::Sender<u64>,
    stop_writer: watch::Sender<bool>,
    slot: Option<OwnedSemaphorePermit>,
}
impl Drop for EventCredit {
    fn drop(&mut self) {
        drop(self.slot.take());
        if self.acknowledgements.try_send(self.sequence).is_err() {
            self.stop_writer.send_replace(true);
        }
    }
}
/// The original SDK frame owns its credit until the final consumer finishes.
pub struct SdkEvent {
    value: J,
    _credit: Option<EventCredit>,
}
impl SdkEvent {
    pub(super) fn take_input(&mut self) -> J {
        self.value.remove("input").unwrap_or(J::Null)
    }
}
impl std::ops::Deref for SdkEvent {
    type Target = J;
    fn deref(&self) -> &J {
        &self.value
    }
}
fn event(
    value: J,
    slots: &[Arc<Semaphore>; 2],
    last: &mut [u64; 2],
    acknowledgements: &mpsc::Sender<u64>,
    stop_writer: &watch::Sender<bool>,
    events: &mpsc::Sender<SdkEvent>,
) -> Result<()> {
    let lane = match field(&value, "type").as_str().ok().as_deref() {
        Some("message") => 0,
        Some("permission" | "callbackCancelled" | "end") => 1,
        _ => return Err(AdapterError::new("Invalid SDK event type")),
    };
    let sequence = field(&value, "eventSequence")
        .as_f64()
        .filter(|n| super::value::safe_integer(*n) && *n > 0.0)
        .ok_or_else(|| AdapterError::new("Invalid SDK event credit"))? as u64;
    if sequence <= last[lane] {
        return Err(AdapterError::new("Repeated SDK event credit"));
    }
    let size = value
        .retained_bytes()
        .saturating_add(std::mem::size_of::<SdkEvent>())
        .saturating_add(256);
    if size
        > if lane == 0 {
            DATA_EVENT_BYTES
        } else {
            CONTROL_EVENT_BYTES
        }
    {
        return Err(capacity());
    }
    let slot = slots[lane]
        .clone()
        .try_acquire_owned()
        .map_err(|_| capacity())?;
    let permit = events
        .try_reserve()
        .map_err(|_| AdapterError::new("SDK event window is unavailable"))?;
    last[lane] = sequence;
    permit.send(SdkEvent {
        value,
        _credit: Some(EventCredit {
            sequence,
            acknowledgements: acknowledgements.clone(),
            stop_writer: stop_writer.clone(),
            slot: Some(slot),
        }),
    });
    Ok(())
}
/// Successful original replies retain byte/count admission through consumption.
#[derive(Debug)]
pub struct SdkReply {
    pub(super) value: J,
    _bytes: Reservation,
    _calls: Reservation,
}
impl std::ops::Deref for SdkReply {
    type Target = J;
    fn deref(&self) -> &J {
        &self.value
    }
}
impl PartialEq<J> for SdkReply {
    fn eq(&self, other: &J) -> bool {
        self.value == *other
    }
}
// Keep error results charged too while their original caller has not polled.
type Reply = (Result<J>, Reservation, Reservation);
type Pending = Arc<Mutex<BTreeMap<u64, (oneshot::Sender<Result<Reply>>, Reservation)>>>;
#[derive(Clone)]
pub struct Bridge {
    sender: mpsc::Sender<CommandMessage>,
    next: Arc<AtomicU64>,
    pending: Pending,
    stops: watch::Sender<bool>,
    completion: watch::Receiver<Option<Result<()>>>,
    closed: Arc<AtomicBool>,
    process: Arc<OwnedProcess>,
    calls: Arc<Semaphore>,
    data_calls: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    data_bytes: Arc<Semaphore>,
}
impl Bridge {
    pub async fn spawn(
        launch: BridgeLaunch,
        diagnostic: Arc<dyn Fn(&[u8]) + Send + Sync>,
    ) -> Result<(Self, mpsc::Receiver<SdkEvent>)> {
        let mut child = process::spawn_exact_environment(
            &process::Command {
                executable: "claude-sdk".into(),
                args: vec![sdk_path(&launch.entry).into_os_string()],
                cwd: Some(launch.cwd.clone()),
                timeout_ms: 30_000,
            },
            &launch.cwd,
            &BTreeMap::from([("claude-sdk".into(), launch.node_executable)]),
            process::LaunchContext {
                environment: launch.environment,
                job_launcher: launch.job_launcher,
                raw_output: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| AdapterError::typed("native_process_error", e.message_js()))?;
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let mut errors = child.stderr.take().unwrap();
        let child = Arc::new(child);
        let process = child.clone();
        let (sender, mut commands) = mpsc::channel::<CommandMessage>(CALLS);
        let (stops, mut stop_receiver) = watch::channel(false);
        // Keep the public stop sender exclusively in Bridge handles: dropping
        // their last owner must still wake the reader and retire a parked writer.
        let (stop_writer, mut write_stop) = watch::channel(false);
        let (completed, completion) = watch::channel(None);
        let (events, event_receiver) = mpsc::channel(EVENT_WINDOW);
        let (acknowledgements, mut credits) = mpsc::channel::<u64>(EVENT_WINDOW);
        let event_slots = [Arc::new(Semaphore::new(1)), Arc::new(Semaphore::new(8))];
        let reply_bytes = Arc::new(Semaphore::new(REPLY_BYTES));
        let data_reply_bytes = Arc::new(Semaphore::new(REPLY_BYTES - CONTROL_BYTES));
        let pending: Pending = Arc::new(Mutex::new(BTreeMap::new()));
        let waiting = pending.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let exited = closed.clone();
        let diagnostics = tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = errors.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                diagnostic(&chunk[..n]);
            }
        });
        // A blocked pipe write must not occupy the SDK reply reader. Reuse the
        // same one command queue; its reservations include the active write.
        let mut writer = tokio::spawn(async move {
            loop {
                let command = tokio::select! {
                    biased;
                    _ = write_stop.changed() => return Ok::<_, std::io::Error>(()),
                    sequence = credits.recv() => match sequence {
                        Some(sequence) => CommandMessage {
                            bytes: format!("{{\"version\":{VERSION},\"method\":\"event/ack\",\"params\":{{\"sequence\":{sequence}}}}}\n").into_bytes(),
                            _bytes: None,
                        },
                        None => return Ok(()),
                    },
                    command = commands.recv() => match command {
                        Some(command) => command,
                        None => return Ok(()),
                    },
                };
                tokio::select! {
                    biased;
                    _ = write_stop.changed() => return Ok(()),
                    result = input.write_all(&command.bytes) => result?,
                }
            }
        });
        tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            let mut buffer = Vec::new();
            let mut stopping = false;
            let mut writer_finished = false;
            let mut last_event = [0, 0];
            let failure: AdapterError = loop {
                tokio::select! {
                    biased;
                    _=stop_receiver.changed()=>{stopping=true;break AdapterError::new("SDK bridge stopped")},
                    result=&mut writer=>{
                        writer_finished=true;
                        break match result {
                            Ok(Err(error))=>error.into(),
                            _=>AdapterError::new("SDK bridge writer closed"),
                        };
                    },
                    read=output.read(&mut chunk)=>{
                        let count=match read{Ok(0)=>break AdapterError::new("SDK bridge output closed"),Ok(n)=>n,Err(e)=>break e.into()};
                        let mut scan=buffer.len();
                        buffer.extend_from_slice(&chunk[..count]);
                        let mut consumed=0;
                        let mut error=None;
                        while let Some(relative)=buffer[scan..].iter().position(|b|*b==b'\n'){
                            let end=scan+relative;
                            if end-consumed>MAXIMUM_BYTES{error=Some(AdapterError::new("SDK bridge frame exceeds byte limit"));break}
                            let mut frame=match decode(&buffer[consumed..end]){Ok(frame)=>frame,Err(e)=>{error=Some(e);break}};consumed=end+1;scan=consumed;
                            if field(&frame,"version").as_f64()!=Some(VERSION){error=Some(AdapterError::new("SDK bridge version mismatch"));break}
                            if let Some(id)=field(&frame,"id").as_js_string().and_then(|s|s.to_utf8().ok()).and_then(|s|s.parse::<u64>().ok()){
                                let reply=waiting.lock().unwrap().remove(&id);
                                if let Some((reply, slots))=reply{
                                    if reply.is_closed(){continue}
                                    let result=if !field(&frame,"error").is_null(){
                                        let mut failure=frame.remove("error").unwrap();
                                        let name=if field(&failure,"name").is_str("native_capacity"){"native_capacity"}else{"adapter_error"};
                                        let message=failure.remove("message").and_then(J::into_js_string).unwrap_or_default();
                                        Err(AdapterError::typed(name,message))
                                    }else{Ok(frame.remove("result").unwrap_or(J::Null))};
                                    let size=match &result{Ok(value)=>value.retained_bytes(),Err(error)=>error.message.0.capacity().saturating_mul(2).saturating_add(std::mem::size_of::<AdapterError>())}
                                        .saturating_add(std::mem::size_of::<Reply>()).saturating_add(256);
                                    let bytes=if size>MAXIMUM_REPLY_BYTES{Err(capacity())}else{reserve(&reply_bytes,&data_reply_bytes,size,slots.1.is_none())};
                                    match bytes{
                                        Ok(bytes)=>{let _=reply.send(Ok((result,bytes,slots)));},
                                        Err(failure)=>{
                                            if reply.send(Err(AdapterError::typed("outcome_unknown",failure.message.clone()))).is_ok(){
                                                error=Some(failure);break;
                                            }
                                        },
                                    }
                                }
                            }else{
                                if let Err(failure)=event(frame,&event_slots,&mut last_event,&acknowledgements,&stop_writer,&events){error=Some(failure);break}
                            }
                        }
                        if let Some(error)=error{break error}
                        if consumed>0{buffer.drain(..consumed);if buffer.is_empty() && buffer.capacity()>65_536{buffer=Vec::new();}}
                        if buffer.len()>MAXIMUM_BYTES{break AdapterError::new("SDK bridge frame exceeds byte limit")}
                    },
                    _=child.completion.wait()=>break AdapterError::new("SDK bridge exited"),
                }
            };
            exited.store(true, Ordering::Release);
            // This is the exact owned child, never a process-name or host scan.
            // ChildStdin::shutdown does not close the pipe and a blocked flush
            // must not precede the finite exact-owner retirement deadline.
            stop_writer.send_replace(true);
            if !writer_finished {
                let _ = writer.await;
            }
            // EOF asks the SDK transport to close its vendor-owned CLI first.
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(2), child.completion.wait())
                    .await;
            let retired = match child.stop(std::time::Duration::from_secs(2)).await {
                Ok(result)
                    if result.error_code.as_deref() != Some("outcome_unknown")
                        && owned_process_tree_stopped(&child).await.unwrap_or(false) =>
                {
                    Ok(())
                }
                _ => Err(AdapterError::typed(
                    "outcome_unknown",
                    "SDK process-tree retirement is unconfirmed",
                )),
            };
            diagnostics.abort();
            let _ = diagnostics.await;
            for (_, (reply, _slots)) in std::mem::take(&mut *waiting.lock().unwrap()) {
                let _ = reply.send(Err(AdapterError::typed(
                    "outcome_unknown",
                    failure.message.clone(),
                )));
            }
            if !stopping {
                let _ = events.try_send(SdkEvent {
                    value: J::object([
                        ("version", VERSION.into_json()),
                        ("type", "transportError".into()),
                        ("message", J::String(failure.message)),
                    ]),
                    _credit: None,
                });
            }
            completed.send_replace(Some(retired));
        });
        Ok((
            Self {
                sender,
                next: Arc::new(AtomicU64::new(1)),
                pending,
                stops,
                completion,
                closed,
                process,
                calls: Arc::new(Semaphore::new(CALLS)),
                data_calls: Arc::new(Semaphore::new(CALLS - CONTROL_CALLS)),
                bytes: Arc::new(Semaphore::new(COMMAND_BYTES)),
                data_bytes: Arc::new(Semaphore::new(COMMAND_BYTES - CONTROL_BYTES)),
            },
            event_receiver,
        ))
    }
    pub async fn call(&self, method: &str, params: J) -> Result<SdkReply> {
        let control = matches!(
            method,
            "query/interrupt" | "query/close" | "permission/reply"
        );
        let slots = reserve(&self.calls, &self.data_calls, 1, control)?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let message = J::object([
            ("version", VERSION.into_json()),
            ("id", id.to_string().into()),
            ("method", method.into()),
            ("params", params),
        ]);
        let size = stringify_size(&message, usize::MAX)
            .map_err(|e| AdapterError::typed("invalid_data", e.message))?;
        if size > MAXIMUM_BYTES {
            return Err(AdapterError::new("SDK bridge frame exceeds byte limit"));
        }
        // Cover the input tree while encoding, the exact output capacity plus
        // newline, and per-command metadata. Codec scratch is separate.
        let charge = message
            .retained_bytes()
            .saturating_add(size)
            .saturating_add(1024);
        let bytes = reserve(&self.bytes, &self.data_bytes, charge, control)?;
        let mut frame = String::with_capacity(size + 1);
        stringify_into(&message, MAXIMUM_BYTES, &mut frame)
            .map_err(|e| AdapterError::typed("invalid_data", e.message))?;
        frame.push('\n');
        drop(message);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if self.closed.load(Ordering::Acquire) {
                return Err(AdapterError::new("SDK bridge is closed"));
            }
            pending.insert(id, (tx, slots));
        }
        if let Err(error) = self.sender.try_send(CommandMessage {
            bytes: frame.into_bytes(),
            _bytes: Some(bytes),
        }) {
            self.pending.lock().unwrap().remove(&id);
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => capacity(),
                mpsc::error::TrySendError::Closed(_) => AdapterError::new("SDK bridge is closed"),
            });
        }
        let (result, bytes, calls) = rx.await.map_err(|_| {
            AdapterError::typed("outcome_unknown", "SDK bridge response owner closed")
        })??;
        result.map(|value| SdkReply {
            value,
            _bytes: bytes,
            _calls: calls,
        })
    }
    pub async fn stop(&self) -> Result<()> {
        // Do not wait on provider/control promises to establish child cleanup.
        self.stops.send_replace(true);
        let mut completion = self.completion.clone();
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result;
            }
            if completion.changed().await.is_err() {
                return Err(AdapterError::typed(
                    "outcome_unknown",
                    "SDK cleanup owner closed without retirement evidence",
                ));
            }
        }
    }
    pub fn owned_process(&self) -> Arc<OwnedProcess> {
        self.process.clone()
    }
}
