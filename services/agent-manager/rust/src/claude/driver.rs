//! One native state owner, with independent SDK/control/diff futures. Provider
//! waits never occupy the owner's receive loop or its shutdown channel.
use super::{
    ask::{self, Callback},
    bridge::{Bridge, SdkEvent, SdkReply},
    error::{AdapterError, Result},
    protocol::{Adapter, Effect},
    sdk_events::SdkTurn,
    sdk_options::{self, SdkEnvironment},
    value::*,
    workflow::{EventSink, IoFuture, WorkflowFiles},
};
use ivy_contracts::{IvyJson as J, JsString};
use ivy_host_runtime::codex_rpc::NativeFrame;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
    task::{AbortHandle, JoinHandle, JoinSet},
    time::Duration,
};

pub struct Bindings {
    pub sdk: SdkEnvironment,
    pub workflow_files: WorkflowFiles,
    /// Admitted nonblocking native-peer sender. Transport ownership, frame
    /// limits and connection closure remain with the containing Agent runtime.
    pub send: Arc<dyn Fn(JsString, J) -> Result<()> + Send + Sync>,
    /// Original gitDiff behavior: failed/missing git produces an empty diff.
    pub git_diff: Arc<dyn Fn(JsString) -> IoFuture<JsString> + Send + Sync>,
}
type Ack = oneshot::Sender<Result<()>>;
enum NativeMessage {
    Value(J),
    Frame(NativeFrame),
}
impl std::ops::Deref for NativeMessage {
    type Target = J;
    fn deref(&self) -> &J {
        match self {
            Self::Value(value) => value,
            Self::Frame(frame) => frame,
        }
    }
}
enum Input {
    BridgeFailed(AdapterError),
    Native {
        peer: JsString,
        message: NativeMessage,
        reply: Ack,
    },
    PeerClosed(JsString),
    Begin(JsString),
    Event {
        turn: JsString,
        event: J,
        reply: Ack,
    },
    Finished {
        turn: JsString,
        result: Result<()>,
    },
    RuntimeReply {
        id: u64,
        result: Result<Option<SdkReply>>,
    },
    Diff {
        turn: JsString,
        diff: JsString,
        reply: Ack,
    },
    FinalDiff {
        turn: JsString,
        diff: JsString,
    },
    Watchdog {
        turn: JsString,
        generation: u64,
    },
}
enum TurnInput {
    Frame(JsString, SdkEvent),
}
struct Running {
    id: JsString,
    context: J,
    auto_allow: bool,
    sender: mpsc::Sender<TurnInput>,
    query: Arc<Mutex<JsString>>,
    interrupted: Arc<AtomicBool>,
}
enum PendingKind {
    Permission(J),
    UserInput { input: J, questions: Vec<J> },
}
struct Pending {
    turn: JsString,
    query: JsString,
    kind: PendingKind,
    _retained: Option<OwnedSemaphorePermit>,
}
#[derive(Clone)]
pub struct Handle {
    sender: mpsc::Sender<Input>,
    stops: mpsc::UnboundedSender<oneshot::Sender<()>>,
}
impl Handle {
    pub async fn receive(&self, peer: JsString, message: J) -> Result<()> {
        self.receive_message(peer, NativeMessage::Value(message))
            .await
    }
    pub async fn receive_frame(&self, peer: JsString, message: NativeFrame) -> Result<()> {
        self.receive_message(peer, NativeMessage::Frame(message))
            .await
    }
    async fn receive_message(&self, peer: JsString, message: NativeMessage) -> Result<()> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Input::Native {
                peer,
                message,
                reply,
            })
            .await
            .map_err(|_| AdapterError::new("Claude adapter owner is closed"))?;
        receiver
            .await
            .map_err(|_| AdapterError::new("Claude adapter response owner closed"))?
    }
    pub async fn close_peer(&self, peer: JsString) -> Result<()> {
        self.sender
            .send(Input::PeerClosed(peer))
            .await
            .map_err(|_| AdapterError::new("Claude adapter owner is closed"))
    }
    pub async fn stop(&self) {
        let (reply, receiver) = oneshot::channel();
        if self.stops.send(reply).is_ok() {
            let _ = receiver.await;
        }
    }
}
pub fn spawn(
    adapter: Adapter,
    bridge: Bridge,
    events: mpsc::Receiver<SdkEvent>,
    bindings: Bindings,
) -> (Handle, JoinHandle<Result<()>>) {
    let (sender, receiver) = mpsc::channel(128);
    let (stops, stop_receiver) = mpsc::unbounded_channel();
    let handle = Handle {
        sender: sender.clone(),
        stops,
    };
    let owner = Owner {
        adapter,
        bridge,
        events,
        bindings,
        sender,
        receiver,
        stops: stop_receiver,
        jobs: JoinSet::new(),
        running: vec![],
        pending: BTreeMap::new(),
        watchdogs: BTreeMap::new(),
        generation: 0,
    };
    (handle, tokio::spawn(owner.run()))
}
struct Owner {
    adapter: Adapter,
    bridge: Bridge,
    events: mpsc::Receiver<SdkEvent>,
    bindings: Bindings,
    sender: mpsc::Sender<Input>,
    receiver: mpsc::Receiver<Input>,
    stops: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
    jobs: JoinSet<()>,
    running: Vec<Running>,
    pending: BTreeMap<u64, Pending>,
    watchdogs: BTreeMap<JsString, (u64, AbortHandle)>,
    generation: u64,
}
impl Owner {
    async fn run(mut self) -> Result<()> {
        let mut blocked: Option<(mpsc::Sender<TurnInput>, TurnInput)> = None;
        let mut stop_reply = None;
        let outcome = loop {
            // Preserve bounded backpressure from a slow SDK turn while still
            // accepting native replies, controls, completed I/O and shutdown.
            let ready = async {
                match &blocked {
                    Some((sender, _)) => sender.clone().reserve_owned().await.ok(),
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                stop=self.stops.recv()=>{stop_reply=stop;break Ok(())},
                input=self.receiver.recv()=>{
                    let Some(input)=input else{break Ok(())};
                    if let Err(error)=self.input(input){break Err(error)}
                },
                permit=ready=>{
                    let (_,message)=blocked.take().unwrap();if let Some(permit)=permit{permit.send(message);}
                },
                event=self.events.recv(),if blocked.is_none()=>{
                    let Some(event)=event else{break Err(AdapterError::new("SDK bridge event owner closed"))};
                    match self.bridge_event(event){Ok(value)=>blocked=value,Err(error)=>break Err(error)}
                },
                result=self.jobs.join_next(),if !self.jobs.is_empty()=>{
                    if let Some(Err(error))=result{if !error.is_cancelled(){break Err(AdapterError::new(format!("Claude adapter worker failed: {error}")))}}
                },
            }
        };
        self.receiver.close();
        self.jobs.abort_all();
        let stored = self.adapter.stop();
        // Reap the exact private child independently of full event queues or
        // unresolved provider/control futures. No PID/name discovery is used.
        let closed = self.bridge.stop().await;
        while self.jobs.join_next().await.is_some() {}
        if let Some(reply) = stop_reply {
            let _ = reply.send(());
        }
        outcome.and(stored).and(closed)
    }
    fn input(&mut self, input: Input) -> Result<()> {
        match input {
            Input::BridgeFailed(error) => return Err(error),
            Input::Native {
                peer,
                message,
                reply,
            } => match self.adapter.receive(&peer, &message) {
                Ok(effects) => self.effects(effects, Some(reply))?,
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
            },
            Input::PeerClosed(peer) => self.adapter.close_peer(&peer),
            Input::Begin(turn) => {
                let effects = self.adapter.begin_turn(&turn)?;
                self.effects(effects, None)?;
            }
            Input::Event { turn, event, reply } => {
                match self.adapter.runtime_event(&turn, &event) {
                    Ok(effects) => self.effects(effects, Some(reply))?,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Input::RuntimeReply { id, result } => {
                let (result, original) = match result {
                    Ok(Some(mut reply)) => (
                        Ok(std::mem::replace(&mut reply.value, J::Null)),
                        Some(reply),
                    ),
                    Ok(None) => (Ok(J::Null), None),
                    Err(error) => (Err(error), None),
                };
                let effects = self.adapter.runtime_reply(id, result)?;
                self.effects(effects, None)?;
                drop(original);
            }
            Input::Diff { turn, diff, reply } => {
                let result = self
                    .adapter
                    .diff_result(&turn, diff)
                    .and_then(|effects| self.effects(effects, None));
                let _ = reply.send(result);
            }
            Input::Finished { turn, result } => {
                self.running.retain(|running| running.id != turn);
                let effects = self.adapter.cancel_callbacks(&turn, None)?;
                self.effects(effects, None)?;
                self.pending.retain(|_, pending| pending.turn != turn);
                self.cancel_watchdog(&turn);
                if result.is_ok() {
                    let cwd = self
                        .adapter
                        .turns
                        .get(&turn)
                        .map(|state| string(field(&state.thread, "cwd")));
                    let effects = self.adapter.prepare_success(&turn)?;
                    self.effects(effects, None)?;
                    if let Some(cwd) = cwd {
                        let sender = self.sender.clone();
                        let diff = self.bindings.git_diff.clone();
                        self.jobs.spawn(async move {
                            let diff = (diff)(cwd).await.unwrap_or_default();
                            let _ = sender.send(Input::FinalDiff { turn, diff }).await;
                        });
                    }
                } else {
                    let effects = self.adapter.runtime_finished(&turn, result)?;
                    self.effects(effects, None)?;
                }
            }
            Input::FinalDiff { turn, diff } => {
                let effects = self.adapter.diff_result(&turn, diff)?;
                self.effects(effects, None)?;
                let effects = self.adapter.runtime_finished(&turn, Ok(()))?;
                self.effects(effects, None)?;
            }
            Input::Watchdog { turn, generation } => {
                if self
                    .watchdogs
                    .get(&turn)
                    .is_some_and(|(current, _)| *current == generation)
                {
                    self.watchdogs.remove(&turn);
                    let effects = self.adapter.watchdog_elapsed(&turn)?;
                    self.effects(effects, None)?;
                    let bridge = self.bridge.clone();
                    self.jobs.spawn(async move {
                        let _ = bridge
                            .call("query/interrupt", J::object([("turnId", turn.into_json())]))
                            .await;
                    });
                }
            }
        }
        Ok(())
    }
    fn cancel_watchdog(&mut self, turn: &JsString) {
        if let Some((_, timer)) = self.watchdogs.remove(turn) {
            timer.abort()
        }
    }
    fn effects(&mut self, effects: Vec<Effect>, mut reply: Option<Ack>) -> Result<()> {
        let mut diffs = vec![];
        for effect in effects {
            let result = match effect {
                Effect::Send { peer, message } => (self.bindings.send)(peer, message),
                Effect::Begin { turn_id } => {
                    let sender = self.sender.clone();
                    self.jobs.spawn(async move {
                        tokio::task::yield_now().await;
                        let _ = sender.send(Input::Begin(turn_id)).await;
                    });
                    Ok(())
                }
                Effect::Run { turn_id, context } => {
                    self.start(turn_id, context);
                    Ok(())
                }
                Effect::Runtime {
                    call_id,
                    method,
                    params,
                } => {
                    self.control(call_id, method, params);
                    Ok(())
                }
                Effect::Permission { callback, result } => {
                    self.permission_reply(callback, result);
                    Ok(())
                }
                Effect::Diff { turn_id, cwd } => {
                    diffs.push((turn_id, cwd));
                    Ok(())
                }
                Effect::CancelWatchdog { turn_id } => {
                    self.cancel_watchdog(&turn_id);
                    Ok(())
                }
                Effect::Watchdog {
                    turn_id,
                    timeout_ms,
                } => {
                    if !self.watchdogs.contains_key(&turn_id) {
                        self.generation = self.generation.wrapping_add(1);
                        let generation = self.generation;
                        let sender = self.sender.clone();
                        let key = turn_id.clone();
                        let timer = self.jobs.spawn(async move {
                            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
                            let _ = sender
                                .send(Input::Watchdog {
                                    turn: key,
                                    generation,
                                })
                                .await;
                        });
                        self.watchdogs.insert(turn_id, (generation, timer));
                    }
                    Ok(())
                }
            };
            if let Err(error) = result {
                if let Some(reply) = reply.take() {
                    let _ = reply.send(Err(error.clone()));
                    return Ok(());
                }
                return Err(error);
            }
        }
        if diffs.is_empty() {
            if let Some(reply) = reply {
                let _ = reply.send(Ok(()));
            }
            return Ok(());
        }
        let sender = self.sender.clone();
        let diff = self.bindings.git_diff.clone();
        self.jobs.spawn(async move {
            let result = async {
                for (turn, cwd) in diffs {
                    let diff = (diff)(cwd).await.unwrap_or_default();
                    let (reply, receiver) = oneshot::channel();
                    sender
                        .send(Input::Diff { turn, diff, reply })
                        .await
                        .map_err(|_| AdapterError::new("Claude diff owner closed"))?;
                    receiver
                        .await
                        .map_err(|_| AdapterError::new("Claude diff response owner closed"))??;
                }
                Ok(())
            }
            .await;
            if let Some(reply) = reply {
                let _ = reply.send(result);
            }
        });
        Ok(())
    }
    fn start(&mut self, turn: JsString, context: J) {
        let options = self.bindings.sdk.query(&context);
        let options = match options {
            Ok(value) => value,
            Err(error) => {
                let sender = self.sender.clone();
                self.jobs.spawn(async move {
                    let _ = sender
                        .send(Input::Finished {
                            turn,
                            result: Err(error),
                        })
                        .await;
                });
                return;
            }
        };
        let (sender, mut receiver) = mpsc::channel(64);
        let query = Arc::new(Mutex::new(turn.clone()));
        let interrupted = Arc::new(AtomicBool::new(false));
        self.running.push(Running {
            id: turn.clone(),
            context: context.clone(),
            auto_allow: options.auto_allow,
            sender,
            query: query.clone(),
            interrupted: interrupted.clone(),
        });
        let bridge = self.bridge.clone();
        let owner = self.sender.clone();
        let event_owner = owner.clone();
        let event_turn = turn.clone();
        let sink: EventSink = Arc::new(move |event| {
            let owner = event_owner.clone();
            let turn = event_turn.clone();
            Box::pin(async move {
                let (reply, receiver) = oneshot::channel();
                owner
                    .send(Input::Event { turn, event, reply })
                    .await
                    .map_err(|_| AdapterError::new("Claude event owner closed"))?;
                receiver
                    .await
                    .map_err(|_| AdapterError::new("Claude event response owner closed"))?
            })
        });
        let files = self.bindings.workflow_files.clone();
        let sdk_home = self.bindings.sdk.home.clone();
        let sdk_environment = self.bindings.sdk.environment.clone();
        let make_state = move || {
            SdkTurn::new(
                context.clone(),
                &sdk_home,
                &sdk_environment,
                files.clone(),
                sink.clone(),
            )
        };
        self.jobs.spawn(async move {
            let mut attempt = 0;
            let result = 'attempt: loop {
                let query_id = query.lock().unwrap().clone();
                let mut params = options.params.clone();
                params.insert("turnId", query_id.clone().into_json());
                let mut state = make_state();
                if interrupted.load(Ordering::Acquire) {
                    let _ = state.end(Some(AdapterError::new("Claude turn interrupted before model work started."))).await;
                } else if let Err(error) = bridge.call("query/start", params).await {
                    let _ = state.end(Some(error)).await;
                } else if interrupted.load(Ordering::Acquire) {
                    // A native interrupt can race the SDK's start acknowledgement.
                    let _ = bridge.call("query/interrupt", J::object([("turnId",query_id.clone().into_json())])).await;
                }
                let changed = state.changed();
                while !state.is_finished() {
                    let result = tokio::select! {
                        input=receiver.recv()=>match input {
                            Some(TurnInput::Frame(id, frame)) if id == query_id => {
                                if is(field(&frame,"type"),"message") { state.message(field(&frame,"message")).await }
                                else { state.end(if field(&frame,"error").is_null(){None}else{Some(AdapterError::new(text(field(field(&frame,"error"),"message"))))}).await }
                            },
                            Some(_) => Ok(()),
                            None => state.end(None).await,
                        },
                        _=changed.notified()=>Ok(()),
                    };
                    if let Err(mut error) = result {
                        if error.name == "empty_turn" {
                            if attempt == 0 && !interrupted.load(Ordering::Acquire) {
                                attempt += 1;
                                // Distinct transport identity fences late SDK messages, EOF and controls.
                                *query.lock().unwrap() = concat(&[&turn, &":retry1".into()]);
                                let _ = bridge.call("query/close", J::object([("turnId", query_id.clone().into_json())])).await;
                                let (reply, receive) = oneshot::channel();
                                let _ = owner.send(Input::Event { turn: turn.clone(), event: J::object([
                                    ("type", "notice".into()), ("level", "warning".into()),
                                    ("message", "Claude returned an empty result before starting model work. Retrying once.".into()),
                                ]), reply }).await;
                                let _ = receive.await;
                                continue 'attempt;
                            }
                            error = AdapterError::new(if attempt > 0 {
                                "Claude returned no response after one retry. No model work was started; the task remains unfinished."
                            } else { "Claude turn interrupted before model work started." });
                        }
                        let _ = state.end(Some(error)).await;
                        break;
                    }
                }
                break state.outcome().unwrap_or_else(||Err(AdapterError::new("Claude SDK turn closed without an outcome")));
            };
            // Complete native state independently of SDK close/abort promises.
            let _=owner.send(Input::Finished{turn:turn.clone(),result}).await;
            let query_id = query.lock().unwrap().clone();
            let _=bridge.call("query/close",J::object([("turnId",query_id.into_json())])).await;
        });
    }
    fn control(&mut self, id: u64, method: &'static str, params: J) {
        let request = if method == "models" || method == "usage" {
            self.bindings
                .sdk
                .discovery(
                    &self.adapter.env.cwd,
                    if method == "models" { 30_000 } else { 15_000 },
                )
                .map(|p| vec![(method, p)])
        } else if method == "steer" {
            let thread = string(field(&params, "threadId"));
            Ok(self
                .running
                .iter()
                .find(|r| field(&r.context, "threadId").as_js_string() == Some(&thread))
                .map(|running| {
                    let messages = J::Array(vec![J::object([
                        ("type", "user".into()),
                        (
                            "message",
                            J::object([
                                ("role", "user".into()),
                                (
                                    "content",
                                    sdk_options::workflow_prompt(&string(field(&params, "prompt")))
                                        .into_json(),
                                ),
                            ]),
                        ),
                        ("parent_tool_use_id", J::Null),
                        ("origin", J::object([("kind", "human".into())])),
                    ])]);
                    (
                        "query/steer",
                        J::object([
                            ("turnId", running.query.lock().unwrap().clone().into_json()),
                            ("messages", messages),
                        ]),
                    )
                })
                .into_iter()
                .collect())
        } else {
            let thread = string(field(&params, "threadId"));
            Ok(self
                .running
                .iter()
                .filter(|r| field(&r.context, "threadId").as_js_string() == Some(&thread))
                .map(|r| {
                    r.interrupted.store(true, Ordering::Release);
                    (
                        "query/interrupt",
                        J::object([("turnId", r.query.lock().unwrap().clone().into_json())]),
                    )
                })
                .collect())
        };
        let owner = self.sender.clone();
        let bridge = self.bridge.clone();
        self.jobs.spawn(async move {
            let result: Result<Option<SdkReply>> = async {
                let mut value = None;
                for (call, params) in request? {
                    let result = bridge.call(call, params).await;
                    if method == "interrupt" {
                        continue;
                    }
                    value = Some(result?);
                }
                if method == "usage" {
                    if let Some(reply) = &mut value {
                        reply.value = if field(reply, "rate_limits_available")
                            .as_bool()
                            .unwrap_or(false)
                        {
                            reply.value.remove("rate_limits").unwrap_or(J::Null)
                        } else {
                            J::Null
                        };
                    }
                }
                Ok(value)
            }
            .await;
            let _ = owner.send(Input::RuntimeReply { id, result }).await;
        });
    }
    fn bridge_event(
        &mut self,
        mut frame: SdkEvent,
    ) -> Result<Option<(mpsc::Sender<TurnInput>, TurnInput)>> {
        if is(field(&frame, "type"), "transportError") {
            return Err(AdapterError::new(text(field(&frame, "message"))));
        }
        let turn = string(field(&frame, "turnId"));
        if is(field(&frame, "type"), "permission") {
            let callback_id = field(&frame, "callbackId")
                .as_f64()
                .filter(|n| safe_integer(*n) && *n >= 0.0)
                .ok_or_else(|| AdapterError::new("Invalid SDK callback identifier"))?
                as u64;
            let Some(running) = self
                .running
                .iter()
                .find(|r| *r.query.lock().unwrap() == turn)
            else {
                self.sdk_reply(
                    callback_id,
                    Ok(J::object([
                        ("behavior", "deny".into()),
                        ("message", "turn already finished".into()),
                    ])),
                    None,
                );
                return Ok(None);
            };
            let turn = running.id.clone();
            let query = string(field(&frame, "turnId"));
            let callback_identity = J::object([("callbackId", (callback_id as f64).into())]);
            let input = frame.take_input();
            let callback = ask::callback(
                &running.context,
                field(&frame, "toolName"),
                input,
                field(&frame, "toolUseId"),
                running.auto_allow,
                &self.adapter.ids,
            );
            let effects = match callback {
                Callback::Immediate(result) => {
                    self.sdk_reply(callback_id, Ok(result), None);
                    return Ok(None);
                }
                Callback::Permission { event, input } => {
                    self.retain_callback(
                        callback_id,
                        Pending {
                            turn: turn.clone(),
                            query,
                            kind: PendingKind::Permission(input),
                            _retained: None,
                        },
                    )?;
                    self.adapter
                        .permission_request(&turn, &event, callback_identity)?
                }
                Callback::UserInput {
                    event,
                    input,
                    questions,
                } => {
                    self.retain_callback(
                        callback_id,
                        Pending {
                            turn: turn.clone(),
                            query,
                            kind: PendingKind::UserInput { input, questions },
                            _retained: None,
                        },
                    )?;
                    self.adapter
                        .user_input_request(&turn, &event, callback_identity)?
                }
            };
            self.effects(effects, None)?;
            return Ok(None);
        }
        if is(field(&frame, "type"), "callbackCancelled") {
            if let Some(id) = field(&frame, "callbackId")
                .as_f64()
                .filter(|n| safe_integer(*n) && *n >= 0.0)
                .map(|n| n as u64)
            {
                if self
                    .pending
                    .get(&id)
                    .is_some_and(|pending| pending.query == turn)
                {
                    let pending = self.pending.remove(&id).unwrap();
                    let effects = self.adapter.cancel_callbacks(&pending.turn, Some(id))?;
                    self.effects(effects, None)?;
                }
            }
            return Ok(None);
        }
        let Some(running) = self
            .running
            .iter()
            .find(|r| *r.query.lock().unwrap() == turn)
        else {
            return Ok(None);
        };
        if !is(field(&frame, "type"), "message") && !is(field(&frame, "type"), "end") {
            return Ok(None);
        }
        Ok(Some((
            running.sender.clone(),
            TurnInput::Frame(turn, frame),
        )))
    }
    fn retain_callback(&mut self, id: u64, mut pending: Pending) -> Result<()> {
        if self.pending.contains_key(&id) {
            return Err(AdapterError::new("Repeated SDK callback identifier"));
        }
        let (input, questions) = match &pending.kind {
            PendingKind::Permission(input) => (input, None),
            PendingKind::UserInput { input, questions } => (input, Some(questions)),
        };
        let mut bytes = std::mem::size_of::<Pending>()
            .saturating_add(256)
            .saturating_add(input.retained_bytes())
            .saturating_add(pending.turn.0.capacity().saturating_mul(2))
            .saturating_add(pending.query.0.capacity().saturating_mul(2));
        if let Some(questions) = questions {
            bytes = bytes.saturating_add(
                questions
                    .capacity()
                    .saturating_mul(std::mem::size_of::<J>()),
            );
            for question in questions {
                bytes = bytes.saturating_add(
                    question
                        .retained_bytes()
                        .saturating_sub(std::mem::size_of::<J>()),
                );
            }
        }
        pending._retained = Some(self.adapter.reserve_callback_input(bytes)?);
        self.pending.insert(id, pending);
        Ok(())
    }
    fn permission_reply(&mut self, callback: J, result: J) {
        let Some(id) = field(&callback, "callbackId").as_f64().map(|n| n as u64) else {
            return;
        };
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        let error = field(&result, "error");
        let response = match pending.kind {
            PendingKind::Permission(input) => {
                if error.is_null() {
                    Ok(ask::permission_result(input, &result, false))
                } else {
                    Err(AdapterError::new(text(field(error, "message"))))
                }
            }
            PendingKind::UserInput { input, questions } => {
                let response = if error.is_null() {
                    ask::user_input_result(&input, &questions, &result)
                } else {
                    Err(AdapterError::new(text(field(error, "message"))))
                };
                Ok(response.unwrap_or_else(|e| {
                    J::object([
                        ("behavior", "deny".into()),
                        (
                            "message",
                            concat(&[&"AskUserQuestion failed: ".into(), &e.message]).into_json(),
                        ),
                    ])
                }))
            }
        };
        self.sdk_reply(id, response, pending._retained)
    }
    fn sdk_reply(&mut self, id: u64, result: Result<J>, retained: Option<OwnedSemaphorePermit>) {
        let bridge = self.bridge.clone();
        let owner = self.sender.clone();
        let mut params = J::object([("callbackId", J::Number(id as f64))]);
        match result {
            Ok(value) => params.insert("result", value),
            Err(error) => params.insert(
                "error",
                J::object([
                    ("name", error.name.into()),
                    ("message", error.message.into_json()),
                ]),
            ),
        };
        self.jobs.spawn(async move {
            let result = bridge.call("permission/reply", params).await;
            drop(retained);
            if let Err(error) = result {
                // An already accepted native answer must not leave its SDK
                // callback silently pending after command admission failed.
                let _ = owner.send(Input::BridgeFailed(error)).await;
            }
        });
    }
}
fn text(value: &J) -> JsString {
    string(&coalesce(value, &"".into()))
}
