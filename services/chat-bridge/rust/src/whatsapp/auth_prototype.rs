//! Finite NAB-3P harness. Deliberately NOT exported by the production library.
//! Integration tests include this source; startup and deployment cannot opt in.
#[path = "auth_prototype_store.rs"]
pub mod store;
#[path = "auth_prototype_wire.rs"]
mod wire;
use ivy_chat_bridge::whatsapp::auth_node::AuthLaunch;
use ivy_contracts::{IvyJson as Json, IvyResult, hash::digest};
use ivy_host_runtime::{
    exec_file::SpawnOptions,
    process::{self, Command, LaunchContext, OwnedProcess},
};
use ivy_sdk::service::ServiceFuture;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use store::{Grant, Store, encode, refused, text, unknown};
use tokio::{
    io::AsyncReadExt,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::JoinHandle,
};

#[derive(Clone)]
pub enum Reply {
    Value(String),
    LibraryError(String),
}
pub struct Invocation {
    pub id: String,
    done: watch::Receiver<Option<IvyResult<Reply>>>,
}
impl Invocation {
    pub async fn result(self) -> IvyResult<Reply> {
        observe(self.done).await
    }
}
#[derive(Clone)]
pub struct QuiescenceProof(Json);
impl QuiescenceProof {
    pub fn value(&self) -> Json {
        self.0.clone()
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
    None,
    Prepared,
    Entered,
    CommitBefore,
    CommitAfter,
    FinishBefore,
    FinishAfter,
    ResultBody,
    ProofBefore,
    ProofAfter,
}
type Completion<T> = watch::Sender<Option<IvyResult<T>>>;
enum Control {
    Run {
        kind: String,
        method: String,
        args: Json,
        reply: oneshot::Sender<IvyResult<Invocation>>,
        credit: OwnedSemaphorePermit,
    },
    Read {
        operation: Option<String>,
        reply: oneshot::Sender<IvyResult<Json>>,
    },
    Fault(Fault, oneshot::Sender<()>),
    RejectInitialization(oneshot::Sender<()>),
    Close(Completion<QuiescenceProof>),
    Retire(Completion<()>),
    Expired(String),
}
enum Output {
    Message(String, Json),
    Close,
}
struct Pending {
    grant: Grant,
    result: Completion<Reply>,
    _credit: Option<OwnedSemaphorePermit>,
}
struct Inner {
    control: mpsc::Sender<Control>,
    credits: Arc<Semaphore>,
    closing: AtomicBool,
    deadline: Duration,
    close: Mutex<Option<watch::Receiver<Option<IvyResult<QuiescenceProof>>>>>,
    retire: Mutex<Option<watch::Receiver<Option<IvyResult<()>>>>>,
    pub stats: Arc<Mutex<wire::Statistics>>,
}
#[derive(Clone)]
pub struct ProtocolAccount(Arc<Inner>);
impl ProtocolAccount {
    pub fn statistics(&self) -> wire::Statistics {
        *self.0.stats.lock().unwrap()
    }
    pub async fn launch(mut store: Store, launch: AuthLaunch) -> IvyResult<Self> {
        if launch.deadline.is_zero() {
            return Err(refused());
        }
        let init = store.prepare("init", "initialize", None)?;
        store.enter(&init.id)?;
        let saved = store.saved_creds()?;
        let options = SpawnOptions::for_component(&launch.artifact_root, launch.helper_layout)?;
        let worker = ivy_host_runtime::paths::realpath(
            &launch
                .artifact_root
                .join("tests/rust/fixtures/chat-auth-protocol-worker.mjs"),
        )
        .map_err(|_| unknown())?;
        if !ivy_host_runtime::paths::inside(&launch.artifact_root, &worker)
            .map_err(|_| unknown())?
        {
            return Err(unknown());
        }
        let mut child = process::spawn(
            &Command {
                executable: "$node".into(),
                args: vec![worker.into_os_string(), store.epoch.clone().into()],
                cwd: None,
                timeout_ms: 0,
            },
            &launch.artifact_root,
            &BTreeMap::from([("$node".into(), launch.node.executable)]),
            LaunchContext {
                environment: launch.environment,
                job_launcher: options.job_launcher,
                raw_output: true,
                ..Default::default()
            },
        )
        .await?;
        let diagnostics = child.stderr.take().ok_or_else(unknown)?;
        let diagnostics = tokio::spawn(async move {
            let mut pipe = diagnostics;
            let mut buffer = [0; 8192];
            loop {
                match pipe.read(&mut buffer).await {
                    Ok(0) => return Ok(()),
                    Ok(_) => {}
                    Err(_) => return Err(unknown()),
                }
            }
        });
        let (reader, mut writer, stats) = wire::pipes(
            child.stdout.take().ok_or_else(unknown)?,
            child.stdin.take().ok_or_else(unknown)?,
            &store.epoch,
        );
        let (out, mut writes) = mpsc::channel(1);
        let (frames, reads) = mpsc::channel(1);
        let (control, commands) = mpsc::channel(1);
        let writer = tokio::spawn(async move {
            while let Some(work) = writes.recv().await {
                match work {
                    Output::Message(id, value) => writer.send(&id, value).await?,
                    Output::Close => return writer.close().await,
                }
            }
            Err(unknown())
        });
        let reader = tokio::spawn(async move {
            let mut reader = reader;
            loop {
                let message = reader.receive().await;
                let ended = !matches!(&message, Ok(Some(_)));
                if frames.send(message).await.is_err() || ended {
                    break;
                }
            }
        });
        let (done, receive) = watch::channel(None);
        let init_id = init.id.clone();
        let initial = Json::object([
            ("kind", "command".into()),
            ("method", "initialize".into()),
            ("id", init.id.as_str().into()),
            ("token", init.token.as_str().into()),
            ("arguments", saved),
        ]);
        out.send(Output::Message(init.id.clone(), initial))
            .await
            .map_err(|_| unknown())?;
        let actor = Actor {
            store,
            child,
            diagnostics,
            writer,
            reader,
            out,
            reads,
            commands,
            pending: HashMap::from([(
                init.id.clone(),
                Pending {
                    grant: init,
                    result: done,
                    _credit: None,
                },
            )]),
            next_callback: 0,
            close: None,
            close_result: None,
            stop_sent: false,
            stop_ack: false,
            raw_drained: false,
            retire_wait: None,
            fault: Fault::None,
            failed: false,
            deadline: launch.deadline,
        };
        tokio::spawn(actor.run());
        let owner = Self(Arc::new(Inner {
            control,
            credits: Arc::new(Semaphore::new(4)),
            closing: AtomicBool::new(false),
            deadline: launch.deadline,
            close: Mutex::new(None),
            retire: Mutex::new(None),
            stats,
        }));
        let initialized = owner.wait(init_id, receive).await?;
        let ready = match initialized {
            Reply::Value(value) => ivy_contracts::json::parse(&value, usize::MAX)
                .ok()
                .is_some_and(|value| {
                    value.get("baileys") == Some(&Json::from("7.0.0-rc13"))
                        && value.get("libsignal") == Some(&Json::from("6.0.0"))
                        && value.get("liveSocket").and_then(Json::as_bool) == Some(false)
                        && value.get("network").and_then(Json::as_bool) == Some(false)
                }),
            Reply::LibraryError(_) => false,
        };
        if !ready {
            let (done, wait) = oneshot::channel();
            let _ = owner
                .0
                .control
                .send(Control::RejectInitialization(done))
                .await;
            let _ = wait.await;
            return Err(unknown());
        }
        Ok(owner)
    }
    async fn wait(
        &self,
        id: String,
        receive: watch::Receiver<Option<IvyResult<Reply>>>,
    ) -> IvyResult<Reply> {
        match tokio::time::timeout(self.0.deadline, observe(receive)).await {
            Ok(result) => result,
            Err(_) => {
                let _ = self.0.control.send(Control::Expired(id)).await;
                Err(unknown())
            }
        }
    }
    pub fn start(&self, kind: &str, method: &str, args: Json) -> ServiceFuture<Invocation> {
        let owner = self.clone();
        let kind = kind.to_owned();
        let method = method.to_owned();
        Box::pin(async move {
            if owner.0.closing.load(Ordering::Acquire) {
                return Err(refused());
            }
            let credit = owner
                .0
                .credits
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| unknown())?;
            if owner.0.closing.load(Ordering::Acquire) {
                return Err(refused());
            }
            let (reply, receive) = oneshot::channel();
            owner
                .0
                .control
                .send(Control::Run {
                    kind,
                    method,
                    args,
                    reply,
                    credit,
                })
                .await
                .map_err(|_| unknown())?;
            receive.await.map_err(|_| unknown())?
        })
    }
    pub async fn call(&self, kind: &str, method: &str, args: Json) -> IvyResult<(String, Reply)> {
        let call = self.start(kind, method, args).await?;
        let id = call.id.clone();
        let reply = self.wait(id.clone(), call.done).await?;
        Ok((id, reply))
    }
    pub async fn read(&self, id: Option<&str>) -> IvyResult<Json> {
        let (reply, receive) = oneshot::channel();
        self.0
            .control
            .send(Control::Read {
                operation: id.map(str::to_owned),
                reply,
            })
            .await
            .map_err(|_| unknown())?;
        receive.await.map_err(|_| unknown())?
    }
    pub async fn fault(&self, fault: Fault) {
        let (reply, receive) = oneshot::channel();
        self.0
            .control
            .send(Control::Fault(fault, reply))
            .await
            .unwrap();
        receive.await.unwrap();
    }
    pub fn quiesce(&self) -> ServiceFuture<QuiescenceProof> {
        self.0.closing.store(true, Ordering::Release);
        let mut close = self.0.close.lock().unwrap();
        let receive = if let Some(value) = &*close {
            value.clone()
        } else {
            let (done, receive) = watch::channel(None);
            *close = Some(receive.clone());
            let sender = self.0.control.clone();
            let deadline = self.0.deadline;
            tokio::spawn(async move {
                let _ = sender.send(Control::Close(done)).await;
                tokio::time::sleep(deadline).await;
                let _ = sender.send(Control::Expired("close".into())).await;
            });
            receive
        };
        Box::pin(observe(receive))
    }
    pub fn retire(&self) -> ServiceFuture<()> {
        let mut retire = self.0.retire.lock().unwrap();
        let receive = if let Some(value) = &*retire {
            value.clone()
        } else {
            let (done, receive) = watch::channel(None);
            *retire = Some(receive.clone());
            let sender = self.0.control.clone();
            tokio::spawn(async move {
                let _ = sender.send(Control::Retire(done)).await;
            });
            receive
        };
        Box::pin(observe(receive))
    }
}
async fn observe<T: Clone>(mut receive: watch::Receiver<Option<IvyResult<T>>>) -> IvyResult<T> {
    loop {
        if let Some(value) = receive.borrow_and_update().clone() {
            return value;
        }
        receive.changed().await.map_err(|_| unknown())?;
    }
}
struct Actor {
    store: Store,
    child: OwnedProcess,
    diagnostics: JoinHandle<IvyResult<()>>,
    writer: JoinHandle<IvyResult<()>>,
    reader: JoinHandle<()>,
    out: mpsc::Sender<Output>,
    reads: mpsc::Receiver<IvyResult<Option<(String, Json)>>>,
    commands: mpsc::Receiver<Control>,
    pending: HashMap<String, Pending>,
    next_callback: u64,
    close: Option<(Grant, Completion<QuiescenceProof>)>,
    close_result: Option<IvyResult<QuiescenceProof>>,
    stop_sent: bool,
    stop_ack: bool,
    raw_drained: bool,
    retire_wait: Option<Completion<()>>,
    fault: Fault,
    failed: bool,
    deadline: Duration,
}
impl Actor {
    async fn run(mut self) {
        loop {
            tokio::select! {
                command=self.commands.recv()=>match command{Some(command)=>{if self.command(command).await.is_err(){self.fail().await;}},None=>{self.fail().await;break;}},
                frame=self.reads.recv(),if !self.raw_drained=>{match frame{Some(Ok(Some((id,value))))=>{if !self.failed&&self.message(id,value).await.is_err(){self.fail().await;}},Some(Ok(None))=>{self.raw_drained=true;if self.finish_close().await.is_err(){self.fail().await;}},_=>{self.raw_drained=true;self.fail().await;}}}
            }
            if !self.failed && self.maybe_stop().await.is_err() {
                self.fail().await;
            }
        }
        self.reader.abort();
    }
    fn send(&self, id: String, value: Json) -> ServiceFuture<()> {
        let output = self.out.clone();
        Box::pin(async move {
            output
                .send(Output::Message(id, value))
                .await
                .map_err(|_| unknown())
        })
    }
    async fn command(&mut self, command: Control) -> IvyResult<()> {
        match command {
            Control::Read { operation, reply } => {
                let _ = reply.send(
                    operation.map_or_else(|| self.store.owner(), |id| self.store.receipt(&id)),
                );
            }
            Control::Fault(fault, done) => {
                self.fault = fault;
                let _ = done.send(());
            }
            Control::RejectInitialization(done) => {
                self.fail().await;
                let _ = done.send(());
            }
            Control::Run {
                kind,
                method,
                args,
                reply,
                credit,
            } => {
                if self.failed || self.store.closing {
                    let _ = reply.send(Err(if self.failed { unknown() } else { refused() }));
                    return Ok(());
                }
                let grant = match self.store.prepare(&kind, &method, None) {
                    Ok(grant) => grant,
                    Err(e) => {
                        let _ = reply.send(Err(e));
                        return Ok(());
                    }
                };
                let (done, receive) = watch::channel(None);
                let id = grant.id.clone();
                let _ = reply.send(Ok(Invocation {
                    id: id.clone(),
                    done: receive,
                }));
                self.pending.insert(
                    id.clone(),
                    Pending {
                        grant,
                        result: done,
                        _credit: Some(credit),
                    },
                );
                if self.fault == Fault::Prepared {
                    return Err(unknown());
                }
                self.store.enter(&id)?;
                if self.fault == Fault::Entered {
                    return Err(unknown());
                }
                let token = self.pending[&id].grant.token.clone();
                self.send(
                    id.clone(),
                    Json::object([
                        ("kind", "command".into()),
                        ("id", id.as_str().into()),
                        ("token", token.into()),
                        ("method", method.into()),
                        ("arguments", args),
                    ]),
                )
                .await?;
            }
            Control::Close(done) => {
                if let Some(result) = &self.close_result {
                    done.send_replace(Some(result.clone()));
                    return Ok(());
                }
                if self.failed {
                    done.send_replace(Some(Err(unknown())));
                    return Ok(());
                }
                let grant = self.store.start_close()?;
                self.close = Some((grant, done));
                self.send("control".into(), Json::object([("kind", "closing".into())]))
                    .await?;
            }
            Control::Retire(done) => {
                if matches!(&self.close_result, Some(Ok(_))) {
                    done.send_replace(Some(self.store.retire()));
                } else if self.close_result.is_none() && self.close.is_some() && !self.failed {
                    self.retire_wait = Some(done);
                } else {
                    done.send_replace(Some(Err(unknown())));
                    if self.close.is_none() && !self.failed {
                        return Err(unknown());
                    }
                }
            }
            Control::Expired(id) => {
                if (id == "close" && self.close_result.is_none()) || self.pending.contains_key(&id)
                {
                    return Err(unknown());
                }
            }
        }
        Ok(())
    }
    async fn message(&mut self, id: String, value: Json) -> IvyResult<()> {
        // A stop acknowledgement seals the protocol. Even a valid old grant
        // cannot admit another callback while the remaining pipe bytes drain.
        if self.stop_ack {
            return Err(unknown());
        }
        match text(&value, "kind")?.as_str() {
            "request" => {
                self.next_callback = self.next_callback.checked_add(1).ok_or_else(unknown)?;
                if id != format!("callback:{}", self.next_callback) {
                    return Err(unknown());
                }
                let method = text(&value, "method")?;
                let args = value.required("arguments").map_err(|_| unknown())?;
                let result = match method.as_str() {
                    "begin" => {
                        let parent = args.required("parent").map_err(|_| unknown())?;
                        let parent = if parent.is_null() {
                            None
                        } else {
                            Some((text(parent, "id")?, text(parent, "token")?))
                        };
                        let kind = text(args, "kind")?;
                        let method = text(args, "method")?;
                        if ![
                            "repository",
                            "raw-store",
                            "credential-save",
                            "inbound-crypto",
                            "background-prekey",
                            "app-state",
                        ]
                        .contains(&kind.as_str())
                        {
                            return Err(unknown());
                        }
                        let grant = self.store.prepare(
                            &kind,
                            &method,
                            parent
                                .as_ref()
                                .map(|(id, token)| (id.as_str(), token.as_str())),
                        )?;
                        self.store.enter(&grant.id)?;
                        Json::object([("id", grant.id.into()), ("token", grant.token.into())])
                    }
                    "raw" => {
                        let kind = text(args, "kind")?;
                        let id = text(args, "id")?;
                        let token = text(args, "token")?;
                        if kind == "set" && self.fault == Fault::CommitBefore {
                            return Err(unknown());
                        }
                        let ordinal_text = text(args, "ordinal")?;
                        let ordinal = ordinal_text.parse::<u64>().map_err(|_| unknown())?;
                        if ordinal.to_string() != ordinal_text {
                            return Err(unknown());
                        }
                        let result = self.store.callback(
                            &id,
                            &token,
                            ordinal,
                            &kind,
                            &text(args, "encoded")?,
                            &text(args, "digest")?,
                        )?;
                        if kind == "set" && self.fault == Fault::CommitAfter {
                            return Err(unknown());
                        }
                        result
                    }
                    "finish" => {
                        if self.fault == Fault::FinishBefore {
                            return Err(unknown());
                        }
                        let op = text(args, "id")?;
                        let token = text(args, "token")?;
                        let outcome = text(args, "outcome")?;
                        let result_digest = text(args, "digest")?;
                        let name = args
                            .get("name")
                            .filter(|v| !v.is_null())
                            .map(Json::as_str)
                            .transpose()
                            .map_err(|_| unknown())?;
                        self.store.finish(
                            &op,
                            &token,
                            &outcome,
                            &result_digest,
                            name.as_deref(),
                        )?;
                        if self.fault == Fault::FinishAfter {
                            return Err(unknown());
                        }
                        Json::Null
                    }
                    "event" => {
                        self.store
                            .check(&text(args, "id")?, &text(args, "token")?)?;
                        // Inert fixture event; ACK is deliberately not a server/disk receipt.
                        args.required("value").map_err(|_| unknown())?.clone()
                    }
                    _ => return Err(unknown()),
                };
                self.send(
                    id,
                    Json::object([("kind", "response".into()), ("result", result)]),
                )
                .await?;
            }
            "result" | "error" => {
                if text(&value, "id")? != id {
                    return Err(unknown());
                }
                let pending = self.pending.get(&id).ok_or_else(unknown)?;
                let receipt = self.store.receipt(&id)?;
                if text(&receipt, "state")? != "completed" {
                    return Err(unknown());
                }
                let (kind, body) = if text(&value, "kind")? == "result" {
                    ("value", text(&value, "encoded")?)
                } else {
                    (
                        "library_error",
                        encode(&Json::object([("name", text(&value, "name")?.into())])),
                    )
                };
                if text(&receipt, "outcome")? != kind
                    || text(&receipt, "resultDigest")? != digest(body.as_bytes())
                {
                    return Err(unknown());
                }
                if self.fault == Fault::ResultBody {
                    return Err(unknown());
                }
                let result = if kind == "value" {
                    Reply::Value(body)
                } else {
                    Reply::LibraryError(text(&value, "name")?)
                };
                pending.result.send_replace(Some(Ok(result)));
                self.pending.remove(&id);
            }
            "stopped" => {
                let (close, _) = self.close.as_ref().ok_or_else(unknown)?;
                if !self.stop_sent
                    || self.stop_ack
                    || id != close.id
                    || text(&value, "id")? != close.id
                    || !self.pending.is_empty()
                    || self.store.active() != 1
                {
                    return Err(unknown());
                }
                self.stop_ack = true;
                self.out.send(Output::Close).await.map_err(|_| unknown())?;
            }
            _ => return Err(unknown()),
        }
        Ok(())
    }
    async fn maybe_stop(&mut self) -> IvyResult<()> {
        if self.stop_sent || !self.pending.is_empty() || self.store.active() != 1 {
            return Ok(());
        }
        if let Some((grant, _)) = &self.close {
            self.stop_sent = true;
            self.send(
                grant.id.clone(),
                Json::object([
                    ("kind", "stop".into()),
                    ("id", grant.id.as_str().into()),
                    ("token", grant.token.as_str().into()),
                ]),
            )
            .await?;
        }
        Ok(())
    }
    async fn finish_close(&mut self) -> IvyResult<()> {
        if self.failed || !self.stop_ack {
            return Err(unknown());
        }
        let result = tokio::time::timeout(self.deadline, self.child.completion.wait())
            .await
            .map_err(|_| unknown())?;
        if result.exit_code != Some(0)
            || result.error_code.is_some()
            || !process::owned_process_tree_stopped(&self.child).await?
        {
            return Err(unknown());
        }
        tokio::time::timeout(self.deadline, &mut self.diagnostics)
            .await
            .map_err(|_| unknown())?
            .map_err(|_| unknown())??;
        tokio::time::timeout(self.deadline, &mut self.writer)
            .await
            .map_err(|_| unknown())?
            .map_err(|_| unknown())??;
        let (grant, done) = self.close.as_ref().ok_or_else(unknown)?;
        let proof = Json::object([
            ("epoch", self.store.epoch.as_str().into()),
            ("closeOperation", grant.id.as_str().into()),
            ("stopAck", true.into()),
            ("treeStopped", true.into()),
            ("protocolDrained", true.into()),
            ("diagnosticsDrained", true.into()),
            ("rootsSettled", self.pending.is_empty().into()),
            ("callbacksSettled", (self.store.active() == 1).into()),
            (
                "eventsSettled",
                (self.pending.is_empty() && self.store.active() == 1).into(),
            ),
            (
                "process",
                Json::object([
                    ("exitCode", Json::Number(0.0)),
                    (
                        "signal",
                        result.signal.clone().map(Json::from).unwrap_or(Json::Null),
                    ),
                    ("errorCode", Json::Null),
                ]),
            ),
        ]);
        if self.fault == Fault::ProofBefore {
            return Err(unknown());
        }
        self.store.quiesce(grant, proof.clone())?;
        if self.fault == Fault::ProofAfter {
            // Lost observer after durable proof must not erase that positive receipt.
            let result = Err(unknown());
            self.close_result = Some(result.clone());
            done.send_replace(Some(result));
            return Ok(());
        }
        let result = Ok(QuiescenceProof(proof));
        self.close_result = Some(result.clone());
        done.send_replace(Some(result));
        if let Some(done) = self.retire_wait.take() {
            done.send_replace(Some(self.store.retire()));
        }
        Ok(())
    }
    async fn fail(&mut self) {
        if self.failed {
            return;
        }
        self.failed = true;
        self.store.fence();
        // No rejected raw callback is returned into upstream commitWithRetry.
        let _ = self.child.stop(Duration::ZERO).await;
        for (_, pending) in self.pending.drain() {
            pending.result.send_replace(Some(Err(unknown())));
        }
        let result = Err(unknown());
        self.close_result = Some(result.clone());
        if let Some((_, done)) = &self.close {
            done.send_replace(Some(result));
        }
        if let Some(done) = self.retire_wait.take() {
            done.send_replace(Some(Err(unknown())));
        }
    }
}
