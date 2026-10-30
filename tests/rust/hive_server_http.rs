use super::*;
use ivy_hive::{
    admission::{RequestBudget, RequestBudgetOptions},
    http_auth::{self, Headers},
    http_body::BodyStream,
    http_common, http_response, server_http,
};
use serde_json::{Value, json};
use tokio::sync::oneshot;
#[path = "../fixtures/hive-dispatch-queue.rs"]
mod fixture_dispatch;

type Log = Arc<Mutex<Vec<Value>>>;
type Controls = Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>;
async fn pause(controls: &Controls, gate: &str) {
    let (send, receive) = oneshot::channel();
    assert!(controls.lock().unwrap().insert(gate.into(), send).is_none());
    receive.await.unwrap();
}
fn wire(error: IvyError) -> Value {
    error.to_wire().to_serde().unwrap()
}
fn failure(spec: &Value) -> IvyError {
    IvyError::new(
        spec["error"].as_str().unwrap(),
        spec["message"]
            .as_str()
            .unwrap_or("Isolated storage failure."),
    )
}
#[derive(Clone)]
struct HttpWorker {
    credential: JsString,
    log: Log,
    controls: Controls,
    replies: Arc<Mutex<HashMap<String, VecDeque<Value>>>>,
    dispatch: fixture_dispatch::DispatchGate,
}
impl HttpWorker {
    fn post(&self, payload: IvyJson, options: RequestOptions) -> Reply {
        self.log.lock().unwrap().push(json!({"kind":"worker","payload":payload.to_serde().unwrap(),"options":{"timeoutMs":options.timeout_ms,"terminateOnTimeout":options.terminate_on_timeout,"reservedBytes":options.reserved_bytes,"backup":false}}));
        let action = payload.get("action").unwrap().as_str().unwrap();
        let spec = self
            .replies
            .lock()
            .unwrap()
            .get_mut(&action)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Value::Null);
        let fallback = match action.as_str() {
            "authenticate" => obj([("principalId", "a".into())]),
            "execute" => super::json(r#"{"kind":"result","value":{"stored":true}}"#),
            "mcp.catalog" => IvyJson::Array(vec![]),
            "session.resolve" => obj([
                ("credentialDigest", IvyJson::String(self.credential.clone())),
                (
                    "sessionDigest",
                    payload.get("sessionDigest").unwrap().clone(),
                ),
                ("principalId", "a".into()),
            ]),
            _ => IvyJson::Null,
        };
        let controls = self.controls.clone();
        Box::pin(async move {
            if let Some(gate) = spec["gate"].as_str() {
                pause(&controls, gate).await;
            }
            if spec["error"].is_string() {
                return Err(failure(&spec));
            }
            Ok(spec
                .get("value")
                .map(|v| IvyJson::from_serde(v).unwrap())
                .unwrap_or(fallback))
        })
    }
}
impl Worker for HttpWorker {
    fn request(&self, payload: ivy_hive::storage::Payload) -> browser_push::RequestFuture {
        let payload = crate::payload_fixture::json(payload);
        self.post(payload, RequestOptions::default())
    }
}
impl Storage for HttpWorker {
    fn dispatch(&self, mut request: ivy_hive::storage::Request, options: RequestOptions) -> Reply {
        let this = self.clone();
        let reply = self.dispatch.run(move || async move {
            request.prepare()?;
            let result = this.post(crate::payload_fixture::json(request.take_payload()), options).await;
            request.complete(result)
        });
        Box::pin(async move { reply.await.map_err(|_| IvyError::internal())? })
    }
    fn request(&self, payload: IvyJson, options: RequestOptions) -> Reply {
        self.post(payload, options)
    }
}
impl PackageStorage for HttpWorker {
    fn request(&self, payload: IvyJson) -> PackageRequest {
        self.post(payload, RequestOptions::default())
    }
}
impl StorageLifecycle for HttpWorker {
    fn start(&self) -> Reply<()> {
        panic!("No storage launch in HTTP composition")
    }
    fn close(&self) -> Reply<()> {
        panic!("No storage shutdown in HTTP composition")
    }
}
fn probe(budget: &RequestBudget, spec: &Value) -> Value {
    match budget.acquire(
        &spec["key"].as_str().unwrap_or("probe").into(),
        (Some(spec["bytes"].as_f64().unwrap_or(0.0))).map(|bytes| bytes as usize),
    ) {
        Ok(mut lease) => {
            lease.release();
            json!({"ok":true})
        }
        Err(error) => json!({"error":wire(error)}),
    }
}
struct Response {
    log: Log,
    spec: Value,
    connection: server_http::Connection,
    budget: RequestBudget,
    ended: bool,
    destroyed: bool,
    sent: bool,
}
impl http_response::Response for Response {
    fn writable_ended(&self) -> bool {
        self.ended
    }
    fn destroyed(&self) -> bool {
        self.destroyed
    }
    fn headers_sent(&self) -> bool {
        self.sent
    }
    fn set_header(&mut self, name: &str, value: &JsString) -> IvyResult<()> {
        self.log
            .lock()
            .unwrap()
            .push(json!({"kind":"set","name":name,"value":value.to_utf8().unwrap()}));
        if self.spec["throwHeader"] == name {
            return Err(IvyError::internal());
        }
        Ok(())
    }
    fn write_head(&mut self, status: u16, headers: IvyJson) -> IvyResult<()> {
        let mut log = self.log.lock().unwrap();
        log.push(json!({"kind":"head","status":status,"headers":headers.to_serde().unwrap()}));
        log.push(json!({"kind":"lifetime","active":self.connection.active_requests(),"probe":probe(&self.budget,&self.spec["probe"])}));
        self.sent = true;
        Ok(())
    }
    fn end(&mut self, text: String) -> IvyResult<()> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        self.log
            .lock()
            .unwrap()
            .push(if self.spec["wireBytes"] == true {
                json!({"kind":"bytes","base64":STANDARD.encode(text.as_bytes())})
            } else {
                json!({"kind":"json","value":serde_json::from_str::<Value>(&text).unwrap()})
            });
        self.ended = true;
        Ok(())
    }
    fn end_empty(&mut self) -> IvyResult<()> {
        self.log.lock().unwrap().push(json!({"kind":"end"}));
        self.ended = true;
        Ok(())
    }
    fn destroy(&mut self) -> IvyResult<()> {
        self.log.lock().unwrap().push(json!({"kind":"destroy"}));
        self.destroyed = true;
        Ok(())
    }
}
struct Stream {
    id: String,
    chunks: VecDeque<Value>,
    cancel_error: bool,
    controls: Controls,
    log: Log,
}
impl BodyStream for Stream {
    async fn next_chunk(&mut self) -> IvyResult<Option<Vec<u8>>> {
        let Some(spec) = self.chunks.pop_front() else {
            return Ok(None);
        };
        if let Some(gate) = spec["gate"].as_str() {
            pause(&self.controls, gate).await;
        }
        if spec["error"].is_string() {
            return Err(failure(&spec));
        }
        use base64::{Engine, engine::general_purpose::STANDARD};
        if let Some(text) = spec["base64"].as_str() {
            return Ok(Some(STANDARD.decode(text).unwrap()));
        }
        Ok(Some(vec![
            spec["byte"].as_u64().unwrap_or(120) as u8;
            spec["bytes"].as_u64().unwrap() as usize
        ]))
    }
    async fn cancel(&mut self) -> IvyResult<()> {
        self.log
            .lock()
            .unwrap()
            .push(json!({"kind":"cancel","id":self.id}));
        if self.cancel_error {
            return Err(IvyError::new(
                "cancel_error",
                "Isolated cancellation failure.",
            ));
        }
        Ok(())
    }
}
fn headers(v: &Value) -> Headers {
    let text = |name: &str| v[name].as_str().map(JsString::from);
    Headers {
        authorization: text("authorization"),
        cookie: text("cookie"),
        origin: v.get("origin").map(|v| IvyJson::from_serde(v).unwrap()),
        forwarded_client_ip: v
            .get("x-ivy-client-ip")
            .map(|v| IvyJson::from_serde(v).unwrap()),
    }
}
async fn flush() {
    for _ in 0..128 {
        tokio::task::yield_now().await;
    }
}
async fn request(
    connection: server_http::Connection,
    http: &ivy_hive::server_constructor::Constructed<HttpWorker>,
    step: Value,
    mut response: Response,
    controls: Controls,
    log: Log,
) -> IvyResult<()> {
    let mut scope = connection.begin(&mut response)?;
    let mut path = JsString::default();
    let mut rpc_id = IvyJson::Null;
    let result = async {
        scope.admit(&headers(&step["headers"]))?;
        let method = JsString::from(step["method"].as_str().unwrap_or("GET"));
        let url = JsString::from(step["url"].as_str().unwrap_or("/ivy/fixture"));
        let route = scope.route(Some(&method), Some(&url))?;
        if route.authorization_metadata {
            return http_response::json(
                &mut response,
                200,
                &http
                    .oauth
                    .authorization_server_metadata(&http.base.base_path)?,
            );
        }
        path = route.path;
        let value = match step["action"].as_str().unwrap_or("route") {
            "httpMcp" => {
                let mut stream = Stream {
                    id: step["id"].as_str().unwrap().into(),
                    chunks: step["chunks"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into(),
                    cancel_error: step["cancelError"] == true,
                    controls: controls.clone(),
                    log: log.clone(),
                };
                let content_type = step["headers"]
                    .get("content-type")
                    .map(|v| IvyJson::from_serde(v).unwrap());
                let content_length = step["headers"]
                    .get("content-length")
                    .map(|v| IvyJson::from_serde(v).unwrap());
                let mut factory = mcp::Protocol {
                    spec: step["mcp"].clone(),
                    log,
                    controls,
                };
                if ivy_hive::server_mcp::handle(
                    &mut scope,
                    &http.mcp,
                    Some(&method),
                    &path,
                    &headers(&step["headers"]),
                    ivy_hive::server_http_rpc::Headers {
                        content_type: content_type.as_ref(),
                        content_length: content_length.as_ref(),
                    },
                    &mut rpc_id,
                    &mut stream,
                    &mut response,
                    &mut factory,
                )
                .await
                .map_err(mcp::outer_error)?
                {
                    return Ok(());
                }
                return Err(IvyError::new("not_found", "Hive path or method not found."));
            }
            "httpRpc" => {
                scope.admit_public_route(Some(&method), &path)?;
                let context = scope
                    .authenticate_http(
                        Some(method.clone()),
                        &headers(&step["headers"]),
                        &mut response,
                    )
                    .await?;
                let mut stream = Stream {
                    id: step["id"].as_str().unwrap().into(),
                    chunks: step["chunks"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into(),
                    cancel_error: step["cancelError"] == true,
                    controls: controls.clone(),
                    log,
                };
                let content_type = step["headers"]
                    .get("content-type")
                    .map(|v| IvyJson::from_serde(v).unwrap());
                let content_length = step["headers"]
                    .get("content-length")
                    .map(|v| IvyJson::from_serde(v).unwrap());
                if ivy_hive::server_http_rpc::handle(
                    &mut scope,
                    Some(&method),
                    &path,
                    ivy_hive::server_http_rpc::Headers {
                        content_type: content_type.as_ref(),
                        content_length: content_length.as_ref(),
                    },
                    &context,
                    &mut rpc_id,
                    &mut stream,
                    &mut response,
                )
                .await?
                {
                    return Ok(());
                }
                return Err(IvyError::new("not_found", "Hive path or method not found."));
            }
            "hold" => {
                pause(&controls, &format!("http:{}", step["id"].as_str().unwrap())).await;
                obj([("held", true.into())])
            }
            "body" => {
                let mut stream = Stream {
                    id: step["id"].as_str().unwrap().into(),
                    chunks: step["chunks"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into(),
                    cancel_error: step["cancelError"] == true,
                    controls: controls.clone(),
                    log,
                };
                let declared = step["headers"]
                    .get("content-length")
                    .map(|v| IvyJson::from_serde(v).unwrap());
                let bytes = scope
                    .body(
                        &mut stream,
                        declared.as_ref(),
                        step["limit"]
                            .as_u64()
                            .unwrap_or(ivy_contracts::limits::MANAGEMENT_FRAME_BYTES as u64)
                            as usize,
                    )
                    .await?;
                if let Some(gate) = step["responseGate"].as_str() {
                    pause(&controls, gate).await;
                }
                use base64::{Engine, engine::general_purpose::STANDARD};
                obj([("body", STANDARD.encode(bytes).into())])
            }
            "auth" | "rpc" => {
                let context = scope
                    .authenticate(http_auth::Request {
                        method: Some(method),
                        headers: headers(&step["headers"]),
                        transport: "http".into(),
                        resource: step["resource"].as_str().map(JsString::from),
                    })
                    .await?;
                if step["action"] == "auth" {
                    if let Some(gate) = step["responseGate"].as_str() {
                        pause(&controls, gate).await;
                    }
                    context
                } else {
                    scope
                        .invoke(
                            context,
                            IvyJson::from_serde(&step["frame"]).unwrap(),
                            (step["bytes"].as_f64().unwrap_or(0.0)) as usize,
                        )
                        .await?
                }
            }
            _ => obj([
                ("href", route.url.href.into()),
                ("pathname", IvyJson::String(route.pathname)),
                ("path", IvyJson::String(path.clone())),
            ]),
        };
        http_response::json(&mut response, 200, &value)
    }
    .await;
    let result = match result {
        Ok(()) => Ok(()),
        Err(error) => http_common::error(
            &http.base.origin,
            &http.base.base_path,
            &path,
            rpc_id,
            Some(&error),
            &mut response,
        ),
    };
    scope.release();
    result
}
fn record(
    results: &Arc<Mutex<serde_json::Map<String, Value>>>,
    id: &str,
    result: IvyResult<IvyJson>,
) {
    results.lock().unwrap().insert(
        id.into(),
        match result {
            Ok(value) => json!({"value":value.to_serde().unwrap()}),
            Err(error) => json!({"error":wire(error)}),
        },
    );
}
fn snapshot(
    name: &str,
    sockets: &[(String, server_http::Connection)],
    http: &server_http::Http,
    log: &Log,
    results: &Arc<Mutex<serde_json::Map<String, Value>>>,
    responses: &[(String, Log)],
) -> Value {
    json!({"name":name,"active":sockets.iter().map(|(id,c)|json!([id,c.active_requests()])).collect::<Vec<_>>(),"buffered":http.buffered_bytes(),"events":std::mem::take(&mut *log.lock().unwrap()),"results":*results.lock().unwrap(),"responses":responses.iter().map(|(id,l)|(id.clone(),json!(*l.lock().unwrap()))).collect::<serde_json::Map<_,_>>()})
}
async fn native(trace: &Value) -> Value {
    let log = Log::default();
    let controls = Controls::default();
    let worker = Arc::new(HttpWorker {
        credential: trace["options"]["credentials"][0]["digest"]
            .as_str()
            .unwrap()
            .into(),
        log: log.clone(),
        controls: controls.clone(),
        dispatch: fixture_dispatch::DispatchGate::default(),
        replies: Arc::new(Mutex::new(
            trace["replies"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_array().unwrap().iter().cloned().collect()))
                        .collect()
                })
                .unwrap_or_default(),
        )),
    });
    let b = trace["budget"].as_array();
    let number = |index: usize, default: f64| {
        b.and_then(|v| v.get(index))
            .and_then(Value::as_f64)
            .unwrap_or(default)
    };
    let budget = RequestBudget::new(RequestBudgetOptions {
        maximum: (number(0, 2048.0)) as usize,
        maximum_bytes: (number(1, 128.0 * 1024.0 * 1024.0)) as usize,
        per_consumer: (number(2, ivy_contracts::limits::CONSUMER_IN_FLIGHT_REQUESTS as f64))
            as usize,
        per_consumer_bytes: (number(3, 64.0 * 1024.0 * 1024.0)) as usize,
        name: "request".into(),
    });
    let mut effects = dependencies(Events::default());
    let ids = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    effects.ids = Arc::new(move || {
        format!(
            "request-{}",
            ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        )
        .into()
    });
    if trace["observe"] == true {
        let log = log.clone();
        let error = trace["observerError"].clone();
        effects.observe_mcp_call = Some(Arc::new(move |observation| {
            log.lock().unwrap().push(
                json!({"kind":"mcp-observe","value":observation.to_json().to_serde().unwrap()}),
            );
            if error["error"].is_string() {
                return Err(failure(&error).into());
            }
            Ok(())
        }));
    }
    let server = Arc::new(
        Prepared::new(options(&IvyJson::from_serde(&trace["options"]).unwrap()))
            .unwrap()
            .bind(
                worker,
                effects,
                Admission {
                    requests: budget.clone(),
                    ..Default::default()
                },
            )
            .unwrap(),
    );
    let mut sockets: Vec<(String, server_http::Connection)> = vec![];
    let mut responses: Vec<(String, Log)> = vec![];
    let mut running = HashMap::new();
    let results = Arc::new(Mutex::new(serde_json::Map::new()));
    let mut snapshots = vec![];
    // Each physical connection is cloned into accepted work; a socket close is
    // an authentication event, not cancellation of these retained tasks.
    for step in trace["steps"].as_array().unwrap() {
        let id = step["id"].as_str().unwrap_or("");
        let socket = |name: &str| sockets.iter().find(|(id, _)| id == name).unwrap().1.clone();
        match step["op"].as_str().unwrap() {
            "connect" => sockets.push((
                id.into(),
                server.http.connection(
                    sockets.len() as u64,
                    step["remoteAddress"].as_str().map(JsString::from),
                ),
            )),
            "clone" => sockets.push((id.into(), socket(step["from"].as_str().unwrap()))),
            "start" => {
                let connection = socket(step["socket"].as_str().unwrap());
                let response_log = Log::default();
                responses.push((id.into(), response_log.clone()));
                let spec = step["response"].clone();
                let response = Response {
                    log: response_log,
                    connection: connection.clone(),
                    budget: budget.clone(),
                    ended: spec["ended"] == true,
                    destroyed: spec["destroyed"] == true,
                    sent: spec["sent"] == true,
                    spec,
                };
                let server = server.clone();
                let controls = controls.clone();
                let log = log.clone();
                let results = results.clone();
                let id = id.to_owned();
                let step = step.clone();
                running.insert(
                    id.clone(),
                    tokio::task::spawn_local(async move {
                        // Constructed has shared owners but no Clone requirement on
                        // the worker. Borrow the retained Arc for the whole scope.
                        let result = request(connection, &server, step, response, controls, log)
                            .await
                            .map(|()| IvyJson::Null);
                        record(&results, &id, result);
                    }),
                );
            }
            "release" => controls
                .lock()
                .unwrap()
                .remove(step["gate"].as_str().unwrap())
                .expect("Missing gate")
                .send(())
                .unwrap(),
            "join" => {
                for id in step["ids"].as_array().unwrap() {
                    running
                        .remove(id.as_str().unwrap())
                        .expect("Missing observation")
                        .await
                        .unwrap();
                }
            }
            "close" => socket(step["socket"].as_str().unwrap()).close(),
            "invoke" => {
                let work = server.network.invocation.invoke(
                    IvyJson::from_serde(&step["context"]).unwrap(),
                    IvyJson::from_serde(&step["frame"]).unwrap(),
                    (step["bytes"].as_f64().unwrap_or(0.0)) as usize,
                );
                if step["discard"] == true {
                    drop(work);
                } else {
                    let results = results.clone();
                    let id = id.to_owned();
                    running.insert(
                        id.clone(),
                        tokio::task::spawn_local(async move {
                            record(&results, &id, work.await);
                        }),
                    );
                }
            }
            "probe" => {
                results
                    .lock()
                    .unwrap()
                    .insert(id.into(), probe(&budget, step));
            }
            "snapshot" => snapshots.push(snapshot(
                step["name"].as_str().unwrap(),
                &sockets,
                &server.http,
                &log,
                &results,
                &responses,
            )),
            op => panic!("Unknown HTTP composition command {op}"),
        }
        flush().await;
    }
    assert!(controls.lock().unwrap().is_empty());
    for (_, work) in running {
        work.await.unwrap();
    }
    flush().await;
    snapshots.push(snapshot(
        "final",
        &sockets,
        &server.http,
        &log,
        &results,
        &responses,
    ));
    json!({"snapshots":snapshots})
}
fn compare(group: &str, traces: Vec<Value>) -> Vec<Value> {
    let original = lifetime_reference("http", &traces);
    assert_eq!(original.len(), traces.len());
    let mut rows = vec![];
    for (input, mut original) in traces.into_iter().zip(original) {
        crate::payload_fixture::project_auth_requests(&mut original);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let native = runtime.block_on(tokio::task::LocalSet::new().run_until(native(&input)));
        let snapshots = input["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|step| step["op"] == "snapshot")
            .count()
            + 1;
        assert_eq!(original["snapshots"].as_array().unwrap().len(), snapshots);
        assert_eq!(native["snapshots"].as_array().unwrap().len(), snapshots);
        // Compare JSON objects semantically and arrays in order. JSON numbers
        // use the original binary64 domain, regardless of integer display form.
        assert_eq!(
            semantic(&native),
            semantic(&original),
            "HTTP composition: {group}, {input}"
        );
        rows.push(native);
    }
    rows
}
fn semantic(value: &Value) -> Value {
    match value {
        Value::Number(number) => json!(number.as_f64().unwrap()),
        Value::Array(values) => Value::Array(values.iter().map(semantic).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), semantic(v)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn trace(name: &str, steps: Vec<Value>) -> Value {
    json!({"name":name,"options":{"filename":"state/hive.sqlite","publicBaseUrl":"https://EXAMPLE.test:443/ivy/","version":"1.2.3","buildId":"build:test","credentials":[{"principalId":"a","digest":"credential"}],"trustedProxyAddresses":["127.0.0.1"]},"steps":steps})
}
fn connect() -> Value {
    json!({"op":"connect","id":"s","remoteAddress":"192.0.2.1"})
}
fn frame() -> Value {
    json!({"jsonrpc":"2.0","id":"read","method":"objects.read","params":{"objectId":"fixture"}})
}

#[path = "hive_server_mcp.rs"]
mod mcp;
#[path = "hive_server_http_rpc.rs"]
mod rpc;

#[test]
fn original_composed_http_admission_route_and_response_lifetime() {
    let mut cases = vec![];
    let mut header = trace(
        "header-failure-before-active-and-budget",
        vec![
            connect(),
            json!({"op":"start","id":"header","socket":"s","response":{"throwHeader":"X-Frame-Options"}}),
            json!({"op":"probe","id":"free","bytes":1024}),
            json!({"op":"snapshot","name":"header-failed"}),
        ],
    );
    header["budget"] = json!([1, 1024, 1, 1024]);
    cases.push(header);
    let mut address = trace(
        "client-address-before-reservation",
        vec![
            json!({"op":"connect","id":"s","remoteAddress":"::ffff:127.0.0.1"}),
            json!({"op":"start","id":"missing","socket":"s"}),
            json!({"op":"start","id":"array","socket":"s","headers":{"x-ivy-client-ip":["192.0.2.1"]}}),
            json!({"op":"start","id":"valid","socket":"s","headers":{"x-ivy-client-ip":"192.0.2.1"}}),
        ],
    );
    address["budget"] = json!([0, 0, 0, 0]);
    cases.push(address);
    let mut clients = trace(
        "normalized-proxy-consumers-and-untrusted-header",
        vec![
            json!({"op":"connect","id":"s","remoteAddress":"127.0.0.1"}),
            json!({"op":"connect","id":"t","remoteAddress":"127.0.0.1"}),
            json!({"op":"connect","id":"u","remoteAddress":"192.0.2.2"}),
            json!({"op":"start","id":"held","socket":"s","action":"hold","headers":{"x-ivy-client-ip":"::ffff:203.0.113.1"}}),
            json!({"op":"start","id":"same","socket":"t","headers":{"x-ivy-client-ip":"203.0.113.1"}}),
            json!({"op":"start","id":"untrusted","socket":"u","headers":{"x-ivy-client-ip":"203.0.113.1"}}),
            json!({"op":"snapshot","name":"consumer-collision"}),
            json!({"op":"release","gate":"http:held"}),
        ],
    );
    clients["budget"] = json!([10, 100000, 1, 100000]);
    cases.push(clients);
    let mut steps = vec![connect(), json!({"op":"clone","id":"alias","from":"s"})];
    for i in 0..ivy_contracts::limits::CONNECTION_IN_FLIGHT_REQUESTS {
        steps.push(json!({"op":"start","id":format!("held-{i}"),"socket":"s","action":"hold"}));
    }
    steps.extend([
        json!({"op":"start","id":"excess","socket":"alias"}),
        json!({"op":"snapshot","name":"connection-full"}),
        json!({"op":"probe","id":"excess-refunded","bytes":0}),
    ]);
    for i in 0..ivy_contracts::limits::CONNECTION_IN_FLIGHT_REQUESTS {
        steps.push(json!({"op":"release","gate":format!("http:held-{i}")}));
    }
    let mut connection = trace("physical-connection-limit-after-reservation", steps);
    connection["budget"] = json!([65, 1000000, 100, 1000000]);
    cases.push(connection);
    cases.push(trace("canonical-route-prefix-and-metadata-order",vec![connect(),json!({"op":"start","id":"base","socket":"s","url":"/ivy"}),json!({"op":"start","id":"outside","socket":"s","url":"/ivory/x"}),json!({"op":"start","id":"metadata","socket":"s","url":"/.well-known/oauth-authorization-server"}),json!({"op":"start","id":"url","socket":"s","url":"/ivy/a/../b?q=x"}),json!({"op":"start","id":"sent","socket":"s","response":{"sent":true}}),json!({"op":"start","id":"ended","socket":"s","response":{"ended":true}})]));
    let rows = compare("admission", cases);
    assert_eq!(rows[0]["snapshots"][0]["active"][0][1], 0);
    assert_eq!(rows[0]["snapshots"][0]["results"]["free"]["ok"], true);
    assert_eq!(
        rows[1]["snapshots"][0]["responses"]["missing"][5]["status"],
        429
    );
    assert_eq!(
        rows[1]["snapshots"][0]["responses"]["array"][5]["status"],
        401
    );
    assert_eq!(
        rows[1]["snapshots"][0]["responses"]["valid"][5]["status"],
        429
    );
    let full = &rows[3]["snapshots"][0];
    assert_eq!(full["active"][0][1], 64);
    assert_eq!(full["responses"]["excess"][6]["active"], 65);
    assert_eq!(
        full["responses"]["excess"][6]["probe"]["error"]["data"]["code"],
        "limit_exceeded"
    );
}

#[test]
fn original_composed_http_body_buffers_and_retained_byte_reservations() {
    let mut shared = trace(
        "shared-buffer-counter-and-retained-body-reservations",
        vec![
            connect(),
            json!({"op":"connect","id":"t","remoteAddress":"192.0.2.2"}),
            json!({"op":"start","id":"a","socket":"s","action":"body","chunks":[{"bytes":3},{"bytes":0,"gate":"a-eof"}],"responseGate":"a-response"}),
            json!({"op":"start","id":"b","socket":"t","action":"body","chunks":[{"bytes":5},{"bytes":0,"gate":"b-eof"}]}),
            json!({"op":"snapshot","name":"both-buffered"}),
            json!({"op":"probe","id":"full","bytes":1}),
            json!({"op":"release","gate":"a-eof"}),
            json!({"op":"snapshot","name":"a-body-done-response-pending"}),
            json!({"op":"probe","id":"still-full","bytes":1}),
            json!({"op":"release","gate":"a-response"}),
            json!({"op":"probe","id":"a-released","bytes":1027}),
            json!({"op":"probe","id":"one-too-many","bytes":1028}),
            json!({"op":"release","gate":"b-eof"}),
        ],
    );
    shared["budget"] = json!([20, 2056, 20, 2056]);
    let mut grow = trace(
        "reservation-before-frame-limit-and-cancel-error",
        vec![
            connect(),
            json!({"op":"start","id":"body","socket":"s","action":"body","limit":2,"chunks":[{"bytes":4}],"cancelError":true,"response":{"probe":{"bytes":4}}}),
            json!({"op":"probe","id":"refunded","bytes":1027}),
        ],
    );
    grow["budget"] = json!([20, 1027, 20, 1027]);
    let mut cases = vec![shared, grow];
    for (name, headers, chunks, limit) in [
        (
            "declared-length-before-stream",
            json!({"content-length":"3x"}),
            json!([{"bytes":1}]),
            2,
        ),
        (
            "actual-body-limit-and-cancel-error",
            json!({"content-length":"2"}),
            json!([{"bytes":3}]),
            2,
        ),
        (
            "stream-error-cleans-without-cancel",
            json!({}),
            json!([{"bytes":2},{"error":"storage_unavailable"}]),
            10,
        ),
    ] {
        cases.push(trace(name,vec![connect(),json!({"op":"start","id":"body","socket":"s","action":"body","headers":headers,"chunks":chunks,"limit":limit,"cancelError":true}),json!({"op":"probe","id":"free","bytes":10000})]));
    }
    let rows = compare("body", cases);
    assert_eq!(rows[0]["snapshots"][0]["buffered"].as_f64(), Some(8.0));
    assert_eq!(rows[0]["snapshots"][1]["buffered"].as_f64(), Some(5.0));
    let end = rows[0]["snapshots"].as_array().unwrap().last().unwrap();
    assert_eq!(
        end["results"]["still-full"]["error"]["data"]["code"],
        "limit_exceeded"
    );
    assert_eq!(end["results"]["a-released"]["ok"], true);
    assert_eq!(end["buffered"].as_f64(), Some(0.0));
    assert_eq!(rows[1]["snapshots"][0]["events"][0]["kind"], "cancel");
    assert_eq!(
        rows[1]["snapshots"][0]["responses"]["body"][7]["value"]["error"]["data"]["code"],
        "limit_exceeded"
    );
    assert!(
        rows[4]["snapshots"][0]["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn original_composed_http_authentication_invocation_and_socket_close() {
    let bearer = json!({"authorization":"Bearer initial"});
    let mut slots = trace(
        "outer-and-invocation-share-slot-budget",
        vec![
            connect(),
            json!({"op":"start","id":"rpc","socket":"s","action":"rpc","headers":bearer,"frame":frame(),"bytes":4}),
            json!({"op":"invoke","id":"after","context":{"credentialDigest":"credential","transport":"http"},"frame":frame(),"bytes":4}),
        ],
    );
    slots["budget"] = json!([1, 10000, 10, 10000]);
    let mut bytes = slots.clone();
    bytes["name"] = json!("outer-and-invocation-share-byte-budget");
    bytes["budget"] = json!([10, 2051, 10, 10000]);
    let mut pending = trace(
        "socket-close-retains-http-and-invocation",
        vec![
            connect(),
            json!({"op":"start","id":"rpc","socket":"s","action":"rpc","headers":bearer,"frame":frame(),"bytes":4}),
            json!({"op":"close","socket":"s"}),
            json!({"op":"snapshot","name":"closed-pending-storage"}),
            json!({"op":"probe","id":"charged","bytes":0}),
            json!({"op":"start","id":"refused","socket":"s"}),
            json!({"op":"release","gate":"execute"}),
            json!({"op":"probe","id":"refunded","bytes":2048}),
        ],
    );
    pending["budget"] = json!([2, 10000, 10, 10000]);
    pending["replies"] = json!({"execute":[{"gate":"execute"}]});
    let mut dropped = trace(
        "dropped-invocation-observer-retains-shared-budget",
        vec![
            connect(),
            json!({"op":"start","id":"held","socket":"s","action":"hold"}),
            json!({"op":"invoke","id":"abandoned","discard":true,"context":{"credentialDigest":"credential","transport":"http"},"frame":frame(),"bytes":4}),
            json!({"op":"release","gate":"http:held"}),
            json!({"op":"close","socket":"s"}),
            json!({"op":"probe","id":"retained","bytes":8973}),
            json!({"op":"snapshot","name":"outer-done-storage-retained"}),
            json!({"op":"release","gate":"execute"}),
            json!({"op":"probe","id":"refunded","bytes":10000}),
        ],
    );
    dropped["budget"] = json!([2, 10000, 10, 10000]);
    dropped["replies"] = json!({"execute":[{"gate":"execute"}]});
    let mut cache = trace(
        "physical-socket-authentication-cache-and-close",
        vec![
            connect(),
            json!({"op":"clone","id":"alias","from":"s"}),
            json!({"op":"connect","id":"t","remoteAddress":"192.0.2.2"}),
            json!({"op":"start","id":"a","socket":"s","action":"auth","headers":bearer,"responseGate":"a-response"}),
            json!({"op":"start","id":"b","socket":"alias","action":"auth","headers":bearer}),
            json!({"op":"start","id":"c","socket":"t","action":"auth","headers":bearer}),
            json!({"op":"snapshot","name":"shared-authentication"}),
            json!({"op":"close","socket":"s"}),
            json!({"op":"start","id":"d","socket":"alias","action":"auth","headers":bearer}),
            json!({"op":"release","gate":"authenticate"}),
            json!({"op":"snapshot","name":"cache-cleared-pending-http-retained"}),
            json!({"op":"release","gate":"a-response"}),
        ],
    );
    cache["replies"] = json!({"authenticate":[{"gate":"authenticate"}]});
    let rows = compare("owners", vec![slots, bytes, pending, dropped, cache]);
    for row in &rows[..2] {
        assert_eq!(row["snapshots"][0]["responses"]["rpc"][5]["status"], 429);
        assert_eq!(
            row["snapshots"][0]["results"]["after"]["value"]["stored"],
            true
        );
    }
    assert_eq!(rows[2]["snapshots"][0]["active"][0][1], 1);
    assert_eq!(
        rows[2]["snapshots"][1]["results"]["charged"]["error"]["data"]["code"],
        "limit_exceeded"
    );
    assert_eq!(
        rows[3]["snapshots"][0]["results"]["retained"]["error"]["data"]["code"],
        "limit_exceeded"
    );
    assert_eq!(
        rows[4]["snapshots"][0]["events"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        rows[4]["snapshots"][1]["events"].as_array().unwrap().len(),
        1
    );
    assert_eq!(rows[4]["snapshots"][1]["active"][0][1], 1);
}
