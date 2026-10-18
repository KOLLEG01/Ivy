use super::{Failure, Handle, Store, equal, field, id, outcome, pin, string, text};
use ivy_agent_manager::{
    native_clock::{NATIVE_CLOCK_PRINCIPAL, answer_native_clock},
    native_dispatch::{NativeDispatch, Observations, caller_identity, journal_events},
    native_journal::{Identity, Limits, NativeJournal, Owner},
    native_operations::{
        NativeOperationStore, NativeOperations, ReceiptWork, SqliteOperationStore,
    },
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    hash::hash_json,
    json::{parse, stringify},
};
use ivy_host_runtime::codex_rpc::{NativeNotification, NativeRpc};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    sync::{mpsc, watch},
};

fn catalog() -> IvyJson {
    parse(
        include_str!("../../specs/native/codex-0.154.0/catalog.json"),
        4 * 1024 * 1024,
    )
    .unwrap()
}
fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let out = Command::new("node")
            .arg("tests/fixtures/agent-native-dispatch-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        parse(std::str::from_utf8(&out.stdout).unwrap(), 4 * 1024 * 1024).unwrap()
    })
}
#[derive(Default)]
struct Trace {
    inputs: Vec<IvyJson>,
    notifications: Vec<IvyJson>,
    closes: Vec<IvyJson>,
    frames: Vec<IvyJson>,
    errors: Vec<IvyJson>,
}
struct Peer {
    output: DuplexStream,
    frames: mpsc::UnboundedReceiver<IvyJson>,
    reader: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn sent(&mut self) -> IvyJson {
        tokio::time::timeout(Duration::from_secs(5), self.frames.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn emit(&mut self, frame: &IvyJson) {
        self.output
            .write_all(format!("{}\n", stringify(frame, 32 * 1024 * 1024).unwrap()).as_bytes())
            .await
            .unwrap();
    }
}
fn rpc(events: ivy_host_runtime::codex_rpc::Events, trace: Arc<Mutex<Trace>>) -> (NativeRpc, Peer) {
    let (input, sent) = tokio::io::duplex(65536);
    let (emit, output) = tokio::io::duplex(65536);
    let rpc = NativeRpc::start(&catalog(), input, output, events).unwrap();
    let (sender, frames) = mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        let mut input = BufReader::new(sent);
        loop {
            let mut line = String::new();
            if input.read_line(&mut line).await.unwrap() == 0 {
                return;
            }
            let frame = parse(&line, 32 * 1024 * 1024).unwrap();
            trace.lock().unwrap().frames.push(frame.clone());
            if sender.send(frame).is_err() {
                return;
            }
        }
    });
    (
        rpc,
        Peer {
            output: emit,
            frames,
            reader,
        },
    )
}
struct Fixture {
    store: Arc<Store>,
    operations: NativeOperations,
    dispatch: NativeDispatch,
    rpc: NativeRpc,
    peer: Peer,
    receipts: ReceiptWork,
    trace: Arc<Mutex<Trace>>,
    closing: Arc<AtomicBool>,
    observed: watch::Receiver<u64>,
    handles: HashMap<String, Handle>,
    last: Option<IvyJson>,
}
impl Fixture {
    fn new(row: &IvyJson) -> Self {
        let store = Arc::new(Store::new());
        Self::with_store(row, store.clone(), store)
    }
    fn with_store(
        row: &IvyJson,
        store: Arc<Store>,
        durable: Arc<dyn NativeOperationStore>,
    ) -> Self {
        let catalog = catalog();
        let at = row
            .get("time")
            .and_then(IvyJson::as_js_string)
            .cloned()
            .unwrap_or("2026-10-18T00:00:00.000Z".into());
        let mut memory = NativeJournal::with_clock(
            Owner {
                service_node_id: "agent".into(),
                native_version: text(&catalog, "version").clone(),
                native_executable_hash: text(&catalog, "nativeExecutableHash").clone(),
            },
            Limits {
                max_operations: row
                    .get("maxOperations")
                    .and_then(IvyJson::as_f64)
                    .unwrap_or(100.0) as usize,
                max_journal_bytes: 128 * 1024 * 1024,
                max_pending_inputs: 16,
            },
            Arc::new(move || at.clone()),
        )
        .unwrap();
        memory.begin_epoch(&"live".into()).unwrap();
        let operations = NativeOperations::new(durable, Arc::new(Mutex::new(memory)));
        let trace = Arc::new(Mutex::new(Trace::default()));
        let receipts = operations.reconcile_terminals(Arc::new({
            let trace = trace.clone();
            move |error| trace.lock().unwrap().errors.push(error.code.into())
        }));
        let (notify, observed) = watch::channel(0);
        let observations = Observations {
            on_input: Arc::new({
                let trace = trace.clone();
                let notify = notify.clone();
                move |input| {
                    let trace = trace.clone();
                    let notify = notify.clone();
                    Box::pin(async move {
                        trace.lock().unwrap().inputs.push(input);
                        notify.send_modify(|n| *n += 1);
                        Ok(())
                    })
                }
            }),
            on_notification: Arc::new({
                let trace = trace.clone();
                let notify = notify.clone();
                move |value: NativeNotification| {
                    let trace = trace.clone();
                    let notify = notify.clone();
                    Box::pin(async move {
                        trace.lock().unwrap().notifications.push(IvyJson::object([
                            ("method", IvyJson::String(value.method)),
                            ("params", value.params),
                        ]));
                        notify.send_modify(|n| *n += 1);
                        Ok(())
                    })
                }
            }),
            on_close: Arc::new({
                let trace = trace.clone();
                move |code| {
                    let trace = trace.clone();
                    let notify = notify.clone();
                    Box::pin(async move {
                        trace.lock().unwrap().closes.push(code.into());
                        notify.send_modify(|n| *n += 1);
                        Ok(())
                    })
                }
            }),
        };
        let (rpc, peer) = rpc(
            journal_events("agent".into(), "live".into(), &operations, observations),
            trace.clone(),
        );
        let closing = Arc::new(AtomicBool::new(false));
        let dispatch = NativeDispatch::new(
            rpc.clone(),
            "live".into(),
            operations.clone(),
            closing.clone(),
        );
        Self {
            store,
            operations,
            dispatch,
            rpc,
            peer,
            receipts,
            trace,
            closing,
            observed,
            handles: HashMap::new(),
            last: None,
        }
    }
    async fn emit(&mut self, frame: &IvyJson) {
        let previous = *self.observed.borrow_and_update();
        self.peer.emit(frame).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while *self.observed.borrow_and_update() == previous {
                self.observed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
    async fn execute(&mut self, c: &IvyJson) -> IvyResult<IvyJson> {
        match string(c, "action").as_str() {
            "invoke" => {
                let context = field(c, "context");
                let identity = caller_identity(
                    text(context, "callerPrincipalId").clone(),
                    context
                        .get("operationId")
                        .and_then(IvyJson::as_js_string)
                        .cloned(),
                );
                let dispatch = self.dispatch.clone();
                let method = text(c, "method").clone();
                let params = field(c, "params").clone();
                self.handles.insert(
                    string(c, "handle"),
                    Handle::track(async move {
                        dispatch.invoke(identity?, method, params)?.wait().await
                    }),
                );
            }
            "answer" => {
                let answer = field(c, "input");
                let context = field(c, "context");
                if let Some(outer) = context
                    .get("operationId")
                    .and_then(IvyJson::as_js_string)
                    .filter(|id| !id.0.is_empty())
                {
                    if Some(outer) != answer.get("operationId").and_then(IvyJson::as_js_string) {
                        return Err(IvyError::new(
                            "mutation_conflict",
                            "Outer and answer operation IDs must match.",
                        ));
                    }
                }
                let identity = caller_identity(
                    text(context, "callerPrincipalId").clone(),
                    Some(text(answer, "operationId").clone()),
                )?;
                let run = self.dispatch.answer(
                    identity,
                    field(answer, "identity").clone(),
                    field(answer, "reply").clone(),
                )?;
                self.handles
                    .insert(string(c, "handle"), Handle::track(run.wait()));
            }
            "sent" => {
                let frame = self.peer.sent().await;
                self.last = Some(frame.clone());
                return Ok(frame);
            }
            "reply" => {
                let mut frame = field(c, "reply").clone();
                frame.insert("id", field(self.last.as_ref().unwrap(), "id").clone());
                self.peer.emit(&frame).await;
            }
            "emit" => {
                let mut frame = field(c, "frame").clone();
                if frame.get("id") == Some(&IvyJson::from("$last")) {
                    frame.insert("id", field(self.last.as_ref().unwrap(), "id").clone());
                }
                self.emit(&frame).await;
            }
            "wait" => return Ok(self.handles[&string(c, "handle")].wait().await),
            "close" => self.rpc.close(string(c, "code")).await,
            "drain" => self.receipts.drain().await,
            "writeFailure" => {
                self.store.state.lock().unwrap().failure = Some(Failure {
                    code: "service_unavailable".into(),
                    after_commit: false,
                })
            }
            "closing" => self.closing.store(true, Ordering::SeqCst),
            "noop" => {}
            "read" => {
                return self
                    .operations
                    .read(&Identity::from_operation(field(c, "identity"))?)
                    .await
                    .map(|v| v.unwrap_or(IvyJson::Null));
            }
            "clock" => {
                let mut input = self
                    .trace
                    .lock()
                    .unwrap()
                    .inputs
                    .last()
                    .cloned()
                    .unwrap_or_else(|| IvyJson::object([] as [(&str, IvyJson); 0]));
                if let Some(patch) = c.get("patch") {
                    for (key, value) in patch.as_object().unwrap() {
                        input.insert(key.clone(), value.clone());
                    }
                }
                answer_native_clock(self.operations.journal(), &self.rpc, &input).await?;
            }
            "occupy" => {
                self.operations.journal().lock().unwrap().accept(
                    &Identity {
                        caller_principal_id: "fixture".into(),
                        operation_id: "occupied".into(),
                    },
                    &"turn/interrupt".into(),
                    &IvyJson::object([] as [(&str, IvyJson); 0]),
                )?;
            }
            "snapshot" => {
                tokio::task::yield_now().await;
                let memory = self.operations.journal();
                let memory = memory.lock().unwrap();
                let state = self.store.state.lock().unwrap();
                let trace = self.trace.lock().unwrap();
                let clocks = trace
                    .inputs
                    .iter()
                    .filter(|input| input.get("method") == Some(&IvyJson::from("currentTime/read")))
                    .map(|input| {
                        memory
                            .get(&Identity {
                                caller_principal_id: NATIVE_CLOCK_PRINCIPAL.into(),
                                operation_id: format!(
                                    "clock:{}",
                                    &hash_json(field(input, "identity")).unwrap()[7..]
                                )
                                .into(),
                            })
                            .unwrap()
                            .unwrap_or(IvyJson::Null)
                    })
                    .collect();
                return Ok(IvyJson::object([
                    (
                        "rows",
                        IvyJson::Array(
                            state
                                .rows
                                .iter()
                                .map(|(key, saved)| {
                                    IvyJson::object([
                                        ("key", key.clone().into()),
                                        ("value", saved.value.clone()),
                                        ("pin", pin(&saved.pin)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "inputs",
                        memory.inputs(&IvyJson::object([("includeExpired", true.into())]))?,
                    ),
                    ("clockOperations", IvyJson::Array(clocks)),
                    ("frames", IvyJson::Array(trace.frames.clone())),
                    ("notifications", IvyJson::Array(trace.notifications.clone())),
                    ("closes", IvyJson::Array(trace.closes.clone())),
                    ("errors", IvyJson::Array(trace.errors.clone())),
                    (
                        "epoch",
                        memory
                            .epoch()
                            .cloned()
                            .map(IvyJson::String)
                            .unwrap_or(IvyJson::Null),
                    ),
                ]));
            }
            action => panic!("unknown dispatch fixture action {action}"),
        }
        Ok(IvyJson::Null)
    }
    async fn finish(self) {
        self.rpc.close("fixture_stop").await;
        self.receipts.drain().await;
        self.peer.reader.abort();
    }
}

#[tokio::test]
async fn real_rpc_dispatch_answers_clock_and_terminal_hooks_match_original_handlers() {
    let rows = field(data(), "cases").as_array().unwrap();
    assert_eq!(rows.len(), 30);
    for row in rows {
        let mut f = Fixture::new(row);
        for (index, command) in field(row, "commands")
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let label = format!(
                "{} step {index}: {}",
                string(row, "name"),
                string(command, "action")
            );
            let result = tokio::time::timeout(Duration::from_secs(5), f.execute(command))
                .await
                .expect(&label);
            let actual = if string(command, "action") == "wait" {
                result.unwrap()
            } else {
                outcome(result)
            };
            equal(
                &actual,
                &field(row, "expected").as_array().unwrap()[index],
                &label,
            );
        }
        tokio::time::timeout(Duration::from_secs(5), f.finish())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn terminal_callback_does_not_retain_a_dropped_coordinator_or_sqlite_owner() {
    let store = Arc::new(Store::new());
    let memory = super::memory(Some(&"live".into()));
    let weak = Arc::downgrade(&store);
    let operations = NativeOperations::new(store.clone(), memory.clone());
    let work = operations.reconcile_terminals(Arc::new(|_| panic!("no receipt should run")));
    drop(operations);
    drop(store);
    assert!(
        weak.upgrade().is_none(),
        "journal terminal callback must not retain the coordinator/store"
    );
    memory
        .lock()
        .unwrap()
        .accept(&id("after-owner"), &"thread/start".into(), &IvyJson::Null)
        .unwrap();
    memory
        .lock()
        .unwrap()
        .reject_before_dispatch(&id("after-owner"), &"gone".into())
        .unwrap();
    work.drain().await;
}

#[tokio::test]
async fn dropping_clock_waiter_preserves_its_original_answer() {
    let mut f = Fixture::new(&IvyJson::Null);
    f.emit(&IvyJson::object([
        ("id", "clock".into()),
        ("method", "currentTime/read".into()),
        ("params", IvyJson::object([("threadId", "thread".into())])),
    ]))
    .await;
    let input = f.trace.lock().unwrap().inputs.last().unwrap().clone();
    // The service work tracker needs an owned future after its callback returns.
    let waiter = tokio::spawn(answer_native_clock(f.operations.journal(), &f.rpc, &input));
    waiter.abort();
    let sent = f.peer.sent().await;
    assert_eq!(sent.get("id"), Some(&IvyJson::from("clock")));
    assert_eq!(
        sent.get("result"),
        Some(&IvyJson::object([(
            "currentTimeAt",
            IvyJson::Number(1792281600.0)
        )]))
    );
    assert_eq!(
        f.operations
            .journal()
            .lock()
            .unwrap()
            .input(field(&input, "identity"))
            .unwrap()
            .unwrap()
            .get("state"),
        Some(&IvyJson::from("answering"))
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_result_and_answer_resolution_persist_through_real_sqlite() {
    let root = super::super::Fixture::new();
    let state = Arc::new(Mutex::new(root.open(10, 128 * 1024 * 1024)));
    let durable = Arc::new(SqliteOperationStore::new(state.clone()).unwrap());
    let mut f = Fixture::with_store(&IvyJson::Null, Arc::new(Store::new()), durable.clone());
    let identity = id(&format!(
        "epoch:{}:turn",
        chrono::Utc::now().timestamp_millis()
    ));
    let run = f
        .dispatch
        .invoke(
            identity.clone(),
            "turn/start".into(),
            IvyJson::object([("threadId", "thread".into())]),
        )
        .unwrap();
    let sent = f.peer.sent().await;
    let key = hash_json(&identity.value()).unwrap();
    assert_eq!(
        durable
            .read(key.clone())
            .await
            .unwrap()
            .unwrap()
            .value
            .get("phase"),
        Some(&IvyJson::from("dispatched"))
    );
    f.peer
        .emit(&IvyJson::object([
            ("id", field(&sent, "id").clone()),
            (
                "result",
                IvyJson::object([("turn", IvyJson::object([("id", "turn".into())]))]),
            ),
        ]))
        .await;
    run.wait().await.unwrap();
    f.receipts.drain().await;
    assert_eq!(
        state
            .lock()
            .unwrap()
            .completion_caller("thread", "turn")
            .unwrap()
            .as_deref(),
        Some("caller")
    );
    let input_identity = IvyJson::object([
        ("serviceNodeId", "agent".into()),
        ("epoch", "live".into()),
        ("requestId", IvyJson::Number(7.0)),
    ]);
    f.emit(&IvyJson::object([
        ("id", IvyJson::Number(7.0)),
        ("method", "item/tool/requestUserInput".into()),
        ("params", IvyJson::object([("threadId", "thread".into())])),
    ]))
    .await;
    let answer_id = id(&format!(
        "epoch:{}:answer",
        chrono::Utc::now().timestamp_millis()
    ));
    let result = f
        .dispatch
        .answer(
            answer_id.clone(),
            input_identity,
            IvyJson::object([(
                "error",
                IvyJson::object([
                    ("code", IvyJson::Number(-32000.0)),
                    ("message", "declined".into()),
                ]),
            )]),
        )
        .unwrap();
    f.peer.sent().await;
    result.wait().await.unwrap();
    let answer_key = hash_json(&answer_id.value()).unwrap();
    assert_eq!(
        durable
            .read(answer_key.clone())
            .await
            .unwrap()
            .unwrap()
            .value
            .get("phase"),
        Some(&IvyJson::from("dispatched"))
    );
    f.emit(&IvyJson::object([
        ("method", "serverRequest/resolved".into()),
        (
            "params",
            IvyJson::object([
                ("requestId", IvyJson::Number(7.0)),
                ("threadId", "thread".into()),
            ]),
        ),
    ]))
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        // On the multi-thread runtime the observer can wake this test before
        // journal_events finishes resolving the answer in the actor.
        while f
            .operations
            .journal()
            .lock()
            .unwrap()
            .get(&answer_id)
            .unwrap()
            .unwrap()
            .get("phase")
            != Some(&IvyJson::from("succeeded"))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.receipts.drain().await;
    assert_eq!(
        durable
            .read(answer_key.clone())
            .await
            .unwrap()
            .unwrap()
            .value
            .get("phase"),
        Some(&IvyJson::from("succeeded"))
    );
    assert!(f.trace.lock().unwrap().errors.is_empty());
    f.finish().await;
    drop(durable);
    super::close_state(state).await;
    let reopened = root.open(10, 128 * 1024 * 1024);
    assert_eq!(
        reopened
            .read("operation", &key)
            .unwrap()
            .unwrap()
            .value
            .get("phase"),
        Some(&IvyJson::from("succeeded"))
    );
    assert_eq!(
        reopened
            .read("operation", &answer_key)
            .unwrap()
            .unwrap()
            .value
            .get("phase"),
        Some(&IvyJson::from("succeeded"))
    );
    reopened.close().unwrap();
}
