use super::*;
use ivy_hive::{
    auth_pages as pages,
    http_body::BodyBuffers,
    http_response::{self, Response as HttpResponse},
    oauth::{Dependencies, Entropy, FamilyStore, HiveOAuth, Redirects},
    oauth_http::Handler as OAuthHandler,
};
struct Unused;
impl FamilyStore for Unused {
    fn create(&self, _: IvyJson, _: Vec<JsString>) -> RequestFuture {
        panic!("Unexpected family creation")
    }
    fn rotate(&self, _: IvyJson) -> RequestFuture {
        panic!("Unexpected family rotation")
    }
}
impl Redirects for Unused {
    fn set_parameters(&self, _: &JsString, _: &[(JsString, JsString)]) -> IvyResult<JsString> {
        panic!("Unexpected redirect owner")
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
        panic!("Unexpected response destroy")
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
    let oauth = HiveOAuth::new(
        &format!("https://example.test{}/", base.to_utf8().unwrap()).into(),
        vec![],
        Dependencies {
            families: Arc::new(Unused),
            redirects: Arc::new(Unused),
            entropy: Arc::new(Random {
                counter: Mutex::new(0),
                events: events.clone(),
            }),
            clock: Arc::new(|| 100000.0),
        },
    )
    .unwrap();
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
                    match op {
                        "headers" => {}
                        "getLogin" => {
                            pages::login_response(&connection, &parameters(&step), &mut reply)?
                        }
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
    let original = original_output("tests/fixtures/hive-auth-pages-oracle.mjs", &input);
    let native = native(&input).await;
    if let Ok(path) = std::env::var("IVY_HIVE_AUTH_PAGES_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(
            f,
            "{}",
            json!({"name":name,"input":input,"original":original,"native":native})
        )
        .unwrap();
    }
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
