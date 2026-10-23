use super::snapshot;
use ivy_chat_bridge::{
    generation::{ChatGeneration, ChatGenerationLease},
    whatsapp::{
        commands::CommandFuture,
        journal::Inbox,
        journal_io::JournalIo,
        journal_owner::Journal,
        messages::Translator,
        runtime::{
            BoolFuture, RuntimeAttached, RuntimeAttachmentFactory, RuntimeCallbacks, RuntimeMedia,
            RuntimeParts, RuntimeResources, RuntimeSocket, RuntimeTimer, RuntimeTimers,
            RuntimeWork, WhatsAppRuntime, retryable_whatsapp_error,
        },
    },
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse, stringify},
};
use ivy_sdk::{clock::Clock, rpc::RequestFuture, service::ServiceFuture};
use std::{
    collections::{HashMap, VecDeque},
    process::Command,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::{Notify, watch};

fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn s(value: &Json, key: &str) -> String {
    f(value, key).as_str().unwrap()
}
fn json(raw: &str) -> Json {
    parse(raw, usize::MAX).unwrap()
}
fn result(value: IvyResult<Json>) -> Json {
    match value {
        Ok(value) => Json::object([("value", value)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.to_json()),
            ]),
        )]),
    }
}
fn unwrap(value: Json) -> IvyResult<Json> {
    if let Some(e) = value.get("throw") {
        return Err(IvyError::new(s(e, "code"), s(e, "message")).with_outcome(
            match s(e, "outcome").as_str() {
                "unknown" => Outcome::Unknown,
                "completed" => Outcome::Completed,
                _ => Outcome::NotExecuted,
            },
        ));
    }
    Ok(value)
}
struct State {
    current: Json,
    now: i64,
    events: Vec<Json>,
    counts: HashMap<String, usize>,
    gates: HashMap<String, watch::Sender<bool>>,
    ready: VecDeque<watch::Sender<bool>>,
    closing: bool,
    recovery: i64,
}
struct Probe {
    state: Mutex<State>,
    start: Instant,
    ready_changed: Notify,
}
impl Clock for Probe {
    fn utc_millis(&self) -> i64 {
        self.state.lock().unwrap().now
    }
    fn monotonic(&self) -> Instant {
        self.start
    }
}
impl Probe {
    fn emit(&self, key: &str, args: Vec<Json>) {
        let mut event = vec![key.into()];
        event.extend(args);
        self.state.lock().unwrap().events.push(Json::Array(event));
    }
    fn take(&self, key: &str, fallback: Json) -> Json {
        let mut state = self.state.lock().unwrap();
        let at = state.counts.entry(key.into()).or_default();
        let index = *at;
        *at += 1;
        f(f(&state.current, "calls"), key)
            .as_array()
            .map(|a| a[index.min(a.len() - 1)].clone())
            .unwrap_or(fallback)
    }
    fn gate(&self, name: &str) -> watch::Receiver<bool> {
        self.state
            .lock()
            .unwrap()
            .gates
            .entry(name.into())
            .or_insert_with(|| watch::channel(false).0)
            .subscribe()
    }
    fn release(&self, name: &str) {
        self.state
            .lock()
            .unwrap()
            .gates
            .entry(name.into())
            .or_insert_with(|| watch::channel(false).0)
            .send_replace(true);
    }
    fn call<'a>(&'a self, key: String, args: Vec<Json>, fallback: Json) -> RequestFuture<'a> {
        Box::pin(async move {
            self.emit(&key, args);
            let entry = self.take(&key, fallback);
            if f(&entry, "ready").as_bool() == Some(true) {
                return unwrap(entry.get("value").cloned().unwrap_or(entry));
            }
            let mut gate = if let Some(name) = entry.get("gate") {
                self.gate(&name.as_str()?)
            } else {
                let (send, receive) = watch::channel(false);
                self.state.lock().unwrap().ready.push_back(send);
                self.ready_changed.notify_one();
                receive
            };
            while !*gate.borrow_and_update() {
                gate.changed().await.unwrap();
            }
            unwrap(entry.get("value").cloned().unwrap_or(entry))
        })
    }
    fn sync(&self, key: &str, args: Vec<Json>, fallback: Json) -> IvyResult<Json> {
        self.emit(key, args);
        unwrap(self.take(key, fallback))
    }
    fn patch(&self, value: &Json) {
        let mut state = self.state.lock().unwrap();
        for (k, v) in value.as_object().unwrap() {
            state.current.insert(k.clone(), v.clone());
        }
        state.counts.clear();
    }
    fn collect(&self) -> Vec<Json> {
        std::mem::take(&mut self.state.lock().unwrap().events)
    }
}
struct Attached {
    probe: Arc<Probe>,
    id: String,
    definition: Json,
    recovery: Mutex<bool>,
}
impl Attached {
    fn call(&self, key: &str, args: Vec<Json>, fallback: Json) -> RequestFuture<'_> {
        self.probe
            .call(format!("{}.{}", self.id, key), args, fallback)
    }
    fn sync(&self, key: &str, args: Vec<Json>, fallback: Json) -> IvyResult<Json> {
        self.probe
            .sync(&format!("{}.{}", self.id, key), args, fallback)
    }
}
fn make(probe: &Arc<Probe>, id: &str) -> Arc<dyn RuntimeAttached> {
    Arc::new(Attached {
        probe: probe.clone(),
        id: id.into(),
        definition: json(
            r#"{"project":{"serviceNodeId":"native-agent"},"nativePlan":{"nativeVersion":"0.159.2"}}"#,
        ),
        recovery: Mutex::new(false),
    })
}
impl RuntimeAttached for Attached {
    fn definition(&self) -> &Json {
        &self.definition
    }
    fn expected(&self, sender: &JsString) -> Json {
        Json::object([
            ("principalId", "bridge".into()),
            ("callerPrincipalId", Json::String(sender.clone())),
            ("workspaceId", "workspace".into()),
            ("definitionHash", "definition".into()),
        ])
    }
    fn invalidate(&self, changed: bool) {
        self.probe
            .emit(&format!("{}.invalidate", self.id), vec![changed.into()]);
    }
    fn set_busy(&self, busy: bool) {
        self.probe
            .emit(&format!("{}.setBusy", self.id), vec![busy.into()]);
    }
    fn last_dispatch_ms(&self) -> Option<u64> {
        Some(7)
    }
    fn prepare(&self) -> RequestFuture<'_> {
        self.call("prepare", vec![], Json::Null)
    }
    fn bound(&self) -> RequestFuture<'_> {
        self.call(
            "bound",
            vec![],
            json(r#"{"main":{"value":{"binding":{"objectId":"binding","revision":1}}}}"#),
        )
    }
    fn same_host<'a>(&'a self, host: &'a JsString) -> BoolFuture<'a> {
        Box::pin(async move {
            Ok(self
                .call("sameHost", vec![Json::String(host.clone())], false.into())
                .await?
                .as_bool()
                .unwrap())
        })
    }
    fn send<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        self.call(
            "send",
            vec![Json::String(sender.clone()), request.clone()],
            Json::Null,
        )
    }
    fn command<'a>(&'a self, row: &'a Inbox, text: &'a JsString) -> CommandFuture<'a> {
        Box::pin(async move {
            let v = self
                .call(
                    "command",
                    vec![row.id.clone().into(), Json::String(text.clone())],
                    "Command reply".into(),
                )
                .await?;
            Ok(v.as_js_string().cloned())
        })
    }
    fn preferences(&self) -> IvyResult<Json> {
        self.sync("preferences", vec![], json(r#"{"commentary":false}"#))
    }
    fn turn_options(&self) -> IvyResult<Json> {
        self.sync("turnOptions", vec![], json(r#"{"model":"chosen"}"#))
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        let hash = hash_json(&scope).unwrap();
        self.call(
            "operationId",
            vec![scope],
            format!("synthetic-operation:{}", &hash[7..]).into(),
        )
    }
    fn ensure<'a>(&'a self, sender: &'a JsString, op: &'a Json) -> RequestFuture<'a> {
        self.call(
            "ensure",
            vec![Json::String(sender.clone()), op.clone()],
            Json::Null,
        )
    }
    fn queue_send<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        self.call(
            "queue",
            vec![Json::String(sender.clone()), request.clone()],
            json(r#"{"phase":"accepted"}"#),
        )
    }
    fn payload<'a>(&'a self, row: &'a Inbox, same: bool) -> RequestFuture<'a> {
        self.call(
            "payload",
            vec![row.id.clone().into(), same.into()],
            json(r#"{"text":"Payload","images":[]}"#),
        )
    }
    fn tick(&self, commentary: bool) -> BoolFuture<'_> {
        Box::pin(async move {
            let busy = self
                .call("tick", vec![commentary.into()], false.into())
                .await?;
            let mut state = self.probe.state.lock().unwrap();
            *self.recovery.lock().unwrap() = state.recovery > 0;
            state.recovery -= 1;
            Ok(busy.as_bool().unwrap())
        })
    }
    fn needs_recovery(&self) -> bool {
        *self.recovery.lock().unwrap()
    }
    fn notification<'a>(&'a self, event: &'a Json, commentary: bool) -> RequestFuture<'a> {
        let busy = match f(event, "method").as_str().unwrap_or_default().as_str() {
            "turn/started" => true.into(),
            "turn/completed" => false.into(),
            _ => Json::Null,
        };
        self.call(
            "notification",
            vec![event.clone(), commentary.into()],
            Json::object([("busy", busy)]),
        )
    }
    fn notices(&self) -> RequestFuture<'_> {
        self.call("notices", vec![], Json::Null)
    }
    fn outbox(&self) -> RequestFuture<'_> {
        self.call("outbox", vec![], Json::Null)
    }
    fn automatic_notifications(&self, now: i64) -> BoolFuture<'_> {
        Box::pin(async move {
            Ok(self
                .call("automatic", vec![Json::Number(now as f64)], true.into())
                .await?
                .as_bool()
                .unwrap())
        })
    }
}
struct Media(Arc<Probe>);
impl RuntimeMedia for Media {
    fn command_text(&self, row: &Inbox) -> IvyResult<JsString> {
        let value = parse(&row.message, usize::MAX).map_err(|_| IvyError::internal())?;
        self.0.emit("text", vec![value.clone()]);
        let message = f(&value, "message");
        Ok(message
            .get("conversation")
            .or_else(|| f(message, "extendedTextMessage").get("text"))
            .and_then(Json::as_js_string)
            .cloned()
            .unwrap_or_default())
    }
    fn content(&self, row: &Inbox) -> IvyResult<Json> {
        let value = parse(&row.message, usize::MAX).map_err(|_| IvyError::internal())?;
        let content = f(&value, "message").clone();
        self.0.emit("content", vec![content.clone()]);
        Ok(content)
    }
    fn payload<'a>(
        &'a self,
        _: &'a Inbox,
        _: &'a ChatGenerationLease,
        _: bool,
    ) -> RequestFuture<'a> {
        Box::pin(async {
            panic!("Runtime corpus supplies payload at the explicit attachment boundary")
        })
    }
    fn prune(&self) -> ServiceFuture<()> {
        let probe = self.0.clone();
        Box::pin(async move {
            probe.call("prune".into(), vec![], Json::Null).await?;
            Ok(())
        })
    }
}
struct Factory;
impl RuntimeAttachmentFactory for Factory {
    fn create(&self, _: Arc<ChatGeneration>) -> IvyResult<Arc<dyn RuntimeAttached>> {
        panic!(
            "Runtime corpus supplies attachments; actual generation assembly is a separate boundary"
        )
    }
}
struct Socket {
    probe: Arc<Probe>,
    callbacks: Mutex<Option<RuntimeCallbacks>>,
}
impl Socket {
    fn effect(&self, key: &str) -> ServiceFuture<()> {
        let probe = self.probe.clone();
        let key = key.to_owned();
        Box::pin(async move {
            probe.call(key, vec![], Json::Null).await?;
            Ok(())
        })
    }
}
impl RuntimeSocket for Socket {
    fn bind(&self, callbacks: RuntimeCallbacks) {
        *self.callbacks.lock().unwrap() = Some(callbacks);
    }
    fn start(&self) {
        self.probe.emit("socketStart", vec![]);
    }
    fn state(&self) -> JsString {
        f(&self.status(), "state").as_js_string().unwrap().clone()
    }
    fn status(&self) -> Json {
        let mut value = json(
            r#"{"state":"connected","connected":true,"online":true,"privacy":true,"lastOnlinePresenceAt":null,"errorCode":null,"lastSendMs":null}"#,
        );
        if let Some(patch) = f(&self.probe.state.lock().unwrap().current, "socket").as_object() {
            for (k, v) in patch {
                value.insert(k.clone(), v.clone());
            }
        }
        value
    }
    fn flush(&self) -> ServiceFuture<()> {
        self.effect("flush")
    }
    fn read_receipts(&self) -> ServiceFuture<()> {
        self.effect("readReceipts")
    }
    fn set_busy(&self, jids: Vec<JsString>) {
        self.probe.emit(
            "setBusy",
            vec![Json::Array(jids.into_iter().map(Json::String).collect())],
        );
    }
    fn presence(&self) -> ServiceFuture<()> {
        self.effect("presence")
    }
    fn close(&self) -> ServiceFuture<()> {
        self.effect("socketClose")
    }
}
struct Timer {
    name: &'static str,
    probe: Arc<Probe>,
    cleared: Arc<Mutex<bool>>,
}
impl RuntimeTimer for Timer {
    fn clear(&self) {
        *self.cleared.lock().unwrap() = true;
        self.probe.emit("clearTimer", vec![self.name.into()]);
    }
}
struct TimerEntry {
    name: &'static str,
    cleared: Arc<Mutex<bool>>,
    tick: Arc<dyn Fn() + Send + Sync>,
}
struct Timers {
    probe: Arc<Probe>,
    entries: Mutex<Vec<TimerEntry>>,
}
impl RuntimeTimers for Timers {
    fn every(
        &self,
        name: &'static str,
        ms: u64,
        tick: Arc<dyn Fn() + Send + Sync>,
    ) -> Box<dyn RuntimeTimer> {
        let cleared = Arc::new(Mutex::new(false));
        self.probe
            .emit("timer", vec![name.into(), Json::Number(ms as f64)]);
        self.entries.lock().unwrap().push(TimerEntry {
            name,
            cleared: cleared.clone(),
            tick,
        });
        Box::new(Timer {
            name,
            probe: self.probe.clone(),
            cleared,
        })
    }
}
fn lane(name: &str) -> RuntimeWork {
    match name {
        "control" => RuntimeWork::Control,
        "input" => RuntimeWork::Input,
        "listener" => RuntimeWork::Listener,
        "output" => RuntimeWork::Output,
        "status" => RuntimeWork::Status,
        "read-receipts" => RuntimeWork::ReadReceipts,
        "activity" => RuntimeWork::Activity,
        "retention" => RuntimeWork::Retention,
        _ => panic!("Unknown lane"),
    }
}
async fn drain(probe: &Probe) {
    // Drain continuations before admitting the next explicit FIFO I/O result.
    // Named gates stay pending; no wall-clock sleeps decide provider ordering.
    for _ in 0..10000 {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        let next = probe.state.lock().unwrap().ready.pop_front();
        match next {
            Some(send) => {
                send.send_replace(true);
            }
            None => return,
        }
    }
    panic!("Runtime probe completion limit");
}
async fn finish<T: Send + 'static>(
    mut work: tokio::task::JoinHandle<T>,
    probe: &Probe,
    label: &str,
) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let changed = probe.ready_changed.notified();
            drain(probe).await;
            tokio::select! {
                value=&mut work => {drain(probe).await;return value.unwrap();},
                _=changed => {},
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Runtime probe completion timed out: {label}"))
}
fn frame(index: usize) -> Json {
    Json::object([(
        "params",
        Json::object([
            ("serviceNodeId", "native-agent".into()),
            (
                "payload",
                Json::object([
                    ("serviceNodeId", "native-agent".into()),
                    ("nativeVersion", "0.159.2".into()),
                    ("sequence", Json::Number((index + 2) as f64)),
                    ("epoch", "epoch".into()),
                    ("method", "item/completed".into()),
                    (
                        "params",
                        Json::object([
                            ("threadId", "thread".into()),
                            ("index", Json::Number(index as f64)),
                        ]),
                    ),
                ]),
            ),
        ]),
    )])
}
async fn apply(
    a: &Json,
    runtime: &WhatsAppRuntime,
    probe: &Arc<Probe>,
    timers: &Timers,
    socket: &Socket,
) -> IvyResult<Json> {
    match s(a, "op").as_str() {
        "input" => runtime.input().await?,
        "control" => runtime.control().await?,
        "activity" => runtime.activity().await?,
        "status" => runtime.status().await?,
        "retention" => runtime.retention().await?,
        "run" => runtime.run(lane(&s(a, "lane"))),
        "runMany" => {
            for _ in 0..f(a, "count").as_f64().unwrap() as usize {
                runtime.run(lane(&s(a, "lane")));
            }
        }
        "notify" => runtime.notification(f(a, "frame"))?,
        "notifyMany" => {
            for index in 0..f(a, "count").as_f64().unwrap() as usize {
                runtime.notification(&frame(index))?;
            }
        }
        "attach" => runtime.attach(if f(a, "id").is_null() {
            None
        } else {
            Some(make(probe, &s(a, "id")))
        }),
        "changed" => runtime.changed(),
        "quiet" => {
            let callback = socket
                .callbacks
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .automatic_notifications
                .clone();
            return Ok(callback().await?.into());
        }
        "release" => probe.release(&s(a, "gate")),
        "patch" => probe.patch(f(a, "value")),
        "time" => probe.state.lock().unwrap().now = f(a, "now").as_f64().unwrap() as i64,
        "start" => runtime.start(),
        "timer" => {
            let tick = timers
                .entries
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.name == s(a, "name") && !*t.cleared.lock().unwrap())
                .unwrap()
                .tick
                .clone();
            tick();
        }
        "closeStartDrop" => {
            probe.state.lock().unwrap().closing = true;
            drop(runtime.close());
        }
        "close" | "startCloseAgain" => runtime.close().await?,
        _ => panic!("Unknown runtime action"),
    }
    Ok(Json::Null)
}
#[tokio::test]
async fn original_runtime_lanes_input_events_and_shutdown_match() {
    let output = Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-whatsapp-runtime-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle = json(&String::from_utf8(output.stdout).unwrap());
    let mut rows = Vec::new();
    for spec in f(&oracle, "specs").as_array().unwrap() {
        let journal = Journal::new(JournalIo::open(":memory:").unwrap());
        let probe = Arc::new(Probe {
            state: Mutex::new(State {
                current: Json::Object(vec![]),
                now: 1000,
                events: vec![],
                counts: HashMap::new(),
                gates: HashMap::new(),
                ready: VecDeque::new(),
                closing: false,
                recovery: 0,
            }),
            start: Instant::now(),
            ready_changed: Notify::new(),
        });
        let timers = Arc::new(Timers {
            probe: probe.clone(),
            entries: Mutex::new(vec![]),
        });
        let socket = Arc::new(Socket {
            probe: probe.clone(),
            callbacks: Mutex::new(None),
        });
        let status = probe.clone();
        let auth = probe.clone();
        let lock = probe.clone();
        let auth_journal = journal.clone();
        let runtime=WhatsAppRuntime::with_dependencies(RuntimeParts{
            config:spec.get("config").cloned().unwrap_or_else(||json(r#"{"accountId":"account","botPhoneNumber":"+15555550200","senders":[{"phoneNumber":"+15555550201","principalId":"sender","channelId":"channel"}],"since":"2024-01-01T00:00:00Z"}"#)),
            host_id:"host".into(),journal:journal.clone(),socket:socket.clone(),media:Arc::new(Media(probe.clone())),factory:Arc::new(Factory),
            resources:RuntimeResources{
                status:Arc::new(move |value|{let probe=status.clone();Box::pin(async move {probe.call("status".into(),vec![value],Json::Null).await?;Ok(())})}),
                retire_account:Arc::new(move ||{let auth=auth.clone();let lock=lock.clone();let journal=auth_journal.clone();Box::pin(async move {auth.emit("authClose",vec![journal.access(|_|Ok(())).is_err().into()]);unwrap(auth.take("authClose",Json::Null))?;lock.sync("lockClose",vec![],Json::Null)?;Ok(())})})},
            translator:Translator::new(spec.get("language").and_then(Json::as_js_string)),
        },probe.clone(),timers.clone());
        runtime.attach(Some(make(&probe, "A")));
        drain(&probe).await;
        probe.collect();
        {
            let mut state = probe.state.lock().unwrap();
            state.current = spec.clone();
            state.counts.clear();
            state.recovery = f(spec, "recoveryCount").as_f64().unwrap_or(0.0) as i64;
        }
        if let Some(seed) = spec.get("rows").and_then(Json::as_array) {
            for row in seed {
                journal
                    .access(|j| {
                        j.accept(
                            &s(row, "id"),
                            &s(row, "sender"),
                            &s(row, "jid"),
                            &s(row, "message"),
                            f(row, "received").as_f64().unwrap() as i64,
                        )
                    })
                    .unwrap();
            }
        }
        if let Some(request) = spec.get("saved") {
            journal
                .access(|j| j.save_request("input", request))
                .unwrap();
        }
        if let Some(raw) = spec.get("savedRaw") {
            journal
                .access(|j| {
                    j.connection()
                        .execute(
                            "UPDATE inbox SET request=? WHERE id='input'",
                            [raw.as_str()?],
                        )
                        .unwrap();
                    Ok(())
                })
                .unwrap();
        }
        if f(spec, "direct").as_bool() == Some(true) {
            journal.set("direct-input:input", &true.into()).unwrap();
        }
        let mut steps = Vec::new();
        for a in f(spec, "actions").as_array().unwrap() {
            let (a, owner, p, t, socket_owner) = (
                a.clone(),
                runtime.clone(),
                probe.clone(),
                timers.clone(),
                socket.clone(),
            );
            let work = tokio::spawn(async move { apply(&a, &owner, &p, &t, &socket_owner).await });
            let value = result(finish(work, &probe, &s(spec, "label")).await);
            let can_finish_close = {
                let state = probe.state.lock().unwrap();
                state.closing && state.gates.values().all(|gate| *gate.borrow())
            };
            if can_finish_close {
                let closing = runtime.close();
                let _ = finish(tokio::spawn(closing), &probe, &s(spec, "label")).await;
            }
            steps.push(Json::object([
                ("result", value),
                ("events", Json::Array(probe.collect())),
                (
                    "snapshot",
                    journal
                        .access(|j| Ok(snapshot(j)))
                        .unwrap_or_else(|_| "closed".into()),
                ),
            ]));
        }
        rows.push(Json::object([
            ("label", f(spec, "label").clone()),
            ("steps", Json::Array(steps)),
        ]));
        if let Some(path) = std::env::var_os("IVY_CHAT_WA_RUNTIME_ACTUAL_CAPTURE") {
            std::fs::write(
                path,
                stringify(
                    &Json::object([
                        ("rows", Json::Array(rows.clone())),
                        ("complete", false.into()),
                    ]),
                    usize::MAX,
                )
                .unwrap(),
            )
            .unwrap();
        }
        for gate in probe.state.lock().unwrap().gates.values() {
            gate.send_replace(true);
        }
        drain(&probe).await;
        let closing = tokio::spawn(async move { runtime.close().await });
        let _ = finish(closing, &probe, &s(spec, "label")).await;
    }
    let retryable = Json::Array(
        f(&oracle, "retryable")
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                Json::object([
                    ("code", f(r, "code").clone()),
                    ("value", retryable_whatsapp_error(&s(r, "code")).into()),
                ])
            })
            .collect(),
    );
    let actual = Json::object([("rows", Json::Array(rows)), ("retryable", retryable)]);
    if let Some(path) = std::env::var_os("IVY_CHAT_WA_RUNTIME_ACTUAL_CAPTURE") {
        std::fs::write(path, stringify(&actual, usize::MAX).unwrap()).unwrap();
    }
    // Output flushing runs on its own Tokio task. Compare its calls separately
    // from the control lane's text parsing, while keeping step boundaries,
    // delivery snapshots, call counts and all other ordering checks exact.
    let scheduled_trace = |row: &Json| {
        let mut row = row.clone();
        let steps = f(&row, "steps")
            .as_array()
            .unwrap()
            .iter()
            .map(|step| {
                let mut step = step.clone();
                let mut flushes = 0;
                let events = f(&step, "events")
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|event| {
                        if event.as_array().and_then(|values| values.first())
                            == Some(&Json::from("flush"))
                        {
                            flushes += 1;
                            None
                        } else {
                            Some(event.clone())
                        }
                    })
                    .collect();
                step.insert("events", Json::Array(events));
                step.insert("flushCalls", Json::Number(flushes as f64));
                step
            })
            .collect();
        row.insert("steps", Json::Array(steps));
        row
    };
    for (a, e) in f(&actual, "rows")
        .as_array()
        .unwrap()
        .iter()
        .zip(f(&oracle, "rows").as_array().unwrap())
    {
        assert_eq!(
            canonical(&scheduled_trace(a), usize::MAX).unwrap(),
            canonical(&scheduled_trace(e), usize::MAX).unwrap(),
            "{}",
            s(e, "label")
        );
    }
    assert_eq!(f(&actual, "retryable"), f(&oracle, "retryable"));
    eprintln!(
        "{} original runtime scenarios matched",
        f(&oracle, "specs").as_array().unwrap().len()
    );
}
