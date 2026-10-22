use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::{
    admission::{RequestBudget, RequestBudgetOptions},
    http_auth::Headers as AuthHeaders,
    http_body::BodyStream,
    http_response::Response as ResponsePort,
    mcp_dispatch as protocol, mcp_request, server_http, server_http_rpc, server_mcp,
    server_peers::PeerSocket,
};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::{mpsc, oneshot};

type Trace = Arc<Mutex<Vec<IvyJson>>>;
#[derive(Default)]
struct Controls {
    observation: Option<IvyJson>,
    clock_active: AtomicBool,
    clock_ms: Mutex<f64>,
    storage_failure: Option<IvyJson>,
    catalogue_gate: Mutex<Option<oneshot::Receiver<()>>>,
    close_gate: Mutex<Option<oneshot::Receiver<()>>>,
    catalogue_started: AtomicBool,
    close_started: AtomicBool,
    destroyed: AtomicBool,
    create_error: bool,
    instruction_error: bool,
    head_error: bool,
    end_error: bool,
    close_error: bool,
}
fn fixture_time(value: &IvyJson) -> f64 {
    match value.as_str().ok().as_deref() {
        Some("NaN") => f64::NAN,
        Some("Infinity") => f64::INFINITY,
        Some("-Infinity") => f64::NEG_INFINITY,
        _ => value.as_f64().unwrap(),
    }
}
impl Controls {
    fn advance_clock(&self) {
        if let Some(spec) = &self.observation {
            *self.clock_ms.lock().unwrap() = fixture_time(&spec.get("clock").unwrap().as_array().unwrap()[1]);
        }
    }
}
struct Backend {
    trace: Trace,
    controls: Arc<Controls>,
}
impl Backend {
    fn post(&self, payload: IvyJson, options: RequestOptions) -> Reply {
        self.trace.lock().unwrap().push(obj([
            ("kind", "worker".into()),
            ("payload", payload.clone()),
            ("reservedBytes", IvyJson::Number(options.reserved_bytes as f64)),
        ]));
        let action = payload.required("action").unwrap().as_str().unwrap();
        if let Some(spec) = self.controls.storage_failure.as_ref().filter(|spec| {
            spec.get("action").and_then(|v| v.as_str().ok()).as_deref() == Some(action.as_str())
        }) {
            let mut error = IvyError::new_js(
                spec.get("code").unwrap().as_js_string().unwrap().clone(),
                spec.get("message").unwrap().as_js_string().unwrap().clone(),
            );
            if let Some(outcome) = spec.get("outcome") {
                error = error.with_provider_outcome(outcome.clone());
            }
            if let Some(details) = spec.get("details") {
                error = error.with_details(details.clone());
            }
            let controls = self.controls.clone();
            let catalogue = action == "mcp.catalog";
            return Box::pin(async move {
                tokio::task::yield_now().await;
                if catalogue { controls.advance_clock(); }
                Err(error)
            });
        }
        let value = match action.as_str() {
            "authenticate" => obj([("principalId", "a".into())]),
            "execute" => {
                assert_eq!(
                    payload.required("request").unwrap().get("method"),
                    Some(&"discovery.instructions".into())
                );
                obj([
                    ("kind", "result".into()),
                    (
                        "value",
                        obj([("instructions", "Dynamic fixture instructions.".into())]),
                    ),
                ])
            }
            "mcp.catalog" => IvyJson::Array(
                if payload
                    .get("name")
                    .is_none_or(|v| v == &"fixture_read".into() || v == &"".into())
                {
                    vec![crate::mcp_dispatch::binding()]
                } else {
                    vec![]
                },
            ),
            "mcp.cursor.create" => obj([("cursor", "fixture-cursor".into())]),
            "mcp.cursor.read" if payload.get("cursor") == Some(&"end".into()) => {
                obj([("offset", IvyJson::Number(1.0))])
            }
            "mcp.cursor.read" => {
                return Box::pin(async {
                    Err(IvyError::new("invalid_cursor", "Invalid fixture cursor."))
                });
            }
            "disconnect" => IvyJson::Null,
            action => panic!("Unexpected worker action {action}"),
        };
        let gate = if action == "mcp.catalog" {
            self.controls
                .catalogue_started
                .store(true, AtomicOrdering::SeqCst);
            self.controls.catalogue_gate.lock().unwrap().take()
        } else {
            None
        };
        let fail = action == "execute" && self.controls.instruction_error;
        let controls = self.controls.clone();
        let catalogue = action == "mcp.catalog";
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.await.unwrap();
            }
            tokio::task::yield_now().await;
            if catalogue { controls.advance_clock(); }
            if fail {
                Err(IvyError::new("instruction_failure", "Instructions failed."))
            } else {
                Ok(value)
            }
        })
    }
}
impl Storage for Backend {
    fn request(&self, payload: IvyJson, options: RequestOptions) -> Reply {
        self.post(payload, options)
    }
}
impl Worker for Backend {
    fn request(&self, payload: IvyJson) -> browser_push::RequestFuture {
        self.post(payload, RequestOptions::default())
    }
}
impl PackageStorage for Backend {
    fn request(&self, payload: IvyJson) -> PackageRequest {
        self.post(payload, RequestOptions::default())
    }
}
impl StorageLifecycle for Backend {
    fn start(&self) -> Reply<()> {
        panic!("No worker launch")
    }
    fn close(&self) -> Reply<()> {
        panic!("No worker shutdown")
    }
}
struct Provider {
    sent: mpsc::UnboundedSender<IvyJson>,
    trace: Trace,
}
impl ivy_hive::ws_send::Socket for Provider {
    fn is_open(&self) -> bool {
        true
    }
    fn buffered_amount(&self) -> f64 {
        0.0
    }
    fn send_text(&mut self, value: String) -> IvyResult<()> {
        let frame = crate::json(&value);
        self.trace.lock().unwrap().push(obj([
            ("kind", "provider-send".into()),
            ("frame", frame.clone()),
        ]));
        self.sent.send(frame).unwrap();
        Ok(())
    }
    fn terminate(&mut self) -> IvyResult<()> {
        panic!("No provider termination")
    }
}
impl PeerSocket for Provider {
    fn ping(&mut self) -> IvyResult<()> {
        panic!("No ping")
    }
}
struct Body(Option<Vec<u8>>);
impl BodyStream for Body {
    async fn next_chunk(&mut self) -> IvyResult<Option<Vec<u8>>> {
        Ok(self.0.take())
    }
    async fn cancel(&mut self) -> IvyResult<()> {
        panic!("The represented bodies are exhausted, not cancelled")
    }
}
struct Response {
    trace: Trace,
    controls: Arc<Controls>,
    status: u16,
    headers: IvyJson,
    body: String,
    ended: bool,
    sent: bool,
}
impl Response {
    fn wire(&self) -> IvyJson {
        let mut headers = crate::json("{}");
        for name in [
            "content-type",
            "mcp-protocol-version",
            "mcp-session-id",
            "allow",
            "cache-control",
        ] {
            if let Some(value) = self.headers.get(name) {
                headers.insert(name, value.clone());
            }
        }
        obj([
            ("status", IvyJson::Number(self.status.into())),
            ("headers", headers),
            ("bodyBase64", STANDARD.encode(self.body.as_bytes()).into()),
        ])
    }
}
impl ResponsePort for Response {
    fn writable_ended(&self) -> bool {
        self.ended
    }
    fn destroyed(&self) -> bool {
        self.controls.destroyed.load(AtomicOrdering::SeqCst)
    }
    fn headers_sent(&self) -> bool {
        self.sent
    }
    fn set_header(&mut self, name: &str, value: &JsString) -> IvyResult<()> {
        self.headers
            .insert(name.to_ascii_lowercase(), IvyJson::String(value.clone()));
        Ok(())
    }
    fn write_head(&mut self, status: u16, headers: IvyJson) -> IvyResult<()> {
        self.trace
            .lock()
            .unwrap()
            .push(obj([("kind", "response-head".into())]));
        if self.controls.head_error {
            return Err(IvyError::new("head_failure", "Head failed."));
        }
        self.status = status;
        for (name, value) in ivy_contracts::json::object_entries(&headers) {
            self.headers
                .insert(name.to_utf8().unwrap().to_ascii_lowercase(), value.clone());
        }
        self.sent = true;
        Ok(())
    }
    fn end(&mut self, value: String) -> IvyResult<()> {
        self.trace
            .lock()
            .unwrap()
            .push(obj([("kind", "response-end".into())]));
        if self.controls.end_error {
            return Err(IvyError::new("end_failure", "End failed."));
        }
        self.body = value;
        self.ended = true;
        Ok(())
    }
    fn end_empty(&mut self) -> IvyResult<()> {
        self.end(String::new())
    }
    fn destroy(&mut self) -> IvyResult<()> {
        self.controls.destroyed.store(true, AtomicOrdering::SeqCst);
        Ok(())
    }
}
struct Factory<'a> {
    inner: protocol::Factory<'a>,
    trace: Trace,
    controls: Arc<Controls>,
}
struct Handler<'a> {
    inner: protocol::Handler<'a>,
    trace: Trace,
    controls: Arc<Controls>,
}
impl<'a> server_mcp::Factory for Factory<'a> {
    type Handler = Handler<'a>;
    fn create(&mut self, app: &server_mcp::Application) -> mcp_request::Result<Self::Handler> {
        self.trace.lock().unwrap().push(obj([
            ("kind", "create".into()),
            ("receivedBytes", IvyJson::Number(app.received_bytes as f64)),
            ("info", app.info.clone()),
            ("configuration", app.configuration()),
        ]));
        if self.controls.create_error {
            return Err(IvyError::new("create_failure", "Create failed.").into());
        }
        Ok(Handler {
            inner: self.inner.create(app)?,
            trace: self.trace.clone(),
            controls: self.controls.clone(),
        })
    }
}
impl server_mcp::Handler for Handler<'_> {
    async fn handle(
        &mut self,
        app: &server_mcp::Application,
        response: &mut impl ResponsePort,
    ) -> mcp_request::Result<()> {
        if self.controls.observation.is_some() { self.controls.clock_active.store(true, AtomicOrdering::SeqCst); }
        self.trace
            .lock()
            .unwrap()
            .push(obj([("kind", "handle".into())]));
        self.inner.handle(app, response).await
    }
    async fn close(&mut self) -> mcp_request::Result<()> {
        self.trace
            .lock()
            .unwrap()
            .push(obj([("kind", "close".into())]));
        self.inner.close().await?;
        self.controls.clock_active.store(false, AtomicOrdering::SeqCst);
        self.controls
            .close_started
            .store(true, AtomicOrdering::SeqCst);
        let gate = self.controls.close_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.await.unwrap();
        }
        if self.controls.close_error {
            return Err(IvyError::new("close_failure", "Close failed.").into());
        }
        Ok(())
    }
}
fn available(budget: &RequestBudget) -> usize {
    let mut leases = vec![];
    while let Ok(lease) = budget.acquire(&"probe".into(), Some(0.0)) {
        leases.push(lease);
    }
    let count = leases.len();
    for mut lease in leases {
        lease.release();
    }
    count
}
type Inspection = (server_http::Connection, RequestBudget);
async fn exchange(
    row: IvyJson,
    controls: Arc<Controls>,
    inspection: Option<oneshot::Sender<Inspection>>,
) -> IvyJson {
    let trace = Trace::default();
    let worker = Arc::new(Backend {
        trace: trace.clone(),
        controls: controls.clone(),
    });
    let options = crate::json(
        r#"{"filename":"state/hive.sqlite","publicBaseUrl":"https://EXAMPLE.invalid:443/ivy/","version":"1.2.3","buildId":"build:test","credentials":[{"principalId":"a","digest":"credential"}]}"#,
    );
    let mut effects = dependencies(Events::default());
    let count = Arc::new(AtomicUsize::new(0));
    let id_trace = trace.clone();
    let record_ids = controls.observation.is_some();
    effects.ids = Arc::new(move || {
        let id: JsString = format!("owner-{}", count.fetch_add(1, AtomicOrdering::SeqCst)).into();
        if record_ids { id_trace.lock().unwrap().push(obj([("kind", "id".into()), ("id", IvyJson::String(id.clone()))])); }
        id
    });
    if let Some(spec) = &controls.observation {
        *controls.clock_ms.lock().unwrap() = fixture_time(&spec.get("clock").unwrap().as_array().unwrap()[0]);
        let clock_controls = controls.clone();
        let clock_trace = trace.clone();
        effects.now = Arc::new(move || {
            if !clock_controls.clock_active.load(AtomicOrdering::SeqCst) { return 100000.0; }
            let now = *clock_controls.clock_ms.lock().unwrap();
            clock_trace.lock().unwrap().push(obj([("kind", "clock".into()), ("value", IvyJson::Number(now))]));
            now
        });
        let mode = spec.get("mode").unwrap().as_str().unwrap();
        if mode != "absent" {
            let observation_trace = trace.clone();
            effects.observe_mcp_call = Some(Arc::new(move |value| {
                observation_trace.lock().unwrap().push(obj([("kind", "observe".into()), ("value", value.to_json())]));
                match mode.as_str() {
                    "ivy-failure" => Err(IvyError::new("observer_failed", "Observer refused.").with_outcome(Outcome::Completed).into()),
                    "protocol-failure" => Err(mcp_request::Error::Protocol(mcp_request::ProtocolError { code: -32602, message: "Observer protocol refusal.".into(), data: obj([("stage", "observer".into())]) })),
                    "ok" => Ok(()),
                    _ => panic!("Unknown fixture observer mode"),
                }
            }));
        }
    }
    if row.get("providerReply").is_some() {
        let constructor = effects.provider_error.clone();
        let events = trace.clone();
        effects.provider_error = Arc::new(move |parts| {
            let mut input = obj([
                ("kind", "provider-error-input".into()),
                ("code", IvyJson::String(parts.code.clone())),
                ("message", IvyJson::String(parts.message.clone())),
                ("outcome", parts.outcome.clone()),
            ]);
            if let Some(details) = &parts.details {
                input.insert("details", details.clone());
            }
            events.lock().unwrap().push(input);
            constructor(parts)
        });
    }
    let budget = RequestBudget::new(RequestBudgetOptions {
        maximum: 2.0,
        per_consumer: 2.0,
        maximum_bytes: 1_000_000.0,
        per_consumer_bytes: 1_000_000.0,
        name: "request".into(),
    });
    let server = Prepared::new(super::options(&options))
        .unwrap()
        .bind(
            worker,
            effects,
            Admission {
                requests: budget.clone(),
                ..Default::default()
            },
        )
        .unwrap();
    let (send, mut receive) = mpsc::unbounded_channel();
    let peer = server.network.peers.connected(Box::new(Provider { sent: send, trace: trace.clone() }), crate::json(r#"{"principalId":"provider-principal","credentialDigest":"provider-credential","transport":"ws","serviceNodeId":"node","generation":1}"#));
    server.network.peers.routing_update(&obj([
        ("node", crate::json(r#"{"serviceNodeId":"node","hostId":"host-node","serviceName":"sample","principalId":"provider-principal","connected":true,"synced":true,"ready":true,"desiredEnabled":true}"#)),
        ("generation", IvyJson::Number(1.0)),
        ("catalog", obj([("namespaces", crate::json(r#"["fixture"]"#)), ("tools", IvyJson::Array(vec![crate::mcp_dispatch::binding()])), ("notifications", IvyJson::Array(vec![]))])),
    ])).unwrap();
    let connection = server.http.connection(1, Some("192.0.2.1".into()));
    if let Some(inspection) = inspection {
        let _ = inspection.send((connection.clone(), budget.clone()));
    }
    let mut response = Response {
        trace: trace.clone(),
        controls: controls.clone(),
        status: 0,
        headers: crate::json("{}"),
        body: String::new(),
        ended: false,
        sent: false,
    };
    let mut scope = connection.begin(&mut response).unwrap();
    let auth = AuthHeaders {
        authorization: Some("Bearer initial".into()),
        ..Default::default()
    };
    scope.admit(&auth).unwrap();
    let inbound = crate::mcp_inbound::at(&row);
    let mut factory = Factory {
        inner: protocol::Factory {
            headers: protocol::Headers {
                protocol_version: inbound.protocol_version.cloned(),
                method: inbound.method.cloned(),
                name: inbound.name.cloned(),
                content_type: inbound.content_type.cloned(),
                accept: inbound.accept.cloned(),
            },
            runtime: &crate::mcp_dispatch::Language,
        },
        trace: trace.clone(),
        controls: controls.clone(),
    };
    let bytes = crate::encode(row.get("body").unwrap()).into_bytes();
    let received_bytes = bytes.len();
    let mut body = Body(Some(bytes));
    let method = row
        .get("method")
        .and_then(IvyJson::as_js_string)
        .cloned()
        .unwrap_or_else(|| "POST".into());
    let path = JsString::from(if row.get("path") == Some(&"/mcp-dev".into()) {
        "/mcp-dev"
    } else {
        "/mcp"
    });
    let content_type = row.required("headers").unwrap().get("content-type");
    let mut rpc_id = IvyJson::Null;
    let result = {
        let work = server_mcp::handle(
            &mut scope,
            &server.mcp,
            Some(&method),
            &path,
            &auth,
            server_http_rpc::Headers {
                content_type,
                content_length: None,
            },
            &mut rpc_id,
            &mut body,
            &mut response,
            &mut factory,
        );
        tokio::pin!(work);
        loop {
            tokio::select! {
                result = &mut work => break result,
                Some(frame) = receive.recv() => {
                    assert_eq!(frame.get("method"), Some(&"provider.invoke".into()));
                    let mut reply = obj([("jsonrpc", "2.0".into()), ("id", frame.get("id").unwrap().clone()), ("result", row.get("reply").cloned().unwrap_or_else(|| "answer".into()))]);
                    if let Some(spec) = row.get("providerReply") {
                        reply = spec.clone();
                        reply.insert("id", frame.get("id").unwrap().clone());
                        trace.lock().unwrap().push(obj([("kind", "provider-reply".into()), ("frame", reply.clone())]));
                    }
                    server.network.peers.provider_result(&peer, &reply);
                }
            }
        }
    };
    let active_before_release = connection.active_requests();
    scope.release();
    server.network.peers.closed(&peer).unwrap();
    tokio::task::yield_now().await;
    let error = match result {
        Ok(value) => {
            assert!(value);
            IvyJson::Null
        }
        Err(mcp_request::Error::Ivy(error)) => error.to_wire(),
        Err(mcp_request::Error::Protocol(error)) => error.to_json(),
    };
    let output = obj([
        ("name", row.get("name").unwrap().clone()),
        ("wire", response.wire()),
        ("error", error),
        ("events", IvyJson::Array(trace.lock().unwrap().clone())),
        ("receivedBytes", IvyJson::Number(received_bytes as f64)),
        ("bodyConsumed", IvyJson::Bool(body.0.is_none())),
        (
            "activeBeforeRelease",
            IvyJson::Number(active_before_release as f64),
        ),
        (
            "activeAfterRelease",
            IvyJson::Number(connection.active_requests() as f64),
        ),
        (
            "availableAfterRelease",
            IvyJson::Number(available(&budget) as f64),
        ),
        ("rpcId", rpc_id),
    ]);
    output
}
fn save(suffix: &str, value: &IvyJson) {
    if let Some(prefix) = std::env::var_os("IVY_HIVE_MCP_OWNER_CORPUS") {
        std::fs::write(
            format!("{}-{suffix}.json", prefix.to_string_lossy()),
            crate::encode(value),
        )
        .unwrap();
    }
}
fn baseline() -> IvyJson {
    crate::json(
        &std::fs::read_to_string(
            std::env::var_os("IVY_HIVE_MCP_OWNER_REFERENCE")
                .expect("Run the composed dispatch check first"),
        )
        .unwrap(),
    )
}
fn kinds(row: &IvyJson) -> Vec<String> {
    row.get("events")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.get("kind").and_then(|v| v.as_str().ok()))
        .collect()
}
#[tokio::test(flavor = "current_thread")]
async fn actual_owner_matches_frozen_original_protocol_baseline() {
    let reference = crate::json(
        &std::fs::read_to_string(
            std::env::var_os("IVY_HIVE_MCP_HTTP_REFERENCE")
                .expect("Run with the frozen original MCP reference"),
        )
        .unwrap(),
    );
    let cases = reference.get("cases").unwrap().as_array().unwrap();
    let original = reference.get("wire").unwrap().as_array().unwrap();
    assert_eq!(cases.len(), 57);
    assert_eq!(original.len(), cases.len());
    let mut rows = vec![];
    for row in cases {
        rows.push(exchange(row.clone(), Arc::new(Controls::default()), None).await);
    }
    save(
        "integrated",
        &obj([
            ("cases", IvyJson::Array(cases.to_vec())),
            ("source", IvyJson::Array(original.to_vec())),
            ("native", IvyJson::Array(rows.clone())),
        ]),
    );
    for ((request, source), row) in cases.iter().zip(original).zip(&rows) {
        let name = request.get("name").unwrap().as_str().unwrap();
        assert_eq!(row.get("error"), Some(&IvyJson::Null), "{name}");
        // The source oracle enters handleMcp (or its outer non-POST guard)
        // directly. The prepared HTTP owner installs no-store first; legacy
        // SSE explicitly replaces it with the source protocol cache value.
        let mut headers = source.get("headers").unwrap().clone();
        if headers.get("cache-control").is_none() {
            headers.insert("cache-control", "no-store".into());
        }
        let expected = obj([
            ("status", source.get("status").unwrap().clone()),
            ("headers", headers),
            ("bodyBase64", source.get("bodyBase64").unwrap().clone()),
        ]);
        assert_eq!(row.get("wire"), Some(&expected), "{name}");
        assert_eq!(row.get("activeBeforeRelease"), Some(&IvyJson::Number(1.0)), "{name}");
        assert_eq!(row.get("activeAfterRelease"), Some(&IvyJson::Number(0.0)), "{name}");
        assert_eq!(row.get("availableAfterRelease"), Some(&IvyJson::Number(2.0)), "{name}");
        let post = request.get("method").is_none();
        assert_eq!(row.get("bodyConsumed"), Some(&IvyJson::Bool(post)), "{name}");
        let events = kinds(row);
        for stage in ["create", "handle", "close"] {
            assert_eq!(events.iter().filter(|v| *v == stage).count(), usize::from(post), "{name}: {stage}");
        }
        let worker: Vec<_> = row.get("events").unwrap().as_array().unwrap().iter()
            .filter(|v| v.get("kind") == Some(&"worker".into()))
            .map(|v| v.get("payload").unwrap().get("action").unwrap().as_str().unwrap())
            .collect();
        assert_eq!(worker.iter().filter(|v| *v == "authenticate").count(), 1, "{name}");
        assert_eq!(worker.iter().filter(|v| *v == "disconnect").count(), 1, "{name}");
        if !post {
            assert_eq!(worker, ["authenticate", "disconnect"], "{name}");
            assert_eq!(row.get("rpcId"), Some(&IvyJson::Null), "{name}");
            assert_eq!(events.iter().filter(|v| *v == "provider-send").count(), 0, "{name}");
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn actual_owner_matches_frozen_sdk_client_exchanges() {
    let reference = crate::json(
        &std::fs::read_to_string(
            std::env::var_os("IVY_HIVE_MCP_HTTP_REFERENCE")
                .expect("Run with the frozen original MCP reference"),
        )
        .unwrap(),
    );
    let clients = reference.get("clients").unwrap();
    let mut cases = vec![];
    let mut sources = vec![];
    for (mode, client, count) in [
        ("legacy", reference.get("client").unwrap(), 6),
        ("auto", clients.get("auto").unwrap(), 4),
        ("pin", clients.get("pin").unwrap(), 4),
    ] {
        assert_eq!(client.get("ok"), Some(&IvyJson::Bool(true)));
        let exchanges = client.get("exchanges").unwrap().as_array().unwrap();
        assert_eq!(exchanges.len(), count);
        for (index, source) in exchanges.iter().enumerate() {
            cases.push(obj([
                ("name", format!("{mode}-{index}").into()),
                ("method", source.get("method").unwrap().clone()),
                ("headers", source.get("headers").unwrap().clone()),
                ("body", source.get("body").cloned().unwrap_or(IvyJson::Null)),
            ]));
            sources.push(source.clone());
        }
    }
    let mut rows = vec![];
    for row in &cases {
        rows.push(exchange(row.clone(), Arc::new(Controls::default()), None).await);
    }
    save(
        "clients",
        &obj([
            ("cases", IvyJson::Array(cases.clone())),
            ("source", IvyJson::Array(sources.clone())),
            ("native", IvyJson::Array(rows.clone())),
        ]),
    );
    assert_eq!(rows.len(), 14);
    for ((request, source), row) in cases.iter().zip(&sources).zip(&rows) {
        let name = request.get("name").unwrap().as_str().unwrap();
        let wire = row.get("wire").unwrap();
        let headers = wire.get("headers").unwrap();
        assert_eq!(row.get("error"), Some(&IvyJson::Null), "{name}");
        assert_eq!(wire.get("status"), source.get("status"), "{name}");
        // Client tracing retained content type and exact bytes, not all reply
        // headers. Compare only the evidence that the source actually saved.
        assert_eq!(
            headers.get("content-type").unwrap_or(&IvyJson::Null),
            source.get("contentType").unwrap(),
            "{name}"
        );
        assert_eq!(
            wire.get("bodyBase64"),
            Some(&source.get("responseBase64").cloned().unwrap_or_else(|| "".into())),
            "{name}"
        );
        let post = request.get("method") == Some(&"POST".into());
        assert_eq!(row.get("bodyConsumed"), Some(&IvyJson::Bool(post)), "{name}");
        assert_eq!(row.get("activeBeforeRelease"), Some(&IvyJson::Number(1.0)), "{name}");
        assert_eq!(row.get("activeAfterRelease"), Some(&IvyJson::Number(0.0)), "{name}");
        assert_eq!(row.get("availableAfterRelease"), Some(&IvyJson::Number(2.0)), "{name}");
        let events = kinds(row);
        for stage in ["create", "handle", "close"] {
            assert_eq!(events.iter().filter(|v| *v == stage).count(), usize::from(post), "{name}: {stage}");
        }
        if !post {
            assert_eq!(headers.get("allow"), Some(&"POST".into()), "{name}");
            assert_eq!(row.get("rpcId"), Some(&IvyJson::Null), "{name}");
            assert_eq!(events.iter().filter(|v| *v == "provider-send").count(), 0, "{name}");
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn prepared_owner_factory_matches_original_wire_and_releases_leases() {
    let reference = baseline();
    let cases = reference.get("cases").unwrap().as_array().unwrap();
    let original = reference
        .get("source")
        .unwrap()
        .get("wire")
        .unwrap()
        .as_array()
        .unwrap();
    let mut rows = vec![];
    for row in cases {
        rows.push(exchange(row.clone(), Arc::new(Controls::default()), None).await);
    }
    save(
        "comparison",
        &obj([
            ("cases", IvyJson::Array(cases.to_vec())),
            ("source", reference.get("source").unwrap().clone()),
            ("native", IvyJson::Array(rows.clone())),
        ]),
    );
    for (row, source) in rows.iter().zip(original) {
        assert_eq!(
            row.get("error"),
            Some(&IvyJson::Null),
            "Owner error: {}",
            crate::encode(row)
        );
        // The original protocol oracle enters handleMcp directly. The actual
        // HTTP owner also installs its qualified common no-store header first;
        // protocol SSE replies explicitly replace that common cache value.
        let mut headers = source.get("headers").unwrap().clone();
        if headers.get("cache-control").is_none() {
            headers.insert("cache-control", "no-store".into());
        }
        let expected = obj([
            ("status", source.get("status").unwrap().clone()),
            ("headers", headers),
            ("bodyBase64", source.get("bodyBase64").unwrap().clone()),
        ]);
        assert_eq!(
            row.get("wire"),
            Some(&expected),
            "Prepared owner wire: {}",
            crate::encode(row)
        );
        assert_eq!(row.get("activeBeforeRelease"), Some(&IvyJson::Number(1.0)));
        assert_eq!(row.get("activeAfterRelease"), Some(&IvyJson::Number(0.0)));
        assert_eq!(
            row.get("availableAfterRelease"),
            Some(&IvyJson::Number(2.0))
        );
        let events = kinds(row);
        assert_eq!(events.iter().filter(|v| *v == "create").count(), 1);
        assert_eq!(events.iter().filter(|v| *v == "handle").count(), 1);
        assert_eq!(events.iter().filter(|v| *v == "close").count(), 1);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn actual_factory_failure_and_pending_close_keep_owner_precedence() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let reference = baseline();
            let cases = reference.get("cases").unwrap().as_array().unwrap();
            let mut rows = vec![];
            for (name, control, index, code, closes) in [
                (
                    "create-error",
                    Controls {
                        create_error: true,
                        ..Default::default()
                    },
                    8,
                    "create_failure",
                    0,
                ),
                (
                    "instructions-error",
                    Controls {
                        instruction_error: true,
                        ..Default::default()
                    },
                    0,
                    "instruction_failure",
                    0,
                ),
                (
                    "head-error",
                    Controls {
                        head_error: true,
                        ..Default::default()
                    },
                    8,
                    "head_failure",
                    1,
                ),
                (
                    "end-error",
                    Controls {
                        end_error: true,
                        ..Default::default()
                    },
                    8,
                    "end_failure",
                    1,
                ),
                (
                    "close-over-head",
                    Controls {
                        head_error: true,
                        close_error: true,
                        ..Default::default()
                    },
                    8,
                    "close_failure",
                    1,
                ),
                (
                    "close-after-response",
                    Controls {
                        close_error: true,
                        ..Default::default()
                    },
                    8,
                    "close_failure",
                    1,
                ),
            ] {
                let mut row = cases[index].clone();
                row.insert("name", name.into());
                let result = exchange(row, Arc::new(control), None).await;
                save(name, &result);
                assert_eq!(
                    result
                        .get("error")
                        .unwrap()
                        .get("data")
                        .unwrap()
                        .get("code"),
                    Some(&code.into())
                );
                assert_eq!(
                    kinds(&result).iter().filter(|v| *v == "close").count(),
                    closes
                );
                assert_eq!(
                    result.get("activeAfterRelease"),
                    Some(&IvyJson::Number(0.0))
                );
                rows.push(result);
            }
            for phase in ["catalogue", "close"] {
                let controls = Arc::new(Controls::default());
                let (release, gate) = oneshot::channel();
                if phase == "catalogue" {
                    *controls.catalogue_gate.lock().unwrap() = Some(gate);
                } else {
                    *controls.close_gate.lock().unwrap() = Some(gate);
                }
                let mut row = cases[8].clone();
                row.insert("name", format!("pending-{phase}").into());
                let (send, ready) = oneshot::channel();
                let work = tokio::task::spawn_local(exchange(row, controls.clone(), Some(send)));
                let (connection, budget) = ready.await.unwrap();
                while !if phase == "catalogue" {
                    controls.catalogue_started.load(AtomicOrdering::SeqCst)
                } else {
                    controls.close_started.load(AtomicOrdering::SeqCst)
                } {
                    tokio::task::yield_now().await;
                }
                assert_eq!(connection.active_requests(), 1);
                assert_eq!(available(&budget), 1);
                let held = obj([
                    (
                        "active",
                        IvyJson::Number(connection.active_requests() as f64),
                    ),
                    ("available", IvyJson::Number(available(&budget) as f64)),
                    (
                        "closeStarted",
                        controls.close_started.load(AtomicOrdering::SeqCst).into(),
                    ),
                ]);
                connection.close();
                if phase == "catalogue" {
                    controls.destroyed.store(true, AtomicOrdering::SeqCst);
                }
                let after_socket_close = obj([
                    (
                        "active",
                        IvyJson::Number(connection.active_requests() as f64),
                    ),
                    ("available", IvyJson::Number(available(&budget) as f64)),
                    (
                        "responseDestroyed",
                        controls.destroyed.load(AtomicOrdering::SeqCst).into(),
                    ),
                ]);
                release.send(()).unwrap();
                let mut result = work.await.unwrap();
                result.insert("held", held);
                result.insert("afterSocketClose", after_socket_close);
                assert_eq!(result.get("error"), Some(&IvyJson::Null));
                assert_eq!(connection.active_requests(), 0);
                assert_eq!(available(&budget), 2);
                assert_eq!(kinds(&result).iter().filter(|v| *v == "close").count(), 1);
                if phase == "catalogue" {
                    assert!(!kinds(&result).contains(&"response-head".to_owned()));
                }
                rows.push(result);
            }
            save("lifetime", &IvyJson::Array(rows));
        })
        .await;
}

fn error_cases() -> Vec<IvyJson> {
    let reference = crate::mcp_inbound::reference();
    let cached = reference.get("cases").unwrap().as_array().unwrap();
    let mut specifications = vec![
        crate::json(r#"{"code":"unauthenticated","message":"Synthetic storage failure."}"#),
        crate::json(
            r#"{"code":"forbidden","message":"Forbidden fixture.","outcome":"unknown","details":null}"#,
        ),
        crate::json(
            r#"{"code":"invalid_arguments","message":"Invalid fixture arguments.","outcome":"completed","details":{"10":true,"2":false,"nested":[null,1.25]}}"#,
        ),
        crate::json(
            r#"{"code":"invalid_frame","message":"Frame fixture.","outcome":["not_executed"]}"#,
        ),
        crate::json(r#"{"code":"storage_unavailable","message":"placeholder"}"#),
        crate::json(
            r#"{"code":"provider_contract_error","message":"Unicode é😀\ud800","outcome":0,"details":{"value":"\ud800"}}"#,
        ),
        crate::json(
            r#"{"code":"__proto__","message":"Prototype-named fixture.","outcome":"not_executed"}"#,
        ),
        crate::json(
            r#"{"code":"constructor","message":"Constructor-named fixture.","outcome":null}"#,
        ),
        crate::json(
            r#"{"code":"unknown_fixture","message":"Unknown fixture.","outcome":{"status":"unknown"},"details":[true,{}]}"#,
        ),
        crate::json(r#"{"code":"\ud800","message":"Unpaired code fixture."}"#),
    ];
    specifications[4].insert("message", format!("{}😀tail", "x".repeat(2047)).into());
    let templates = [
        "claimless-tools/list",
        "modern-tools/list",
        "legacy-tool-success",
        "modern-valid-tool",
    ];
    let mut cases = vec![];
    for template in templates {
        let base = cached
            .iter()
            .find(|row| row.get("name") == Some(&template.into()))
            .unwrap();
        for (index, spec) in specifications.iter().enumerate() {
            let mut row = base.clone();
            row.insert("name", format!("{template}-storage-{index}").into());
            let mut spec = spec.clone();
            spec.insert("action", "mcp.catalog".into());
            row.insert("storageError", spec);
            cases.push(row);
        }
    }
    for template in &templates[..2] {
        for action in ["mcp.cursor.create", "mcp.cursor.read"] {
            let mut row = cached
                .iter()
                .find(|row| row.get("name") == Some(&(*template).into()))
                .unwrap()
                .clone();
            row.insert("name", format!("{template}-{action}").into());
            let mut spec = specifications[1].clone();
            spec.insert("action", action.into());
            row.insert("storageError", spec);
            if action == "mcp.cursor.read" {
                crate::mcp_inbound::set(&mut row, &["body", "params", "cursor"], "bad".into());
            }
            cases.push(row);
        }
    }
    for template in &templates[2..] {
        let mut row = cached
            .iter()
            .find(|row| row.get("name") == Some(&(*template).into()))
            .unwrap()
            .clone();
        row.insert("name", format!("development-{template}-auth").into());
        row.insert("path", "/mcp-dev".into());
        let mut spec = specifications[0].clone();
        spec.insert("action", "mcp.catalog".into());
        row.insert("storageError", spec);
        cases.push(row);
    }
    cases
}
#[tokio::test(flavor = "current_thread")]
async fn actual_owner_storage_errors_match_original_protocol_and_release_leases() {
    let cases = error_cases();
    assert_eq!(cases.len(), 46);
    let source = crate::mcp_protocol::oracle(&obj([
        ("mode", "baseline".into()),
        ("client", false.into()),
        ("recordEffects", true.into()),
        ("recordStorageEffects", true.into()),
        ("cases", IvyJson::Array(cases.clone())),
    ]));
    save("errors-source", &source);
    let mut rows = vec![];
    for row in &cases {
        rows.push(
            exchange(
                row.clone(),
                Arc::new(Controls {
                    storage_failure: row.get("storageError").cloned(),
                    ..Default::default()
                }),
                None,
            )
            .await,
        );
    }
    save(
        "errors-comparison",
        &obj([
            ("cases", IvyJson::Array(cases)),
            ("source", source.clone()),
            ("native", IvyJson::Array(rows.clone())),
        ]),
    );
    for (actual, original) in rows
        .iter()
        .zip(source.get("wire").unwrap().as_array().unwrap())
    {
        let mut headers = original.get("headers").unwrap().clone();
        if headers.get("cache-control").is_none() {
            headers.insert("cache-control", "no-store".into());
        }
        assert_eq!(
            actual.get("wire"),
            Some(&obj([
                ("status", original.get("status").unwrap().clone()),
                ("headers", headers),
                ("bodyBase64", original.get("bodyBase64").unwrap().clone()),
            ])),
            "{}",
            crate::encode(actual.get("name").unwrap())
        );
        assert_eq!(actual.get("error"), Some(&IvyJson::Null));
        assert_eq!(
            actual.get("activeBeforeRelease"),
            Some(&IvyJson::Number(1.0))
        );
        assert_eq!(
            actual.get("activeAfterRelease"),
            Some(&IvyJson::Number(0.0))
        );
        assert_eq!(
            actual.get("availableAfterRelease"),
            Some(&IvyJson::Number(2.0))
        );
        assert_eq!(
            kinds(actual).iter().filter(|kind| *kind == "close").count(),
            1
        );
        assert!(!kinds(actual).contains(&"provider-send".to_owned()));
    }
}

fn provider_cases() -> Vec<IvyJson> {
    let base = crate::json(
        r#"{"jsonrpc":"2.0","error":{"code":-32000,"message":"Provider fixture failed.","data":{"code":"remote_fixture","outcome":"unknown"}}}"#,
    );
    let mut specifications = vec![("base".to_owned(), "error", base.clone())];
    let mut max_code = JsString::from("c".repeat(127));
    max_code.0.push(0xd800);
    let mut max_message = JsString::from("x".repeat(2047));
    max_message.0.push(0xd800);
    let valid: &[(&str, &[&str], IvyJson)] = &[
        (
            "not-executed",
            &["error", "data", "outcome"],
            "not_executed".into(),
        ),
        (
            "completed",
            &["error", "data", "outcome"],
            "completed".into(),
        ),
        (
            "array-unknown",
            &["error", "data", "outcome"],
            crate::json(r#"["unknown"]"#),
        ),
        (
            "nested-array-completed",
            &["error", "data", "outcome"],
            crate::json(r#"[[["completed"]]]"#),
        ),
        (
            "array-not-executed",
            &["error", "data", "outcome"],
            crate::json(r#"["not_executed"]"#),
        ),
        ("null-details", &["error", "data", "details"], IvyJson::Null),
        (
            "json-details",
            &["error", "data", "details"],
            crate::json(r#"{"10":true,"2":false,"unicode":"é😀\ud800","nested":[null,{},1.25]}"#),
        ),
        (
            "unauthenticated",
            &["error", "data", "code"],
            "unauthenticated".into(),
        ),
        ("forbidden", &["error", "data", "code"], "forbidden".into()),
        (
            "invalid-arguments",
            &["error", "data", "code"],
            "invalid_arguments".into(),
        ),
        (
            "invalid-frame",
            &["error", "data", "code"],
            "invalid_frame".into(),
        ),
        (
            "prototype-code",
            &["error", "data", "code"],
            "__proto__".into(),
        ),
        (
            "constructor-code",
            &["error", "data", "code"],
            "constructor".into(),
        ),
        (
            "unpaired-code",
            &["error", "data", "code"],
            crate::json(r#""\ud800""#),
        ),
        (
            "max-code",
            &["error", "data", "code"],
            IvyJson::String(max_code),
        ),
        (
            "max-message",
            &["error", "message"],
            IvyJson::String(max_message),
        ),
        (
            "minimum-wire-code",
            &["error", "code"],
            IvyJson::Number(-9_007_199_254_740_991.0),
        ),
        (
            "maximum-wire-code",
            &["error", "code"],
            IvyJson::Number(9_007_199_254_740_991.0),
        ),
    ];
    let invalid: &[(&str, &[&str], IvyJson)] = &[
        ("wrong-jsonrpc", &["jsonrpc"], "1.0".into()),
        ("extra-frame-field", &["extra"], true.into()),
        ("result-and-error", &["result"], IvyJson::Null),
        ("null-error", &["error"], IvyJson::Null),
        ("array-error", &["error"], crate::json("[]")),
        ("string-error", &["error"], "failed".into()),
        ("number-error", &["error"], IvyJson::Number(1.0)),
        ("empty-message", &["error", "message"], "".into()),
        (
            "long-message",
            &["error", "message"],
            "x".repeat(2049).into(),
        ),
        ("null-message", &["error", "message"], IvyJson::Null),
        (
            "number-message",
            &["error", "message"],
            IvyJson::Number(1.0),
        ),
        (
            "fraction-wire-code",
            &["error", "code"],
            IvyJson::Number(1.25),
        ),
        (
            "unsafe-wire-code",
            &["error", "code"],
            IvyJson::Number(9_007_199_254_740_992.0),
        ),
        ("string-wire-code", &["error", "code"], "-32000".into()),
        ("null-wire-code", &["error", "code"], IvyJson::Null),
        ("null-data", &["error", "data"], IvyJson::Null),
        ("array-data", &["error", "data"], crate::json("[]")),
        ("extra-error-field", &["error", "extra"], true.into()),
        ("empty-data-code", &["error", "data", "code"], "".into()),
        (
            "long-data-code",
            &["error", "data", "code"],
            "c".repeat(129).into(),
        ),
        (
            "number-data-code",
            &["error", "data", "code"],
            IvyJson::Number(1.0),
        ),
        ("extra-data-field", &["error", "data", "extra"], true.into()),
        ("null-outcome", &["error", "data", "outcome"], IvyJson::Null),
        (
            "boolean-outcome",
            &["error", "data", "outcome"],
            true.into(),
        ),
        (
            "number-outcome",
            &["error", "data", "outcome"],
            IvyJson::Number(0.0),
        ),
        (
            "object-outcome",
            &["error", "data", "outcome"],
            crate::json("{}"),
        ),
        (
            "empty-array-outcome",
            &["error", "data", "outcome"],
            crate::json("[]"),
        ),
        (
            "multiple-outcomes",
            &["error", "data", "outcome"],
            crate::json(r#"["unknown","completed"]"#),
        ),
        (
            "shadowed-string-conversion",
            &["error", "data", "outcome"],
            crate::json(r#"{"toString":"completed"}"#),
        ),
    ];
    for (expected, mutations) in [("error", valid), ("invalid", invalid)] {
        for (name, path, value) in mutations {
            let mut spec = base.clone();
            crate::mcp_inbound::set(&mut spec, path, value.clone());
            specifications.push(((*name).to_owned(), expected, spec));
        }
    }
    for path in [
        &["jsonrpc"][..],
        &["error"],
        &["error", "code"],
        &["error", "message"],
        &["error", "data"],
        &["error", "data", "code"],
        &["error", "data", "outcome"],
    ] {
        fn remove(value: &mut IvyJson, path: &[&str]) {
            if path.len() == 1 {
                value.remove(path[0]);
            } else {
                let mut child = value.get(path[0]).unwrap().clone();
                remove(&mut child, &path[1..]);
                value.insert(path[0], child);
            }
        }
        let mut spec = base.clone();
        remove(&mut spec, path);
        specifications.push((format!("missing-{}", path.join("-")), "invalid", spec));
    }
    for (name, value) in [
        ("null-result", IvyJson::Null),
        ("text-result", "answer".into()),
        (
            "json-result",
            crate::json(r#"{"nested":[null,"é😀\ud800"],"10":true,"2":false}"#),
        ),
    ] {
        specifications.push((
            name.to_owned(),
            "result",
            obj([("jsonrpc", "2.0".into()), ("result", value)]),
        ));
    }
    assert_eq!(specifications.len(), 58);
    let reference = crate::mcp_inbound::reference();
    let mut cases = vec![];
    for template in ["legacy-tool-success", "modern-valid-tool"] {
        let row = reference
            .get("cases")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row.get("name") == Some(&template.into()))
            .unwrap();
        for (name, expected, spec) in &specifications {
            let mut row = row.clone();
            row.insert("name", format!("{template}-provider-{name}").into());
            row.insert("providerReply", spec.clone());
            row.insert("providerExpected", (*expected).into());
            cases.push(row);
        }
    }
    cases
}

#[tokio::test(flavor = "current_thread")]
async fn actual_owner_provider_responses_match_original_normalization_and_release_leases() {
    let cases = provider_cases();
    let source = if let Some(path) = std::env::var_os("IVY_HIVE_MCP_PROVIDER_REFERENCE") {
        crate::json(&std::fs::read_to_string(path).unwrap())
    } else {
        crate::mcp_protocol::oracle(&obj([
            ("mode", "baseline".into()),
            ("client", false.into()),
            ("recordEffects", true.into()),
            ("recordStorageEffects", true.into()),
            ("cases", IvyJson::Array(cases.clone())),
        ]))
    };
    assert_eq!(
        crate::encode(source.get("cases").unwrap()),
        crate::encode(&IvyJson::Array(cases.clone())),
        "A reused original observation must cover the exact same corpus"
    );
    save("provider-source", &source);
    let mut rows = vec![];
    for row in &cases {
        rows.push(exchange(row.clone(), Arc::new(Controls::default()), None).await);
    }
    save(
        "provider-comparison",
        &obj([
            ("cases", IvyJson::Array(cases)),
            ("source", source.clone()),
            ("native", IvyJson::Array(rows.clone())),
        ]),
    );
    for (index, actual) in rows.iter().enumerate() {
        let original = &source.get("wire").unwrap().as_array().unwrap()[index];
        let provider = &source.get("providerResults").unwrap().as_array().unwrap()[index];
        let mut headers = original.get("headers").unwrap().clone();
        if headers.get("cache-control").is_none() {
            headers.insert("cache-control", "no-store".into());
        }
        assert_eq!(
            actual.get("wire"),
            Some(&obj([
                ("status", original.get("status").unwrap().clone()),
                ("headers", headers),
                ("bodyBase64", original.get("bodyBase64").unwrap().clone()),
            ])),
            "{}",
            crate::encode(actual.get("name").unwrap())
        );
        assert_eq!(actual.get("error"), Some(&IvyJson::Null));
        assert_eq!(
            actual.get("activeBeforeRelease"),
            Some(&IvyJson::Number(1.0))
        );
        assert_eq!(
            actual.get("activeAfterRelease"),
            Some(&IvyJson::Number(0.0))
        );
        assert_eq!(
            actual.get("availableAfterRelease"),
            Some(&IvyJson::Number(2.0))
        );
        assert_eq!(provider.get("pending"), Some(&IvyJson::Number(0.0)));
        let events = actual.get("events").unwrap().as_array().unwrap();
        let inputs = events
            .iter()
            .filter(|e| e.get("kind") == Some(&"provider-error-input".into()))
            .map(|e| {
                let mut e = e.clone();
                e.remove("kind");
                e
            })
            .collect::<Vec<_>>();
        // JS enumerates integer-like object keys numerically, including inside
        // details. Compare the exact JS-compatible serialization, not the
        // fixture's internal member insertion order.
        assert_eq!(
            crate::encode(provider.get("inputs").unwrap()),
            crate::encode(&IvyJson::Array(inputs))
        );
        let phases = kinds(actual);
        for kind in [
            "create",
            "handle",
            "provider-send",
            "provider-reply",
            "response-head",
            "response-end",
            "close",
        ] {
            assert_eq!(phases.iter().filter(|v| *v == kind).count(), 1);
        }
        assert!(
            phases.iter().position(|v| v == "provider-reply")
                < phases.iter().position(|v| v == "response-head")
        );
        assert!(
            phases.iter().position(|v| v == "response-end")
                < phases.iter().position(|v| v == "close")
        );
    }
}

fn observation_cases() -> Vec<IvyJson> {
    let reference = crate::mcp_inbound::reference();
    let mut cases = vec![];
    for template in ["legacy-tool-success", "modern-valid-tool"] {
        let original = reference.get("cases").unwrap().as_array().unwrap().iter()
            .find(|row| row.get("name") == Some(&template.into())).unwrap();
        let mut add = |label: &str, mut row: IvyJson, mode: &str, clock: IvyJson| {
            row.insert("name", format!("{template}-observe-{label}").into());
            row.insert("observation", obj([("mode", mode.into()), ("clock", clock)]));
            cases.push(row);
        };
        for mode in ["ok", "absent", "ivy-failure", "protocol-failure"] {
            for outcome in ["success", "invalid-arguments", "missing-tool", "catalogue-error", "provider-error"] {
                let mut row = original.clone();
                match outcome {
                    "invalid-arguments" => crate::mcp_inbound::set(&mut row, &["body", "params", "arguments", "input", "value"], number(3)),
                    "missing-tool" => {
                        crate::mcp_inbound::set(&mut row, &["body", "params", "name"], "missing".into());
                        if template == "modern-valid-tool" { crate::mcp_inbound::set(&mut row, &["headers", "mcp-name"], "missing".into()); }
                    }
                    "catalogue-error" => { row.insert("storageError", crate::json(r#"{"action":"mcp.catalog","code":"storage_unavailable","message":"Observation catalog refused.","outcome":["unknown"],"details":{"posted":true}}"#)); }
                    "provider-error" => { row.insert("providerReply", crate::json(r#"{"jsonrpc":"2.0","error":{"code":-32000,"message":"Observation provider refused.","data":{"code":"unauthenticated","outcome":["not_executed"],"details":{"posted":true}}}}"#)); }
                    "success" => {}
                    _ => unreachable!(),
                }
                add(&format!("{mode}-{outcome}"), row, mode, crate::json("[1000,1250]"));
            }
        }
        let mut unknown = original.clone();
        crate::mcp_inbound::set(&mut unknown, &["body", "params", "name"], "missing".into());
        if template == "modern-valid-tool" { crate::mcp_inbound::set(&mut unknown, &["headers", "mcp-name"], "missing".into()); }
        for (label, arguments) in [
            ("empty-first", crate::json(r#"{"operationId":"","mutationId":"unused","input":{"operationId":"inner"}}"#)),
            ("null-first", crate::json(r#"{"operationId":null,"mutationId":"mutation","input":{"operationId":"inner"}}"#)),
            ("numeric-first", crate::json(r#"{"operationId":7,"mutationId":"unused","input":{"operationId":"inner"}}"#)),
            ("false-first", crate::json(r#"{"operationId":false,"mutationId":"unused","input":{"operationId":"inner"}}"#)),
            ("inner-last", crate::json(r#"{"operationId":null,"mutationId":null,"input":{"operationId":"inner"}}"#)),
            ("inner-null-mutation", crate::json(r#"{"mutationId":null,"input":{"operationId":"nested"}}"#)),
            ("outer-first", crate::json(r#"{"operationId":"outer","mutationId":"unused","input":{"operationId":"inner"}}"#)),
        ] {
            let mut row = unknown.clone();
            crate::mcp_inbound::set(&mut row, &["body", "params", "arguments"], arguments);
            add(label, row, "ok", crate::json("[1000,1250]"));
        }
        let mut long = unknown.clone();
        let request: JsString = format!("{}😀tail", "r".repeat(127)).into();
        let tool: JsString = if template == "modern-valid-tool" { format!("{}tail", "t".repeat(129)).into() } else { format!("{}😀tail", "t".repeat(127)).into() };
        let operation: JsString = format!("{}😀tail", "o".repeat(511)).into();
        crate::mcp_inbound::set(&mut long, &["body", "id"], IvyJson::String(request));
        crate::mcp_inbound::set(&mut long, &["body", "params", "name"], IvyJson::String(tool.clone()));
        if template == "modern-valid-tool" { crate::mcp_inbound::set(&mut long, &["headers", "mcp-name"], IvyJson::String(tool)); }
        crate::mcp_inbound::set(&mut long, &["body", "params", "arguments"], obj([("operationId", IvyJson::String(operation))]));
        add("utf16-cut", long, "ok", crate::json("[1000,1250]"));
        for (index, id) in [number(0), IvyJson::Number(-1.0), IvyJson::Number(9_007_199_254_740_991.0), IvyJson::Number(9_007_199_254_740_992.0), IvyJson::Number(1.25), "".into(), IvyJson::String(JsString(vec![0xd800])), "short".into(), IvyJson::Null, true.into()].into_iter().enumerate() {
            let mut row = unknown.clone();crate::mcp_inbound::set(&mut row, &["body", "id"], id);
            add(&format!("request-id-{index}"), row, "ok", crate::json("[1000,1250]"));
        }
        for (index, clock) in ["[1000,900]", "[0,86400001]", "[1.25,3.75]", "[0,\"Infinity\"]", "[\"Infinity\",0]", "[0,\"NaN\"]"].into_iter().enumerate() {
            add(&format!("clock-{index}"), unknown.clone(), "ok", crate::json(clock));
        }
    }
    assert_eq!(cases.len(), 88);
    cases
}

fn normalized_observation(value: &IvyJson, ids: &[IvyJson]) -> IvyJson {
    let mut value = value.clone();
    let normalized = value.get("internalRequestIds").unwrap().as_array().unwrap().iter().map(|id| {
        let index = ids.iter().position(|candidate| candidate == id).expect("Observation must reference an actual allocated/invoked ID");
        format!("internal-{index}").into()
    }).collect();
    value.insert("internalRequestIds", IvyJson::Array(normalized));
    value
}

#[tokio::test(flavor = "current_thread")]
async fn actual_owner_observations_match_original_finally_identity_and_clock_boundaries() {
    let cases = observation_cases();
    let source = if let Some(path) = std::env::var_os("IVY_HIVE_MCP_OBSERVER_REFERENCE") {
        crate::json(&std::fs::read_to_string(path).unwrap())
    } else {
        crate::mcp_protocol::oracle(&obj([("mode", "baseline".into()), ("client", false.into()), ("recordEffects", true.into()), ("recordStorageEffects", true.into()), ("recordObservations", true.into()), ("cases", IvyJson::Array(cases.clone()))]))
    };
    assert_eq!(crate::encode(source.get("cases").unwrap()), crate::encode(&IvyJson::Array(cases.clone())), "Reused original observations require the same complete corpus");
    save("observer-source", &source);
    let mut rows = vec![];
    for row in &cases {
        let controls = Controls { observation: row.get("observation").cloned(), storage_failure: row.get("storageError").cloned(), ..Default::default() };
        rows.push(exchange(row.clone(), Arc::new(controls), None).await);
    }
    save("observer-comparison", &obj([("cases", IvyJson::Array(cases)), ("source", source.clone()), ("native", IvyJson::Array(rows.clone()))]));
    for (index, actual) in rows.iter().enumerate() {
        let original = &source.get("wire").unwrap().as_array().unwrap()[index];
        let name = actual.get("name").unwrap();
        let mut headers = original.get("headers").unwrap().clone();
        if headers.get("cache-control").is_none() { headers.insert("cache-control", "no-store".into()); }
        assert_eq!(actual.get("wire"), Some(&obj([("status", original.get("status").unwrap().clone()), ("headers", headers), ("bodyBase64", original.get("bodyBase64").unwrap().clone())])), "Wire {}", crate::encode(name));
        assert_eq!(actual.get("error"), Some(&IvyJson::Null));
        assert_eq!(actual.get("activeBeforeRelease"), Some(&number(1)));assert_eq!(actual.get("activeAfterRelease"), Some(&number(0)));assert_eq!(actual.get("availableAfterRelease"), Some(&number(2)));
        let events = actual.get("events").unwrap().as_array().unwrap();
        let original_ids = source.get("effects").unwrap().as_array().unwrap().iter().filter(|event| event.get("name") == Some(name) && event.get("kind") == Some(&"invoke".into())).map(|event| event.get("frame").unwrap().get("id").unwrap().clone()).collect::<Vec<_>>();
        let native_ids = events.iter().filter(|event| event.get("kind") == Some(&"id".into())).map(|event| event.get("id").unwrap().clone()).collect::<Vec<_>>();
        assert!(original_ids.len() <= 1);assert_eq!(native_ids.len(), original_ids.len() * 2);
        let source_values = source.get("callObservations").unwrap().as_array().unwrap().iter().filter(|value| value.get("name") == Some(name)).map(|value| normalized_observation(value.get("value").unwrap(), &original_ids)).collect::<Vec<_>>();
        let native_values = events.iter().filter(|value| value.get("kind") == Some(&"observe".into())).map(|value| normalized_observation(value.get("value").unwrap(), &native_ids)).collect::<Vec<_>>();
        assert_eq!(crate::encode(&IvyJson::Array(native_values)), crate::encode(&IvyJson::Array(source_values.clone())), "Observation {}", crate::encode(name));
        assert!(source_values.len() <= 1);
        let phases = kinds(actual);
        for phase in ["create", "handle", "response-head", "response-end", "close"] { assert_eq!(phases.iter().filter(|kind| *kind == phase).count(), 1, "{}", crate::encode(name)); }
        if !source_values.is_empty() { assert!(phases.iter().position(|value| value == "observe") < phases.iter().position(|value| value == "response-head")); }
        assert!(phases.iter().position(|value| value == "response-end") < phases.iter().position(|value| value == "close"));
    }
}
