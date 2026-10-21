use super::FixtureMut;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::digest,
    json::{parse, stringify},
};
use ivy_hive::{
    admission::{DispatchGate, RequestBudget},
    live_routing::{LiveRouting, PreparedCall},
    server_lifecycle::{Lifecycle, Listener, OAuth, Packages, Push, StorageLifecycle},
    server_peers::{Peer, PeerSocket, Peers, UnhandledKind},
    server_rpc::{Reply, Storage},
    store::Credential,
    tool_binding_diagnostics::ToolBindingDiagnostics,
    worker_client::RequestOptions,
    worker_protocol::Notification,
    worker_runtime::Event,
    ws_send::Socket,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    fs::OpenOptions,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{runtime::Builder, sync::oneshot, task::AbortHandle};
fn wire(value: &Value) -> IvyJson {
    parse(&value.to_string(), usize::MAX).unwrap()
}
fn encode(value: &IvyJson) -> String {
    stringify(value, usize::MAX).unwrap()
}
fn error(value: Option<&Value>) -> IvyError {
    let value = value.cloned().unwrap_or(Value::Null);
    let mut error = IvyError::new(
        value["code"].as_str().unwrap_or("fixture_error"),
        value["message"]
            .as_str()
            .unwrap_or("Fixture operation failed."),
    )
    .with_outcome(match value["outcome"].as_str().unwrap_or("unknown") {
        "completed" => Outcome::Completed,
        "not_executed" => Outcome::NotExecuted,
        _ => Outcome::Unknown,
    });
    if let Some(details) = value.get("details") {
        error = error.with_details(wire(details));
    }
    error
}
fn credentials(value: Option<&Value>) -> Vec<Credential> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|v| Credential {
            principal_id: v["principalId"].as_str().unwrap().into(),
            digest: v["digest"].as_str().unwrap().into(),
        })
        .collect()
}
fn credentials_value(value: &[Credential]) -> IvyJson {
    IvyJson::Array(
        value
            .iter()
            .map(|v| {
                IvyJson::object([
                    ("principalId", IvyJson::String(v.principal_id.clone())),
                    ("digest", IvyJson::String(v.digest.clone())),
                ])
            })
            .collect(),
    )
}
type Events = Arc<Mutex<Vec<IvyJson>>>;
struct Fixture {
    events: Events,
    controls: Mutex<HashMap<String, oneshot::Sender<IvyResult<IvyJson>>>>,
    counters: Mutex<HashMap<String, u64>>,
    trace: Value,
}
impl Fixture {
    fn emit(&self, event: IvyJson) {
        self.events.lock().unwrap().push(event);
    }
    fn fail(&self, key: &str) -> IvyResult<()> {
        match self.trace.get("fail").and_then(|v| v.get(key)) {
            Some(value) => Err(error(Some(value))),
            None => Ok(()),
        }
    }
    fn post(&self, key: &str, event: IvyJson) -> Reply {
        self.emit(event);
        let n = {
            let mut counters = self.counters.lock().unwrap();
            let n = counters.entry(key.to_owned()).or_default();
            *n += 1;
            *n
        };
        let (sender, reply) = oneshot::channel();
        assert!(
            self.controls
                .lock()
                .unwrap()
                .insert(format!("{key}:{n}"), sender)
                .is_none()
        );
        Box::pin(async move { reply.await.expect("Fixture retains pending controls") })
    }
    fn unit(&self, key: &str, event: IvyJson) -> Reply<()> {
        let reply = self.post(key, event);
        Box::pin(async move { reply.await.map(|_| ()) })
    }
    fn kind(&self, kind: &str) {
        self.emit(IvyJson::object([("kind", kind.into())]));
    }
}
impl Storage for Fixture {
    fn request(&self, payload: IvyJson, options: RequestOptions) -> Reply {
        let key = payload.get("action").unwrap().as_str().unwrap();
        self.post(
            &key,
            IvyJson::object([
                ("kind", "worker".into()),
                ("payload", payload),
                (
                    "options",
                    IvyJson::object([
                        ("timeoutMs", IvyJson::Number(options.timeout_ms as f64)),
                        ("terminateOnTimeout", options.terminate_on_timeout.into()),
                        ("reservedBytes", IvyJson::Number(options.reserved_bytes)),
                        ("backup", options.backup.into()),
                    ]),
                ),
            ]),
        )
    }
}
impl StorageLifecycle for Fixture {
    fn start(&self) -> Reply<()> {
        self.unit("start", IvyJson::object([("kind", "storage-start".into())]))
    }
    fn close(&self) -> Reply<()> {
        self.unit(
            "storage-close",
            IvyJson::object([("kind", "storage-close".into())]),
        )
    }
}
impl OAuth for Fixture {
    fn initialize(&self, value: &IvyJson) -> IvyResult<()> {
        self.emit(IvyJson::object([
            ("kind", "oauth-initialize".into()),
            ("value", value.clone()),
        ]));
        self.fail("oauth-initialize")
    }
    fn set_credentials(&self, value: Vec<Credential>) -> IvyResult<()> {
        self.emit(IvyJson::object([
            ("kind", "oauth-set".into()),
            ("value", credentials_value(&value)),
        ]));
        self.fail("oauth-set")
    }
}
impl Push for Fixture {
    fn start(&self) -> IvyResult<()> {
        self.kind("push-start");
        self.fail("push-start")
    }
    fn close(&self) -> Reply<()> {
        self.unit(
            "push-close",
            IvyJson::object([("kind", "push-close".into())]),
        )
    }
}
impl Packages for Fixture {
    fn initialize(&self) -> Reply<()> {
        self.unit(
            "packages-initialize",
            IvyJson::object([("kind", "packages-initialize".into())]),
        )
    }
    fn collect_artifacts(&self) -> Reply<()> {
        self.unit(
            "packages-collect",
            IvyJson::object([("kind", "packages-collect".into())]),
        )
    }
}
impl Listener for Fixture {
    fn listen(&self, port: f64, host: JsString) -> Reply<()> {
        self.kind("listen-error-on");
        let reply = self.post(
            "listen",
            IvyJson::object([
                ("kind", "listen".into()),
                ("port", IvyJson::Number(port)),
                ("host", IvyJson::String(host)),
            ]),
        );
        let events = self.events.clone();
        Box::pin(async move {
            reply.await?;
            events
                .lock()
                .unwrap()
                .push(IvyJson::object([("kind", "listen-error-off".into())]));
            Ok(())
        })
    }
    fn address(&self) -> IvyResult<IvyJson> {
        self.kind("address");
        self.fail("address")?;
        Ok(self
            .trace
            .get("address")
            .map(wire)
            .unwrap_or_else(|| wire(&json!({"port":39082,"address":"127.0.0.1"}))))
    }
    fn close_websockets(&self) -> IvyResult<()> {
        self.kind("ws-close");
        self.fail("ws-close")
    }
    fn close_connections(&self) -> IvyResult<()> {
        self.kind("connections-close");
        self.fail("connections-close")
    }
    fn close(&self) -> Reply<()> {
        self.unit(
            "http-close",
            IvyJson::object([("kind", "http-close".into())]),
        )
    }
}
struct TestRuntime;
impl ivy_hive::retention::Runtime for TestRuntime {
    fn parse_date(&self, _: &JsString) -> f64 {
        panic!("No date parse in lifecycle fixture")
    }
    fn iso_date(&self, millis: f64) -> IvyResult<JsString> {
        assert_eq!(millis, 10000.0);
        Ok("1970-01-01T00:00:10.000Z".into())
    }
    fn compare_contract_keys(&self, _: &JsString, _: &JsString) -> std::cmp::Ordering {
        panic!("No locale comparison in lifecycle fixture")
    }
}
struct TestSocket {
    name: String,
    fixture: Arc<Fixture>,
    open: bool,
}
impl Socket for TestSocket {
    fn is_open(&self) -> bool {
        self.open
    }
    fn buffered_amount(&self) -> f64 {
        0.0
    }
    fn send_text(&mut self, text: String) -> IvyResult<()> {
        self.fixture.emit(IvyJson::object([
            ("kind", "send".into()),
            ("peer", self.name.as_str().into()),
            ("value", parse(&text, usize::MAX).unwrap()),
            ("byteLength", IvyJson::Number(text.len() as f64)),
            ("sha256", digest(text.as_bytes()).into()),
        ]));
        Ok(())
    }
    fn terminate(&mut self) -> IvyResult<()> {
        self.fixture.emit(IvyJson::object([
            ("kind", "terminate".into()),
            ("peer", self.name.as_str().into()),
        ]));
        self.fixture.fail("terminate")?;
        self.open = false;
        Ok(())
    }
}
impl PeerSocket for TestSocket {
    fn ping(&mut self) -> IvyResult<()> {
        self.fixture.emit(IvyJson::object([
            ("kind", "ping".into()),
            ("peer", self.name.as_str().into()),
        ]));
        self.fixture.fail("ping")
    }
}
fn prepared(value: &IvyJson) -> PreparedCall {
    PreparedCall {
        service_node_id: value
            .get("serviceNodeId")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        generation: value.get("generation").unwrap().as_f64().unwrap(),
        definition: value.get("definition").unwrap().clone(),
        definition_hash: value
            .get("definitionHash")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        arguments: value.get("arguments").unwrap().clone(),
        caller_principal_id: value
            .get("callerPrincipalId")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        operation_id: value.get("operationId").cloned(),
    }
}
fn observe<T: Send + 'static>(
    work: Reply<T>,
    name: String,
    events: Events,
    convert: impl FnOnce(T) -> Option<IvyJson> + Send + 'static,
) -> AbortHandle {
    tokio::spawn(async move {
        let event = match work.await {
            Ok(value) => {
                let mut event = IvyJson::object([("kind", "ok".into()), ("name", name.into())]);
                if let Some(value) = convert(value) {
                    event.insert("value", value);
                }
                event
            }
            Err(error) => IvyJson::object([
                ("kind", "failed".into()),
                ("name", name.into()),
                ("error", error.to_wire()),
            ]),
        };
        events.lock().unwrap().push(event);
    })
    .abort_handle()
}
async fn flush() {
    for _ in 0..128 {
        tokio::task::yield_now().await;
    }
}
async fn native(trace: &Value) -> Value {
    let events: Events = Arc::default();
    let fixture = Arc::new(Fixture {
        events: events.clone(),
        controls: Mutex::default(),
        counters: Mutex::default(),
        trace: trace.clone(),
    });
    let diagnostics = Arc::new(Mutex::new(ToolBindingDiagnostics::new(
        {
            let events = events.clone();
            Box::new(move |value| {
                events.lock().unwrap().push(IvyJson::object([
                    ("kind", "binding-record".into()),
                    ("value", value),
                ]));
                Ok(())
            })
        },
        Arc::new(|| 10000.0),
        Arc::new(TestRuntime),
    )));
    let routing = Arc::new(Mutex::new(LiveRouting::new(false, None)));
    let dispatch = DispatchGate::new(
        trace["dispatch"]["maximum"].as_f64().unwrap_or(1024.0),
        trace["dispatch"]["waitMs"].as_f64().unwrap_or(30000.0),
    );
    let unhandled: ivy_hive::server_lifecycle::Unhandled = {
        let events = events.clone();
        Arc::new(move |kind, error| {
            events.lock().unwrap().push(IvyJson::object([
                ("kind", "unhandled".into()),
                (
                    "channel",
                    match kind {
                        UnhandledKind::Callback => "callback",
                        UnhandledKind::Promise => "promise",
                    }
                    .into(),
                ),
                ("error", error.to_wire()),
            ]))
        })
    };
    let peers = Peers::new(
        ivy_hive::server_peers::Owners {
            storage: fixture.clone(),
            routing: routing.clone(),
            diagnostics: diagnostics.clone(),
            requests: RequestBudget::default(),
            dispatch: dispatch.clone(),
            direct: Arc::new(|_, _, _, _| Ok(None)),
            now: Arc::new(|| 10000.0),
            ids: {
                let fixture = fixture.clone();
                let id = Mutex::new(0);
                Arc::new(move || {
                    let value = {
                        let mut n = id.lock().unwrap();
                        *n += 1;
                        format!("fixture-{n}")
                    };
                    fixture.emit(IvyJson::object([
                        ("kind", "id".into()),
                        ("value", value.as_str().into()),
                    ]));
                    value.into()
                })
            },
            provider_error: Arc::new(|_| {
                panic!("No provider error construction in lifecycle fixture")
            }),
            wake_browser_push: {
                let fixture = fixture.clone();
                Arc::new(move || fixture.kind("wake"))
            },
            clear_authentication: {
                let fixture = fixture.clone();
                Arc::new(move || fixture.kind("cache-clear"))
            },
            unhandled: unhandled.clone(),
        },
        ivy_hive::server_peers::Options {
            call_timeout_ms: trace["options"]["callTimeoutMs"].as_f64(),
            validate_messages: false,
        },
    );
    let immediates: Arc<Mutex<VecDeque<Box<dyn FnOnce() + Send>>>> = Arc::default();
    let lifecycle = Lifecycle::new(
        ivy_hive::server_lifecycle::Owners {
            storage: fixture.clone(),
            peers: peers.clone(),
            dispatch,
            oauth: fixture.clone(),
            push: fixture.clone(),
            packages: trace["packages"]
                .as_bool()
                .unwrap_or(false)
                .then(|| fixture.clone() as Arc<dyn Packages>),
            listener: fixture.clone(),
            immediate: {
                let queue = immediates.clone();
                Arc::new(move |work| queue.lock().unwrap().push_back(work))
            },
            unhandled,
        },
        ivy_hive::server_lifecycle::Options {
            listen_host: trace["options"]["listenHost"].as_str().map(Into::into),
            listen_port: trace["options"]["listenPort"].as_f64(),
            credentials: credentials(trace.get("credentials")),
        },
    );
    let mut members: HashMap<String, Peer> = HashMap::new();
    let mut observations: HashMap<String, AbortHandle> = HashMap::new();
    let mut rows = vec![];
    for step in trace["steps"].as_array().unwrap() {
        let result: IvyResult<()> = async {
            let name = step["name"].as_str().unwrap_or("").to_owned();
            match step["op"].as_str().unwrap() {
                "start" => {
                    observations.insert(
                        name.clone(),
                        observe(lifecycle.start(), name, events.clone(), Some),
                    );
                }
                "close" => {
                    observations.insert(
                        name.clone(),
                        observe(lifecycle.close(), name, events.clone(), |()| None),
                    );
                }
                "credentials" => {
                    observations.insert(
                        name.clone(),
                        observe(
                            lifecycle.set_credentials(credentials(step.get("value"))),
                            name,
                            events.clone(),
                            |()| None,
                        ),
                    );
                }
                "collect" => {
                    observations.insert(
                        name.clone(),
                        observe(
                            lifecycle.collect_package_artifacts(),
                            name,
                            events.clone(),
                            |()| None,
                        ),
                    );
                }
                "snapshot" => fixture.emit(IvyJson::object([
                    ("kind", "credentials".into()),
                    ("value", credentials_value(&lifecycle.credentials())),
                ])),
                "drop" => {
                    observations.remove(&name).unwrap().abort();
                }
                "resolve" | "reject" => {
                    let sender = fixture
                        .controls
                        .lock()
                        .unwrap()
                        .remove(step["key"].as_str().unwrap())
                        .expect("Missing fixture control");
                    let _ = sender.send(if step["op"] == "resolve" {
                        Ok(step.get("value").map(wire).unwrap_or(IvyJson::Null))
                    } else {
                        Err(error(step.get("error")))
                    });
                }
                "recover" => lifecycle.schedule_recovery(),
                "failure" => lifecycle.worker_event(Event::Failure(IvyError::internal())),
                "routing" => lifecycle.worker_event(Event::Notification(Notification::Routing(
                    wire(&step["value"]),
                ))),
                "events" => lifecycle.worker_event(Event::Notification(
                    Notification::EventsAvailable(wire(&step["through"])),
                )),
                "changes" => lifecycle.worker_event(Event::Notification(Notification::Changes(
                    step["scopes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap().into())
                        .collect(),
                ))),
                "immediates" => {
                    let work = immediates.lock().unwrap().drain(..).collect::<Vec<_>>();
                    for work in work {
                        work();
                    }
                }
                "diagnostic-completed" => diagnostics
                    .lock()
                    .unwrap()
                    .completed(&wire(&step["resource"]))?,
                "route" => fixture.emit(IvyJson::object([
                    ("kind", "prepared".into()),
                    (
                        "value",
                        routing
                            .lock()
                            .unwrap()
                            .prepare(&wire(&step["context"]), &wire(&step["params"]))?
                            .to_json(),
                    ),
                ])),
                "connect" => {
                    members.insert(
                        name.clone(),
                        peers.connected(
                            Box::new(TestSocket {
                                name,
                                fixture: fixture.clone(),
                                open: true,
                            }),
                            wire(&step["context"]),
                        ),
                    );
                }
                "pong" => peers.pong(members.get(&name).unwrap()),
                "forward" => {
                    observations.insert(
                        name.clone(),
                        observe(
                            peers.forward(&wire(&step["context"]), prepared(&wire(&step["call"]))),
                            name,
                            events.clone(),
                            Some,
                        ),
                    );
                }
                "advance" => {
                    tokio::time::advance(Duration::from_millis(step["ms"].as_u64().unwrap())).await
                }
                "flush" => {}
                other => panic!("Unknown lifecycle step {other}"),
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            fixture.emit(IvyJson::object([
                ("kind", "error".into()),
                ("error", error.to_wire()),
            ]));
        }
        if !step["noFlush"].as_bool().unwrap_or(false) {
            flush().await;
        }
        rows.push(
            serde_json::from_str::<Value>(&encode(&IvyJson::Array(std::mem::take(
                &mut *events.lock().unwrap(),
            ))))
            .unwrap(),
        );
    }
    Value::Array(rows)
}
fn original(traces: &[Value]) -> Vec<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg("tests/fixtures/hive-server-lifecycle-oracle.mjs")
            .current_dir(root)
            .env("NODE_NO_WARNINGS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(traces).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn compare(group: &str, traces: Vec<Value>) {
    let old = original(&traces);
    assert_eq!(old.len(), traces.len());
    for (input, original) in traces.into_iter().zip(old) {
        let native = Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(native(&input));
        if let Some(path) = std::env::var_os("IVY_HIVE_LIFECYCLE_CORPUS") {
            let line = json!({"group":group,"input":input,"original":original,"native":native});
            writeln!(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .unwrap(),
                "{line}"
            )
            .unwrap();
        }
        assert_eq!(native, original, "lifecycle group={group} input={input}");
    }
}
fn settle(key: &str, value: Value) -> Value {
    json!({"op":"resolve","key":key,"value":value})
}
fn reject(key: &str) -> Value {
    json!({"op":"reject","key":key})
}
fn boot() -> Vec<Value> {
    vec![
        json!({"op":"start","name":"start"}),
        settle("start:1", Value::Null),
        settle("binding.diagnostics:1", json!([])),
        settle(
            "oauth.initialize:1",
            json!({"signingKey":"signing","referenceKey":"reference"}),
        ),
        settle("listen:1", Value::Null),
    ]
}
fn shutdown() -> Vec<Value> {
    vec![
        json!({"op":"close","name":"close"}),
        settle("push-close:1", Value::Null),
        settle("http-close:1", Value::Null),
        settle("storage-close:1", Value::Null),
    ]
}
fn connect(name: &str, digest: &str, node: Option<(&str, f64)>) -> Value {
    let mut context = json!({"principalId":"caller","credentialDigest":digest,"transport":"http"});
    if let Some((id, generation)) = node {
        context["serviceNodeId"] = json!(id);
        context["generation"] = json!(generation);
    }
    json!({"op":"connect","name":name,"context":context})
}
fn forward() -> Value {
    let definition = super::binding("provider.call", true);
    json!({"op":"forward","name":"forward","context":{"principalId":"caller","credentialDigest":"old","transport":"http"},"call":{"serviceNodeId":"node","generation":1,"definition":serde_json::from_str::<Value>(&encode(definition.get("definition").unwrap())).unwrap(),"definitionHash":definition.get("definitionHash").unwrap().as_str().unwrap(),"arguments":1,"callerPrincipalId":"caller"}})
}

#[test]
fn original_startup_cleanup_and_post_listen_boundaries() {
    let mut cases = vec![
        json!({"name":"normal-start-and-close","steps":([boot(),shutdown()].concat())}),
        json!({"name":"configured-address","options":{"listenPort":0,"listenHost":"::1"},"address":{"port":39999,"address":"::1"},"steps":([boot(),shutdown()].concat())}),
    ];
    for stage in [
        "start:1",
        "binding.diagnostics:1",
        "oauth.initialize:1",
        "listen:1",
    ] {
        let steps = boot();
        let stop = steps.iter().position(|s| s["key"] == stage).unwrap();
        let mut steps = steps[..stop].to_vec();
        steps.push(reject(stage));
        steps.push(settle("storage-close:1", Value::Null));
        steps.push(json!({"op":"recover"}));
        steps.push(json!({"op":"advance","ms":1000}));
        cases.push(json!({"name":format!("failed-{stage}"),"steps":steps}));
    }
    let mut steps = boot()[..1].to_vec();
    steps.extend([
        reject("start:1"),
        json!({"op":"reject","key":"storage-close:1","error":{"code":"close_failed"}}),
    ]);
    cases.push(json!({"name":"cleanup-error-replaces-startup-error","steps":steps}));
    let mut steps = boot()[..4].to_vec();
    steps.push(settle("storage-close:1", Value::Null));
    cases.push(
        json!({"name":"oauth-initialize-throws","fail":{"oauth-initialize":{}},"steps":steps}),
    );
    for stage in ["push-start", "address"] {
        let mut steps = boot();
        steps.extend([json!({"op":"recover"}), json!({"op":"advance","ms":1000})]);
        cases.push(
            json!({"name":format!("post-listen-{stage}-throws"),"fail":{stage:{}},"steps":steps}),
        );
    }
    for address in [
        Value::Null,
        json!("named-pipe"),
        json!(false),
        json!(0),
        json!({}),
    ] {
        let mut steps = boot();
        steps.push(json!({"op":"advance","ms":10000}));
        cases.push(json!({"name":"unavailable-or-empty-address-after-listen","address":address,"steps":steps}));
    }
    let mut steps = boot();
    steps.insert(1, json!({"op":"drop","name":"start"}));
    steps.extend(shutdown());
    cases.push(json!({"name":"abandoned-start-keeps-initialization","steps":steps}));
    let mut steps = boot()[..4].to_vec();
    steps.push(settle("packages-initialize:1", Value::Null));
    steps.push(settle("listen:1", Value::Null));
    steps.extend(shutdown());
    cases
        .push(json!({"name":"package-initialization-before-listen","packages":true,"steps":steps}));
    let mut steps = boot()[..4].to_vec();
    steps.push(reject("packages-initialize:1"));
    steps.push(settle("storage-close:1", Value::Null));
    cases.push(json!({"name":"package-initialization-failure","packages":true,"steps":steps}));
    compare("startup", cases);
}

#[test]
fn original_shutdown_callback_errors_and_abandonment() {
    let mut steps = vec![connect("provider", "old", Some(("node", 1.0))), forward()];
    steps.extend(boot());
    steps.extend([
        json!({"op":"close","name":"close"}),
        json!({"op":"advance","ms":10000}),
        settle("push-close:1", Value::Null),
        json!({"op":"events","through":5}),
        json!({"op":"immediates"}),
        settle("http-close:1", Value::Null),
        settle("storage-close:1", Value::Null),
    ]);
    let mut cases = vec![
        json!({"name":"push-close-before-timer-cancel-and-forward-reject","options":{"callTimeoutMs":100000},"steps":steps}),
    ];
    for stage in ["push-close:1", "storage-close:1"] {
        let mut steps = shutdown();
        let at = steps.iter().position(|s| s["key"] == stage).unwrap();
        steps[at] = reject(stage);
        steps.truncate(at + 1);
        cases.push(json!({"name":format!("shutdown-failed-{stage}"),"steps":steps}));
    }
    let mut steps = shutdown();
    steps[2] = reject("http-close:1");
    cases.push(json!({"name":"http-close-callback-error-ignored","steps":steps}));
    for stage in ["ws-close", "connections-close"] {
        cases.push(json!({"name":format!("shutdown-sync-{stage}-failure"),"fail":{stage:{}},"steps":shutdown()[..2].to_vec()}));
    }
    cases.push(json!({"name":"termination-error-prevents-later-close-effects","fail":{"terminate":{}},"steps":([vec![connect("provider","old",None)],shutdown()[..2].to_vec()].concat())}));
    let mut steps = shutdown();
    steps.insert(1, json!({"op":"drop","name":"close"}));
    cases.push(json!({"name":"abandoned-close-still-closes-storage","steps":steps}));
    compare("shutdown", cases);
}

#[test]
fn original_recovery_coalescing_failures_and_close_races() {
    let mut cases = vec![];
    let steps = vec![
        json!({"op":"recover"}),
        json!({"op":"recover"}),
        json!({"op":"advance","ms":999}),
        json!({"op":"advance","ms":1}),
        reject("start:1"),
        json!({"op":"advance","ms":999}),
        json!({"op":"advance","ms":1}),
        settle("start:2", Value::Null),
        reject("binding.diagnostics:1"),
        json!({"op":"advance","ms":1000}),
        settle("start:3", Value::Null),
        settle("binding.diagnostics:2", json!([])),
    ];
    cases.push(json!({"name":"retry-start-and-restore-failures","steps":steps}));
    let mut steps = vec![json!({"op":"recover"})];
    steps.extend(shutdown());
    steps.push(json!({"op":"advance","ms":1000}));
    cases.push(json!({"name":"close-cancels-scheduled-recovery","steps":steps}));
    let resource = json!({"callerPrincipalId":"caller","serviceNodeId":"node","qualifiedName":"provider.call"});
    let diagnostic = json!({"source":"hive.tool-bindings","code":"tool_definition_changed","status":"current","resource":resource,"firstObservedAt":"retained"});
    let mut steps = vec![json!({"op":"recover"}), json!({"op":"advance","ms":1000})];
    steps.extend(shutdown());
    steps.extend([
        settle("start:1", Value::Null),
        settle("binding.diagnostics:1", json!([diagnostic.clone()])),
        json!({"op":"diagnostic-completed","resource":resource.clone()}),
    ]);
    cases.push(json!({"name":"posted-recovery-restores-after-close","steps":steps}));
    let mut steps = vec![json!({"op":"recover"}), json!({"op":"advance","ms":1000})];
    steps.extend(shutdown());
    steps.extend([reject("start:1"), json!({"op":"advance","ms":2000})]);
    cases.push(json!({"name":"posted-recovery-failure-after-close-does-not-retry","steps":steps}));
    let mut steps = vec![
        json!({"op":"recover"}),
        json!({"op":"close","name":"close"}),
        reject("push-close:1"),
        json!({"op":"advance","ms":1000}),
        settle("start:1", Value::Null),
        settle("binding.diagnostics:1", json!([])),
    ];
    steps.push(json!({"op":"recover"}));
    cases.push(json!({"name":"failed-push-close-does-not-cancel-existing-recovery","steps":steps}));
    let mut steps = boot();
    steps[2] = settle("binding.diagnostics:1", json!([diagnostic]));
    steps.push(json!({"op":"diagnostic-completed","resource":resource}));
    steps.extend(shutdown());
    cases.push(json!({"name":"startup-restores-the-shared-diagnostic-owner","steps":steps}));
    compare("recovery", cases);
}

#[test]
fn original_credential_dispatch_commit_order_and_retirement() {
    let old = json!([{"principalId":"caller","digest":"old"}]);
    let new = json!([{"principalId":"caller","digest":"new"}]);
    let mut steps = vec![
        connect("retired", "old", Some(("node", 1.0))),
        connect("kept", "new", None),
        forward(),
        json!({"op":"credentials","name":"configure","value":new,"noFlush":true}),
        json!({"op":"snapshot","noFlush":true}),
        json!({"op":"flush"}),
        json!({"op":"snapshot"}),
        settle("credentials.configure:1", Value::Null),
        json!({"op":"snapshot"}),
    ];
    steps.extend(shutdown());
    let mut cases = vec![
        json!({"name":"accepted-configure-commits-before-retirement","credentials":old,"steps":steps}),
    ];
    cases.push(json!({"name":"rejected-configure-leaves-caches-and-options","credentials":old,"steps":[json!({"op":"credentials","name":"configure","value":new}),reject("credentials.configure:1"),json!({"op":"snapshot"})]}));
    cases.push(json!({"name":"oauth-set-throw-retains-earlier-cache-and-options-effects","credentials":old,"fail":{"oauth-set":{}},"steps":[json!({"op":"credentials","name":"configure","value":new}),settle("credentials.configure:1",Value::Null),json!({"op":"snapshot"})]}));
    cases.push(json!({"name":"terminate-throw-retains-committed-options","credentials":old,"fail":{"terminate":{}},"steps":[connect("retired","old",Some(("node",1.0))),json!({"op":"credentials","name":"configure","value":new}),settle("credentials.configure:1",Value::Null),json!({"op":"snapshot"})]}));
    cases.push(json!({"name":"abandoned-configure-still-commits","credentials":old,"steps":[json!({"op":"credentials","name":"configure","value":new}),json!({"op":"drop","name":"configure"}),settle("credentials.configure:1",Value::Null),json!({"op":"snapshot"})]}));
    cases.push(json!({"name":"credential-changes-serialize-on-the-global-dispatch-owner","credentials":old,"steps":[json!({"op":"credentials","name":"first","value":new}),json!({"op":"credentials","name":"second","value":[]}),settle("credentials.configure:1",Value::Null),json!({"op":"snapshot"}),settle("credentials.configure:2",Value::Null),json!({"op":"snapshot"})]}));
    cases.push(json!({"name":"queued-credential-change-can-expire-before-posting","dispatch":{"maximum":3,"waitMs":50},"steps":[json!({"op":"credentials","name":"first","value":new}),json!({"op":"credentials","name":"second","value":[]}),json!({"op":"advance","ms":50}),settle("credentials.configure:1",Value::Null),json!({"op":"snapshot"})]}));
    cases.push(json!({"name":"dispatch-admission-refusal-never-posts-configure","dispatch":{"maximum":1},"steps":[json!({"op":"credentials","name":"first","value":new}),json!({"op":"credentials","name":"second","value":[]}),settle("credentials.configure:1",Value::Null)]}));
    let routing = |id: &str, generation: f64, principal: &str| {
        let mut update = super::update(id, generation);
        update
            .get_mut("node")
            .unwrap()
            .insert("principalId", principal.into());
        json!({"op":"routing","value":serde_json::from_str::<Value>(&encode(&update)).unwrap()})
    };
    let route = |id: &str| {
        let mut params = super::params();
        params.insert("serviceNodeId", id.into());
        json!({"op":"route","context":{"principalId":"caller","transport":"http"},"params":serde_json::from_str::<Value>(&encode(&params)).unwrap()})
    };
    let mut retired = connect("retired", "provider", Some(("retired-node", 1.0)));
    retired["context"]["principalId"] = json!("provider-principal");
    cases.push(json!({"name":"unconfigured-principal-retires-its-route-and-keeps-configured-route","credentials":[{"principalId":"caller","digest":"old"},{"principalId":"provider-principal","digest":"provider"}],"steps":[routing("retired-node",1.0,"provider-principal"),routing("kept-node",2.0,"caller"),retired,connect("kept","old",Some(("kept-node",2.0))),route("retired-node"),route("kept-node"),json!({"op":"credentials","name":"configure","value":[{"principalId":"caller","digest":"old"}]}),settle("credentials.configure:1",Value::Null),route("retired-node"),route("kept-node"),json!({"op":"snapshot"})]}));
    cases.push(json!({"name":"old-peer-retirement-does-not-disconnect-newer-route-generation","credentials":old,"steps":[routing("node",2.0,"caller"),connect("old-generation","old",Some(("node",1.0))),json!({"op":"credentials","name":"configure","value":new}),settle("credentials.configure:1",Value::Null),route("node")]}));
    compare("credentials", cases);
}

#[test]
fn original_network_timer_publication_and_failure_dispatch() {
    let mut steps = vec![
        connect("provider", "old", Some(("node", 1.0))),
        connect("browser", "old", None),
    ];
    steps.extend(boot());
    steps.extend([
        json!({"op":"events","through":2}),
        json!({"op":"events","through":8}),
        json!({"op":"events","through":3}),
        json!({"op":"immediates"}),
        json!({"op":"events","through":0}),
        json!({"op":"events","through":1.5}),
        json!({"op":"advance","ms":9999}),
        json!({"op":"advance","ms":1}),
        json!({"op":"pong","name":"provider"}),
        json!({"op":"advance","ms":10000}),
    ]);
    steps.extend(shutdown());
    steps.extend([
        json!({"op":"events","through":9}),
        json!({"op":"immediates"}),
    ]);
    let mut cases = vec![json!({"name":"scheduled-publication-and-health","steps":steps})];
    let mut steps = vec![connect("provider", "old", Some(("node", 1.0)))];
    steps.extend(boot());
    steps.extend([
        json!({"op":"advance","ms":10001}),
        json!({"op":"pong","name":"provider"}),
        json!({"op":"advance","ms":9999}),
        json!({"op":"advance","ms":1}),
        json!({"op":"pong","name":"provider"}),
        json!({"op":"advance","ms":35000}),
        json!({"op":"pong","name":"provider"}),
        json!({"op":"advance","ms":9999}),
        json!({"op":"advance","ms":1}),
    ]);
    cases.push(json!({"name":"timer-reinsertion-uses-callback-start-without-small-drift-tolerance","steps":steps}));
    let mut steps = vec![connect("provider", "old", None)];
    steps.extend(boot());
    steps.extend([
        json!({"op":"advance","ms":10000}),
        json!({"op":"advance","ms":10000}),
    ]);
    cases.push(json!({"name":"health-callback-error-reaches-runtime-owner-and-interval-remains","fail":{"ping":{}},"steps":steps}));
    let mut steps = vec![
        connect("provider", "old", Some(("node", 1.0))),
        forward(),
        json!({"op":"failure"}),
        json!({"op":"failure"}),
        json!({"op":"advance","ms":1000}),
        settle("start:1", Value::Null),
        settle("binding.diagnostics:1", json!([])),
    ];
    steps.extend(shutdown());
    cases
        .push(json!({"name":"worker-failure-clears-peer-state-before-one-recovery","steps":steps}));
    cases.push(json!({"name":"worker-failure-callback-error-prevents-recovery","fail":{"terminate":{}},"steps":[connect("provider","old",None),json!({"op":"failure"}),json!({"op":"advance","ms":1000})]}));
    let value = super::update("node", 1.0);
    let update: Value = serde_json::from_str(&encode(&value)).unwrap();
    cases.push(json!({"name":"routing-and-change-notification-dispatch","steps":[json!({"op":"routing","value":update}),json!({"op":"changes","scopes":["wiki"]}),json!({"op":"failure"})]}));
    let mut steps = vec![connect("provider", "old", Some(("node", 1.0)))];
    steps.extend(boot());
    steps.extend([
        json!({"op":"start","name":"second-start"}),
        settle("start:2", Value::Null),
        settle("binding.diagnostics:2", json!([])),
        settle(
            "oauth.initialize:2",
            json!({"signingKey":"signing","referenceKey":"reference"}),
        ),
        settle("listen:2", Value::Null),
        json!({"op":"close","name":"close"}),
        json!({"op":"advance","ms":10000}),
        settle("push-close:1", Value::Null),
        settle("http-close:1", Value::Null),
        settle("storage-close:1", Value::Null),
    ]);
    cases.push(json!({"name":"controlled-listener-repeated-start-retains-both-health-timers-until-close","steps":steps}));
    compare("events-and-timers", cases);
}

#[test]
fn original_optional_package_collection() {
    compare(
        "packages",
        vec![
            json!({"name":"no-package-owner","steps":[{"op":"collect","name":"collect"}]}),
            json!({"name":"collection-owner-error","packages":true,"steps":[{"op":"collect","name":"collect"},{"op":"reject","key":"packages-collect:1"}]}),
            json!({"name":"abandoned-collection-keeps-owner-work","packages":true,"steps":[{"op":"collect","name":"collect"},{"op":"drop","name":"collect"},{"op":"resolve","key":"packages-collect:1"}]}),
        ],
    );
}

#[test]
fn original_startup_and_credentials_keep_optional_await_order() {
    let mut with_packages = boot()[..4].to_vec();
    with_packages.extend([
        json!({"op":"resolve","key":"packages-initialize:1","noFlush":true}),
        json!({"op":"credentials","name":"configure","value":[],"noFlush":true}),
        json!({"op":"flush"}),
        settle("credentials.configure:1", Value::Null),
        settle("listen:1", Value::Null),
    ]);
    with_packages.extend(shutdown());
    let mut without_packages = boot()[..3].to_vec();
    without_packages.extend([
        json!({"op":"resolve","key":"oauth.initialize:1","value":{"signingKey":"signing","referenceKey":"reference"},"noFlush":true}),
        json!({"op":"credentials","name":"configure","value":[],"noFlush":true}),
        json!({"op":"flush"}),
        settle("credentials.configure:1",Value::Null),
        settle("listen:1",Value::Null),
    ]);
    without_packages.extend(shutdown());
    let mut credentials_first = boot()[..3].to_vec();
    credentials_first.extend([
        json!({"op":"credentials","name":"configure","value":[],"noFlush":true}),
        json!({"op":"resolve","key":"oauth.initialize:1","value":{"signingKey":"signing","referenceKey":"reference"},"noFlush":true}),
        json!({"op":"flush"}),
        settle("credentials.configure:1",Value::Null),
        settle("listen:1",Value::Null),
    ]);
    credentials_first.extend(shutdown());
    compare(
        "optional-await-order",
        vec![
            json!({"name":"ready-package-initialization-and-credential-dispatch","packages":true,"steps":with_packages}),
            json!({"name":"absent-package-await-still-yields-before-listen","steps":without_packages}),
            json!({"name":"credential-dispatch-before-startup-continuation","steps":credentials_first}),
        ],
    );
}
