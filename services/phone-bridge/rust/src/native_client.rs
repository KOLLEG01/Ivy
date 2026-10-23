//! The service side of one authenticated inherited Phone pipe epoch. Original
//! intent/result hooks precede dispatch/observation; neither loss nor deadlines
//! resend an effect or hand its process ownership to a request observer.
pub use crate::contracts::phone_schema;
use crate::{
    contracts::validate_native,
    ipc::{self, FrameReader},
    journal::PhoneJournal,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::digest,
    json::{canonical, parse},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, watch},
    task::AbortHandle,
    time::{Instant, MissedTickBehavior},
};
pub const FRAME_BYTES: usize = ipc::FRAME_BYTES;
pub fn new_phone_epoch() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
pub type PhoneReceiptWork = Pin<Box<dyn Future<Output = IvyResult<()>> + Send>>;
pub trait PhoneReceiptHooks: Send + Sync {
    fn before_send(&self, intent: IvyJson) -> PhoneReceiptWork;
    fn before_resolve(&self, intent: IvyJson, reply: IvyJson) -> PhoneReceiptWork;
}
impl PhoneReceiptHooks for PhoneJournal {
    fn before_send(&self, intent: IvyJson) -> PhoneReceiptWork {
        let journal = self.clone();
        Box::pin(async move { journal.submit(intent).await })
    }
    fn before_resolve(&self, intent: IvyJson, reply: IvyJson) -> PhoneReceiptWork {
        let journal = self.clone();
        Box::pin(async move { journal.finish(intent, reply).await.map(|_| ()) })
    }
}
pub fn validate_phone(name: &str, value: &IvyJson) -> IvyResult<()> {
    validate_native(name, value)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn identifier(v: &JsString) -> bool {
    v.0.len() == 36
        && v.0.iter().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                *c == 45
            } else {
                matches!(*c,48..=57|65..=70|97..=102)
            }
        })
}
fn unknown(code: &str) -> IvyError {
    IvyError::new(
        code,
        "The original Phone command outcome requires reconciliation.",
    )
    .with_outcome(Outcome::Unknown)
}
#[derive(Clone)]
pub struct PhoneNativeRequest(watch::Receiver<Option<IvyResult<IvyJson>>>);
impl PhoneNativeRequest {
    pub async fn wait(&self) -> IvyResult<IvyJson> {
        let mut receive = self.0.clone();
        loop {
            if let Some(result) = receive.borrow_and_update().clone() {
                return result;
            }
            receive
                .changed()
                .await
                .map_err(|_| unknown("phone_unavailable"))?;
        }
    }
}
struct Pending {
    intent: Option<IvyJson>,
    result: watch::Sender<Option<IvyResult<IvyJson>>>,
    receiving: bool,
    timer: Option<AbortHandle>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(timer) = &self.timer {
            timer.abort();
        }
    }
}
#[derive(Default)]
struct State {
    closed: Option<String>,
    sequence: u64,
    queued_bytes: usize,
    pending: BTreeMap<u64, Arc<Mutex<Pending>>>,
    expired_reads: BTreeSet<u64>,
    expired_effects: BTreeMap<u64, Arc<Mutex<Pending>>>,
    heartbeat: bool,
}
struct Inner {
    epoch: JsString,
    hooks: Arc<dyn PhoneReceiptHooks>,
    state: Mutex<State>,
    writes: mpsc::UnboundedSender<Vec<u8>>,
    closed: watch::Sender<Option<String>>,
}
#[derive(Clone)]
pub struct PhoneNativeClient(Arc<Inner>);
impl PhoneNativeClient {
    pub fn start<R: AsyncRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static>(
        epoch: JsString,
        input: W,
        output: R,
        hooks: Arc<dyn PhoneReceiptHooks>,
    ) -> IvyResult<Self> {
        require(
            identifier(&epoch),
            "invalid_arguments",
            "Phone native owner requires an exact epoch UUID.",
        )?;
        let (send, receive) = mpsc::unbounded_channel();
        let (closed, _) = watch::channel(None);
        let owner = Arc::new(Inner {
            epoch,
            hooks,
            state: Mutex::new(State::default()),
            writes: send,
            closed,
        });
        tokio::spawn(read(owner.clone(), output));
        tokio::spawn(write(owner.clone(), input, receive));
        Ok(Self(owner))
    }
    pub fn epoch(&self) -> &JsString {
        &self.0.epoch
    }
    pub fn is_closed(&self) -> bool {
        self.0.state.lock().unwrap().closed.is_some()
    }
    pub async fn closed(&self) -> String {
        let mut closed = self.0.closed.subscribe();
        loop {
            if let Some(code) = closed.borrow_and_update().clone() {
                return code;
            }
            if closed.changed().await.is_err() {
                return "phone_owner_stopping".into();
            }
        }
    }
    pub fn close(&self, code: &str) {
        self.0.close(code)
    }
    pub fn start_heartbeat(&self) -> IvyResult<()> {
        {
            let mut s = self.0.state.lock().unwrap();
            require(
                s.closed.is_none(),
                "phone_unavailable",
                "Phone native owner is closed.",
            )?;
            if s.heartbeat {
                return Ok(());
            }
            s.heartbeat = true;
        }
        let owner = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut failures = 0;
            loop {
                tokio::select! {_ = owner.closed()=>break,_ = interval.tick()=>{}}
                let reply = match owner.request(
                    "heartbeat",
                    IvyJson::object([] as [(&str, IvyJson); 0]),
                    None,
                    5000,
                ) {
                    Ok(work) => work.wait().await,
                    Err(e) => Err(e),
                };
                match reply {
                    Ok(reply) => {
                        failures = 0;
                        if reply.get("ok") != Some(&true.into()) {
                            owner.close("phone_heartbeat_rejected");
                        }
                    }
                    Err(_) => {
                        failures += 1;
                        if failures >= 3 {
                            owner.close("phone_heartbeat_lost");
                        }
                    }
                }
                if owner.is_closed() {
                    break;
                }
            }
        });
        Ok(())
    }
    pub fn request(
        &self,
        method: &str,
        args: IvyJson,
        operation_id: Option<JsString>,
        timeout_ms: u64,
    ) -> IvyResult<PhoneNativeRequest> {
        let (request_id, encoded, intent) = {
            let mut state = self.0.state.lock().unwrap();
            state.writable()?;
            require(
                (1..=150000).contains(&timeout_ms),
                "invalid_arguments",
                "Phone request timeout must be bounded.",
            )?;
            require(
                if ipc::is_read_only(method) {
                    operation_id.is_none()
                } else {
                    operation_id.as_ref().is_some_and(identifier)
                },
                "invalid_arguments",
                "Phone effects require the original operation UUID.",
            )?;
            require(
                ipc::has_pipe_capacity(method, state.pending.len()),
                "phone_capacity",
                "Phone request capacity is full.",
            )?;
            state.sequence += 1;
            let request_id = state.sequence;
            let params = parse(&canonical(&args, FRAME_BYTES)?, FRAME_BYTES)?;
            let request = IvyJson::object([
                ("version", 1.0.into()),
                ("epoch", self.0.epoch.clone().into()),
                ("requestId", (request_id as f64).into()),
                (
                    "operationId",
                    operation_id.clone().map_or(IvyJson::Null, Into::into),
                ),
                ("method", method.into()),
                ("params", params.clone()),
            ]);
            validate_native("Request", &request)?;
            if let Some(shape) = argument_shape(method, &params) {
                validate_native(shape, &params)?;
            } else {
                require(
                    params.as_object().is_some_and(|p| p.is_empty()),
                    "invalid_arguments",
                    "Phone control commands have no arguments.",
                )?;
            }
            if method.starts_with("call.") {
                require(
                    params
                        .get("callId")
                        .and_then(IvyJson::as_js_string)
                        .is_some_and(identifier),
                    "invalid_arguments",
                    "Phone commands require the exact call UUID.",
                )?;
            }
            let encoded = canonical(&request, FRAME_BYTES)?;
            let intent = operation_id
                .map(|operation_id| -> IvyResult<IvyJson> {
                    let mut intent = IvyJson::object([
                        ("epoch", self.0.epoch.clone().into()),
                        ("operationId", operation_id.into()),
                        (
                            "callId",
                            if method.starts_with("call.") {
                                params.get("callId").cloned().unwrap_or(IvyJson::Null)
                            } else {
                                IvyJson::Null
                            },
                        ),
                        ("method", method.into()),
                        (
                            "requestHash",
                            digest(canonical(
                                &IvyJson::object([
                                    ("method", method.into()),
                                    ("params", params.clone()),
                                ]),
                                FRAME_BYTES,
                            )?)
                            .into(),
                        ),
                    ]);
                    if ["call.desktop.pauseVoice", "call.desktop.resumeVoice"].contains(&method)
                        || method.starts_with("call.realtime.")
                    {
                        intent.insert(
                            "voiceGeneration",
                            params.get("generation").cloned().unwrap_or(IvyJson::Null),
                        );
                    }
                    if method == "call.command.feedback" {
                        intent.insert(
                            "commandSequence",
                            params
                                .get("commandSequence")
                                .cloned()
                                .unwrap_or(IvyJson::Null),
                        );
                    }
                    Ok(intent)
                })
                .transpose()?;
            (request_id, encoded, intent)
        };
        let (finished, receive) = watch::channel(None);
        let observer = PhoneNativeRequest(receive);
        if let Some(intent) = intent {
            let owner = self.0.clone();
            let method = method.to_owned();
            tokio::spawn(async move {
                let result = owner
                    .hooks
                    .before_send(intent.clone())
                    .await
                    .and_then(|()| {
                        owner.dispatch(
                            &method,
                            request_id,
                            encoded,
                            Some(intent),
                            timeout_ms,
                            finished.clone(),
                        )
                    });
                if let Err(error) = result {
                    finished.send_replace(Some(Err(error)));
                }
            });
        } else if let Err(error) = self.0.dispatch(
            method,
            request_id,
            encoded,
            None,
            timeout_ms,
            finished.clone(),
        ) {
            finished.send_replace(Some(Err(error)));
        }
        Ok(observer)
    }
}
impl State {
    fn writable(&self) -> IvyResult<()> {
        require(
            self.closed.is_none(),
            "phone_unavailable",
            "Phone pipe is unavailable.",
        )?;
        require(
            self.queued_bytes < FRAME_BYTES * 2,
            "phone_capacity",
            "Phone command pipe is full.",
        )
    }
}
impl Inner {
    fn dispatch(
        self: &Arc<Self>,
        method: &str,
        id: u64,
        encoded: String,
        intent: Option<IvyJson>,
        timeout: u64,
        finished: watch::Sender<Option<IvyResult<IvyJson>>>,
    ) -> IvyResult<()> {
        let mut state = self.state.lock().unwrap();
        state.writable()?;
        require(
            ipc::has_pipe_capacity(method, state.pending.len()),
            "phone_capacity",
            "Phone request capacity changed before dispatch.",
        )?;
        let mut bytes = encoded.into_bytes();
        bytes.push(b'\n');
        state.queued_bytes += bytes.len();
        let pending = Arc::new(Mutex::new(Pending {
            intent,
            result: finished,
            receiving: false,
            timer: None,
        }));
        state.pending.insert(id, pending.clone());
        let owner = Arc::downgrade(self);
        let deadline = Instant::now() + Duration::from_millis(timeout);
        let timer = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            if let Some(owner) = owner.upgrade() {
                owner.expire(id);
            }
        });
        pending.lock().unwrap().timer = Some(timer.abort_handle());
        if self.writes.send(bytes).is_err() {
            drop(state);
            self.close("phone_write_failed");
        }
        Ok(())
    }
    fn expire(&self, id: u64) {
        let mut state = self.state.lock().unwrap();
        let Some(pending) = state.pending.get(&id).cloned() else {
            return;
        };
        let effect = pending.lock().unwrap().intent.is_some();
        if if effect {
            state.expired_effects.len() >= ipc::LATE_EFFECT_SLOTS
        } else {
            state.expired_reads.len() >= ipc::LATE_READ_SLOTS
        } {
            drop(state);
            self.close("phone_request_timeout");
            return;
        }
        state.pending.remove(&id);
        if effect {
            state.expired_effects.insert(id, pending.clone());
        } else {
            state.expired_reads.insert(id);
        }
        let error = if effect {
            IvyError::new(
                "phone_request_timeout",
                "The original Phone command is still unconfirmed.",
            )
            .with_outcome(Outcome::Unknown)
        } else {
            IvyError::new(
                "phone_request_timeout",
                "Phone observation timed out; active calls remain owned.",
            )
        };
        pending
            .lock()
            .unwrap()
            .result
            .send_replace(Some(Err(error)));
    }
    fn receive(self: &Arc<Self>, text: &str) -> IvyResult<()> {
        // TextDecoder's default BOM handling is applied afresh to each frame.
        let reply = parse(text.strip_prefix('\u{feff}').unwrap_or(text), FRAME_BYTES)?;
        validate_native("Response", &reply)?;
        require(
            reply.get("epoch") == Some(&self.epoch.clone().into())
                && (reply.get("ok") == Some(&true.into()))
                    == reply.get("error").is_some_and(IvyJson::is_null)
                && (reply.get("ok") == Some(&true.into())
                    || reply.get("result").is_some_and(IvyJson::is_null)),
            "phone_invalid_frame",
            "Native Phone response ownership/outcome mismatch.",
        )?;
        let id = reply
            .get("requestId")
            .and_then(IvyJson::as_f64)
            .ok_or_else(IvyError::internal)? as u64;
        let mut state = self.state.lock().unwrap();
        if state.closed.is_some() {
            return Ok(());
        }
        if state.expired_reads.remove(&id) {
            return Ok(());
        }
        let pending = state
            .expired_effects
            .get(&id)
            .or_else(|| state.pending.get(&id))
            .cloned();
        require(
            pending
                .as_ref()
                .is_some_and(|p| !p.lock().unwrap().receiving),
            "phone_invalid_frame",
            "Native Phone response has no unconsumed original request.",
        )?;
        let pending = pending.unwrap();
        let mut original = pending.lock().unwrap();
        original.receiving = true;
        let intent = original.intent.clone();
        if intent.is_none() {
            state.pending.remove(&id);
            if let Some(timer) = original.timer.take() {
                timer.abort();
            }
            original.result.send_replace(Some(Ok(reply)));
            return Ok(());
        }
        drop(original);
        drop(state);
        let owner = self.clone();
        tokio::spawn(async move {
            let result = owner
                .hooks
                .before_resolve(intent.unwrap(), reply.clone())
                .await;
            if result.is_err() {
                owner.close("phone_receipt_save_failed");
                return;
            }
            let mut state = owner.state.lock().unwrap();
            if state
                .pending
                .get(&id)
                .is_some_and(|p| Arc::ptr_eq(p, &pending))
            {
                state.pending.remove(&id);
                let mut pending = pending.lock().unwrap();
                if let Some(timer) = pending.timer.take() {
                    timer.abort();
                }
                pending.result.send_replace(Some(Ok(reply)));
            } else if state
                .expired_effects
                .get(&id)
                .is_some_and(|p| Arc::ptr_eq(p, &pending))
            {
                state.expired_effects.remove(&id);
            }
        });
        Ok(())
    }
    fn close(&self, code: &str) {
        let pending = {
            let mut state = self.state.lock().unwrap();
            if state.closed.is_some() {
                return;
            }
            state.closed = Some(code.into());
            state.expired_reads.clear();
            state.expired_effects.clear();
            std::mem::take(&mut state.pending)
        };
        for (_, pending) in pending {
            let mut pending = pending.lock().unwrap();
            if let Some(timer) = pending.timer.take() {
                timer.abort();
            }
            pending.result.send_replace(Some(Err(unknown(code))));
        }
        self.closed.send_replace(Some(code.into()));
    }
}
fn argument_shape(method: &str, params: &IvyJson) -> Option<&'static str> {
    Some(match method {
        "call.realtime.prepare" => "PrepareRealtime",
        "call.realtime.answer" => "AnswerRealtime",
        "call.realtime.stop" => "StopRealtime",
        "call.desktop.pauseVoice" | "call.desktop.resumeVoice" => "CallDesktopVoiceTransition",
        "status" if params.get("callId").is_some_and(|v| !v.is_null()) => "CallIdentity",
        "call.screening.prepare" => "PrepareScreening",
        "call.features" => "ConfigureFeatures",
        "call.windows.connect" | "call.screening.bridge" => "ConnectWindows",
        "configure" => "Configure",
        "call.dial" => "Dial",
        "call.answer" => "Answer",
        "call.command.feedback" => "CommandFeedback",
        "call.audio.prepare" => "PrepareAudio",
        "call.audio.rebind" => "RebindAudio",
        "desktop.observe" => "ObserveDesktop",
        "desktop.launch" => "DesktopApplication",
        "desktop.capture" => "ObserveCapture",
        "desktop.captureOwner" => "ObserveCaptureOwner",
        "desktop.controls" => "ObserveControls",
        "desktop.process" => "ObserveProcess",
        "call.desktop.launch" => "CallDesktopLaunch",
        "call.desktop.startVoice" | "call.desktop.stopVoice" => "CallDesktopVoiceInput",
        m if m.starts_with("call.") => "CallIdentity",
        _ => return None,
    })
}
async fn read<R: AsyncRead + Unpin>(owner: Arc<Inner>, mut output: R) {
    let mut closed = owner.closed.subscribe();
    let mut reader = FrameReader::default();
    let mut bytes = [0u8; 8192];
    loop {
        if closed.borrow().is_some() {
            break;
        }
        let count = tokio::select! {_ = closed.changed()=>break,result=output.read(&mut bytes)=>match result{Ok(count)=>count,Err(_)=>{owner.close("phone_read_failed");break;}}};
        if count == 0 {
            owner.close(if reader.finish() == Err(ipc::FrameError::Truncated) {
                "phone_truncated_frame"
            } else {
                "phone_pipe_closed"
            });
            break;
        }
        if reader
            .feed(&bytes[..count], |text| {
                owner
                    .receive(text)
                    .map_err(|_| ipc::FrameError::InvalidRequest)
            })
            .is_err()
        {
            owner.close("phone_invalid_frame");
            break;
        }
    }
}
async fn write<W: AsyncWrite + Unpin>(
    owner: Arc<Inner>,
    mut input: W,
    mut receive: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    let mut closed = owner.closed.subscribe();
    loop {
        let ended = closed.borrow().is_some();
        let next = if ended {
            receive.try_recv().ok()
        } else {
            tokio::select! {_ = closed.changed()=>continue,next=receive.recv()=>next}
        };
        let Some(bytes) = next else { break };
        let size = bytes.len();
        if input.write_all(&bytes).await.is_err() || input.flush().await.is_err() {
            owner.close("phone_write_failed");
            break;
        }
        let mut state = owner.state.lock().unwrap();
        state.queued_bytes = state.queued_bytes.saturating_sub(size);
    }
}

mod observations;
#[cfg(test)]
#[path = "../../../../tests/rust/phone_service_native_client.rs"]
mod tests;
