use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::digest,
    json::{parse, stringify},
    validation::validate_request,
};
use ivy_hive::{
    admission::{DispatchGate, RequestBudget, RequestBudgetOptions},
    live_routing::{LiveRouting, PreparedCall},
    packages::{PackageClock, PackageOptions, PackageRegistry, PackageRequest, PackageStorage},
    server_direct::{Dispatcher, Options as DirectOptions},
    server_peers::{Options, Owners, Peer, PeerSocket, Peers, UnhandledKind},
    server_rpc::{Clock, Invocation, Options as InvocationOptions, Reply, Storage},
    tool_binding_diagnostics::ToolBindingDiagnostics,
    worker_client::RequestOptions,
    ws_send::Socket,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{runtime::Builder, sync::oneshot};
#[path = "hive_server_composition.rs"]
mod composition;
#[path = "hive_server_direct.rs"]
mod direct;
#[path = "../fixtures/reference-data.rs"]
mod reference_data;
fn wire(value: &Value) -> IvyJson {
    parse(&value.to_string(), usize::MAX).unwrap()
}
fn encode(value: &IvyJson) -> String {
    stringify(value, usize::MAX).unwrap()
}
fn number(value: Option<&Value>, default: f64) -> f64 {
    match value.and_then(|v| v.get("$number")).and_then(Value::as_str) {
        Some("nan") => f64::NAN,
        Some("infinity") => f64::INFINITY,
        Some("negativeInfinity") => f64::NEG_INFINITY,
        Some("negativeZero") => -0.0,
        Some(other) => panic!("Unknown number {other}"),
        None => value.and_then(Value::as_f64).unwrap_or(default),
    }
}
fn expanded(value: &Value) -> IvyJson {
    if let Some(count) = value.get("$repeat").and_then(Value::as_u64) {
        return "x".repeat(count as usize).into();
    }
    if let Some(units) = value.get("$units").and_then(Value::as_array) {
        return IvyJson::String(JsString(
            units.iter().map(|v| v.as_u64().unwrap() as u16).collect(),
        ));
    }
    match value {
        Value::Array(values) => IvyJson::Array(values.iter().map(expanded).collect()),
        Value::Object(values) => {
            IvyJson::object(values.iter().map(|(k, v)| (k.as_str(), expanded(v))))
        }
        _ => wire(value),
    }
}
fn failure(value: &Value) -> IvyError {
    let message = value
        .get("messageUnits")
        .and_then(Value::as_array)
        .map(|units| JsString(units.iter().map(|v| v.as_u64().unwrap() as u16).collect()))
        .unwrap_or_else(|| value["message"].as_str().unwrap_or("Work failed.").into());
    let outcome = match value["outcome"].as_str().unwrap_or("unknown") {
        "not_executed" => Outcome::NotExecuted,
        "completed" => Outcome::Completed,
        _ => Outcome::Unknown,
    };
    let mut error = IvyError::new_js(
        value["code"].as_str().unwrap_or("remote_error").into(),
        message,
    )
    .with_outcome(outcome);
    if let Some(details) = value.get("details") {
        error = error.with_details(wire(details));
    }
    error
}
type Events = Arc<Mutex<Vec<IvyJson>>>;
type Controls = Arc<Mutex<HashMap<String, oneshot::Sender<IvyResult<IvyJson>>>>>;
fn post(controls: &Controls, events: &Events, key: String, event: IvyJson) -> Reply {
    events.lock().unwrap().push(event);
    let (sender, reply) = oneshot::channel();
    assert!(controls.lock().unwrap().insert(key, sender).is_none());
    Box::pin(async move { reply.await.expect("Fixture must retain accepted controls") })
}
struct Worker {
    events: Events,
    controls: Controls,
}
struct RegistryClock {
    now: Clock,
    dates: super::TraceRuntime,
}
impl PackageClock for RegistryClock {
    fn now(&self) -> f64 {
        (self.now)()
    }
    fn iso_date(&self, value: f64) -> IvyResult<JsString> {
        ivy_hive::retention::Runtime::iso_date(&self.dates, value)
    }
}
struct PackageWorker {
    events: Events,
    controls: Controls,
    count: Mutex<usize>,
}
impl PackageStorage for PackageWorker {
    fn request(&self, payload: IvyJson) -> PackageRequest {
        let mut count = self.count.lock().unwrap();
        *count += 1;
        let name = format!("package:{}", *count);
        post(
            &self.controls,
            &self.events,
            name.clone(),
            IvyJson::object([
                ("kind", "package-worker".into()),
                ("name", name.into()),
                ("payload", payload),
            ]),
        )
    }
}
impl Storage for Worker {
    fn request(&self, payload: IvyJson, options: RequestOptions) -> Reply {
        let execute = payload.get("action") == Some(&"execute".into());
        let key = payload
            .get("request")
            .and_then(|v| v.get("params"))
            .and_then(|v| v.get("label"))
            .and_then(|v| v.as_str().ok())
            .filter(|_| execute)
            .map(|label| format!("worker:{label}"))
            .or_else(|| {
                payload
                    .get("action")
                    .and_then(|v| v.as_str().ok())
                    .filter(|action| {
                        ["diagnostic", "credentials.configure", "binding.diagnostics"]
                            .contains(&action.as_str())
                    })
            });
        let event = IvyJson::object([
            ("kind", "worker".into()),
            ("payload", payload),
            (
                "options",
                IvyJson::object([
                    ("timeoutMs", IvyJson::Number(options.timeout_ms as f64)),
                    ("terminateOnTimeout", options.terminate_on_timeout.into()),
                    (
                        "reservedBytes",
                        IvyJson::Number(options.reserved_bytes as f64),
                    ),
                    ("backup", options.backup.into()),
                ]),
            ),
        ]);
        if let Some(key) = key {
            post(&self.controls, &self.events, key, event)
        } else {
            self.events.lock().unwrap().push(event);
            Box::pin(std::future::ready(if execute {
                Err(IvyError::new(
                    "invalid_frame",
                    "Fixture worker rejected this frame.",
                ))
            } else {
                Ok(IvyJson::Null)
            }))
        }
    }
}
struct SocketState {
    open: bool,
    buffered: f64,
    reads: VecDeque<f64>,
    throw_send: u64,
    throw_terminate: u64,
    callback_error: bool,
}
struct TestSocket {
    name: String,
    state: Arc<Mutex<SocketState>>,
    events: Events,
}
impl Socket for TestSocket {
    fn is_open(&self) -> bool {
        self.state.lock().unwrap().open
    }
    fn buffered_amount(&self) -> f64 {
        let mut s = self.state.lock().unwrap();
        s.reads.pop_front().unwrap_or(s.buffered)
    }
    fn send_text(&mut self, text: String) -> IvyResult<()> {
        let mut event = IvyJson::object([
            ("kind", "send".into()),
            ("peer", self.name.as_str().into()),
            ("byteLength", IvyJson::Number(text.len() as f64)),
            ("sha256", digest(text.as_bytes()).into()),
        ]);
        if text.encode_utf16().count() <= 4096 {
            event.insert("value", parse(&text, usize::MAX).unwrap());
        }
        self.events.lock().unwrap().push(event);
        let callback = {
            let mut state = self.state.lock().unwrap();
            if state.throw_send > 0 {
                state.throw_send -= 1;
                return Err(IvyError::internal());
            }
            state.callback_error
        };
        if callback {
            self.terminate()?;
        }
        Ok(())
    }
    fn terminate(&mut self) -> IvyResult<()> {
        self.events.lock().unwrap().push(IvyJson::object([
            ("kind", "terminate".into()),
            ("peer", self.name.as_str().into()),
        ]));
        let mut state = self.state.lock().unwrap();
        if state.throw_terminate > 0 {
            state.throw_terminate -= 1;
            return Err(IvyError::internal());
        }
        state.open = false;
        Ok(())
    }
}
impl PeerSocket for TestSocket {
    fn ping(&mut self) -> IvyResult<()> {
        self.events.lock().unwrap().push(IvyJson::object([
            ("kind", "ping".into()),
            ("peer", self.name.as_str().into()),
        ]));
        Ok(())
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
fn summary(value: IvyJson) -> IvyJson {
    if let Some(s) = value.as_js_string().filter(|s| s.0.len() > 4096) {
        let text = s.to_utf8().unwrap();
        IvyJson::object([
            ("stringBytes", IvyJson::Number(text.len() as f64)),
            ("sha256", digest(text.as_bytes()).into()),
        ])
    } else {
        value
    }
}
fn original(traces: &[Value]) -> &'static IvyJson {
    static CASES: std::sync::OnceLock<IvyJson> = std::sync::OnceLock::new();
    let cases = CASES.get_or_init(|| {
        let bytes = reference_data::read(include_bytes!(
            "../fixtures/hive-server-peers-reference.json.gz"
        ));
        parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX).unwrap()
    });
    let key = digest(serde_json::to_string(traces).unwrap());
    cases
        .get("traces")
        .unwrap()
        .get(&key)
        .unwrap_or_else(|| panic!("Missing original Hive server peers group: {key}"))
}
fn compare(group: &str, traces: Vec<Value>) {
    compare_sources(group, &traces, original(&traces));
}
// Reporter completion and admission of the next queued operation share no response ordering contract.
fn compared(value: &IvyJson, trace: &Value) -> IvyJson {
    let mut value = value.clone();
    let mut rows = value.get("rows").unwrap().as_array().unwrap().to_vec();
    for (row, step) in rows.iter_mut().zip(trace["steps"].as_array().unwrap()) {
        if step["op"] == "complete"
            && step["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("worker:"))
        {
            let IvyJson::Array(events) = row else {
                unreachable!()
            };
            let reporter = |event: &IvyJson| event.get("kind") == Some(&"invoke-ok".into());
            let credential = |event: &IvyJson| {
                event.get("kind") == Some(&"worker".into())
                    && event
                        .get("payload")
                        .and_then(|payload| payload.get("action"))
                        == Some(&"credentials.configure".into())
            };
            if events.len() == 2 && credential(&events[0]) && reporter(&events[1]) {
                events.swap(0, 1);
            }
        }
    }
    value.insert("rows", IvyJson::Array(rows));
    value
}
fn compare_sources(group: &str, traces: &[Value], original: &IvyJson) {
    assert_eq!(traces.len(), original.as_array().unwrap().len());
    for (trace, source) in traces.iter().zip(original.as_array().unwrap()) {
        let runtime = Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        let actual = runtime.block_on(run_trace(trace, source));
        let actual = parse(&encode(&actual), usize::MAX).unwrap();
        let expected = source.get("value").unwrap();
        if let Some(kinds) = trace["expectKinds"].as_array() {
            let events = expected
                .get("rows")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|row| row.as_array().unwrap());
            let events = events.collect::<Vec<_>>();
            for kind in kinds {
                assert!(
                    events
                        .iter()
                        .any(|event| event.get("kind") == Some(&wire(kind))),
                    "Original trace {} did not reach {kind}: {}",
                    trace["name"],
                    encode(expected)
                );
            }
        }
        let count = trace["steps"].as_array().unwrap().len();
        assert_eq!(actual.get("rows").unwrap().as_array().unwrap().len(), count);
        assert_eq!(
            expected.get("rows").unwrap().as_array().unwrap().len(),
            count
        );
        let actual = compared(&actual, trace);
        let expected = compared(expected, trace);
        if actual != expected {
            let rows = actual.get("rows").unwrap().as_array().unwrap();
            let original = expected.get("rows").unwrap().as_array().unwrap();
            let index = rows.iter().zip(original).position(|(a, b)| a != b).unwrap();
            panic!(
                "{group}: {} step {index}: {}\noriginal: {}\nnative: {}",
                trace["name"],
                trace["steps"][index],
                encode(&original[index]),
                encode(&rows[index])
            );
        }
    }
}

async fn run_trace(trace: &Value, source: &IvyJson) -> IvyJson {
    let events = Events::default();
    let controls = Controls::default();
    let clock = Arc::new(Mutex::new(VecDeque::<f64>::new()));
    let wall = Arc::new(Mutex::new(number(trace.get("now"), 10000.0)));
    let now: Clock = {
        let (events, clock, wall) = (events.clone(), clock.clone(), wall.clone());
        Arc::new(move || {
            let value = clock
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| *wall.lock().unwrap());
            events.lock().unwrap().push(IvyJson::object([
                ("kind", "now".into()),
                ("value", IvyJson::Number(value)),
            ]));
            value
        })
    };
    let diagnostics = {
        let record_events = events.clone();
        let throws = trace["throwRecord"].as_bool().unwrap_or(false);
        Arc::new(Mutex::new(ToolBindingDiagnostics::new(
            Box::new(move |value| {
                record_events.lock().unwrap().push(IvyJson::object([
                    ("kind", "binding-record".into()),
                    ("value", value),
                ]));
                if throws {
                    Err(IvyError::new("fixture_record", "Record failed.")
                        .with_outcome(Outcome::Unknown))
                } else {
                    Ok(())
                }
            }),
            now.clone(),
            Arc::new(super::TraceRuntime(
                source.get("profile").unwrap().clone(),
                events.clone(),
            )),
        )))
    };
    let routing = {
        let diagnostics = diagnostics.clone();
        Arc::new(Mutex::new(LiveRouting::new(
            trace["options"]["validateMessages"] == true,
            Some(Box::new(move |target| {
                diagnostics.lock().unwrap().rejected(&target)
            })),
        )))
    };
    let o = &trace["requests"];
    let requests = RequestBudget::new(RequestBudgetOptions {
        maximum: (number(o.get("maximum"), 2048.0)) as usize,
        maximum_bytes: (number(o.get("maximumBytes"), 134_217_728.0)) as usize,
        per_consumer: (number(o.get("perConsumer"), 256.0)) as usize,
        per_consumer_bytes: (number(o.get("perConsumerBytes"), 67_108_864.0)) as usize,
        name: o["name"].as_str().unwrap_or("request").into(),
    });
    let ids_seen = Arc::new(Mutex::new(Vec::<JsString>::new()));
    let ids = {
        let (events, seen) = (events.clone(), ids_seen.clone());
        let values = Arc::new(Mutex::new(
            trace["ids"]
                .as_array()
                .map(|vs| {
                    vs.iter()
                        .map(|v| v.as_str().unwrap().to_owned())
                        .collect::<VecDeque<_>>()
                })
                .unwrap_or_default(),
        ));
        Arc::new(move || {
            let mut seen = seen.lock().unwrap();
            let value: JsString = values
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| format!("fixture-{}", seen.len() + 1))
                .into();
            events.lock().unwrap().push(IvyJson::object([
                ("kind", "id".into()),
                ("value", IvyJson::String(value.clone())),
            ]));
            seen.push(value.clone());
            value
        })
    };
    let factory_events = events.clone();
    let capture = trace["captureErrorFactory"] == true;
    let direct_events = events.clone();
    let direct_controls = controls.clone();
    let tool_routing = trace["toolRouting"] == true;
    let real_direct = trace["realDirect"] == true;
    let packages = (trace["packages"] != false).then(|| {
        PackageRegistry::new(
            "unused-fixture-root",
            "/hive".into(),
            Arc::new(RegistryClock {
                now: now.clone(),
                dates: super::TraceRuntime(source.get("profile").unwrap().clone(), events.clone()),
            }),
            (trace["packageWorker"] != false).then(|| {
                Arc::new(PackageWorker {
                    events: events.clone(),
                    controls: controls.clone(),
                    count: Mutex::new(0),
                }) as Arc<dyn PackageStorage>
            }),
            PackageOptions::default(),
        )
    });
    let dispatcher = Dispatcher::new(
        now.clone(),
        packages.clone(),
        DirectOptions {
            validate_messages: trace["options"]["validateMessages"] == true,
            package_publisher_principal_ids: trace["options"]["packagePublisherPrincipalIds"]
                .as_array()
                .map(|values| values.iter().map(|v| v.as_str().unwrap().into()).collect())
                .unwrap_or_default(),
        },
    );
    let peer_dispatcher = dispatcher.clone();
    let wake_events = events.clone();
    let cache_events = events.clone();
    let unhandled_events = events.clone();
    let storage = Arc::new(Worker {
        events: events.clone(),
        controls: controls.clone(),
    });
    let peer_owners = Owners {
        storage: storage.clone(),
        routing: routing.clone(),
        diagnostics: diagnostics.clone(),
        requests: requests.clone(),
        dispatch: DispatchGate::new(
            trace["dispatch"]["maximum"].as_u64().unwrap_or(1024) as usize,
            trace["dispatch"]["waitMs"].as_u64().unwrap_or(30000),
        ),
        now: now.clone(),
        ids,
        direct: Arc::new(move |owner, context, request, peer| {
            if real_direct {
                return peer_dispatcher.dispatch(owner, context, request, Some(peer));
            }
            if tool_routing
                && (request.get("method") == Some(&"tools.call".into())
                    || request.get("method") == Some(&"discovery.call".into()))
            {
                direct_events.lock().unwrap().push(IvyJson::object([
                    ("kind", "routing".into()),
                    ("context", context.clone()),
                    ("params", request.required("params")?.clone()),
                ]));
                return Err(IvyError::new(
                    "fixture_routing",
                    "Selected routing owner reached.",
                ));
            }
            if request.get("method") == Some(&"notifications.subscribe".into()) {
                validate_request(request)?;
                return owner
                    .subscribe(Some(peer), context, request.required("params")?)
                    .map(Some);
            }
            let p = request.get("params");
            match p
                .and_then(|p| p.get("fixtureDirect"))
                .and_then(|v| v.as_str().ok())
                .as_deref()
            {
                Some("return") => {
                    let label = p.unwrap().get("label").unwrap().as_str().unwrap();
                    Ok(Some(post(
                        &direct_controls,
                        &direct_events,
                        format!("direct:{label}"),
                        IvyJson::object([("kind", "direct-start".into()), ("label", label.into())]),
                    )))
                }
                Some("throw") => Err(failure(
                    &serde_json::from_str::<Value>(&encode(
                        p.unwrap().get("error").unwrap_or(&IvyJson::Null),
                    ))
                    .unwrap(),
                )),
                _ => Ok(None),
            }
        }),
        provider_error: Arc::new(move |parts| {
            let mut event = IvyJson::object([
                ("kind", "provider-error-input".into()),
                ("code", IvyJson::String(parts.code.clone())),
                ("message", IvyJson::String(parts.message.clone())),
                ("outcome", parts.outcome.clone()),
            ]);
            if let Some(details) = &parts.details {
                event.insert("details", details.clone());
            }
            factory_events.lock().unwrap().push(event);
            if capture {
                return IvyError::new("fixture_factory", "Captured canonical error input.")
                    .with_outcome(Outcome::Unknown);
            }
            let mut error =
                IvyError::new_js(parts.code, parts.message).with_provider_outcome(parts.outcome);
            if let Some(details) = parts.details {
                error = error.with_details(details);
            }
            error
        }),
        wake_browser_push: Arc::new(move || {
            wake_events
                .lock()
                .unwrap()
                .push(IvyJson::object([("kind", "wake".into())]))
        }),
        clear_authentication: Arc::new(move || {
            cache_events
                .lock()
                .unwrap()
                .push(IvyJson::object([("kind", "cache-clear".into())]))
        }),
        unhandled: Arc::new(move |kind, error| {
            unhandled_events.lock().unwrap().push(IvyJson::object([
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
        }),
    };
    let (owner, server, immediates) = if trace["composed"] == true {
        let (server, immediates) = composition::compose(
            trace,
            source,
            peer_owners,
            storage.clone(),
            packages,
            events.clone(),
            controls.clone(),
        );
        (server.peers.clone(), Some(server), immediates)
    } else {
        (
            Peers::new(
                peer_owners,
                Options {
                    call_timeout_ms: trace["options"]
                        .get("callTimeoutMs")
                        .map(|v| number(Some(v), 30000.0)),
                    validate_messages: trace["options"]["validateMessages"] == true,
                },
            ),
            None,
            composition::Immediates::default(),
        )
    };
    let http_peers = owner.clone();
    let invocation = if let Some(server) = &server {
        server.invocation.clone()
    } else {
        Invocation::new(
            storage,
            Arc::new(move |context, request| {
                dispatcher.dispatch(&http_peers, context, request, None)
            }),
            now,
            requests.clone(),
            DispatchGate::new(1024, 30_000),
            InvocationOptions::default(),
        )
    };
    let mut peers = HashMap::<String, Peer>::new();
    let mut sockets = HashMap::<String, Arc<Mutex<SocketState>>>::new();
    let mut call_ids = HashMap::<String, JsString>::new();
    let mut watchers = HashMap::new();
    let mut publication = false;
    let mut rows = vec![];
    for step in trace["steps"].as_array().unwrap() {
        if let Some(values) = step["clock"].as_array() {
            *clock.lock().unwrap() = values.iter().map(|v| number(Some(v), 0.0)).collect();
        }
        if step.get("now").is_some() {
            *wall.lock().unwrap() = number(step.get("now"), 0.0);
        }
        let name = step["name"].as_str().unwrap_or("");
        let peer = step["peer"].as_str().map(|p| peers.get(p).unwrap().clone());
        let result = (|| -> IvyResult<()> {
            match step["op"].as_str().unwrap() {
                "connect" => {
                    let s = &step["socket"];
                    let state = Arc::new(Mutex::new(SocketState {
                        open: s["open"] != false,
                        buffered: number(s.get("buffered"), 0.0),
                        reads: s["bufferReads"]
                            .as_array()
                            .map(|vs| vs.iter().map(|v| number(Some(v), 0.0)).collect())
                            .unwrap_or_default(),
                        throw_send: s["throwSend"].as_u64().unwrap_or(0),
                        throw_terminate: s["throwTerminate"].as_u64().unwrap_or(0),
                        callback_error: s["callbackError"] == true,
                    }));
                    let peer = owner.connected(
                        Box::new(TestSocket {
                            name: name.to_owned(),
                            state: state.clone(),
                            events: events.clone(),
                        }),
                        wire(&step["context"]),
                    );
                    peers.insert(name.to_owned(), peer);
                    sockets.insert(name.to_owned(), state);
                }
                "update" => owner.routing_update(&wire(step))?,
                "prepare" => {
                    let value = routing
                        .lock()
                        .unwrap()
                        .prepare(&wire(&step["context"]), &wire(&step["params"]))?
                        .to_json();
                    events.lock().unwrap().push(IvyJson::object([
                        ("kind", "prepared".into()),
                        ("value", value),
                    ]));
                }
                "forward" => {
                    let before = ids_seen.lock().unwrap().len();
                    let work =
                        owner.forward(&wire(&step["context"]), prepared(&expanded(&step["call"])));
                    {
                        let seen = ids_seen.lock().unwrap();
                        if seen.len() > before {
                            let mut id = JsString::from("hive-call-");
                            id.0.extend(seen.last().unwrap().0.iter());
                            call_ids.insert(name.to_owned(), id);
                        }
                    }
                    let (events, name) = (events.clone(), name.to_owned());
                    watchers.insert(
                        name.clone(),
                        tokio::spawn(async move {
                            let event = match work.await {
                                Ok(value) => IvyJson::object([
                                    ("kind", "forward-ok".into()),
                                    ("name", name.into()),
                                    ("value", summary(value)),
                                ]),
                                Err(error) => IvyJson::object([
                                    ("kind", "forward-error".into()),
                                    ("name", name.into()),
                                    ("error", error.to_wire()),
                                ]),
                            };
                            events.lock().unwrap().push(event);
                        }),
                    );
                }
                "result" => {
                    let mut frame = expanded(&step["frame"]);
                    if let Some(name) = step["call"].as_str() {
                        frame.insert("id", IvyJson::String(call_ids[name].clone()));
                    }
                    owner.provider_result(peer.as_ref().unwrap(), &frame);
                }
                "message" => {
                    let bytes = if let Some(bytes) = step["bytes"].as_array() {
                        bytes.iter().map(|b| b.as_u64().unwrap() as u8).collect()
                    } else if let Some(text) = step["text"].as_str() {
                        text.as_bytes().to_vec()
                    } else {
                        encode(&expanded(&step["frame"])).into_bytes()
                    };
                    owner.message(peer.as_ref().unwrap(), &bytes, step["binary"] == true)?;
                }
                "direct" => {
                    let work = invocation
                        .direct(&expanded(&step["context"]), &expanded(&step["frame"]))?;
                    events.lock().unwrap().push(IvyJson::object([
                        ("kind", "direct-return".into()),
                        ("name", name.into()),
                    ]));
                    match work {
                        None => events.lock().unwrap().push(IvyJson::object([
                            ("kind", "direct-fallback".into()),
                            ("name", name.into()),
                        ])),
                        Some(work) => {
                            if step["dropResult"] == true {
                                drop(work);
                                return Ok(());
                            }
                            let (events, name) = (events.clone(), name.to_owned());
                            watchers.insert(
                                name.clone(),
                                tokio::spawn(async move {
                                    let event = match work.await {
                                        Ok(value) => IvyJson::object([
                                            ("kind", "direct-ok".into()),
                                            ("name", name.into()),
                                            ("value", value),
                                        ]),
                                        Err(error) => IvyJson::object([
                                            ("kind", "direct-error".into()),
                                            ("name", name.into()),
                                            ("error", error.to_wire()),
                                        ]),
                                    };
                                    events.lock().unwrap().push(event);
                                }),
                            );
                        }
                    }
                }
                "invoke" => {
                    let work = invocation.invoke(
                        expanded(&step["context"]),
                        expanded(&step["frame"]),
                        (number(step.get("receivedBytes"), 0.0)) as usize,
                    );
                    let (events, name) = (events.clone(), name.to_owned());
                    watchers.insert(
                        name.clone(),
                        tokio::spawn(async move {
                            let event = match work.await {
                                Ok(value) => IvyJson::object([
                                    ("kind", "invoke-ok".into()),
                                    ("name", name.into()),
                                    ("value", value),
                                ]),
                                Err(error) => IvyJson::object([
                                    ("kind", "invoke-error".into()),
                                    ("name", name.into()),
                                    ("error", error.to_wire()),
                                ]),
                            };
                            events.lock().unwrap().push(event);
                        }),
                    );
                }
                "credentials" => {
                    let work = server
                        .as_ref()
                        .unwrap()
                        .lifecycle
                        .set_credentials(composition::credentials(&step["value"]));
                    let (events, name) = (events.clone(), name.to_owned());
                    watchers.insert(
                        name.clone(),
                        tokio::spawn(async move {
                            let event = match work.await {
                                Ok(()) => IvyJson::object([
                                    ("kind", "credentials-ok".into()),
                                    ("name", name.into()),
                                ]),
                                Err(error) => IvyJson::object([
                                    ("kind", "credentials-error".into()),
                                    ("name", name.into()),
                                    ("error", error.to_wire()),
                                ]),
                            };
                            events.lock().unwrap().push(event);
                        }),
                    );
                }
                "complete" => {
                    let control = controls.lock().unwrap().remove(name).unwrap_or_else(|| {
                        panic!("Unknown fixture control {name} in {}", trace["name"])
                    });
                    let value = if let Some(error) = step.get("error") {
                        Err(failure(error))
                    } else {
                        let value = expanded(&step["value"]);
                        Ok(if name.starts_with("worker:") {
                            IvyJson::object([
                                ("kind", step["kind"].as_str().unwrap_or("result").into()),
                                ("value", value),
                            ])
                        } else {
                            value
                        })
                    };
                    let _ = control.send(value);
                }
                "close" => {
                    sockets[step["peer"].as_str().unwrap()].lock().unwrap().open = false;
                    owner.closed(peer.as_ref().unwrap())?;
                }
                "pong" => owner.pong(peer.as_ref().unwrap()),
                "socket" => {
                    let mut s = sockets[step["peer"].as_str().unwrap()].lock().unwrap();
                    let v = &step["values"];
                    if let Some(value) = v.get("open") {
                        s.open = value == true;
                    }
                    if let Some(value) = v.get("buffered") {
                        s.buffered = number(Some(value), 0.0);
                    }
                    if let Some(value) = v["reads"].as_array() {
                        s.reads = value.iter().map(|v| number(Some(v), 0.0)).collect();
                    }
                    if let Some(value) = v["throwSend"].as_u64() {
                        s.throw_send = value;
                    }
                    if let Some(value) = v["throwTerminate"].as_u64() {
                        s.throw_terminate = value;
                    }
                    if let Some(value) = v.get("callbackError") {
                        s.callback_error = value == true;
                    }
                }
                "changes" => {
                    if let Some(server) = &server {
                        server.lifecycle.worker_event(
                            ivy_hive::worker_runtime::Event::Notification(
                                ivy_hive::worker_protocol::Notification::Changes(
                                    step["scopes"]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .map(|v| v.as_str().unwrap().into())
                                        .collect(),
                                ),
                            ),
                        );
                    } else {
                        owner.changes(
                            &step["scopes"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|v| v.as_str().unwrap().into())
                                .collect::<Vec<_>>(),
                        )?;
                    }
                }
                "health" => owner.health()?,
                "events" => {
                    if let Some(server) = &server {
                        server.lifecycle.worker_event(
                            ivy_hive::worker_runtime::Event::Notification(
                                ivy_hive::worker_protocol::Notification::EventsAvailable(
                                    IvyJson::object([(
                                        "throughSequence",
                                        expanded(&step["through"]),
                                    )]),
                                ),
                            ),
                        );
                    } else {
                        publication =
                            owner.events_available(number(step.get("through"), 0.0)) || publication;
                    }
                }
                "publish" => {
                    let work = immediates.lock().unwrap().drain(..).collect::<Vec<_>>();
                    for work in work {
                        work();
                    }
                    if publication {
                        publication = false;
                        owner.publish_events()?;
                    }
                }
                "stop" => owner.stop_publication(),
                "shutdown" => owner.reject_for_shutdown()?,
                "failure" => {
                    if let Some(server) = &server {
                        server
                            .lifecycle
                            .worker_event(ivy_hive::worker_runtime::Event::Failure(
                                IvyError::internal(),
                            ));
                    } else {
                        owner.storage_failed()?;
                        events
                            .lock()
                            .unwrap()
                            .push(IvyJson::object([("kind", "recovery".into())]));
                    }
                }
                "reject" => owner.reject_all(failure(&step["error"])),
                "drop" => watchers[name].abort(),
                "inspect" => events.lock().unwrap().push(IvyJson::object([
                    ("kind", "context".into()),
                    ("peer", step["peer"].as_str().unwrap().into()),
                    ("value", peer.as_ref().unwrap().context()),
                ])),
                "rejected" => diagnostics
                    .lock()
                    .unwrap()
                    .rejected(&wire(&step["target"]))?,
                "probe" => {
                    let result = requests.acquire(
                        &step["key"].as_str().unwrap().into(),
                        (step.get("bytes").map(|v| number(Some(v), 1024.0)))
                            .map(|bytes| bytes as usize),
                    );
                    events.lock().unwrap().push(match result {
                        Ok(mut lease) => {
                            lease.release();
                            IvyJson::object([("kind", "probe".into()), ("ok", true.into())])
                        }
                        Err(error) => {
                            IvyJson::object([("kind", "probe".into()), ("error", error.to_wire())])
                        }
                    });
                }
                "advance" => {}
                other => panic!("Unknown step {other}"),
            }
            Ok(())
        })();
        if let Err(error) = result {
            events.lock().unwrap().push(IvyJson::object([
                ("kind", "operation-error".into()),
                ("error", error.to_wire()),
            ]));
        }
        if step["op"] == "advance" {
            tokio::time::advance(Duration::from_millis(step["ms"].as_u64().unwrap())).await;
        }
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        rows.push(IvyJson::Array(std::mem::take(&mut *events.lock().unwrap())));
    }
    IvyJson::object([
        ("name", wire(&trace["name"])),
        ("rows", IvyJson::Array(rows)),
    ])
}

fn context(node: Option<(&str, f64)>) -> Value {
    let mut value = json!({"principalId":"provider-principal","credentialDigest":"credential","transport":"ws"});
    if let Some((id, generation)) = node {
        value["principalId"] = json!("provider-principal");
        value["serviceNodeId"] = json!(id);
        value["generation"] = json!(generation);
    }
    value
}

#[test]
fn tool_routing_websocket_validation_before_routing_and_session_release() {
    let mut traces = vec![];
    for (name, request) in super::tool_routing_cases::requests() {
        traces.push(json!({"name":name,"toolRouting":true,"requests":{"perConsumer":1},"steps":[connect("caller",None),message("caller",request),{"op":"probe","key":"credential","bytes":0}]}));
    }
    let malformed = json!({"jsonrpc":"2.0","id":"call","method":"tools.call","params":{}});
    let mut steps = vec![connect("caller", None)];
    for _ in 0..70 {
        steps.push(message("caller", malformed.clone()));
    }
    steps.push(message(
        "caller",
        super::tool_routing_cases::requests()[0].1.clone(),
    ));
    steps.push(json!({"op":"probe","key":"credential","bytes":0}));
    traces.push(json!({"name":"invalid-calls-release-session-and-budget","toolRouting":true,"requests":{"perConsumer":1},"steps":steps}));
    for (name, requests, caller, clock) in [
        (
            "expired-before-admission",
            json!({}),
            json!({"principalId":"caller","credentialDigest":"credential","transport":"ws","expiresAt":0}),
            json!([]),
        ),
        (
            "expired-at-direct",
            json!({"perConsumer":1}),
            json!({"principalId":"caller","credentialDigest":"credential","transport":"ws","expiresAt":10}),
            json!([0, 10]),
        ),
        (
            "budget-before-validation",
            json!({"maximum":0}),
            context(None),
            json!([]),
        ),
    ] {
        traces.push(json!({"name":name,"toolRouting":true,"requests":requests,"steps":[{"op":"connect","name":"caller","context":caller},{"op":"message","peer":"caller","frame":malformed,"clock":clock},{"op":"probe","key":"credential","bytes":0}]}));
    }
    compare("tool-routing-peers", traces);
}
fn connect(name: &str, node: Option<(&str, f64)>) -> Value {
    json!({"op":"connect","name":name,"context":context(node)})
}
fn update(node: &str, generation: f64) -> Value {
    let value = super::update(node, generation);
    serde_json::from_str(&encode(&value)).unwrap()
}
fn call(node: &str, generation: f64) -> Value {
    let binding = super::binding("provider.call", true);
    json!({"serviceNodeId":node,"generation":generation,"definition":serde_json::from_str::<Value>(&encode(binding.get("definition").unwrap())).unwrap(),"definitionHash":binding.get("definitionHash").unwrap().as_str().unwrap(),"arguments":1,"callerPrincipalId":"caller"})
}
fn forward(name: &str, node: &str, generation: f64) -> Value {
    json!({"op":"forward","name":name,"context":{"principalId":"caller","credentialDigest":"credential","transport":"http"},"call":call(node,generation)})
}
fn result(peer: &str, call: &str, value: Value) -> Value {
    json!({"op":"result","peer":peer,"call":call,"frame":{"jsonrpc":"2.0","id":"assigned","result":value}})
}
fn request(label: &str, direct: bool) -> Value {
    json!({"jsonrpc":"2.0","id":label,"method":"fixture.work","params":{"label":label,"fixtureDirect":if direct{"return"}else{"none"}}})
}
fn message(peer: &str, frame: Value) -> Value {
    json!({"op":"message","peer":peer,"frame":frame})
}
fn complete(label: &str, direct: bool, value: Value) -> Value {
    json!({"op":"complete","name":format!("{}:{label}",if direct{"direct"}else{"worker"}),"value":value})
}
fn subscribe(peer: &str, filters: Value, changes: Option<Value>) -> Value {
    let mut params = json!({"filters":filters});
    if let Some(changes) = changes {
        params["changes"] = changes;
    }
    message(
        peer,
        json!({"jsonrpc":"2.0","id":"subscribe","method":"notifications.subscribe","params":params}),
    )
}

#[test]
fn original_provider_forwarding_correlation_deadlines_and_errors() {
    let mut traces = vec![];
    for (name, mut steps) in [
        (
            "success",
            vec![
                connect("p", Some(("node", 1.0))),
                update("node", 1.0),
                forward("a", "node", 1.0),
                result("p", "a", json!("done")),
            ],
        ),
        (
            "no-peer",
            vec![update("node", 1.0), forward("a", "node", 1.0)],
        ),
        (
            "wrong-generation",
            vec![connect("p", Some(("node", 2.0))), forward("a", "node", 1.0)],
        ),
        (
            "first-open-exact-peer",
            vec![
                connect("closed", Some(("node", 1.0))),
                json!({"op":"socket","peer":"closed","values":{"open":false}}),
                connect("p", Some(("node", 1.0))),
                connect("other", Some(("node", 1.0))),
                update("node", 1.0),
                forward("a", "node", 1.0),
                result("other", "a", json!("wrong")),
                result("p", "a", json!("right")),
            ],
        ),
        (
            "stale-routing-completion",
            vec![
                connect("p", Some(("node", 1.0))),
                update("node", 1.0),
                forward("a", "node", 1.0),
                update("node", 2.0),
                result("p", "a", json!("old")),
            ],
        ),
        (
            "close-rejects-and-disconnects",
            vec![
                connect("p", Some(("node", 1.0))),
                update("node", 1.0),
                forward("a", "node", 1.0),
                json!({"op":"close","peer":"p"}),
            ],
        ),
        (
            "old-close-does-not-disconnect-replacement",
            vec![
                connect("p", Some(("node", 1.0))),
                update("node", 1.0),
                forward("a", "node", 1.0),
                connect("replacement", Some(("node", 2.0))),
                update("node", 2.0),
                json!({"op":"close","peer":"p"}),
            ],
        ),
        (
            "dropped-result-still-completes",
            vec![
                connect("p", Some(("node", 1.0))),
                update("node", 1.0),
                forward("a", "node", 1.0),
                json!({"op":"drop","name":"a"}),
                result("p", "a", json!("done")),
            ],
        ),
        (
            "deadline",
            vec![
                connect("p", Some(("node", 1.0))),
                forward("a", "node", 1.0),
                json!({"op":"advance","ms":30000}),
                result("p", "a", json!("late")),
            ],
        ),
        (
            "reject-all",
            vec![
                connect("p", Some(("node", 1.0))),
                forward("a", "node", 1.0),
                forward("b", "node", 1.0),
                json!({"op":"reject","error":{"code":"held","message":"Reconcile.","outcome":"unknown"}}),
                json!({"op":"advance","ms":30000}),
            ],
        ),
    ] {
        traces.push(json!({"name":name,"steps":std::mem::take(&mut steps)}));
    }
    for operation in [
        json!(null),
        json!(false),
        json!(0),
        json!(""),
        json!("operation"),
        json!({"raw":true}),
    ] {
        let mut f = forward("a", "node", 1.0);
        f["call"]["operationId"] = operation.clone();
        if operation.is_object() {
            // Malformed IDs enter through the public guard, before a typed PreparedCall exists.
            let params = json!({"qualifiedName":"provider.call","expectedDefinitionHash":f["call"]["definitionHash"],"arguments":1,"serviceNodeId":"node","operationId":operation});
            let call = json!({"op":"direct","name":"invalid-operation","context":context(None),"frame":{"jsonrpc":"2.0","id":"call","method":"tools.call","params":params}});
            traces.push(json!({"name":format!("operation-{operation}"),"realDirect":true,"expectKinds":["operation-error"],"steps":[connect("p",Some(("node",1.0))),call,{"op":"close","peer":"p"}]}));
        } else {
            traces.push(json!({"name":format!("operation-{operation}"),"steps":[connect("p",Some(("node",1.0))),f,{"op":"close","peer":"p"}]}));
        }
    }
    for outcome in ["not_executed", "unknown", "completed"] {
        traces.push(json!({"name":format!("error-{outcome}"),"steps":[connect("p",Some(("node",1.0))),forward("a","node",1.0),{"op":"result","peer":"p","call":"a","frame":{"jsonrpc":"2.0","id":"assigned","error":{"code":27,"message":{"$units":[101,55296]},"data":{"code":{"$units":[99,56320]},"outcome":outcome,"details":{"kept":true}}}}}]}));
    }
    for bad in [
        json!({"result":1}),
        json!({"jsonrpc":"2.0","result":1,"error":{}}),
        json!({"jsonrpc":"2.0","result":1,"extra":true}),
        json!({"jsonrpc":"2.0","error":{"code":-32000,"message":"m","data":{"code":"x","outcome":"invalid"}}}),
    ] {
        traces.push(json!({"name":format!("bad-response-{bad}"),"steps":[connect("p",Some(("node",1.0))),forward("a","node",1.0),{"op":"result","peer":"p","call":"a","frame":bad},{"op":"advance","ms":30000}]}));
    }
    for timeout in [
        json!(0),
        json!(-1),
        json!(1.9),
        json!({"$number":"nan"}),
        json!({"$number":"infinity"}),
        json!(2147483648u64),
    ] {
        traces.push(json!({"name":format!("timeout-{timeout}"),"options":{"callTimeoutMs":timeout},"steps":[connect("p",Some(("node",1.0))),forward("a","node",1.0),{"op":"advance","ms":1}]}));
    }
    let mut expiry = forward("a", "node", 1.0);
    expiry["context"]["expiresAt"] = json!(10000);
    traces.push(json!({"name":"expired-forward-before-selection","steps":[expiry]}));
    let mut steps = vec![connect("p", Some(("node", 1.0)))];
    for index in 0..65 {
        steps.push(forward(&format!("call-{index}"), "node", 1.0));
    }
    steps.push(json!({"op":"close","peer":"p"}));
    traces.push(json!({"name":"per-provider-64","steps":steps}));
    let mut steps = vec![];
    for n in 0..9 {
        steps.push(connect(&format!("p{n}"), Some((&format!("node{n}"), 1.0))));
    }
    for index in 0..513 {
        steps.push(forward(
            &format!("call-{index}"),
            &format!("node{}", index / 64),
            1.0,
        ));
    }
    steps.push(
        json!({"op":"reject","error":{"code":"clear","message":"Clear.","outcome":"unknown"}}),
    );
    traces.push(json!({"name":"global-512","steps":steps}));
    traces.push(json!({"name":"synchronous-send-keeps-prior-pending","steps":[{"op":"connect","name":"p","context":context(Some(("node",1.0))),"socket":{"throwSend":1}},forward("a","node",1.0),{"op":"close","peer":"p"},{"op":"advance","ms":30000}]}));
    traces.push(json!({"name":"uuid-collision-keeps-original-timer-rejector","ids":["same","same"],"steps":[connect("p",Some(("node",1.0))),forward("a","node",1.0),{"op":"advance","ms":1000},forward("b","node",1.0),{"op":"advance","ms":29000},{"op":"advance","ms":1000}]}));
    for validate in [false, true] {
        traces.push(json!({"name":format!("dispatched-output-schema-{validate}"),"options":{"validateMessages":validate},"steps":[connect("p",Some(("node",1.0))),update("node",1.0),forward("a","node",1.0),result("p","a",json!(7))]}));
    }
    compare("forwarding", traces);
}

#[test]
fn original_provider_buffer_and_encoding_boundaries() {
    let packet = json!({"jsonrpc":"2.0","id":"hive-call-fixture-1","method":"provider.invoke","params":{"qualifiedName":"provider.call","arguments":1,"definitionHash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","generation":1,"callerPrincipalId":"caller"}});
    let bytes = packet.to_string().len() as i64;
    let mut traces = vec![];
    for delta in [-1, 0, 1] {
        traces.push(json!({"name":format!("provider-buffer-{delta}"),"steps":[{"op":"connect","name":"p","context":context(Some(("node",1.0))),"socket":{"buffered":67_108_864-bytes+delta}},forward("a","node",1.0),{"op":"close","peer":"p"}]}));
    }
    traces.push(json!({"name":"second-buffer-read-terminates-after-insertion","steps":[{"op":"connect","name":"p","context":context(Some(("node",1.0))),"socket":{"bufferReads":[0,67_108_864]}},forward("a","node",1.0),{"op":"close","peer":"p"}]}));
    traces.push(json!({"name":"asynchronous-send-error-owned-by-socket","steps":[{"op":"connect","name":"p","context":context(Some(("node",1.0))),"socket":{"callbackError":true}},forward("a","node",1.0),{"op":"close","peer":"p"}]}));
    let mut oversized = forward("a", "node", 1.0);
    oversized["call"]["arguments"] = json!({"$repeat":33_554_433});
    traces.push(json!({"name":"encoded-call-overflow-before-pending","steps":[connect("p",Some(("node",1.0))),oversized,{"op":"advance","ms":30000}]}));
    compare("provider-bounds", traces);
}

#[test]
fn original_socket_admission_queues_and_connection_generations() {
    let mut traces = vec![];
    traces.push(json!({"name":"worker-direct-interleave","steps":[connect("c",None),message("c",request("a",false)),message("c",request("b",false)),message("c",request("c",true)),complete("c",true,json!(3)),complete("a",false,json!(1)),complete("b",false,json!(2))]}));
    traces.push(json!({"name":"abandoned-socket-still-owns-admission","requests":{"perConsumer":1},"steps":[connect("c",None),message("c",request("a",false)),{"op":"close","peer":"c"},{"op":"probe","key":"credential","bytes":0},complete("a",false,json!(1)),{"op":"probe","key":"credential","bytes":0}]}));
    let mut frame = request("throw", true);
    frame["params"]["fixtureDirect"] = json!("throw");
    frame["params"]["error"] = json!({"code":"direct_error","outcome":"completed"});
    traces.push(json!({"name":"direct-synchronous-failure-refunds","requests":{"perConsumer":1},"steps":[connect("c",None),message("c",frame),{"op":"probe","key":"credential","bytes":0}]}));
    traces.push(json!({"name":"worker-failure-refunds","requests":{"perConsumer":1},"steps":[connect("c",None),message("c",request("a",false)),{"op":"complete","name":"worker:a","error":{"code":"deadline_exceeded","message":"Retain operation.","outcome":"unknown"}},{"op":"probe","key":"credential","bytes":0}]}));
    traces.push(json!({"name":"response-send-failure-retries-error-envelope","steps":[connect("c",None),message("c",request("a",false)),{"op":"socket","peer":"c","values":{"throwSend":1}},complete("a",false,json!(1)),{"op":"probe","key":"credential","bytes":0}]}));
    for frame in [
        json!(null),
        json!([]),
        json!({}),
        json!({"method":"ignored"}),
        json!({"id":true}),
        json!({"id":"response","result":1}),
        json!({"id":1,"result":1}),
    ] {
        let needs_worker = frame == Value::Null
            || frame == json!([])
            || frame.get("id").is_some_and(|id| !id.is_string());
        let mut steps = vec![connect("c", None)];
        if needs_worker {
            let mut f = frame.clone();
            if f.is_object() {
                f["params"] = json!({"label":"x"});
                steps.push(message("c", f));
                steps.push(complete("x", false, json!(1)));
            } else {
                steps.push(message("c", f));
            }
        } else {
            steps.push(message("c", frame.clone()));
        }
        traces.push(json!({"name":format!("classification-{frame}"),"steps":steps}));
    }
    for raw in [
        json!({"text":"{"}),
        json!({"bytes":[255]}),
        json!({"text":"{}","binary":true}),
        json!({"bytes":[239,187,191,123,125]}),
    ] {
        let mut msg = json!({"op":"message","peer":"c"});
        for (k, v) in raw.as_object().unwrap() {
            msg[k] = v.clone();
        }
        traces.push(json!({"name":format!("parse-{raw}"),"steps":[connect("c",None),msg]}));
    }
    let mut steps = vec![connect("c", None)];
    for index in 0..65 {
        steps.push(message("c", request(&format!("direct-{index}"), true)));
    }
    steps.push(complete("direct-0", true, json!(0)));
    steps.push(message("c", request("replacement", true)));
    for index in 1..64 {
        steps.push(complete(&format!("direct-{index}"), true, json!(index)));
    }
    steps.push(complete("replacement", true, json!(9)));
    steps.push(json!({"op":"probe","key":"credential","bytes":0}));
    traces.push(json!({"name":"64-socket-active-slots","steps":steps}));
    traces.push(json!({"name":"global-gate-rejects-and-refunds","dispatch":{"maximum":1},"steps":[connect("a",None),connect("b",None),message("a",request("a",false)),message("b",request("b",false)),complete("a",false,json!(1)),{"op":"probe","key":"credential","bytes":0}]}));
    let connect_frame = |name: &str| json!({"jsonrpc":"2.0","id":name,"method":"service.connect","params":{"label":name}});
    traces.push(json!({"name":"generation-replacement-and-spread-context","steps":[connect("old",None),message("old",connect_frame("old")),update("node",1.0),complete("old",false,json!({"serviceNodeId":"node","generation":1,"extra":"retained"})),{"op":"inspect","peer":"old"},connect("new",None),message("new",connect_frame("new")),update("node",2.0),complete("new",false,json!({"serviceNodeId":"node","generation":2})),{"op":"inspect","peer":"new"},{"op":"close","peer":"old"},message("old",request("stale",false)),{"op":"close","peer":"new"}]}));
    traces.push(json!({"name":"connect-non-result-does-not-rebind","steps":[connect("c",None),message("c",connect_frame("a")),{"op":"complete","name":"worker:a","kind":"other","value":{"serviceNodeId":"node","generation":99}},{"op":"inspect","peer":"c"}]}));
    let mut registry = request("sync", false);
    registry["method"] = json!("registry.sync");
    traces.push(json!({"name":"registry-sync-suspends-before-dispatch","steps":[connect("p",Some(("node",1.0))),update("node",1.0),message("p",registry),complete("sync",false,json!({"ok":true}))]}));
    let mut suspend = request("sync-prepare", false);
    suspend["method"] = json!("registry.sync");
    let params: Value = serde_json::from_str(&encode(&super::params())).unwrap();
    traces.push(json!({"name":"registry-suspension-visible-to-independent-routing","steps":[connect("p",Some(("node",1.0))),update("node",1.0),message("p",suspend),{"op":"prepare","context":{"principalId":"caller","transport":"http"},"params":params},complete("sync-prepare",false,json!({"ok":true}))]}));
    let mut expiring = context(None);
    expiring["expiresAt"] = json!(10);
    traces.push(json!({"name":"expiry-at-peer-chain-entry","now":0,"steps":[{"op":"connect","name":"p","context":expiring},message("p",request("a",false)),message("p",request("b",false)),{"op":"complete","name":"worker:a","value":1,"now":10},{"op":"probe","key":"credential","bytes":0}]}));
    traces.push(json!({"name":"expiry-at-global-gate-entry","now":0,"steps":[connect("a",None),{"op":"connect","name":"b","context":expiring},message("a",request("a",false)),message("b",request("b",false)),{"op":"complete","name":"worker:a","value":1,"now":10},{"op":"probe","key":"credential","bytes":0}]}));
    let mut expired = context(None);
    expired["expiresAt"] = json!(10000);
    traces.push(json!({"name":"arrival-expiry-terminates-after-reply","steps":[{"op":"connect","name":"c","context":expired},message("c",request("a",false)),{"op":"probe","key":"credential","bytes":0},{"op":"close","peer":"c"}]}));
    compare("socket-requests", traces);
}

#[test]
fn original_peer_notifications_health_publication_and_close_effects() {
    let filter = json!({"namespace":"provider","name":"changed","version":"1.0.0"});
    let notification = |payload: Value| json!({"jsonrpc":"2.0","method":"service.notification","params":{"namespace":"provider","name":"changed","version":"1.0.0","payload":payload}});
    let mut traces = vec![];
    for changes in [None, Some(json!([])), Some(json!(["objects/a"]))] {
        traces.push(json!({"name":format!("subscription-{changes:?}"),"options":{"validateMessages":true},"steps":[connect("p",Some(("node",1.0))),update("node",1.0),connect("c",None),subscribe("c",json!([filter.clone()]),changes),message("p",notification(json!(1))),{"op":"changes","scopes":["objects","objects/a/child","objects/ab","services"]}]}));
    }
    traces.push(json!({"name":"node-specific-and-origin-exclusion","steps":[connect("p",Some(("node",1.0))),connect("q",Some(("other",1.0))),update("node",1.0),update("other",1.0),connect("c",None),subscribe("p",json!([filter.clone()]),None),subscribe("c",json!([{ "namespace":"provider","name":"changed","version":"1.0.0","serviceNodeId":"node"}]),None),message("q",notification(json!(1))),message("p",notification(json!(2)))]}));
    traces.push(json!({"name":"invalid-transient-throttled-diagnostic","steps":[connect("p",Some(("node",1.0))),update("node",1.0),message("p",notification(json!("wrong-schema"))),message("p",notification(json!("wrong-schema"))),{"op":"message","peer":"p","frame":notification(json!("wrong-schema")),"clock":[14999]},{"op":"message","peer":"p","frame":notification(json!("wrong-schema")),"clock":[15000,15001]}]}));
    traces.push(json!({"name":"health-and-pong","steps":[connect("c",None),subscribe("c",json!([]),Some(json!(["uis"]))),{"op":"health"},{"op":"pong","peer":"c"},{"op":"health"},{"op":"health"},{"op":"close","peer":"c"}]}));
    traces.push(json!({"name":"events-coalesce-safe-sequences-stop","steps":[connect("c",None),connect("p",Some(("node",1.0))),{"op":"events","through":0},{"op":"events","through":1.5},{"op":"events","through":3},{"op":"events","through":2},{"op":"events","through":5},{"op":"publish"},{"op":"events","through":8},{"op":"stop"},{"op":"events","through":9},{"op":"publish"}]}));
    for interval in [json!(0), json!(1), json!(5), json!(2147483648u64)] {
        let mut ctx = context(None);
        ctx["expiresAt"] = interval.clone();
        traces.push(json!({"name":format!("expiry-{interval}"),"now":0,"steps":[{"op":"connect","name":"c","context":ctx},{"op":"advance","ms":1},{"op":"advance","ms":4},{"op":"close","peer":"c"}]}));
    }
    traces.push(json!({"name":"close-cancels-expiry","now":0,"steps":[{"op":"connect","name":"c","context":{"principalId":"p","credentialDigest":"credential","transport":"ws","expiresAt":3}},{"op":"close","peer":"c"},{"op":"advance","ms":3}]}));
    for action in ["failure", "shutdown"] {
        traces.push(json!({"name":format!("{action}-ordering"),"steps":[connect("p",Some(("node",1.0))),update("node",1.0),forward("a","node",1.0),{"op":action},{"op":"close","peer":"p"},{"op":"advance","ms":30000}]}));
    }
    traces.push(json!({"name":"binding-resolved-only-after-valid-completion","options":{"validateMessages":true},"steps":[connect("p",Some(("node",1.0))),update("node",1.0),{"op":"rejected","target":{"callerPrincipalId":"caller","serviceNodeId":"node","qualifiedName":"provider.call"}},forward("a","node",1.0),result("p","a",json!(7)),forward("b","node",1.0),result("p","b",json!("valid"))]}));
    traces.push(json!({"name":"binding-record-error-preserves-prior-effects","throwRecord":true,"steps":[connect("p",Some(("node",1.0))),update("node",1.0),{"op":"rejected","target":{"callerPrincipalId":"caller","serviceNodeId":"node","qualifiedName":"provider.call"}},forward("a","node",1.0),result("p","a",json!("valid"))]}));
    for params in [
        json!({"filters":"bad"}),
        json!({"filters":[],"changes":["bad"]}),
        json!({"filters":[filter.clone(),filter.clone()]}),
    ] {
        traces.push(json!({"name":format!("subscription-guards-{params}"),"options":{"validateMessages":true},"steps":[connect("c",None),message("c",json!({"jsonrpc":"2.0","id":"subscribe","method":"notifications.subscribe","params":params}))]}));
    }
    let mut broad = update("node", 1.0);
    broad["catalog"]["notifications"][0]["payloadSchema"] = json!({});
    traces.push(json!({"name":"full-notification-envelope-byte-bound","steps":[connect("p",Some(("node",1.0))),broad,connect("c",None),subscribe("c",json!([filter.clone()]),None),message("p",notification(json!({"$repeat":1_048_500})))]}));
    compare("peer-effects", traces);
}

fn native_trace(mut trace: Value) -> Value {
    trace["packages"] = false.into();
    let source = wire(&json!({"profile":{"iso":[[10000,"1970-01-01T00:00:10.000Z"]],"dates":[]}}));
    let runtime = Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    serde_json::from_str(&encode(&runtime.block_on(run_trace(&trace, &source)))).unwrap()
}

#[test]
fn native_provider_subscription_acknowledges_requested_change_scopes() {
    for changes in [
        None,
        Some(json!([])),
        Some(json!(["uis", "system/runtime"])),
    ] {
        let value = native_trace(
            json!({"name":"change-scope-acknowledgement","options":{"validateMessages":true},"steps":[
                connect("c", None), subscribe("c", json!([]), changes.clone())
            ]}),
        );
        let reply = value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap())
            .find(|event| event["kind"] == "send" && event["value"]["id"] == "subscribe")
            .expect("The browser must receive its subscription acknowledgement");
        let mut expected = json!({"subscribed": 0});
        if let Some(changes) = changes {
            expected["changes"] = changes.as_array().unwrap().len().into();
        }
        assert_eq!(reply["value"]["result"], expected);
    }
}

#[test]
fn native_provider_ids_retain_pending_work_until_the_exact_peer_reply() {
    for id in [
        None,
        Some(json!(null)),
        Some(json!(false)),
        Some(json!(7)),
        Some(json!(["7"])),
        Some(json!([["7"]])),
        Some(json!({})),
    ] {
        let mut frame = json!({"jsonrpc":"2.0","result":"malformed"});
        if let Some(id) = id {
            frame["id"] = id;
        }
        let value = native_trace(json!({"name":"typed-correlation","ids":["7"],"steps":[
            connect("p",Some(("node",1.0))), connect("other",Some(("node",1.0))),
            update("node",1.0), forward("a","node",1.0),
            {"op":"result","peer":"p","frame":frame},
            result("other","a",json!("wrong peer")), result("p","a",json!("done")),
            result("p","a",json!("duplicate")), {"op":"advance","ms":30000}
        ]}));
        let rows = value["rows"].as_array().unwrap();
        for index in [4, 5] {
            assert!(
                !rows[index]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["kind"] == "forward-ok" || event["kind"] == "forward-error")
            );
        }
        let settled = rows
            .iter()
            .flat_map(|row| row.as_array().unwrap())
            .filter(|event| event["kind"] == "forward-ok" || event["kind"] == "forward-error")
            .collect::<Vec<_>>();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0]["kind"], "forward-ok");
        assert_eq!(settled[0]["value"], "done");
    }
}

#[test]
fn native_provider_outcomes_use_the_typed_contract_and_preserve_error_fields() {
    for outcome in [
        json!("not_executed"),
        json!("unknown"),
        json!("completed"),
        json!(["unknown"]),
        json!(["completed"]),
        json!([["not_executed"]]),
        json!(null),
        json!(false),
        json!(1),
        json!("invalid"),
    ] {
        let value = native_trace(
            json!({"name":"typed-outcome","steps":[connect("p",Some(("node",1.0))), update("node",1.0), forward("a","node",1.0),
                {"op":"result","peer":"p","call":"a","frame":{"jsonrpc":"2.0","error":{"code":4,"message":"Retained é 🙂.","data":{"code":"remote","outcome":outcome,"details":null}}}},
                {"op":"advance","ms":30000}
            ]}),
        );
        let events = value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap())
            .collect::<Vec<_>>();
        let failed = events
            .iter()
            .filter(|event| event["kind"] == "forward-error")
            .collect::<Vec<_>>();
        assert_eq!(failed.len(), 1);
        if outcome.is_array()
            || matches!(
                outcome.as_str(),
                Some("not_executed" | "unknown" | "completed")
            )
        {
            assert_eq!(failed[0]["error"]["message"], "Retained é 🙂.");
            assert_eq!(failed[0]["error"]["data"]["code"], "remote");
            assert_eq!(failed[0]["error"]["data"]["outcome"], outcome);
            let input = events
                .iter()
                .find(|event| event["kind"] == "provider-error-input")
                .unwrap();
            assert!(input.as_object().unwrap().contains_key("details"));
            assert_eq!(input["details"], Value::Null);
        } else {
            assert_eq!(
                failed[0]["error"]["data"]["code"],
                "provider_contract_error"
            );
            assert_eq!(failed[0]["error"]["data"]["outcome"], "unknown");
            assert!(
                !events
                    .iter()
                    .any(|event| event["kind"] == "provider-error-input")
            );
        }
    }
}

#[test]
fn original_replacement_fences_queued_frames_and_prior_provider_responses() {
    let registration = |label: &str| {
        message(
            "old",
            json!({"jsonrpc":"2.0","id":label,"method":"service.connect","params":{"label":label}}),
        )
    };
    let node = |generation: u32| json!({"serviceNodeId":"node","generation":generation});
    let boot = vec![
        connect("old", None),
        registration("boot"),
        update("node", 1.0),
        complete("boot", false, node(1)),
    ];
    let mut queued = boot.clone();
    queued.extend([
        message("old", request("active", false)), message("old", request("queued", false)),
        connect("replacement", None),
        message("replacement", json!({"jsonrpc":"2.0","id":"replace","method":"service.connect","params":{"label":"replace"}})),
        complete("active", false, json!(1)), update("node", 2.0),
        complete("replace", false, node(2)),
        json!({"op":"probe","key":"other","bytes":134217728}),
    ]);
    let mut pending = boot;
    pending.extend([
        forward("prior", "node", 1.0),
        registration("rebind"),
        update("node", 2.0),
        complete("rebind", false, node(2)),
        result("old", "prior", json!("stale")),
        json!({"op":"advance","ms":30000}),
    ]);
    compare(
        "generation-races",
        vec![
            json!({"name":"replacement-overtakes-peer-chain-before-global-dispatch","steps":queued}),
            json!({"name":"same-peer-rebind-rejects-old-correlation-until-deadline","steps":pending}),
        ],
    );
}

#[test]
fn original_unhandled_runtime_signals_and_reservation_effects() {
    compare(
        "unhandled-runtime",
        vec![
            json!({"name":"direct-send-rejection-releases-before-unhandled-promise","steps":[connect("c",None),message("c",request("a",true)),{"op":"socket","peer":"c","values":{"throwSend":1}},complete("a",true,json!(1)),{"op":"probe","key":"other","bytes":134217728}]}),
            json!({"name":"three-worker-response-send-failures-retain-original-fatal-path","steps":[connect("c",None),message("c",request("a",false)),{"op":"socket","peer":"c","values":{"throwSend":3}},complete("a",false,json!(1)),{"op":"probe","key":"other","bytes":134217728}]}),
            json!({"name":"expiry-termination-failure-is-an-uncaught-callback","now":0,"steps":[{"op":"connect","name":"c","context":{"principalId":"p","credentialDigest":"credential","transport":"ws","expiresAt":1},"socket":{"throwTerminate":1}},{"op":"advance","ms":1},{"op":"close","peer":"c"}]}),
        ],
    );
}
