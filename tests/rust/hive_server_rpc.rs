#[path = "../fixtures/hive-dispatch-queue.rs"]
mod fixture_dispatch;
use fixture_dispatch::DispatchGate;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::digest,
    json::{parse, stringify},
};
use ivy_hive::{
    admission::{RequestBudget, RequestBudgetOptions},
    server_rpc::{Invocation, Options, Reply, Storage},
    storage::RequestOptions,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{runtime::Builder, sync::oneshot};
#[path = "../fixtures/reference-data.rs"]
mod reference_data;

fn wire(value: &Value) -> IvyJson {
    parse(&value.to_string(), usize::MAX).unwrap()
}
fn encode(value: &IvyJson) -> String {
    stringify(value, usize::MAX).unwrap()
}
fn number(value: Option<&Value>, default: f64) -> f64 {
    let Some(value) = value else {
        return default;
    };
    match value.get("$number").and_then(Value::as_str) {
        Some("nan") => f64::NAN,
        Some("infinity") => f64::INFINITY,
        Some("negativeInfinity") => f64::NEG_INFINITY,
        Some("negativeZero") => -0.0,
        Some(other) => panic!("Unknown fixture number: {other}"),
        None => value.as_f64().unwrap_or(default),
    }
}
fn failure(value: &Value) -> IvyError {
    let message = value
        .get("messageUnits")
        .and_then(Value::as_array)
        .map(|units| {
            JsString(
                units
                    .iter()
                    .map(|value| value.as_u64().unwrap() as u16)
                    .collect(),
            )
        })
        .unwrap_or_else(|| value["message"].as_str().unwrap_or("Work failed.").into());
    let outcome = match value["outcome"].as_str().unwrap_or("unknown") {
        "not_executed" => Outcome::NotExecuted,
        "completed" => Outcome::Completed,
        "unknown" => Outcome::Unknown,
        other => panic!("Unknown outcome: {other}"),
    };
    let error = IvyError::new_js(
        value["code"].as_str().unwrap_or("remote_error").into(),
        message,
    )
    .with_outcome(outcome);
    value
        .get("details")
        .map_or(error.clone(), |details| error.with_details(wire(details)))
}
fn result(value: &Value) -> IvyJson {
    value.get("$repeat").map_or_else(
        || wire(value),
        |count| "x".repeat(count.as_u64().unwrap() as usize).into(),
    )
}
fn summary(value: IvyJson) -> IvyJson {
    if let Some(text) = value.as_js_string().filter(|text| text.0.len() > 4096) {
        let text = text.to_utf8().unwrap();
        IvyJson::object([
            ("stringBytes", IvyJson::Number(text.len() as f64)),
            ("sha256", digest(text.as_bytes()).into()),
        ])
    } else {
        value
    }
}
fn batch_summary(body: &str) -> IvyJson {
    let items = parse(body, usize::MAX)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let mut item = item.clone();
            if let Some(value) = item.get("result").cloned() {
                item.insert("result", summary(value));
            }
            item
        })
        .collect();
    IvyJson::object([
        ("byteLength", IvyJson::Number(body.len() as f64)),
        ("sha256", digest(body.as_bytes()).into()),
        ("items", IvyJson::Array(items)),
    ])
}
type Events = Arc<Mutex<Vec<IvyJson>>>;
type Controls = Arc<Mutex<HashMap<String, oneshot::Sender<IvyResult<IvyJson>>>>>;
fn post(controls: &Controls, events: &Events, name: String, event: IvyJson) -> Reply {
    events.lock().unwrap().push(event);
    let (result, reply) = oneshot::channel();
    assert!(
        controls.lock().unwrap().insert(name, result).is_none(),
        "Reused fixture control"
    );
    Box::pin(async move {
        reply
            .await
            .expect("Fixture must retain every accepted input")
    })
}
#[derive(Clone)]
struct Worker {
    events: Events,
    controls: Controls,
    dispatch: DispatchGate,
}
impl Storage for Worker {
    fn dispatch(&self, mut request: ivy_hive::storage::Request, options: RequestOptions) -> Reply {
        let this = self.clone();
        let reply = self.dispatch.run(move || async move {
            request.prepare()?;
            let result = this.request(crate::payload_fixture::json(request.take_payload()), options).await;
            request.complete(result)
        });
        Box::pin(async move { reply.await.map_err(|_| IvyError::internal())? })
    }
    fn request(&self, payload: IvyJson, options: RequestOptions) -> Reply {
        let label = payload
            .required("request")
            .unwrap()
            .required("params")
            .unwrap()
            .required("label")
            .unwrap()
            .as_str()
            .unwrap();
        post(
            &self.controls,
            &self.events,
            format!("storage:{label}"),
            IvyJson::object([
                ("kind", "storage".into()),
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
                        ("backup", false.into()),
                    ]),
                ),
            ]),
        )
    }
}
fn observe(name: String, result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([
            ("kind", "ok".into()),
            ("name", name.into()),
            ("value", summary(value)),
        ]),
        Err(error) => IvyJson::object([
            ("kind", "error".into()),
            ("name", name.into()),
            ("error", error.to_wire()),
        ]),
    }
}
fn observable_rows(value: &IvyJson) -> IvyJson {
    let mut observed = value.clone();
    let rows = value
        .get("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let (mut owner, mut replies) = (Vec::new(), Vec::new());
            for event in row.as_array().unwrap() {
                let kind = event.get("kind").unwrap().as_str().unwrap();
                if matches!(kind.as_str(), "ok" | "error" | "batch") {
                    replies.push(event.clone());
                } else {
                    owner.push(event.clone());
                }
            }
            IvyJson::object([
                ("owner", IvyJson::Array(owner)),
                ("replies", IvyJson::Array(replies)),
            ])
        })
        .collect();
    observed.insert("rows", IvyJson::Array(rows));
    observed
}
fn compare(group: &str, traces: Vec<Value>) {
    static CASES: std::sync::OnceLock<IvyJson> = std::sync::OnceLock::new();
    let cases = CASES.get_or_init(|| {
        let bytes = reference_data::read(include_bytes!(
            "../fixtures/hive-server-rpc-reference.json.gz"
        ));
        parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX).unwrap()
    });
    let key = digest(format!(
        "server-rpc\n{}",
        serde_json::to_string(&traces).unwrap()
    ));
    let expected = cases
        .get("traces")
        .unwrap()
        .get(&key)
        .unwrap_or_else(|| panic!("Missing original Hive server RPC group {group}: {key}"));
    assert_eq!(traces.len(), expected.as_array().unwrap().len());
    for (trace, expected) in traces.iter().zip(expected.as_array().unwrap()) {
        let runtime = Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        let actual = runtime.block_on(async {
            let events = Events::default();
            let controls = Controls::default();
            let clock = Arc::new(Mutex::new(VecDeque::<f64>::new()));
            let wall = Arc::new(Mutex::new(number(trace.get("now"), 0.0)));
            let now = {
                let (clock, wall, events) = (clock.clone(), wall.clone(), events.clone());
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
            let o = &trace["requests"];
            let requests = RequestBudget::new(RequestBudgetOptions {
                maximum: (number(o.get("maximum"), 2048.0)) as usize,
                maximum_bytes: (number(o.get("maximumBytes"), 134_217_728.0)) as usize,
                per_consumer: (number(o.get("perConsumer"), 256.0)) as usize,
                per_consumer_bytes: (number(o.get("perConsumerBytes"), 67_108_864.0)) as usize,
                name: o["name"].as_str().unwrap_or("request").into(),
            });
            let direct_events = events.clone();
            let direct_controls = controls.clone();
            let tool_routing = trace["toolRouting"] == true;
            let direct = Arc::new(
                move |context: &IvyJson, request: &mut IvyJson| -> IvyResult<Option<Reply>> {
                    if tool_routing {
                        if request.get("method") == Some(&"tools.call".into())
                            || request.get("method") == Some(&"discovery.call".into())
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
                        return Ok(None);
                    }
                    direct_events.lock().unwrap().push(IvyJson::object([
                        ("kind", "direct".into()),
                        ("context", context.clone()),
                        ("request", request.clone()),
                    ]));
                    let params = request.get("params");
                    match params
                        .and_then(|v| v.get("fixtureDirect"))
                        .and_then(|v| v.as_str().ok())
                        .as_deref()
                    {
                        Some("throw") => Err(failure(
                            &serde_json::from_str::<Value>(&encode(
                                params.unwrap().get("error").unwrap_or(&IvyJson::Null),
                            ))
                            .unwrap(),
                        )),
                        Some("return") => {
                            let label = params.unwrap().required("label")?.as_str()?;
                            Ok(Some(post(
                                &direct_controls,
                                &direct_events,
                                format!("direct:{label}"),
                                IvyJson::object([
                                    ("kind", "direct-start".into()),
                                    ("label", label.into()),
                                ]),
                            )))
                        }
                        _ => Ok(None),
                    }
                },
            );
            let owner = Invocation::new(
                Arc::new(Worker {
                    events: events.clone(),
                    controls: controls.clone(),
                    dispatch: DispatchGate::new(
                        trace["dispatch"]["maximum"].as_u64().unwrap_or(1024) as usize,
                        trace["dispatch"]["waitMs"].as_u64().unwrap_or(30000),
                    ),
                }),
                direct,
                now,
                requests.clone(),
                Options {
                    reset_bootstrap_credential_digest:
                        trace["options"]["resetBootstrapCredentialDigest"]
                            .as_str()
                            .map(Into::into),
                },
            );
            let mut rows = vec![];
            let mut watchers = HashMap::new();
            for step in trace["steps"].as_array().unwrap() {
                if let Some(values) = step.get("clock").and_then(Value::as_array) {
                    *clock.lock().unwrap() = values.iter().map(|v| number(Some(v), 0.0)).collect();
                }
                if step.get("now").is_some() {
                    *wall.lock().unwrap() = number(step.get("now"), 0.0);
                }
                match step["op"].as_str().unwrap() {
                    "invoke" => {
                        let work = owner.invoke(
                            wire(&step["context"]),
                            wire(&step["request"]),
                            (number(step.get("bytes"), 0.0)) as usize,
                        );
                        let (events, name) =
                            (events.clone(), step["name"].as_str().unwrap().to_owned());
                        watchers.insert(
                            name.clone(),
                            tokio::spawn(async move {
                                let result = work.await;
                                events.lock().unwrap().push(observe(name, result));
                            }),
                        );
                    }
                    "batch" => {
                        let work = owner.batch(
                            wire(&step["context"]),
                            step["frames"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(wire)
                                .collect(),
                            (number(step.get("bytes"), 0.0)) as usize,
                        );
                        let (events, name) =
                            (events.clone(), step["name"].as_str().unwrap().to_owned());
                        watchers.insert(
                            name.clone(),
                            tokio::spawn(async move {
                                let value = match work.await {
                                    Ok(body) => IvyJson::object([
                                        ("kind", "batch".into()),
                                        ("name", name.into()),
                                        ("value", batch_summary(&body)),
                                    ]),
                                    Err(error) => observe(name, Err(error)),
                                };
                                events.lock().unwrap().push(value);
                            }),
                        );
                    }
                    "complete" => {
                        let name = step["name"].as_str().unwrap();
                        let control = controls.lock().unwrap().remove(name).unwrap_or_else(|| {
                            panic!("Unknown control {name} in {}", trace["name"])
                        });
                        let value = match step.get("error") {
                            Some(error) => Err(failure(error)),
                            None => {
                                let value = result(&step["value"]);
                                Ok(if name.starts_with("storage:") {
                                    IvyJson::object([("kind", "result".into()), ("value", value)])
                                } else {
                                    value
                                })
                            }
                        };
                        let _ = control.send(value);
                    }
                    "drop" => watchers[step["name"].as_str().unwrap()].abort(),
                    "probe" => {
                        let probe = requests.acquire(
                            &step["key"].as_str().unwrap().into(),
                            (step.get("bytes").map(|v| number(Some(v), 1024.0)))
                                .map(|bytes| bytes as usize),
                        );
                        events.lock().unwrap().push(match probe {
                            Ok(mut lease) => {
                                lease.release();
                                IvyJson::object([("kind", "probe".into()), ("ok", true.into())])
                            }
                            Err(error) => IvyJson::object([
                                ("kind", "probe".into()),
                                ("error", error.to_wire()),
                            ]),
                        });
                    }
                    "advance" => {
                        tokio::time::advance(Duration::from_millis(step["ms"].as_u64().unwrap()))
                            .await
                    }
                    other => panic!("Unknown fixture operation: {other}"),
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
        });
        let actual = parse(&encode(&actual), usize::MAX).unwrap();
        // Preserve every event, row boundary and order within the owner and
        // caller replies. Independent executor interleaving is not an RPC order.
        assert_eq!(
            observable_rows(&actual),
            observable_rows(expected),
            "{group}: {}",
            trace["name"]
        );
    }
}
fn context() -> Value {
    json!({"principalId":"caller","credentialDigest":"credential","transport":"http"})
}
fn request(label: &str, direct: bool) -> Value {
    json!({"jsonrpc":"2.0","id":label,"method":"fixture.work","params":{"label":label,"fixtureDirect":if direct {"return"} else {"none"}}})
}
fn invoke(label: &str, direct: bool) -> Value {
    json!({"op":"invoke","name":label,"context":context(),"request":request(label,direct)})
}
fn complete(label: &str, direct: bool, value: Value) -> Value {
    json!({"op":"complete","name":format!("{}:{label}",if direct {"direct"} else {"storage"}),"value":value})
}
fn probe(key: &str, bytes: usize) -> Value {
    json!({"op":"probe","key":key,"bytes":bytes})
}

#[test]
fn tool_routing_rpc_validation_before_routing_and_admission_release() {
    let mut traces = vec![];
    for (name, request) in super::tool_routing_cases::requests() {
        traces.push(json!({"name":name,"toolRouting":true,"requests":{"perConsumer":1},"steps":[{"op":"invoke","name":"call","context":context(),"request":request},probe("credential",0)]}));
    }
    let malformed = json!({"jsonrpc":"2.0","id":"call","method":"tools.call","params":{}});
    for (name, options, requests, caller, clock) in [
        (
            "expired-before-admission",
            json!({}),
            json!({}),
            json!({"principalId":"caller","credentialDigest":"credential","transport":"http","expiresAt":0}),
            json!([]),
        ),
        (
            "expired-at-direct",
            json!({}),
            json!({"perConsumer":1}),
            json!({"principalId":"caller","credentialDigest":"credential","transport":"http","expiresAt":10}),
            json!([0, 0, 10]),
        ),
        (
            "maintenance-before-validation",
            json!({"resetBootstrapCredentialDigest":"other"}),
            json!({"perConsumer":1}),
            context(),
            json!([]),
        ),
        (
            "budget-before-validation",
            json!({}),
            json!({"maximum":0}),
            context(),
            json!([]),
        ),
    ] {
        traces.push(json!({"name":name,"toolRouting":true,"options":options,"requests":requests,"steps":[{"op":"invoke","name":"call","context":caller,"request":malformed,"clock":clock},probe("credential",0)]}));
    }
    traces.push(json!({"name":"unrelated-method-still-queues","toolRouting":true,"steps":[invoke("queued",false),complete("queued",false,json!("queued")),probe("credential",0)]}));
    compare("tool-routing-rpc", traces);
}

#[test]
fn tool_routing_batch_keeps_item_envelope_precedence_and_release() {
    let frames: Vec<_> = super::tool_routing_cases::requests()
        .into_iter()
        .enumerate()
        .map(|(index, (_, mut request))| {
            if !request["id"].is_null() {
                request["id"] = json!(index);
            }
            request
        })
        .collect();
    let traces=frames.chunks(8).enumerate().map(|(index,frames)|json!({"name":format!("mixed-routing-batch-{index}"),"toolRouting":true,"requests":{"perConsumer":1},"steps":[{"op":"batch","name":"batch","context":context(),"frames":frames},probe("credential",0)]})).collect();
    compare("tool-routing-batch", traces);
}

#[test]
fn original_server_invocation_admission() {
    let mut traces = vec![];
    for (name, options) in [
        ("ordinary", json!({})),
        ("reset-empty", json!({"resetBootstrapCredentialDigest":""})),
        (
            "reset-matching",
            json!({"resetBootstrapCredentialDigest":"credential"}),
        ),
        (
            "reset-other",
            json!({"resetBootstrapCredentialDigest":"other"}),
        ),
    ] {
        let denied = name == "reset-other";
        let mut steps = vec![invoke("a", false), probe("credential", 0)];
        if !denied {
            steps.push(complete("a", false, json!({"ok":true})));
        }
        steps.push(probe("credential", 0));
        traces.push(
            json!({"name":name,"options":options,"requests":{"perConsumer":1},"steps":steps}),
        );
    }
    for (name, context, clock) in [
        (
            "expired-first",
            json!({"principalId":"caller","credentialDigest":"credential","transport":"http","expiresAt":10}),
            vec![10],
        ),
        (
            "expired-second",
            json!({"principalId":"caller","credentialDigest":"credential","transport":"http","expiresAt":10}),
            vec![9, 10],
        ),
        (
            "expired-direct-entry",
            json!({"principalId":"caller","credentialDigest":"credential","transport":"http","expiresAt":10}),
            vec![9, 9, 10],
        ),
        (
            "expired-dispatch-entry",
            json!({"principalId":"caller","credentialDigest":"credential","transport":"http","expiresAt":10}),
            vec![9, 9, 9, 10],
        ),
    ] {
        let mut step = invoke("a", false);
        step["context"] = context;
        step["clock"] = json!(clock);
        traces.push(
            json!({"name":name,"requests":{"perConsumer":1},"steps":[step,probe("credential",0)]}),
        );
    }
    // Native ingress supplies usize byte counts. Fractional and infinite JS
    // arguments cannot reach it; the invocation/dispatch case checks overflow.
    for bytes in [
        json!(0),
        json!(-1),
        json!({"$number":"nan"}),
        json!(1),
        json!(65536),
    ] {
        let mut step = invoke("a", true);
        step["bytes"] = bytes.clone();
        let mut steps = vec![step, probe("credential", 0)];
        steps.push(complete("a", true, json!("é😀")));
        steps.push(probe("credential", 0));
        traces.push(
            json!({"name":format!("received-{bytes}"),"requests":{"perConsumer":1},"steps":steps}),
        );
    }
    let mut synchronous = invoke("throw", true);
    synchronous["request"]["params"]["fixtureDirect"] = json!("throw");
    synchronous["request"]["params"]["error"] = json!({"code":"direct_failure","messageUnits":[101,55296],"outcome":"completed","details":{"scope":"kept"}});
    traces.push(json!({"name":"synchronous-direct-error-refunds","requests":{"perConsumer":1},"steps":[synchronous,probe("credential",0)]}));
    traces.push(json!({"name":"dropped-direct-retains-request","requests":{"perConsumer":1},"steps":[invoke("a",true),{"op":"drop","name":"a"},probe("credential",0),complete("a",true,json!(1)),probe("credential",0)]}));
    traces.push(json!({"name":"dropped-storage-retains-request","requests":{"perConsumer":1},"steps":[invoke("a",false),{"op":"drop","name":"a"},probe("credential",0),complete("a",false,json!(1)),probe("credential",0)]}));
    traces.push(json!({"name":"direct-bypasses-dispatch-queue","dispatch":{"maximum":1},"steps":[invoke("a",false),invoke("b",false),invoke("c",true),complete("c",true,json!(3)),complete("a",false,json!(1))]}));
    let mut queued = invoke("b", false);
    queued["context"]["expiresAt"] = json!(10);
    traces.push(json!({"name":"queued-expiry-at-entry","steps":[invoke("a",false),queued,{"op":"complete","name":"storage:a","value":1,"now":10},probe("credential",0)]}));
    traces.push(json!({"name":"queued-gate-timeout-refunds","dispatch":{"waitMs":5},"steps":[invoke("a",false),invoke("b",false),{"op":"advance","ms":5},complete("a",false,json!(1)),probe("credential",0)]}));
    traces.push(json!({"name":"byte-reservation-exact-boundary","requests":{"maximumBytes":5120},"steps":[invoke("a",true),probe("other",1),probe("other",0),complete("a",true,json!(1)),probe("other",5120)]}));
    traces.push(json!({"name":"reset-transport-fence","options":{"resetBootstrapCredentialDigest":"credential"},"steps":[{"op":"invoke","name":"a","context":{"principalId":"caller","credentialDigest":"credential","transport":"mcp"},"request":request("a",false)},probe("credential",0)]}));
    traces.push(json!({"name":"worker-error-keeps-outcome-details","steps":[invoke("a",false),{"op":"complete","name":"storage:a","error":{"code":"storage_failure","message":"Original failure.","outcome":"unknown","details":{"retained":true}}},probe("credential",0)]}));
    compare("invocation", traces);
}

#[test]
fn original_server_batch_identity_order_and_budget() {
    let mut traces = vec![];
    for (name, frames, bytes) in [
        ("empty", json!([]), 0),
        ("too-many", json!(vec![request("a", true); 9]), 0),
        ("over-input-byte-limit", json!([request("a", true)]), 65537),
        (
            "duplicate",
            json!([request("a", true), request("a", true)]),
            0,
        ),
        (
            "invalid-envelope-identities",
            json!([null,{}, {"id":0}, {"id":""}, {"id":-1}, {"id":true}, {"id":1.5}, {"id":"valid"}]),
            0,
        ),
    ] {
        traces.push(json!({"name":name,"steps":[{"op":"batch","name":"batch","context":context(),"frames":frames,"bytes":bytes}]}));
    }
    let (mut a, mut b) = (request("a", true), request("b", true));
    a["id"] = json!(0);
    b["id"] = json!(-0.0);
    traces.push(json!({"name":"negative-zero-duplicates-zero","steps":[{"op":"batch","name":"batch","context":context(),"frames":[a,b]}]}));
    let (mut a, mut b) = (request("a", false), request("b", true));
    a["id"] = json!(0);
    b["id"] = json!("0");
    traces.push(json!({"name":"numeric-and-string-id-distinct","steps":[{"op":"batch","name":"batch","context":context(),"frames":[a,b],"bytes":65536},complete("b",true,json!(2)),complete("a",false,json!(1))]}));
    traces.push(json!({"name":"mixed-worker-direct-error","steps":[{"op":"batch","name":"batch","context":context(),"frames":[request("a",false),request("b",true),{"id":"invalid"}]},complete("b",true,json!({"done":2})),{"op":"complete","name":"storage:a","error":{"code":"work_failure","message":"Retained error.","outcome":"completed","details":{"kept":true}}}]}));
    traces.push(json!({"name":"batch-request-budget","requests":{"perConsumer":1},"steps":[{"op":"batch","name":"batch","context":context(),"frames":[request("a",true),request("b",true)]},complete("a",true,json!(1)),probe("credential",0)]}));
    traces.push(json!({"name":"batch-drop-does-not-cancel","requests":{"perConsumer":2},"steps":[{"op":"batch","name":"batch","context":context(),"frames":[request("a",true),request("b",true)]},{"op":"drop","name":"batch"},probe("credential",0),complete("b",true,json!(2)),probe("credential",0),complete("a",true,json!(1)),probe("credential",0)]}));
    for reverse in [false, true] {
        let (first, last) = if reverse { ("b", "a") } else { ("a", "b") };
        traces.push(json!({"name":format!("completion-budget-reverse-{reverse}"),"steps":[{"op":"batch","name":"batch","context":context(),"frames":[request("a",true),request("b",true)]},complete(first,true,json!({"$repeat":20_000_000})),complete(last,true,json!({"$repeat":20_000_000}))]}));
    }
    traces.push(json!({"name":"single-item-encoding-overflow-fallback","steps":[{"op":"batch","name":"batch","context":context(),"frames":[request("a",true)]},complete("a",true,json!({"$repeat":33_554_433}))]}));
    compare("batch", traces);
}
