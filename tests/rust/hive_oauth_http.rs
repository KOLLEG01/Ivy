use super::*;
use ivy_hive::{
    http_auth::{
        Authentication, Connection, Headers as AuthHeaders, Options as AuthOptions,
        Request as AuthRequest,
    },
    oauth::{Dependencies, Entropy, FamilyStore, HiveOAuth, Redirects},
    oauth_http::{Handler as OAuthHandler, Headers as OAuthHeaders},
};
use std::sync::atomic::{AtomicU64, Ordering};

struct Unused;
impl Worker for Unused {
    fn request(&self, _: IvyJson) -> RequestFuture {
        panic!("Unexpected worker")
    }
}
impl FamilyStore for Unused {
    fn create(&self, _: IvyJson, _: Vec<JsString>) -> RequestFuture {
        panic!("Unexpected family create")
    }
    fn rotate(&self, _: IvyJson) -> RequestFuture {
        panic!("Unexpected family rotation")
    }
}
impl Redirects for Unused {
    fn set_parameters(&self, _: &JsString, _: &[(JsString, JsString)]) -> IvyResult<JsString> {
        panic!("Unexpected redirect")
    }
}
impl Entropy for Unused {
    fn bytes(&self, _: usize) -> IvyResult<Vec<u8>> {
        panic!("Unexpected entropy")
    }
}
fn headers(input: &Value) -> AuthHeaders {
    let string = |key| input["headers"][key].as_str().map(JsString::from);
    AuthHeaders {
        authorization: string("authorization"),
        cookie: string("cookie"),
        origin: input["headers"]
            .get("origin")
            .map(|v| IvyJson::from_serde(v).unwrap()),
        forwarded_client_ip: input["headers"]
            .get("x-ivy-client-ip")
            .map(|v| IvyJson::from_serde(v).unwrap()),
    }
}
fn probe(trace: &Trace, connection: &Connection, headers: &AuthHeaders, id: &Value, phase: &str) {
    let result = match connection.admit_authentication(headers) {
        Ok(mut lease) => {
            lease.release();
            json!({"kind":"auth-probe","id":id,"phase":phase,"value":null})
        }
        Err(error) => {
            json!({"kind":"auth-probe","id":id,"phase":phase,"error":carrier(&error.to_wire())})
        }
    };
    trace.lock().unwrap().push(result);
}
struct ProbedReply<'a> {
    inner: Reply,
    connection: &'a Connection,
    headers: &'a AuthHeaders,
}
impl ProbedReply<'_> {
    fn probe(&self, phase: &str) {
        if self.inner.spec["probeAt"] == phase {
            probe(
                &self.inner.trace,
                self.connection,
                self.headers,
                &self.inner.spec["id"],
                phase,
            );
        }
    }
}
impl Response for ProbedReply<'_> {
    fn writable_ended(&self) -> bool {
        self.inner.writable_ended()
    }
    fn destroyed(&self) -> bool {
        self.inner.destroyed()
    }
    fn headers_sent(&self) -> bool {
        self.inner.headers_sent()
    }
    fn write_head(&mut self, status: u16, headers: IvyJson) -> IvyResult<()> {
        let result = self.inner.write_head(status, headers);
        self.probe("head");
        result
    }
    fn end(&mut self, text: String) -> IvyResult<()> {
        let result = self.inner.end(text);
        self.probe("end");
        result
    }
    fn destroy(&mut self) -> IvyResult<()> {
        let result = self.inner.destroy();
        self.probe("destroy");
        result
    }
}
struct Owner {
    trace: Trace,
    buffers: BodyBuffers,
    gates: Gates,
    budget: RequestBudget,
    connection: Arc<Connection>,
    oauth: HiveOAuth,
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
        let auth_headers = headers(&input);
        let mut reply = ProbedReply {
            inner: Reply {
                spec: input.clone(),
                trace: self.trace.clone(),
                ended: input["ended"] == true,
                destroyed: input["destroyed"] == true,
                headers_sent: input["headersSent"] == true,
            },
            connection: &self.connection,
            headers: &auth_headers,
        };
        let header = |key| {
            input["headers"]
                .get(key)
                .map(|v| IvyJson::from_serde(v).unwrap())
        };
        let content_type = header("content-type");
        let content_length = header("content-length");
        let method = input
            .get("method")
            .map(|v| v.as_str().map(JsString::from))
            .unwrap_or_else(|| Some("POST".into()));
        let result = OAuthHandler {
            connection: &self.connection,
            oauth: &self.oauth,
            buffers: &self.buffers,
        }
        .try_handle(
            method.as_ref(),
            &input["pathname"].as_str().unwrap().into(),
            OAuthHeaders {
                authentication: &auth_headers,
                content_type: content_type.as_ref(),
                content_length: content_length.as_ref(),
            },
            &mut stream,
            Some(&mut lease),
            &mut reply,
        )
        .await;
        self.trace.lock().unwrap().push(match result {
            Ok(matched) => json!({"kind":"result","id":input["id"],"value":matched}),
            Err(error) => {
                json!({"kind":"result","id":input["id"],"error":carrier(&error.to_wire())})
            }
        });
        for amount in input["probes"].as_array().into_iter().flatten() {
            self.trace.lock().unwrap().push(match lease.grow(amount.as_f64().unwrap()) {
                Ok(())=>json!({"kind":"probe","id":input["id"],"amount":amount,"value":null}),
                Err(error)=>json!({"kind":"probe","id":input["id"],"amount":amount,"error":carrier(&error.to_wire())}),
            });
        }
        lease.release();
        self.trace.lock().unwrap().push(
            json!({"kind":"released","id":input["id"],"buffered":self.buffers.buffered_bytes()}),
        );
    }
}
async fn native(input: &Value) -> Value {
    let trace: Trace = Arc::default();
    let clock = Arc::new(AtomicU64::new(100000.0f64.to_bits()));
    let reading = clock.clone();
    let read_clock: Arc<dyn Fn() -> f64 + Send + Sync> =
        Arc::new(move || f64::from_bits(reading.load(Ordering::Relaxed)));
    let base = input["basePath"].as_str().unwrap_or("/ivy");
    let oauth = HiveOAuth::new(
        &format!("https://example.test{base}/").into(),
        vec![],
        Dependencies {
            families: Arc::new(Unused),
            redirects: Arc::new(Unused),
            entropy: Arc::new(Unused),
            clock: read_clock.clone(),
        },
    )
    .unwrap();
    let authentication = Authentication::new(
        AuthOptions {
            canonical_origin: "https://example.test".into(),
            base_path: base.into(),
            trusted_proxy_addresses: input["trusted"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| v.as_str().unwrap().into())
                .collect(),
        },
        Arc::new(Unused),
        Arc::new(oauth.clone()),
        read_clock,
    )
    .unwrap();
    let number = |key: &str, fallback| input["budget"][key].as_f64().unwrap_or(fallback);
    let owner = Rc::new(Owner {
        trace: trace.clone(),
        buffers: BodyBuffers::default(),
        gates: Arc::default(),
        budget: RequestBudget::new(RequestBudgetOptions {
            maximum: number("maximum", 2048.0),
            maximum_bytes: number("maximumBytes", 128.0 * 1024.0 * 1024.0),
            per_consumer: number("perConsumer", 128.0),
            per_consumer_bytes: number("perConsumerBytes", 64.0 * 1024.0 * 1024.0),
            name: "request".into(),
        }),
        connection: Arc::new(
            authentication.connection(Some(
                input["remoteAddress"]
                    .as_str()
                    .unwrap_or("192.0.2.1")
                    .into(),
            )),
        ),
        oauth,
    });
    let mut running = HashMap::new();
    for step in input["steps"].as_array().unwrap() {
        match step["op"].as_str().unwrap() {
            "run"=>{let held=owner.clone();let spec=step.clone();assert!(running.insert(step["id"].as_str().unwrap().to_owned(),tokio::task::spawn_local(async move{held.run(spec).await})).is_none());},
            "join"=>running.remove(step["id"].as_str().unwrap()).unwrap().await.unwrap(),
            "release"=>owner.gates.lock().unwrap().remove(step["gate"].as_str().unwrap()).unwrap().send(()).unwrap(),
            "probe"=>probe(&trace,&owner.connection,&headers(step),&step["id"],"explicit"),
            "clock"=>clock.store(step["now"].as_f64().unwrap().to_bits(),Ordering::Relaxed),
            "mark"=>trace.lock().unwrap().push(json!({"kind":"mark","name":step["name"],"buffered":owner.buffers.buffered_bytes()})),
            "auth"=>{let result=owner.connection.authenticate(AuthRequest{method:Some("GET".into()),headers:AuthHeaders{authorization:Some("Bearer credential".into()),..Default::default()},transport:"http".into(),resource:None}).await;trace.lock().unwrap().push(match result{Ok(context)=>json!({"kind":"auth","id":step["id"],"encoded":stringify(&context,usize::MAX).unwrap()}),Err(error)=>json!({"kind":"auth","id":step["id"],"error":carrier(&error.to_wire())})});},
            _=>panic!("Unknown OAuth HTTP fixture operation"),
        }
        flush().await;
    }
    for (_, task) in running {
        task.await.unwrap();
    }
    flush().await;
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
    carrier(&IvyJson::from_serde(&json!({"trace":*trace.lock().unwrap()})).unwrap())
}
async fn compare(name: &str, input: Value) -> Value {
    let original = original_output("tests/fixtures/hive-oauth-http-oracle.mjs", &input);
    let native = tokio::task::LocalSet::new().run_until(native(&input)).await;
    if let Ok(path) = std::env::var("IVY_HIVE_OAUTH_HTTP_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({"name":name,"input":input,"original":original,"native":native})
        )
        .unwrap();
    }
    assert_eq!(native, original, "OAuth HTTP: {name}");
    native
}
fn registration(id: &str, text: &str) -> Value {
    json!({"op":"run","id":id,"pathname":"/ivy/oauth/register","headers":{"content-type":"application/json"},"chunks":[{"text":text}]})
}
fn valid() -> String {
    json!({"redirect_uris":["https://client.test/cb?x=1#fragment"],"client_name":"Client"})
        .to_string()
}
fn sequence(requests: Vec<Value>) -> Vec<Value> {
    requests
        .into_iter()
        .flat_map(|r| {
            let join = json!({"op":"join","id":r["id"]});
            [r, join]
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_http_metadata_paths_methods_and_installation_boundaries_match_original() {
    for base in ["/ivy", ""] {
        let mut requests = Vec::new();
        for (method_index, method) in [json!("GET"), json!("POST"), json!("get"), Value::Null]
            .into_iter()
            .enumerate()
        {
            for (path_index, path) in [
                "/.well-known/oauth-authorization-server".to_owned(),
                format!("{base}/.well-known/oauth-authorization-server"),
                format!("{base}/.well-known/oauth-protected-resource"),
                format!("{base}/.well-known/oauth-protected-resource/mcp-dev"),
                format!("{base}/.well-known/oauth-protected-resource/mcp"),
                format!("{base}/.well-known/oauth-protected-resource/"),
                "/ivy-other/.well-known/oauth-authorization-server".to_owned(),
                base.to_owned(),
            ]
            .into_iter()
            .enumerate()
            {
                requests.push(json!({"op":"run","id":format!("{method_index}-{path_index}"),"method":method,"pathname":path,"headers":{"origin":"https://other.test","x-ivy-client-ip":"bad"},"chunks":[{"error":"internal_error"}]}));
            }
        }
        compare(
            if base.is_empty() {
                "root-metadata-routing"
            } else {
                "prefixed-and-global-metadata-routing"
            },
            json!({"basePath":base,"trusted":["192.0.2.1"],"steps":sequence(requests)}),
        )
        .await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn oauth_http_registration_headers_fatal_utf8_and_native_client_bytes_match_original() {
    let mut requests = Vec::new();
    for (i, value) in [
        None,
        Some(Value::Null),
        Some(json!("application/json")),
        Some(json!("application/json; charset=anything")),
        Some(json!("application/json ; charset=utf-8")),
        Some(json!("Application/Json")),
        Some(json!("application/json\n")),
        Some(json!([])),
        Some(json!(true)),
        Some(json!(7)),
        Some(json!({"split":"blocked"})),
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = registration(&format!("header-{i}"), &valid());
        if let Some(value) = value {
            r["headers"]["content-type"] = value;
        } else {
            r["headers"].as_object_mut().unwrap().remove("content-type");
        }
        requests.push(r);
    }
    for (i, origin) in [json!("https://other.test"), Value::Null, json!(["bad"])]
        .into_iter()
        .enumerate()
    {
        let mut r = registration(&format!("origin-{i}"), &valid());
        r["headers"]["origin"] = origin;
        requests.push(r);
    }
    compare(
        "registration-header-guards-and-no-origin-restriction",
        json!({"steps":sequence(requests)}),
    )
    .await;
    let mut requests = Vec::new();
    for (i, text) in [
        "",
        "{",
        "null",
        "[]",
        "true",
        "17",
        "{}",
        "{\"redirect_uris\":[\"https://client.test\"],\"redirect_uris\":[]}",
        "{\"redirect_uris\":[\"https://client.test\"],\"client_name\":\"\\ud800A\"}",
        "\u{feff}{\"redirect_uris\":[\"https://client.test\"]}",
        "\u{feff}\u{feff}{}",
        "{\"redirect_uris\":[\"https://client.test\"],\"extra\":1e999}",
        "{\"redirect_uris\":[\"https://client.test\"],\"extra\":NaN}",
        "{\"redirect_uris\":[\"https://client.test\"],\"__proto__\":{\"client_name\":\"ignored\"}}",
    ]
    .into_iter()
    .enumerate()
    {
        requests.push(registration(&format!("json-{i}"), text));
    }
    for (i, base64) in ["wK8=", "7aCA", "//4=", "77u/", "AA=="]
        .into_iter()
        .enumerate()
    {
        let mut r = registration(&format!("utf8-{i}"), "");
        r["chunks"] = json!([{"base64":base64}]);
        requests.push(r);
    }
    let mut split = registration("split-bom-utf8", "");
    split["chunks"] =
        json!([{"base64":"7w=="},{"base64":"uw=="},{"base64":"vw=="},{"text":valid()}]);
    requests.push(split);
    compare(
        "registration-fatal-json-bom-surrogates-and-duplicates",
        json!({"steps":sequence(requests)}),
    )
    .await;
    let mut requests = vec![registration("real-provider", &valid())];
    for (i, body) in [
        json!({"redirect_uris":["http://127.0.0.1:4200/cb"]}),
        json!({"redirect_uris":["http://other.test/cb"]}),
        json!({"redirect_uris":["https://client.test"],"token_endpoint_auth_method":"secret"}),
        json!({"redirect_uris":["https://client.test"],"client_name":"🙂".repeat(129)}),
        json!({"redirect_uris":["https://client.test"],"client_name":""}),
    ]
    .into_iter()
    .enumerate()
    {
        requests.push(registration(&format!("client-{i}"), &body.to_string()));
    }
    compare(
        "registration-native-public-client-response-bytes",
        json!({"steps":sequence(requests)}),
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn oauth_http_registration_body_limits_response_transitions_and_finally_match_original() {
    let mut requests = Vec::new();
    for (i, length) in [
        json!("32768"),
        json!("32769"),
        json!(" 1"),
        json!(0),
        Value::Null,
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = registration(&format!("length-{i}"), &valid());
        r["headers"]["content-length"] = length;
        requests.push(r);
    }
    for (i, length) in [32768, 32769].into_iter().enumerate() {
        let mut r = registration(&format!("actual-length-{i}"), "");
        let mut text = valid();
        text.push_str(&" ".repeat(length - text.len()));
        r["chunks"] = json!([{"text":text}]);
        requests.push(r);
    }
    for (i, chunk) in [
        json!({"error":"service_unavailable"}),
        json!({"fill":65,"length":32769}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = registration(&format!("stream-{i}"), "");
        r["chunks"] = json!([chunk]);
        r["cancelError"] = "internal_error".into();
        requests.push(r);
    }
    compare(
        "registration-length-stream-and-cancel-errors",
        json!({"steps":sequence(requests)}),
    )
    .await;
    let mut request = registration("request-bytes", &valid());
    request["probes"] = json!([1]);
    compare(
        "registration-shared-request-byte-budget",
        json!({"budget":{"perConsumerBytes":16},"steps":sequence(vec![request])}),
    )
    .await;
    let mut requests = Vec::new();
    for (i, (key, value)) in [
        ("ended", json!(true)),
        ("destroyed", json!(true)),
        ("headersSent", json!(true)),
        ("throwAt", json!("head")),
        ("throwAt", json!("end")),
        ("throwAt", json!("destroy")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = registration(&format!("response-{i}"), &valid());
        r[key] = value;
        if r["throwAt"] == "destroy" {
            r["headersSent"] = true.into();
        }
        r["probeAt"] = if r["headersSent"] == true {
            "destroy".into()
        } else {
            "head".into()
        };
        requests.push(r);
    }
    let mut steps = sequence(requests);
    steps.push(json!({"op":"probe","id":"recovered"}));
    compare(
        "registration-response-finally-and-admission-callbacks",
        json!({"steps":steps}),
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn oauth_http_registration_shares_authentication_slots_and_metadata_bypasses_them() {
    let mut steps = Vec::new();
    for i in 0..64 {
        let mut r = registration(&format!("held-{i}"), &valid());
        r["chunks"][0]["gate"] = format!("gate-{i}").into();
        if i == 63 {
            r["probeAt"] = "head".into();
        }
        steps.push(r);
    }
    let mut overflow = registration("overflow-before-content-type", &valid());
    overflow["headers"]["content-type"] = "text/plain".into();
    steps.push(overflow);
    steps.push(json!({"op":"join","id":"overflow-before-content-type"}));
    steps.push(json!({"op":"auth","id":"bounded-auth-full"}));
    steps.push(json!({"op":"run","id":"metadata-while-full","method":"GET","pathname":"/.well-known/oauth-authorization-server","probeAt":"head"}));
    steps.push(json!({"op":"join","id":"metadata-while-full"}));
    steps.push(json!({"op":"release","gate":"gate-63"}));
    steps.push(json!({"op":"join","id":"held-63"}));
    steps.push(json!({"op":"probe","id":"released-after-response"}));
    for i in 0..63 {
        steps.push(json!({"op":"release","gate":format!("gate-{i}")}));
        steps.push(json!({"op":"join","id":format!("held-{i}")}));
    }
    steps.push(json!({"op":"probe","id":"all-recovered"}));
    compare(
        "registration-shared64-slots-finally-and-metadata-bypass",
        json!({"steps":steps}),
    )
    .await;
}
