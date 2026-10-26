use super::*;
use ivy_hive::{
    admission::{RequestBudget, RequestBudgetOptions},
    auth_pages as pages,
    http_body::{BodyBuffers, BodyStream},
    http_response::{self, Response as HttpResponse},
    oauth::{Dependencies, Entropy, FamilyStore, HiveOAuth, Secrets, UrlRedirects},
    oauth_http::{Handler as OAuthHandler, Headers as OAuthHeaders},
};
struct Families {
    events: Arc<Mutex<Vec<Value>>>,
    replies: Mutex<std::collections::VecDeque<IvyJson>>,
}
impl Families {
    fn call(&self, action: &str, payload: IvyJson, active: Option<Vec<JsString>>) -> RequestFuture {
        let mut event = json!({"kind":"family","action":action,"encoded":stringify(&payload,usize::MAX).unwrap()});
        if let Some(active) = active {
            event["activeEncoded"] = stringify(
                &IvyJson::Array(active.into_iter().map(IvyJson::String).collect()),
                usize::MAX,
            )
            .unwrap()
            .into();
        }
        self.events.lock().unwrap().push(event);
        let spec = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Explicit family fixture reply");
        Box::pin(async move {
            if let Some(code) = optional(&spec, "error") {
                Err(IvyError::new(
                    code.to_utf8().unwrap(),
                    "Isolated provider failure.",
                ))
            } else {
                Ok(IvyJson::Null)
            }
        })
    }
}
impl FamilyStore for Families {
    fn create(&self, state: IvyJson, active: Vec<JsString>) -> RequestFuture {
        self.call("create", state, Some(active))
    }
    fn rotate(&self, rotation: IvyJson) -> RequestFuture {
        self.call("rotate", rotation, None)
    }
}
struct PageStream {
    chunks: Vec<IvyJson>,
    index: usize,
    spec: IvyJson,
    events: Arc<Mutex<Vec<Value>>>,
    buffers: BodyBuffers,
}
impl BodyStream for PageStream {
    async fn next_chunk(&mut self) -> IvyResult<Option<Vec<u8>>> {
        let index = self.index;
        self.index += 1;
        self.events.lock().unwrap().push(json!({"kind":"read","id":string(&self.spec,"id"),"index":index,"buffered":self.buffers.buffered_bytes()}));
        let Some(chunk) = self.chunks.get(index) else {
            return Ok(None);
        };
        if let Some(code) = optional(chunk, "error") {
            return Err(IvyError::new(
                code.to_utf8().unwrap(),
                "Isolated provider failure.",
            ));
        }
        Ok(Some(if let Some(value) = optional(chunk, "base64") {
            base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                value.to_utf8().unwrap(),
            )
            .unwrap()
        } else if let Some(fill) = chunk.get("fill") {
            vec![
                fill.as_f64().unwrap() as u8;
                chunk.get("length").unwrap().as_f64().unwrap() as usize
            ]
        } else {
            String::from_utf16_lossy(&optional(chunk, "text").unwrap_or_default().0).into_bytes()
        }))
    }
    async fn cancel(&mut self) -> IvyResult<()> {
        self.events.lock().unwrap().push(json!({"kind":"cancel","id":string(&self.spec,"id"),"buffered":self.buffers.buffered_bytes()}));
        if let Some(code) = optional(&self.spec, "cancelError") {
            Err(IvyError::new(
                code.to_utf8().unwrap(),
                "Isolated provider failure.",
            ))
        } else {
            Ok(())
        }
    }
}
struct Random {
    counter: Mutex<u64>,
    events: Arc<Mutex<Vec<Value>>>,
}
impl Entropy for Random {
    fn bytes(&self, length: usize) -> IvyResult<Vec<u8>> {
        let mut counter = self.counter.lock().unwrap();
        let number = *counter;
        *counter += 1;
        let mut bytes = (0..length)
            .map(|i| ((number * 17 + i as u64) % 256) as u8)
            .collect::<Vec<_>>();
        bytes[..8].copy_from_slice(&number.to_le_bytes());
        self.events.lock().unwrap().push(json!({"kind":"random","length":length,"hex":bytes.iter().map(|b|format!("{b:02x}")).collect::<String>()}));
        Ok(bytes)
    }
}
struct Reply {
    spec: IvyJson,
    events: Arc<Mutex<Vec<Value>>>,
    connection: Arc<Connection>,
    headers: AuthHeaders,
    ended: bool,
}
impl Reply {
    fn check(&self, phase: &str) -> IvyResult<()> {
        if optional(&self.spec, "probeAt") == Some(phase.into()) {
            page_probe(
                &self.events,
                &self.connection,
                &self.headers,
                &string(&self.spec, "id"),
                phase,
            );
        }
        if optional(&self.spec, "throwAt") == Some(phase.into()) {
            Err(IvyError::internal())
        } else {
            Ok(())
        }
    }
}
impl HttpResponse for Reply {
    fn writable_ended(&self) -> bool {
        self.ended
    }
    fn destroyed(&self) -> bool {
        self.spec.get("destroyed") == Some(&true.into())
    }
    fn headers_sent(&self) -> bool {
        self.spec.get("headersSent") == Some(&true.into())
    }
    fn set_header(&mut self, name: &str, value: &JsString) -> IvyResult<()> {
        self.events.lock().unwrap().push(json!({"kind":"header","id":string(&self.spec,"id"),"name":name,"encoded":stringify(&IvyJson::String(value.clone()),usize::MAX).unwrap()}));
        self.check(name)
    }
    fn write_head(&mut self, status: u16, headers: IvyJson) -> IvyResult<()> {
        self.events.lock().unwrap().push(json!({"kind":"head","id":string(&self.spec,"id"),"status":status,"encoded":stringify(&headers,usize::MAX).unwrap()}));
        self.check("head")
    }
    fn end(&mut self, text: String) -> IvyResult<()> {
        self.events
            .lock()
            .unwrap()
            .push(json!({"kind":"end","id":string(&self.spec,"id"),"body":text}));
        self.check("end")?;
        self.ended = true;
        Ok(())
    }
    fn end_empty(&mut self) -> IvyResult<()> {
        self.events
            .lock()
            .unwrap()
            .push(json!({"kind":"end","id":string(&self.spec,"id"),"body":null}));
        self.check("end")?;
        self.ended = true;
        Ok(())
    }
    fn destroy(&mut self) -> IvyResult<()> {
        self.events
            .lock()
            .unwrap()
            .push(json!({"kind":"destroy","id":string(&self.spec,"id")}));
        self.check("destroy")
    }
}
fn page_probe(
    events: &Arc<Mutex<Vec<Value>>>,
    connection: &Connection,
    headers: &AuthHeaders,
    id: &str,
    phase: &str,
) {
    let entry = match connection.admit_authentication(headers) {
        Ok(mut lease) => {
            lease.release();
            json!({"kind":"probe","id":id,"phase":phase,"value":null})
        }
        Err(e) => json!({"kind":"probe","id":id,"phase":phase,"error":carrier(&e.to_wire())}),
    };
    events.lock().unwrap().push(entry);
}
fn resolve(value: &IvyJson, values: &HashMap<String, IvyJson>) -> IvyJson {
    if let Some(path) = value.get("$ref").map(|v| v.as_str().unwrap()) {
        let mut path = path.split('.');
        let mut value = &values[path.next().unwrap()];
        for key in path {
            value = value.get(key).unwrap();
        }
        return value.clone();
    }
    match value {
        IvyJson::Array(a) => IvyJson::Array(a.iter().map(|v| resolve(v, values)).collect()),
        IvyJson::Object(o) => {
            IvyJson::object(o.iter().map(|(k, v)| (k.clone(), resolve(v, values))))
        }
        _ => value.clone(),
    }
}
fn parameters(step: &IvyJson) -> Parameters {
    Parameters::new(
        step.get("form")
            .and_then(IvyJson::as_array)
            .unwrap_or(&[])
            .iter()
            .map(|p| {
                let p = p.as_array().unwrap();
                (
                    p[0].as_js_string().unwrap().clone(),
                    p[1].as_js_string().unwrap().clone(),
                )
            }),
    )
}
fn pairs(value: &IvyJson) -> Vec<(JsString, JsString)> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let p = p.as_array().unwrap();
            (
                p[0].as_js_string().unwrap().clone(),
                p[1].as_js_string().unwrap().clone(),
            )
        })
        .collect()
}
fn wire_chunks(step: &IvyJson) -> Vec<IvyJson> {
    if let Some(chunks) = step.get("chunks") {
        return chunks.as_array().unwrap().to_vec();
    }
    let body = if let Some(form) = step.get("formWire") {
        // Construct the ordered pair sequence, retaining duplicates. Applying
        // setters to the whole sequence would change the tested wire input.
        pairs(form)
            .iter()
            .map(|pair| {
                let url = ivy_contracts::whatwg_url::set_url_search_params(
                    &"https://wire.test/".into(),
                    std::slice::from_ref(pair),
                )
                .unwrap()
                .to_utf8()
                .unwrap();
                url.split_once('?').unwrap().1.to_owned()
            })
            .collect::<Vec<_>>()
            .join("&")
            .into()
    } else {
        optional(step, "body").unwrap_or_default()
    };
    vec![IvyJson::object([("text", IvyJson::String(body))])]
}
async fn native(input: &Value) -> Value {
    let data = input_json(input);
    if data.get("mode") == Some(&"render".into()) {
        let rows = data
            .get("cases")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                let rejected = s.get("rejected") == Some(&true.into());
                let page = if s.get("kind") == Some(&"login".into()) {
                    pages::login(field(s, "base"), field(s, "target"), rejected)
                } else {
                    pages::oauth(
                        field(s, "base"),
                        field(s, "transaction"),
                        field(s, "client"),
                        field(s, "scope"),
                        rejected,
                    )
                };
                json!({"encoded":stringify(&IvyJson::String(page),usize::MAX).unwrap()})
            })
            .collect::<Vec<_>>();
        return json!({"rows":rows,"script":pages::login_theme_script()});
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let providers = Arc::new(Providers {
        events: events.clone(),
        replies: Mutex::new(
            data.get("workerReplies")
                .and_then(IvyJson::as_array)
                .unwrap_or(&[])
                .to_vec()
                .into(),
        ),
        gates: Mutex::default(),
        actor: None,
    });
    let base = optional(&data, "basePath").unwrap_or_else(|| "/ivy".into());
    let authorized = data.get("authorized") == Some(&true.into());
    let oauth = HiveOAuth::new(
        &format!("https://example.test{}/", base.to_utf8().unwrap()).into(),
        if authorized {
            vec![ivy_hive::store::Credential {
                principal_id: "alpha".into(),
                digest: ivy_contracts::hash::digest("credential").into(),
            }]
        } else {
            vec![]
        },
        Dependencies {
            families: Arc::new(Families {
                events: events.clone(),
                replies: Mutex::new(
                    data.get("familyReplies")
                        .and_then(IvyJson::as_array)
                        .unwrap_or(&[])
                        .to_vec()
                        .into(),
                ),
            }),
            redirects: Arc::new(UrlRedirects),
            entropy: Arc::new(Random {
                counter: Mutex::new(0),
                events: events.clone(),
            }),
            clock: Arc::new(|| 100000.0),
        },
    )
    .unwrap();
    if authorized {
        oauth
            .initialize(&Secrets {
                signing_key: "11".repeat(32).into(),
                reference_key: "22".repeat(32).into(),
            })
            .unwrap();
    }
    let authentication = Authentication::new(
        AuthOptions {
            canonical_origin: "https://example.test".into(),
            base_path: base,
            trusted_proxy_addresses: data
                .get("trusted")
                .and_then(IvyJson::as_array)
                .unwrap_or(&[])
                .iter()
                .map(|v| v.as_js_string().unwrap().clone())
                .collect(),
        },
        providers.clone(),
        Arc::new(oauth.clone()),
        Arc::new(|| 100000.0),
    )
    .unwrap();
    let connection = Arc::new(authentication.connection(Some(
        optional(&data, "remoteAddress").unwrap_or_else(|| "192.0.2.1".into()),
    )));
    let mut values = HashMap::new();
    let mut results = serde_json::Map::new();
    let mut held = Vec::new();
    let buffers = BodyBuffers::default();
    let bodies = RequestBudget::new(RequestBudgetOptions {
        maximum: 2048,
        maximum_bytes: 128 * 1024 * 1024,
        per_consumer: 128,
        per_consumer_bytes: 64 * 1024 * 1024,
        name: "request".into(),
    });
    for raw in data.get("steps").unwrap().as_array().unwrap() {
        let step = resolve(raw, &values);
        let id = string(&step, "id");
        let headers = auth_headers(&step);
        let result: IvyResult<IvyJson> = async {
            let mut value = IvyJson::Null;
            match string(&step, "op").as_str() {
                "reserve" => {
                    let count = step.get("count").unwrap().as_f64().unwrap() as usize;
                    for _ in 0..count {
                        held.push(connection.admit_authentication(&headers)?);
                    }
                    events
                        .lock()
                        .unwrap()
                        .push(json!({"kind":"reserve","count":count}));
                }
                "release" => {
                    for mut lease in held.drain(..) {
                        lease.release();
                    }
                    events.lock().unwrap().push(json!({"kind":"release"}));
                }
                "probe" => page_probe(&events, &connection, &headers, &id, "explicit"),
                "register" => value = oauth.register(step.get("body").unwrap())?,
                "begin" => value=oauth.begin(&parameters(&step))?,
                "transaction" => value = oauth.transaction(field(&step, "transaction"))?,
                op => {
                    let mut reply = Reply {
                        spec: step.clone(),
                        events: events.clone(),
                        connection: connection.clone(),
                        headers: headers.clone(),
                        ended: step.get("ended") == Some(&true.into()),
                    };
                    http_response::security_headers(&mut reply)?;
                    if op.ends_with("Wire") {
                        let mut stream=PageStream{chunks:wire_chunks(&step),index:0,spec:step.clone(),events:events.clone(),buffers:buffers.clone()};
                        let mut lease=bodies.acquire(&"http:peer".into(),Some(0))?;
                        let content_type=step.get("headers").and_then(|h|h.get("content-type"));
                        let content_length=step.get("headers").and_then(|h|h.get("content-length"));
                        let result=if op=="loginWire" {
                            Session{connection:&connection,worker:providers.as_ref()}.login_post(ivy_hive::browser_session::LoginHeaders{authentication:&headers,content_type,content_length},&buffers,&mut stream,Some(&mut lease),&mut pages::SessionResponse{connection:&connection,response:&mut reply}).await
                        }else{
                            OAuthHandler{connection:&connection,oauth:&oauth,buffers:&buffers}.try_handle(Some(&"POST".into()),&format!("{}{}",connection.authentication().base_path().to_utf8().unwrap(),if op=="authorizeWire"{"/oauth/authorize"}else{"/oauth/token"}).into(),OAuthHeaders{authentication:&headers,content_type,content_length},&mut stream,Some(&mut lease),&mut reply).await.map(|matched|{assert!(matched);})
                        };
                        lease.release();
                        events.lock().unwrap().push(json!({"kind":"wire-state","id":id,"buffered":buffers.buffered_bytes()}));
                        result?;
                        return Ok(value);
                    }
                    match op {
                        "headers" => {}
                        "getLogin" => {
                            pages::login_response(&connection, &parameters(&step), &mut reply)?
                        }
                        "getLoginQuery"=>pages::login_query_response(&connection,field(&step,"query"),&mut reply)?,
                        "theme" => pages::theme_response(&mut reply)?,
                        "login" => {
                            let mut response = pages::SessionResponse {
                                connection: &connection,
                                response: &mut reply,
                            };
                            Session {
                                connection: &connection,
                                worker: providers.as_ref(),
                            }
                            .login(&headers, &parameters(&step), &mut response)
                            .await?;
                        }
                        "authorize" => OAuthHandler {
                            connection: &connection,
                            oauth: &oauth,
                            buffers: &BodyBuffers::default(),
                        }
                        .authorize_page(
                            &headers,
                            &parameters(&step),
                            &mut reply,
                        )?,
                        "authorizeQuery"=>OAuthHandler{connection:&connection,oauth:&oauth,buffers:&buffers}.authorize_query(&headers,field(&step,"query"),&mut reply)?,
                        _ => panic!("Unknown page operation"),
                    }
                }
            }
            Ok(value)
        }
        .await;
        if let Ok(value) = &result {
            values.insert(id.clone(), value.clone());
        }
        results.insert(id, outcome(result));
    }
    carrier(
        &IvyJson::from_serde(&json!({"events":*events.lock().unwrap(),"results":results})).unwrap(),
    )
}
async fn compare(name: &str, input: Value) -> Value {
    let original = auth_reference("auth-pages", &input);
    let native = native(&input).await;
    assert_eq!(native, original, "Authentication pages: {name}");
    native
}
fn step(op: &str, id: &str) -> Value {
    json!({"op":op,"id":id})
}
fn form_step(op: &str, id: &str, form: Value) -> Value {
    json!({"op":op,"id":id,"form":form})
}
fn login_form() -> Value {
    json!([
        ["token", "credential"],
        ["returnTo", "/ivy/ui/item?x=1#target"]
    ])
}
fn client() -> Value {
    json!({"redirect_uris":["https://client.test/cb"],"client_name":"Example <script>&\"'"})
}
fn approval_form() -> Value {
    json!([["response_type","code"],["client_id",{"$ref":"client.client_id"}],["redirect_uri","https://client.test/cb"],["state","state"],["code_challenge","a".repeat(43)],["code_challenge_method","S256"],["resource","https://example.test/ivy/mcp"],["scope","hive"]])
}
fn transaction(number: u64) -> String {
    use base64::Engine;
    let mut bytes = (0..32)
        .map(|i| ((number * 17 + i) % 256) as u8)
        .collect::<Vec<_>>();
    bytes[..8].copy_from_slice(&number.to_le_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[tokio::test(flavor = "current_thread")]
async fn auth_page_renderers_preserve_exact_markup_utf16_escaping_and_client_script() {
    let mut cases = vec![];
    for rejected in [false, true] {
        for value in [
            json!(""),
            json!("/ivy"),
            json!("<&>\"'&amp;{{target}}"),
            json!("日本語 🙂 العربية"),
            json!({"$units":[55296,65,56320]}),
        ] {
            cases.push(json!({"kind":"login","base":value,"target":value,"rejected":rejected}));
            cases.push(json!({"kind":"oauth","base":value,"transaction":value,"client":value,"scope":value,"rejected":rejected}));
        }
    }
    compare(
        "page-renderer-utf16-escaping-and-theme",
        json!({"mode":"render","cases":cases}),
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn auth_page_security_headers_keep_order_and_partial_failure_effects() {
    let mut steps = vec![step("headers", "normal")];
    for (i, name) in [
        "Content-Security-Policy",
        "X-Content-Type-Options",
        "X-Frame-Options",
        "Referrer-Policy",
        "Cache-Control",
    ]
    .into_iter()
    .enumerate()
    {
        let mut s = step("headers", &format!("failure-{i}"));
        s["throwAt"] = name.into();
        steps.push(s);
    }
    for flag in ["ended", "destroyed", "headersSent"] {
        let mut s = step("headers", flag);
        s[flag] = true.into();
        steps.push(s);
    }
    compare(
        "common-security-header-order-and-partial-failures",
        json!({"steps":steps}),
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn auth_page_login_get_theme_and_native_session_response_binding_match_original() {
    let mut steps = vec![
        step("theme", "theme"),
        form_step(
            "getLogin",
            "normal",
            json!([["returnTo", "/ivy/ui/item?x=1"]]),
        ),
        form_step(
            "getLogin",
            "first-duplicate",
            json!([["returnTo", "/ivy/a"], ["returnTo", "/outside"]]),
        ),
        form_step("getLogin", "refused", json!([["returnTo", "/outside"]])),
        form_step("login", "accepted", login_form()),
        form_step("login", "rejected", login_form()),
    ];
    for phase in ["head", "end", "Set-Cookie"] {
        let mut s = form_step("login", phase, login_form());
        s["throwAt"] = phase.into();
        steps.push(s);
    }
    steps.push(step("probe", "recovered"));
    compare(
        "native-login-pages-and-session-response-binding",
        json!({"workerReplies":[{}, {"error":"unauthenticated"}],"steps":steps}),
    )
    .await;
    let mut rejection = form_step("login", "rejected", login_form());
    rejection["probeAt"] = "head".into();
    compare("login-rejection-rendering-inside-shared-admission",json!({"workerReplies":[{"error":"unauthenticated"}],"steps":[json!({"op":"reserve","id":"held","count":63}),rejection,step("probe","released"),step("release","all")]})).await;
}
#[tokio::test(flavor = "current_thread")]
async fn auth_page_oauth_approval_uses_native_grants_and_releases_admission_after_rendering() {
    let mut steps = vec![
        json!({"op":"register","id":"client","body":client()}),
        form_step("authorize", "approval", approval_form()),
        json!({"op":"transaction","id":"pending","transaction":transaction(0)}),
    ];
    let mut rejected = form_step("authorize", "invalid-query", approval_form());
    rejected["form"][0][1] = "token".into();
    steps.push(rejected);
    steps.push(step("probe", "recovered"));
    compare(
        "oauth-approval-native-page-and-pending-state",
        json!({"steps":steps}),
    )
    .await;
    let mut approval = form_step("authorize", "approval", approval_form());
    approval["probeAt"] = "head".into();
    approval["throwAt"] = "head".into();
    compare("oauth-approval-admission-and-callback-failures",json!({"steps":[json!({"op":"register","id":"client","body":client()}),json!({"op":"reserve","id":"held","count":63}),approval,json!({"op":"transaction","id":"pending","transaction":transaction(0)}),step("probe","released"),step("release","all")]})).await;
}

fn wire(op: &str, id: &str, body: &str) -> Value {
    json!({"op":op,"id":id,"body":body,"headers":{"origin":"https://example.test","content-type":"application/x-www-form-urlencoded; charset=utf-8"}})
}
fn grant_setup(redirect: &str, pkce: bool) -> Vec<Value> {
    let mut c = client();
    c["redirect_uris"] = json!([redirect]);
    let mut form = approval_form();
    form[2][1] = redirect.into();
    form[3][1] = "State +&~日本語".into();
    if pkce {
        use sha2::Digest;
        form[4][1] = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            sha2::Sha256::digest("v".repeat(43)),
        )
        .into();
    }
    vec![
        json!({"op":"register","id":"client","body":c}),
        json!({"op":"begin","id":"pending","form":form}),
    ]
}
fn approval_wire(id: &str, credential: &str) -> Value {
    let mut s = wire("authorizeWire", id, "");
    s["formWire"] = json!([["transaction",{"$ref":"pending.transaction"}],["credential",credential],["credential","ignored"]]);
    s
}
#[tokio::test(flavor = "current_thread")]
async fn raw_login_form_decoder_limits_and_guard_order_match_original() {
    let mut steps = vec![
        json!({"op":"getLoginQuery","id":"query","query":"?returnTo=%2Fivy%2Fa%3Fx%3D1%23frag&returnTo=%2Foutside"}),
    ];
    for (i, body) in [
        "?token=credential&token=ignored&returnTo=%2Fivy%2Fa%3Fx%3D1%23f",
        "token=a+b%2Bc%26%00%FF&returnTo=%2Fivy%2F",
        "token=&token=credential",
        "%EF%BB%BFtoken=credential",
        "token=credential&returnTo=%2Foutside",
        "token=credential&returnTo=%2Fivy%5Cbad",
    ]
    .iter()
    .enumerate()
    {
        steps.push(wire("loginWire", &format!("form-{i}"), body));
    }
    for (i, value) in [
        json!(null),
        json!("Application/x-www-form-urlencoded"),
        json!("application/x-www-form-urlencoded "),
        json!([]),
    ]
    .into_iter()
    .enumerate()
    {
        let mut s = wire("loginWire", &format!("type-{i}"), "token=credential");
        s["headers"]["content-type"] = value;
        steps.push(s);
    }
    let mut origin = wire("loginWire", "origin-first", "token=credential");
    origin["headers"]["origin"] = "https://other.test".into();
    origin["headers"]["content-type"] = "invalid".into();
    steps.push(origin);
    let mut malformed = wire("loginWire", "utf8", " ");
    malformed["chunks"] = json!([{"base64":"dG9rZW49/+2ggCZyZXR1cm5Ubz0lMkZpdnklMkY="}]);
    steps.push(malformed);
    let mut bom = wire("loginWire", "bom", "");
    bom["chunks"] = json!([{"base64":"77u/dG9rZW49Y3JlZGVudGlhbA=="}]);
    steps.push(bom);
    for n in [4096, 4097] {
        steps.push(wire(
            "loginWire",
            &format!("token-{n}"),
            &format!("token={}", "x".repeat(n)),
        ));
    }
    for n in [8192, 8193] {
        let mut s = wire("loginWire", &format!("bytes-{n}"), "");
        s["chunks"] = json!([{"fill":120,"length":n}]);
        steps.push(s);
    }
    let mut declared = wire("loginWire", "declared", "");
    declared["headers"]["content-length"] = "8193".into();
    steps.push(declared);
    let mut failed = wire("loginWire", "stream-error", "");
    failed["chunks"] = json!([{"text":"token=credential"},{"error":"storage_unavailable"}]);
    steps.push(failed);
    steps.push(step("probe", "recovered"));
    let result = compare(
        "raw-login-forms-decoding-limits-and-guard-order",
        json!({"steps":steps}),
    )
    .await;
    assert!(
        result["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["kind"] != "wire-state" || e["buffered"] == 0)
    );
}
#[tokio::test(flavor = "current_thread")]
async fn raw_oauth_authorization_uses_canonical_final_redirect_and_error_state() {
    let uri =
        "https://client.test/cb?existing=~&code=old&code=later&state=old&iss=old%20iss&after=x+y";
    let mut steps = grant_setup(uri, false);
    steps.push(approval_wire("approved", "credential"));
    steps.push(
        json!({"op":"transaction","id":"consumed","transaction":{"$ref":"pending.transaction"}}),
    );
    let result = compare(
        "raw-oauth-canonical-final-redirect",
        json!({"authorized":true,"steps":steps}),
    )
    .await;
    let head = result["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "head" && e["id"] == "approved")
        .unwrap();
    let headers: Value = serde_json::from_str(head["encoded"].as_str().unwrap()).unwrap();
    let target = headers["Location"].as_str().unwrap();
    assert!(target.starts_with("https://client.test/cb?existing=%7E&code="));
    assert_eq!(target.matches("code=").count(), 1);
    assert!(target.ends_with("&iss=https%3A%2F%2Fexample.test&after=x+y"));
    let mut steps = grant_setup("https://client.test/cb", false);
    steps.push(approval_wire("rejected", "wrong"));
    steps.push(
        json!({"op":"transaction","id":"retained","transaction":{"$ref":"pending.transaction"}}),
    );
    let mut failure = approval_wire("failed-head", "credential");
    failure["throwAt"] = "head".into();
    steps.push(failure);
    steps.push(
        json!({"op":"transaction","id":"spent","transaction":{"$ref":"pending.transaction"}}),
    );
    steps.push(step("probe", "recovered"));
    compare(
        "raw-oauth-rejection-and-redirect-callback-failure",
        json!({"authorized":true,"steps":steps}),
    )
    .await;
    let query = format!(
        "?response_type=code&client_id={}&redirect_uri=https%3A%2F%2Fclient.test%2Fcb&state=a%2Bb+%26&code_challenge={}&code_challenge_method=S256&resource=https%3A%2F%2Fexample.test%2Fivy%2Fmcp",
        "invalid",
        "a".repeat(43)
    );
    compare("raw-oauth-query-validation",json!({"steps":[json!({"op":"authorizeQuery","id":"query","query":query}),step("probe","released")]})).await;
}
#[tokio::test(flavor = "current_thread")]
async fn raw_authentication_forms_keep_distinct_admission_and_callback_order() {
    for op in ["loginWire", "authorizeWire", "tokenWire"] {
        let mut s = wire(op, "request", "token=credential&grant_type=unknown");
        s["headers"]["content-type"] = "invalid".into();
        compare(&format!("raw-{op}-full-admission-order"),json!({"steps":[json!({"op":"reserve","id":"full","count":64}),s,step("release","all"),step("probe","recovered")]})).await;
    }
    let mut login = wire(
        "loginWire",
        "rejected",
        "token=credential&returnTo=%2Fivy%2Fa",
    );
    login["probeAt"] = "head".into();
    compare("raw-login-rejection-callback-admission",json!({"workerReplies":[{"error":"unauthenticated"}],"steps":[json!({"op":"reserve","id":"held","count":63}),login,step("probe","released"),step("release","all")]})).await;
    let mut steps = grant_setup("https://client.test/cb", false);
    steps.push(json!({"op":"reserve","id":"held","count":63}));
    let mut approved = approval_wire("approved", "credential");
    approved["probeAt"] = "head".into();
    steps.push(approved);
    steps.push(step("probe", "released"));
    steps.push(step("release", "all"));
    compare(
        "raw-oauth-redirect-callback-admission",
        json!({"authorized":true,"steps":steps}),
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn raw_oauth_token_form_exchange_and_body_failures_match_original() {
    let mut steps = grant_setup("https://client.test/cb", true);
    steps.push(approval_wire("approved", "credential"));
    let mut token = wire("tokenWire", "tokens", "");
    token["headers"]["origin"] = "https://foreign.test".into();
    token["formWire"] = json!([["grant_type","authorization_code"],["grant_type","ignored"],["client_id",{"$ref":"client.client_id"}],["code",transaction(1)],["redirect_uri","https://client.test/cb"],["resource","https://example.test/ivy/mcp"],["code_verifier","v".repeat(43)]]);
    steps.push(token);
    steps.push(step("probe", "released"));
    let result = compare(
        "raw-oauth-token-real-grant-and-family-issue",
        json!({"authorized":true,"familyReplies":[{}],"steps":steps}),
    )
    .await;
    let body = result["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "end" && e["id"] == "tokens")
        .unwrap()["body"]
        .as_str()
        .unwrap();
    let value: Value = serde_json::from_str(body).unwrap();
    assert_eq!(value["token_type"], "Bearer");
    assert_eq!(value["expires_in"], 900);
    let mut steps = vec![];
    for (i, body) in [
        "grant_type=unknown",
        "?grant_type=unknown&grant_type=authorization_code",
        "%EF%BB%BFgrant_type=authorization_code",
        "grant_type=authorization_code&code=%FF",
    ]
    .iter()
    .enumerate()
    {
        steps.push(wire("tokenWire", &format!("form-{i}"), body));
    }
    for op in ["authorizeWire", "tokenWire"] {
        let limit = if op == "tokenWire" { 32768 } else { 8192 };
        for n in [limit, limit + 1] {
            let mut s = wire(op, &format!("{op}-{n}"), "");
            s["chunks"] = json!([{"fill":120,"length":n}]);
            s["cancelError"] = "storage_unavailable".into();
            steps.push(s);
        }
    }
    let mut origin = wire("authorizeWire", "origin-before-type", "");
    origin["headers"]["origin"] = "https://foreign.test".into();
    origin["headers"]["content-type"] = "invalid".into();
    steps.push(origin);
    let mut declared = wire("tokenWire", "declared", "");
    declared["headers"]["content-length"] = "32769".into();
    steps.push(declared);
    steps.push(step("probe", "released"));
    compare(
        "raw-oauth-form-errors-byte-caps-and-cancel-precedence",
        json!({"steps":steps}),
    )
    .await;
}
