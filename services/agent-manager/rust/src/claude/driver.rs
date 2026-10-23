//! One native state owner, with independent SDK/control/diff futures. Provider
//! waits never occupy the owner's receive loop or its shutdown channel.
use super::{
    ask::{self, Callback},
    bridge::Bridge,
    error::{AdapterError, Result},
    protocol::{Adapter, Effect},
    sdk_events::SdkTurn,
    sdk_options::{self, SdkEnvironment},
    value::*,
    workflow::{EventSink, IoFuture, WorkflowFiles},
};
use ivy_contracts::{IvyJson as J, JsString};
use std::{collections::BTreeMap, sync::Arc};
use tokio::{
    sync::{mpsc, oneshot},
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
enum Input {
    Native {
        peer: JsString,
        message: J,
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
        result: Result<J>,
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
    Message(J),
    End(Option<AdapterError>),
}
struct Running {
    id: JsString,
    context: J,
    auto_allow: bool,
    sender: mpsc::Sender<TurnInput>,
}
enum PendingKind {
    Permission(J),
    UserInput { input: J, questions: Vec<J> },
}
struct Pending {
    turn: JsString,
    kind: PendingKind,
}
#[derive(Clone)]
pub struct Handle {
    sender: mpsc::Sender<Input>,
    stops: mpsc::UnboundedSender<oneshot::Sender<()>>,
}
impl Handle {
    pub async fn receive(&self, peer: JsString, message: J) -> Result<()> {
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
    events: mpsc::Receiver<J>,
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
    events: mpsc::Receiver<J>,
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
                let effects = self.adapter.runtime_reply(id, result)?;
                self.effects(effects, None)?;
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
        self.running.push(Running {
            id: turn.clone(),
            context: context.clone(),
            auto_allow: options.auto_allow,
            sender,
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
        let mut state = SdkTurn::new(
            context,
            &self.bindings.sdk.home,
            &self.bindings.sdk.environment,
            self.bindings.workflow_files.clone(),
            sink,
        );
        self.jobs.spawn(async move {
            let initial=bridge.call("query/start",options.params).await;
            if let Err(error)=initial {let _=owner.send(Input::Finished{turn,result:Err(error)}).await;return}
            let changed = state.changed();
            while !state.is_finished() {
                let result=tokio::select! {
                    input=receiver.recv()=>match input {Some(TurnInput::Message(message))=>state.message(&message).await,Some(TurnInput::End(error))=>state.end(error).await,None=>state.end(None).await},
                    _=changed.notified()=>Ok(()),
                };
                if let Err(error)=result {let _=state.end(Some(error)).await;break}
            }
            let result=state.outcome().unwrap_or_else(||Err(AdapterError::new("Claude SDK turn closed without an outcome")));
            // Complete native state independently of SDK close/abort promises.
            let _=owner.send(Input::Finished{turn:turn.clone(),result}).await;
            let _=bridge.call("query/close",J::object([("turnId",turn.into_json())])).await;
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
                            ("turnId", running.id.clone().into_json()),
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
                    (
                        "query/interrupt",
                        J::object([("turnId", r.id.clone().into_json())]),
                    )
                })
                .collect())
        };
        let owner = self.sender.clone();
        let bridge = self.bridge.clone();
        self.jobs.spawn(async move {
            let result = async {
                let mut value = J::Null;
                for (call, params) in request? {
                    let result = bridge.call(call, params).await;
                    if method == "interrupt" {
                        continue;
                    }
                    value = result?;
                }
                if method == "usage" {
                    Ok(
                        if field(&value, "rate_limits_available")
                            .as_bool()
                            .unwrap_or(false)
                        {
                            coalesce(field(&value, "rate_limits"), &J::Null)
                        } else {
                            J::Null
                        },
                    )
                } else {
                    Ok(value)
                }
            }
            .await;
            let _ = owner.send(Input::RuntimeReply { id, result }).await;
        });
    }
    fn bridge_event(&mut self, frame: J) -> Result<Option<(mpsc::Sender<TurnInput>, TurnInput)>> {
        let kind = field(&frame, "type");
        if is(kind, "transportError") {
            return Err(AdapterError::new(text(field(&frame, "message"))));
        }
        let turn = string(field(&frame, "turnId"));
        if is(kind, "permission") {
            let callback_id = field(&frame, "callbackId")
                .as_f64()
                .filter(|n| safe_integer(*n) && *n >= 0.0)
                .ok_or_else(|| AdapterError::new("Invalid SDK callback identifier"))?
                as u64;
            let Some(running) = self.running.iter().find(|r| r.id == turn) else {
                self.sdk_reply(
                    callback_id,
                    Ok(J::object([
                        ("behavior", "deny".into()),
                        ("message", "turn already finished".into()),
                    ])),
                );
                return Ok(None);
            };
            let callback = ask::callback(
                &running.context,
                field(&frame, "toolName"),
                field(&frame, "input"),
                field(&frame, "toolUseId"),
                running.auto_allow,
                &self.adapter.ids,
            );
            let effects = match callback {
                Callback::Immediate(result) => {
                    self.sdk_reply(callback_id, Ok(result));
                    return Ok(None);
                }
                Callback::Permission { event, input } => {
                    self.pending.insert(
                        callback_id,
                        Pending {
                            turn: turn.clone(),
                            kind: PendingKind::Permission(input),
                        },
                    );
                    self.adapter
                        .permission_request(&turn, &event, frame.clone())?
                }
                Callback::UserInput {
                    event,
                    input,
                    questions,
                } => {
                    self.pending.insert(
                        callback_id,
                        Pending {
                            turn: turn.clone(),
                            kind: PendingKind::UserInput { input, questions },
                        },
                    );
                    self.adapter
                        .user_input_request(&turn, &event, frame.clone())?
                }
            };
            self.effects(effects, None)?;
            return Ok(None);
        }
        if is(kind, "callbackCancelled") {
            if let Some(id) = field(&frame, "callbackId").as_f64() {
                self.pending.remove(&(id as u64));
            }
            return Ok(None);
        }
        let Some(running) = self.running.iter().find(|r| r.id == turn) else {
            return Ok(None);
        };
        let input = if is(kind, "message") {
            TurnInput::Message(field(&frame, "message").clone())
        } else if is(kind, "end") {
            TurnInput::End(if field(&frame, "error").is_null() {
                None
            } else {
                Some(AdapterError::new(text(field(
                    field(&frame, "error"),
                    "message",
                ))))
            })
        } else {
            return Ok(None);
        };
        Ok(Some((running.sender.clone(), input)))
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
                    Ok(ask::permission_result(&input, &result, false))
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
        self.sdk_reply(id, response)
    }
    fn sdk_reply(&mut self, id: u64, result: Result<J>) {
        let bridge = self.bridge.clone();
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
            let _ = bridge.call("permission/reply", params).await;
        });
    }
}
fn text(value: &J) -> JsString {
    string(&coalesce(value, &"".into()))
}
