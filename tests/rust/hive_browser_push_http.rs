use super::*;
use ivy_hive::{
    admission::{RequestBudget, RequestBudgetOptions},
    browser_push_http::{Handler, Headers, Wake},
    http_body::{BodyBuffers, BodyStream},
    http_response::Response,
};

type Trace = Arc<Mutex<Vec<Value>>>;
type Gates = Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>;
fn failure(code: &str) -> IvyError {
    if code == "internal_error" {
        IvyError::internal()
    } else {
        IvyError::new(code, "Isolated provider failure.")
    }
}
struct Stream {
    spec: Value,
    index: usize,
    trace: Trace,
    buffers: BodyBuffers,
    gates: Gates,
}
impl BodyStream for Stream {
    async fn next_chunk(&mut self) -> IvyResult<Option<Vec<u8>>> {
        let at = self.index;
        self.index += 1;
        self.trace.lock().unwrap().push(json!({"kind":"read","id":self.spec["id"],"index":at,"buffered":self.buffers.buffered_bytes()}));
        let Some(item) = self.spec["chunks"].get(at) else {
            return Ok(None);
        };
        if let Some(gate) = item["gate"].as_str() {
            let (send, receive) = oneshot::channel();
            assert!(
                self.gates
                    .lock()
                    .unwrap()
                    .insert(gate.into(), send)
                    .is_none()
            );
            receive.await.unwrap();
        }
        if let Some(code) = item["error"].as_str() {
            return Err(failure(code));
        }
        let bytes = if let Some(encoded) = item["base64"].as_str() {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap()
        } else if let Some(fill) = item["fill"].as_u64() {
            vec![u8::try_from(fill).unwrap(); item["length"].as_u64().unwrap() as usize]
        } else {
            item["text"].as_str().unwrap_or("").as_bytes().to_vec()
        };
        Ok(Some(bytes))
    }
    async fn cancel(&mut self) -> IvyResult<()> {
        self.trace.lock().unwrap().push(
            json!({"kind":"cancel","id":self.spec["id"],"buffered":self.buffers.buffered_bytes()}),
        );
        if let Some(code) = self.spec["cancelError"].as_str() {
            Err(failure(code))
        } else {
            Ok(())
        }
    }
}
struct Reply {
    spec: Value,
    trace: Trace,
    ended: bool,
    destroyed: bool,
    headers_sent: bool,
}
impl Response for Reply {
    fn writable_ended(&self) -> bool {
        self.ended
    }
    fn destroyed(&self) -> bool {
        self.destroyed
    }
    fn headers_sent(&self) -> bool {
        self.headers_sent
    }
    fn write_head(&mut self, status: u16, headers: IvyJson) -> IvyResult<()> {
        self.trace.lock().unwrap().push(
            json!({"kind":"head","id":self.spec["id"],"status":status,"headers":carrier(&headers)}),
        );
        self.headers_sent = true;
        if self.spec["throwAt"] == "head" {
            Err(IvyError::internal())
        } else {
            Ok(())
        }
    }
    fn end(&mut self, text: String) -> IvyResult<()> {
        self.trace
            .lock()
            .unwrap()
            .push(json!({"kind":"end","id":self.spec["id"],"text":text}));
        if self.spec["throwAt"] == "end" {
            return Err(IvyError::internal());
        }
        self.ended = true;
        Ok(())
    }
    fn destroy(&mut self) -> IvyResult<()> {
        self.trace
            .lock()
            .unwrap()
            .push(json!({"kind":"destroy","id":self.spec["id"]}));
        self.destroyed = true;
        if self.spec["throwAt"] == "destroy" {
            Err(IvyError::internal())
        } else {
            Ok(())
        }
    }
}
struct Bridge {
    trace: Trace,
    input: Value,
    actor: Option<mpsc::UnboundedSender<Posted>>,
}
impl Worker for Bridge {
    fn request(&self, payload: IvyJson) -> RequestFuture {
        self.trace
            .lock()
            .unwrap()
            .push(json!({"kind":"request","encoded":stringify(&payload,usize::MAX).unwrap()}));
        if let Some(actor) = &self.actor {
            let (send, receive) = oneshot::channel();
            actor.send((payload, send)).unwrap();
            return Box::pin(async move { receive.await.unwrap() });
        }
        let result = if let Some(code) = self.input["workerError"].as_str() {
            Err(failure(code))
        } else {
            Ok(
                IvyJson::from_serde(self.input.get("reply").unwrap_or(&json!({"ok":true})))
                    .unwrap(),
            )
        };
        Box::pin(async move { result })
    }
}
struct Deliver(Trace);
impl Sender for Deliver {
    fn send(&self, subscription: IvyJson, payload: String, options: IvyJson) -> SendFuture {
        self.0.lock().unwrap().push(json!({"kind":"send","subscription":carrier(&subscription),"payload":payload,"options":carrier(&options)}));
        Box::pin(async { SendOutcome::Response(Some(IvyJson::Number(201.0))) })
    }
}
struct WakePush {
    trace: Trace,
    coordinator: Option<BrowserPush>,
}
impl Wake for WakePush {
    fn wake(&self) {
        self.trace.lock().unwrap().push(json!({"kind":"wake"}));
        if let Some(coordinator) = &self.coordinator {
            coordinator.wake();
        }
    }
}
struct Owner {
    trace: Trace,
    buffers: BodyBuffers,
    gates: Gates,
    budget: RequestBudget,
    bridge: Arc<Bridge>,
    push: WakePush,
}
impl Owner {
    async fn run(&self, input: Value) {
        let mut stream = Stream {
            spec: input.clone(),
            index: 0,
            trace: self.trace.clone(),
            buffers: self.buffers.clone(),
            gates: self.gates.clone(),
        };
        let mut lease = self
            .budget
            .acquire(
                &"http:peer".into(),
                Some(input["reserve"].as_f64().unwrap_or(0.0)),
            )
            .unwrap();
        let origin = input["headers"]
            .get("origin")
            .map(|v| IvyJson::from_serde(v).unwrap());
        let length = input["headers"]
            .get("content-length")
            .map(|v| IvyJson::from_serde(v).unwrap());
        let result = if input["mode"] == "body" {
            self.buffers
                .read(
                    &mut stream,
                    length.as_ref(),
                    input["limit"].as_u64().unwrap() as usize,
                    Some(&mut lease),
                )
                .await
                .map(|bytes| {
                    self.trace
                        .lock()
                        .unwrap()
                        .push(json!({"kind":"body","id":input["id"],"length":bytes.len()}));
                })
        } else {
            let mut reply = Reply {
                spec: input.clone(),
                trace: self.trace.clone(),
                ended: input["ended"] == true,
                destroyed: input["destroyed"] == true,
                headers_sent: input["headersSent"] == true,
            };
            let context = input
                .get("context")
                .unwrap_or_else(|| self.bridge.input.get("context").unwrap());
            Handler {
                canonical_origin: &"https://example.test".into(),
                buffers: &self.buffers,
                worker: self.bridge.as_ref(),
                push: &self.push,
            }
            .handle(
                &IvyJson::from_serde(context).unwrap(),
                Headers {
                    origin: origin.as_ref(),
                    content_length: length.as_ref(),
                },
                &mut stream,
                Some(&mut lease),
                &mut reply,
            )
            .await
        };
        self.trace.lock().unwrap().push(match result {
            Ok(()) => json!({"kind":"result","id":input["id"],"value":null}),
            Err(error) => {
                json!({"kind":"result","id":input["id"],"error":carrier(&error.to_wire())})
            }
        });
        for amount in input["probes"].as_array().into_iter().flatten() {
            let entry = match lease.grow(amount.as_f64().unwrap()) {
                Ok(()) => json!({"kind":"probe","id":input["id"],"amount":amount,"value":null}),
                Err(error) => {
                    json!({"kind":"probe","id":input["id"],"amount":amount,"error":carrier(&error.to_wire())})
                }
            };
            self.trace.lock().unwrap().push(entry);
        }
        lease.release();
        self.trace.lock().unwrap().push(
            json!({"kind":"released","id":input["id"],"buffered":self.buffers.buffered_bytes()}),
        );
    }
}
async fn native_http(input: &Value) -> Value {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let bridge = Arc::new(Bridge {
        trace: trace.clone(),
        input: input.clone(),
        actor: actor(input),
    });
    let coordinator = bridge.actor.as_ref().map(|_| {
        BrowserPush::new(
            bridge.clone(),
            "https://example.test/ivy/".into(),
            Arc::new(Deliver(trace.clone())),
        )
    });
    let number = |key: &str, fallback| input["budget"][key].as_f64().unwrap_or(fallback);
    let owner = Rc::new(Owner {
        trace: trace.clone(),
        buffers: BodyBuffers::default(),
        gates: Arc::default(),
        budget: RequestBudget::new(RequestBudgetOptions {
            maximum: number("maximum", 2048.0),
            maximum_bytes: number("maximumBytes", 128.0 * 1024.0 * 1024.0),
            per_consumer: number("perConsumer", 64.0),
            per_consumer_bytes: number("perConsumerBytes", 64.0 * 1024.0 * 1024.0),
            name: "request".into(),
        }),
        bridge,
        push: WakePush {
            trace: trace.clone(),
            coordinator,
        },
    });
    let mut running = HashMap::new();
    for step in input["steps"].as_array().unwrap() {
        match step["op"].as_str().unwrap() {
            "run" => { let held=owner.clone();let spec=step.clone();assert!(running.insert(step["id"].as_str().unwrap().to_owned(),tokio::task::spawn_local(async move {held.run(spec).await})).is_none()); }
            "release" => owner.gates.lock().unwrap().remove(step["gate"].as_str().unwrap()).unwrap().send(()).unwrap(),
            "join" => running.remove(step["id"].as_str().unwrap()).unwrap().await.unwrap(),
            "mark" => trace.lock().unwrap().push(json!({"kind":"mark","name":step["name"],"buffered":owner.buffers.buffered_bytes()})),
            _ => panic!("Unknown fixture command"),
        }
        flush().await;
    }
    for (_, task) in running {
        task.await.unwrap();
    }
    if let Some(coordinator) = &owner.push.coordinator {
        coordinator.close().await;
    }
    flush().await;
    if owner.bridge.actor.is_some() {
        let (send, receive) = oneshot::channel();
        owner
            .bridge
            .actor
            .as_ref()
            .unwrap()
            .send((
                IvyJson::object([("action", "fixture.snapshot".into())]),
                send,
            ))
            .unwrap();
        let state = receive.await.unwrap().unwrap();
        trace.lock().unwrap().push(carrier(&state));
    }
    let mut final_lease = owner
        .budget
        .acquire(
            &"http:peer".into(),
            Some(number("perConsumerBytes", 64.0 * 1024.0 * 1024.0)),
        )
        .unwrap();
    final_lease.release();
    trace
        .lock()
        .unwrap()
        .push(json!({"kind":"final-budget","buffered":owner.buffers.buffered_bytes()}));
    let result = json!({"trace":*trace.lock().unwrap()});
    result
}
async fn compare_http(name: &str, input: Value) -> Value {
    let expected = original_output("tests/fixtures/hive-browser-push-http-oracle.mjs", &input);
    let observed = tokio::task::LocalSet::new()
        .run_until(native_http(&input))
        .await;
    // The carrier records integer-valued JS numbers as JSON integers. Preserve
    // exact encoded request/response text while normalizing Rust's 0.0 spelling.
    let observed = carrier(&IvyJson::from_serde(&observed).unwrap());
    if let Ok(path) = std::env::var("IVY_HIVE_PUSH_HTTP_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({"name":name,"input":input,"original":expected,"native":observed})
        )
        .unwrap();
    }
    assert_eq!(observed, expected, "Browser push HTTP: {name}");
    observed
}
fn request(id: &str, body: &str) -> Value {
    json!({"op":"run","id":id,"headers":{"origin":"https://example.test"},"chunks":[{"text":body}]})
}
fn script(steps: Vec<Value>) -> Value {
    json!({"context":{"principalId":"alpha","credentialDigest":"captured","sessionDigest":"session","transport":"http"},"steps":steps})
}

#[tokio::test(flavor = "current_thread")]
async fn push_http_origin_body_and_action_admission_match_original() {
    let mut steps = vec![];
    for (i, origin) in [
        None,
        Some(json!(null)),
        Some(json!(["https://example.test"])),
        Some(json!("HTTPS://example.test")),
        Some(json!("https://example.test/")),
        Some(json!("https://example.test:443")),
        Some(json!("https://other.test")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut step = request(&format!("origin-{i}"), "{\"action\":\"test\"}");
        if let Some(origin) = origin {
            step["headers"]["origin"] = origin;
        } else {
            step["headers"].as_object_mut().unwrap().remove("origin");
        }
        steps.push(step);
    }
    for (i, body) in [
        "null",
        "[]",
        "true",
        "42",
        "\"test\"",
        "{}",
        "{\"action\":null}",
        "{\"action\":\"TEST\"}",
        "{\"action\":[\"test\"]}",
        "{",
        "\u{feff}{\"action\":\"status\"}",
        "{\"action\":\"status\",\"action\":\"test\"}",
    ]
    .iter()
    .enumerate()
    {
        steps.push(request(&format!("body-{i}"), body));
    }
    for action in ["status", "subscribe", "unsubscribe", "test"] {
        steps.push(request(
            action,
            &json!({"action":action,"extra":{"sessionDigest":"untrusted"}}).to_string(),
        ));
    }
    let mut invalid = request("lossy-utf8", "");
    invalid["chunks"] =
        json!([{"base64":"eyJhY3Rpb24iOiJzdGF0dXMiLCJub3RlIjoi/w=="},{"text":"\"}"}]);
    steps.push(invalid);
    steps.push(request(
        "lone-surrogate",
        "{\"action\":\"status\",\"extra\":\"\\ud800\"}",
    ));
    let result = compare_http("origin-action-json-and-decoding", script(steps)).await;
    assert_eq!(
        result["trace"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["kind"] == "wake")
            .count(),
        2
    );
}
#[tokio::test(flavor = "current_thread")]
async fn push_http_length_stream_and_retained_admission_failures_match_original() {
    let mut steps = vec![];
    for (i, length) in [
        json!("8193"),
        json!("-1"),
        json!("+1"),
        json!("1.0"),
        json!(" 1"),
        json!("1\n"),
        json!("1e1"),
        json!(""),
        json!(null),
        json!([]),
        json!(["8193"]),
        json!("9".repeat(400)),
        json!("0000"),
        json!("1"),
        json!(["17"]),
    ]
    .into_iter()
    .enumerate()
    {
        let mut step = request(&format!("declared-{i}"), "{\"action\":\"test\"}");
        step["headers"]["content-length"] = length;
        steps.push(step);
    }
    let body = format!("{{\"action\":\"status\"}}{}", " ".repeat(8192 - 19));
    assert_eq!(body.len(), 8192);
    steps.push(request("exact-limit", &body));
    let mut excess = request("excess", "{\"action\":\"test\"}");
    excess["chunks"] = json!([{"fill":32,"length":8192},{"text":"x"}]);
    excess["cancelError"] = "forbidden".into();
    steps.push(excess);
    let mut failed = request("stream-error", "");
    failed["chunks"] = json!([{"text":"{"},{"error":"internal_error"}]);
    steps.push(failed);
    compare_http("declared-and-observed-body-limits", script(steps)).await;
    let mut charged = request("retained-charge", "");
    charged["chunks"] = json!([{"fill":32,"length":10},{"fill":32,"length":11}]);
    charged["probes"] = json!([10, 1]);
    let mut value = script(vec![charged]);
    value["budget"] = json!({"maximumBytes":20,"perConsumerBytes":20});
    compare_http("grow-before-buffering-and-explicit-lease-release", value).await;
}
#[tokio::test(flavor = "current_thread")]
async fn push_http_response_transitions_and_worker_failures_match_original() {
    let mut steps = vec![request("ordinary-test", "{\"action\":\"test\"}")];
    for key in ["ended", "destroyed", "headersSent"] {
        let mut step = request(key, "{\"action\":\"test\"}");
        step[key] = true.into();
        steps.push(step);
    }
    for at in ["head", "end", "destroy"] {
        let mut step = request(at, "{\"action\":\"test\"}");
        step["throwAt"] = at.into();
        if at == "destroy" {
            step["headersSent"] = true.into();
        }
        steps.push(step);
    }
    compare_http(
        "response-before-wake-and-suppressed-response",
        script(steps),
    )
    .await;
    let mut value = script(vec![request("worker-fails", "{\"action\":\"test\"}")]);
    value["workerError"] = "forbidden".into();
    compare_http("worker-failure-does-not-wake", value).await;
}
#[tokio::test(flavor = "current_thread")]
async fn shared_http_buffers_recover_after_concurrent_global_limit() {
    let maximum = 64 * 1024 * 1024;
    let first = json!({"op":"run","mode":"body","id":"held","limit":maximum,"chunks":[{"fill":32,"length":maximum},{"text":"","gate":"finish"}]});
    let mut value = script(vec![
        first,
        json!({"op":"mark","name":"first body retained"}),
        request("contender", "{\"action\":\"test\"}"),
        json!({"op":"mark","name":"contender refunded"}),
        json!({"op":"release","gate":"finish"}),
        json!({"op":"join","id":"held"}),
        request("recovered", "{\"action\":\"status\"}"),
    ]);
    value["budget"] = json!({"maximumBytes":3*maximum,"perConsumerBytes":2*maximum});
    compare_http("concurrent-64-mib-buffer-accounting-and-recovery", value).await;
}
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn push_http_integrates_actual_session_policy_store_queue_and_coordinator_wake() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let endpoint = "https://fcm.googleapis.com/fcm/send/one";
    let subscription =
        json!({"endpoint":endpoint,"keys":{"auth":"existing-auth","p256dh":"existing-key"}});
    let mut value = script(vec![
        request(
            "status",
            &json!({"action":"status","endpoint":endpoint}).to_string(),
        ),
        request(
            "test",
            &json!({"action":"test","endpoint":endpoint}).to_string(),
        ),
        request(
            "unsubscribe",
            &json!({"action":"unsubscribe","endpoint":endpoint}).to_string(),
        ),
    ]);
    let mut no_session = request("no-session", "{\"action\":\"test\"}");
    no_session["context"] =
        json!({"credentialDigest":digest,"principalId":"alpha","transport":"http"});
    value["steps"].as_array_mut().unwrap().push(no_session);
    value["realStore"] = true.into();
    value["context"]["credentialDigest"] = digest.clone().into();
    value["keys"] = json!({"publicKey":"existing-public","privateKey":"existing-private"});
    value["seedSql"]=format!("INSERT OR IGNORE INTO principals VALUES('alpha');INSERT INTO credentials VALUES('{digest}','alpha',0);INSERT INTO browser_sessions VALUES('session','{digest}','2026-01-01');INSERT INTO browser_push VALUES('{endpoint}','session','{}',NULL,0);",subscription.to_string().replace('\'',"''")).into();
    let result = compare_http("real-store-session-queue-delivery-and-unsubscribe", value).await;
    let trace = result["trace"].as_array().unwrap();
    assert_eq!(trace.iter().filter(|v| v["kind"] == "send").count(), 1);
    let state = trace.iter().find(|v| v["kind"] == "store").unwrap();
    assert_eq!(state["subscriptions"], json!([]));
    assert_eq!(state["outbox"], json!([]));
}
