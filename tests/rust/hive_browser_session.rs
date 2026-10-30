use super::*;
#[path = "hive_auth_pages.rs"]
mod auth_pages;
use ivy_hive::http_auth::Connection;
use ivy_hive::{
    browser_session::{self, Peer, Peers, Response as SessionResponse, Session},
    oauth::Parameters,
};
use std::sync::Weak;

fn input_json(value: &Value) -> IvyJson {
    if let Some(units) = value.get("$units").and_then(Value::as_array) {
        return IvyJson::String(JsString(
            units.iter().map(|v| v.as_u64().unwrap() as u16).collect(),
        ));
    }
    match value {
        Value::Array(v) => IvyJson::Array(v.iter().map(input_json).collect()),
        Value::Object(v) => IvyJson::object(v.iter().map(|(k, v)| (k.as_str(), input_json(v)))),
        _ => IvyJson::from_serde(value).unwrap(),
    }
}
fn field<'a>(v: &'a IvyJson, name: &str) -> &'a JsString {
    v.get(name).unwrap().as_js_string().unwrap()
}
fn optional(v: &IvyJson, name: &str) -> Option<JsString> {
    v.get(name).and_then(IvyJson::as_js_string).cloned()
}
fn string(v: &IvyJson, name: &str) -> String {
    field(v, name).to_utf8().unwrap()
}
fn auth_headers(v: &IvyJson) -> AuthHeaders {
    let v = v
        .get("headers")
        .cloned()
        .unwrap_or_else(|| IvyJson::object([] as [(&str, IvyJson); 0]));
    AuthHeaders {
        authorization: optional(&v, "authorization"),
        cookie: optional(&v, "cookie"),
        origin: v.get("origin").cloned(),
        forwarded_client_ip: v.get("x-ivy-client-ip").cloned(),
    }
}
struct Providers {
    events: Arc<Mutex<Vec<Value>>>,
    replies: Mutex<VecDeque<IvyJson>>,
    gates: Mutex<HashMap<String, oneshot::Sender<()>>>,
    actor: Option<mpsc::UnboundedSender<Posted>>,
}
impl Worker for Providers {
    fn request(&self, payload: ivy_hive::storage::Payload) -> RequestFuture {
        let payload = crate::payload_fixture::json(payload);
        self.events
            .lock()
            .unwrap()
            .push(json!({"kind":"worker","encoded":stringify(&payload,usize::MAX).unwrap()}));
        let spec = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| IvyJson::object([] as [(&str, IvyJson); 0]));
        let waiting = optional(&spec, "gate").map(|gate| {
            let (send, receive) = oneshot::channel();
            assert!(
                self.gates
                    .lock()
                    .unwrap()
                    .insert(gate.to_utf8().unwrap(), send)
                    .is_none()
            );
            receive
        });
        let actor = self.actor.clone();
        Box::pin(async move {
            if let Some(waiting) = waiting {
                waiting.await.unwrap();
            }
            if let Some(code) = optional(&spec, "error") {
                return Err(isolated(&code.to_utf8().unwrap()));
            }
            if let Some(actor) = actor {
                let (send, receive) = oneshot::channel();
                actor.send((payload, send)).unwrap();
                return receive.await.unwrap();
            }
            Ok(spec.get("value").cloned().unwrap_or_else(|| {
                match payload.get("action").unwrap().as_str().unwrap().as_str() {
                    "session.create" => IvyJson::object([("sessionToken", "issued-token".into())]),
                    "authenticate" => IvyJson::object([("principalId", "alpha".into())]),
                    _ => IvyJson::object([
                        ("credentialDigest", "credential".into()),
                        (
                            "sessionDigest",
                            payload
                                .get("sessionDigest")
                                .cloned()
                                .unwrap_or(IvyJson::Null),
                        ),
                        ("principalId", "alpha".into()),
                    ]),
                }
            }))
        })
    }
}
impl OAuth for Providers {
    fn authenticate(
        &self,
        token: &JsString,
        resource: Option<&JsString>,
    ) -> IvyResult<Option<IvyJson>> {
        self.events.lock().unwrap().push(json!({"kind":"oauth","encoded":stringify(&IvyJson::Array(vec![IvyJson::String(token.clone()),resource.map(|v|IvyJson::String(v.clone())).unwrap_or(IvyJson::Null)]),usize::MAX).unwrap()}));
        Ok(None)
    }
}
fn probe(
    trace: &Arc<Mutex<Vec<Value>>>,
    connection: &Connection,
    headers: &AuthHeaders,
    id: &str,
    phase: &str,
) {
    let result = match connection.admit_authentication(headers) {
        Ok(mut lease) => {
            lease.release();
            json!({"kind":"probe","id":id,"phase":phase,"value":null})
        }
        Err(error) => {
            json!({"kind":"probe","id":id,"phase":phase,"error":carrier(&error.to_wire())})
        }
    };
    trace.lock().unwrap().push(result);
}
struct Reply {
    spec: IvyJson,
    events: Arc<Mutex<Vec<Value>>>,
    connection: Arc<Connection>,
    headers: AuthHeaders,
}
impl Reply {
    fn id(&self) -> String {
        string(&self.spec, "id")
    }
    fn check(&self, phase: &str) -> IvyResult<()> {
        if optional(&self.spec, "probeAt") == Some(phase.into()) {
            probe(
                &self.events,
                &self.connection,
                &self.headers,
                &self.id(),
                phase,
            );
        }
        if optional(&self.spec, "throwAt") == Some(phase.into()) {
            Err(IvyError::internal())
        } else {
            Ok(())
        }
    }
    fn head(&self, status: u16, headers: IvyJson) -> IvyResult<()> {
        self.events.lock().unwrap().push(
            json!({"kind":"head","id":self.id(),"status":status,"headers":carrier(&headers)}),
        );
        self.check(if status == 401 {
            "rejected.head"
        } else {
            "redirect.head"
        })
    }
    fn end(&self, text: Option<&str>) -> IvyResult<()> {
        let mut event = json!({"kind":"end","id":self.id()});
        if let Some(text) = text {
            event["text"] = text.into();
        }
        self.events.lock().unwrap().push(event);
        self.check("end")
    }
}
impl SessionResponse for Reply {
    fn set_cookie(&mut self, value: &JsString) -> IvyResult<()> {
        self.events.lock().unwrap().push(json!({"kind":"header","id":self.id(),"name":"Set-Cookie","valueEncoded":stringify(&IvyJson::String(value.clone()),usize::MAX).unwrap()}));
        self.check("cookie")
    }
    fn redirect(&mut self, target: &JsString) -> IvyResult<()> {
        self.head(
            303,
            IvyJson::object([("Location", IvyJson::String(target.clone()))]),
        )?;
        self.end(None)
    }
    fn login_rejected(&mut self, target: &JsString) -> IvyResult<()> {
        self.head(
            401,
            IvyJson::object([("Content-Type", "text/html; charset=utf-8".into())]),
        )?;
        self.events.lock().unwrap().push(json!({"kind":"page","id":self.id(),"base":carrier(&IvyJson::String(self.connection.authentication().base_path().clone())),"target":carrier(&IvyJson::String(target.clone())),"rejected":true}));
        self.check("page")?;
        self.end(Some("rejected-page"))
    }
}
#[derive(Default)]
struct PeerState {
    items: Vec<Option<Arc<SocketPeer>>>,
}
struct SocketPeer {
    spec: IvyJson,
    context: IvyJson,
    events: Arc<Mutex<Vec<Value>>>,
    state: Weak<Mutex<PeerState>>,
}
fn add_peer(state: &Arc<Mutex<PeerState>>, events: &Arc<Mutex<Vec<Value>>>, spec: IvyJson) {
    let mut held = state.lock().unwrap();
    if held
        .items
        .iter()
        .flatten()
        .any(|p| p.spec.get("id") == spec.get("id"))
    {
        return;
    }
    let context = spec
        .get("context")
        .cloned()
        .unwrap_or_else(|| IvyJson::object([] as [(&str, IvyJson); 0]));
    held.items.push(Some(Arc::new(SocketPeer {
        spec,
        context,
        events: events.clone(),
        state: Arc::downgrade(state),
    })));
}
impl Peer for SocketPeer {
    fn context(&self) -> &IvyJson {
        &self.context
    }
    fn terminate(&self) -> IvyResult<()> {
        self.events
            .lock()
            .unwrap()
            .push(json!({"kind":"terminate","id":string(&self.spec,"id")}));
        if let Some(code) = optional(&self.spec, "error") {
            return Err(isolated(&code.to_utf8().unwrap()));
        }
        let state = self.state.upgrade().unwrap();
        if let Some(remove) = self.spec.get("remove").and_then(IvyJson::as_array) {
            let mut held = state.lock().unwrap();
            for peer in &mut held.items {
                if peer
                    .as_ref()
                    .is_some_and(|p| remove.iter().any(|id| Some(id) == p.spec.get("id")))
                {
                    *peer = None;
                }
            }
        }
        if let Some(append) = self.spec.get("append").and_then(IvyJson::as_array) {
            for spec in append {
                add_peer(&state, &self.events, spec.clone());
            }
        }
        Ok(())
    }
}
struct LivePeers(Arc<Mutex<PeerState>>);
struct PeerIterator {
    state: Arc<Mutex<PeerState>>,
    index: usize,
}
impl Iterator for PeerIterator {
    type Item = Arc<dyn Peer>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let value = {
                let held = self.state.lock().unwrap();
                if self.index >= held.items.len() {
                    return None;
                }
                held.items[self.index].clone()
            };
            self.index += 1;
            if let Some(peer) = value {
                return Some(peer);
            }
        }
    }
}
impl Peers for LivePeers {
    fn iter(&self) -> Box<dyn Iterator<Item = Arc<dyn Peer>> + '_> {
        Box::new(PeerIterator {
            state: self.0.clone(),
            index: 0,
        })
    }
}
async fn raw(actor: &mpsc::UnboundedSender<Posted>, payload: IvyJson) -> IvyJson {
    let (send, receive) = oneshot::channel();
    actor.send((payload, send)).unwrap();
    receive.await.unwrap().unwrap()
}
async fn native_session(input: &Value) -> Value {
    let data = input_json(input);
    let events = Arc::new(Mutex::new(Vec::new()));
    let results: Arc<Mutex<serde_json::Map<String, Value>>> = Default::default();
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
        actor: actor(input),
    });
    let clock = Arc::new(AtomicU64::new(100000.0f64.to_bits()));
    let reading = clock.clone();
    let authentication = Authentication::new(
        AuthOptions {
            canonical_origin: "https://example.test".into(),
            base_path: input["basePath"].as_str().unwrap_or("/ivy").into(),
            trusted_proxy_addresses: input["trusted"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| v.as_str().unwrap().into())
                .collect(),
        },
        providers.clone(),
        providers.clone(),
        Arc::new(move || f64::from_bits(reading.load(Ordering::Relaxed))),
    )
    .unwrap();
    let peers = Rc::new(LivePeers(Arc::default()));
    for spec in data.get("peers").and_then(IvyJson::as_array).unwrap_or(&[]) {
        add_peer(&peers.0, &events, spec.clone());
    }
    let mut sockets: HashMap<String, Arc<Connection>> = HashMap::new();
    let mut running = HashMap::new();
    let mut snapshots = Vec::new();
    for step in data.get("steps").unwrap().as_array().unwrap() {
        let op = string(step, "op");
        match op.as_str() {
            "connect"=>{assert!(sockets.insert(string(step,"id"),Arc::new(authentication.connection(optional(step,"remoteAddress")))).is_none());},
            "login"|"logout"|"auth"=>{
                let connection=sockets[&string(step,"socket")].clone();let spec=step.clone();let id=string(step,"id");let provider=providers.clone();let events=events.clone();let results=results.clone();let peers=peers.clone();
                running.insert(id.clone(),tokio::task::spawn_local(async move {
                    let headers=auth_headers(&spec);let session=Session{connection:&connection,worker:provider.as_ref()};
                    let mut response=Reply{spec:spec.clone(),events,connection:connection.clone(),headers:headers.clone()};
                    let result=if op=="auth" {
                        connection.authenticate(AuthRequest{method:Some(optional(&spec,"method").unwrap_or_else(||"POST".into())),headers,transport:optional(&spec,"transport").unwrap_or_else(||"http".into()),resource:optional(&spec,"resource")}).await
                    } else if op=="login" {
                        let form=Parameters::new(spec.get("form").unwrap().as_array().unwrap().iter().map(|p|{let p=p.as_array().unwrap();(p[0].as_js_string().unwrap().clone(),p[1].as_js_string().unwrap().clone())}));
                        session.login(&headers,&form,&mut response).await.map(|_|IvyJson::Null)
                    } else {let context=spec.get("context").cloned().unwrap_or_else(||IvyJson::object([] as [(&str,IvyJson);0]));session.logout(&headers,&context,peers.as_ref(),&mut response).await.map(|_|IvyJson::Null)};
                    results.lock().unwrap().insert(id,outcome(result));
                }));
            },
            "join"=>for id in step.get("ids").unwrap().as_array().unwrap(){running.remove(&id.as_str().unwrap()).unwrap().await.unwrap();},
            "release"=>providers.gates.lock().unwrap().remove(&string(step,"gate")).unwrap().send(()).unwrap(),
            "clock"=>clock.store(step.get("now").unwrap().as_f64().unwrap().to_bits(),Ordering::Relaxed),
            "probe"=>probe(&events,&sockets[&string(step,"socket")],&auth_headers(step),&string(step,"id"),"explicit"),
            "snapshot"=>snapshots.push(json!({"name":string(step,"name"),"events":std::mem::take(&mut*events.lock().unwrap()),"results":results.lock().unwrap().clone()})),
            "sql"=>{raw(providers.actor.as_ref().unwrap(),IvyJson::object([("action","fixture.sql".into()),("sql",step.get("sql").unwrap().clone())])).await;},
            "returnPath"=>{
                let result=browser_session::return_path(authentication.canonical_origin(),authentication.base_path(),step.get("value").and_then(IvyJson::as_js_string)).map(IvyJson::String);
                results.lock().unwrap().insert(string(step,"id"),outcome(result));
            },
            _=>panic!("Unknown browser session operation"),
        }
        flush().await;
    }
    for (_, work) in running {
        work.await.unwrap();
    }
    flush().await;
    snapshots.push(
        json!({"name":"final","events":*events.lock().unwrap(),"results":*results.lock().unwrap()}),
    );
    let mut output = json!({"snapshots":snapshots});
    if let Some(actor) = &providers.actor {
        output["store"] = json!({"kind":"store","sessions":carrier(&raw(
                actor,
                IvyJson::object([("action", "fixture.sessions".into())]),
            )
            .await)});
    }
    carrier(&IvyJson::from_serde(&output).unwrap())
}
async fn compare_session(name: &str, input: Value) -> Value {
    let original = auth_reference("browser-session", &input);
    let native = tokio::task::LocalSet::new()
        .run_until(native_session(&input))
        .await;
    assert_eq!(native, original, "Browser sessions: {name}");
    native
}
fn connect() -> Value {
    json!({"op":"connect","id":"socket","remoteAddress":"192.0.2.1"})
}
fn login(id: &str) -> Value {
    json!({"op":"login","id":id,"socket":"socket","form":[["token","credential"],["returnTo","/ivy/ui/item?x=1#target"]]})
}
fn logout(id: &str) -> Value {
    json!({"op":"logout","id":id,"socket":"socket","headers":{"origin":"https://example.test","cookie":"ivy_session=token"},"context":{"credentialDigest":"credential","sessionDigest":"session","principalId":"alpha","transport":"http"}})
}
fn join(ids: &[&str]) -> Value {
    json!({"op":"join","ids":ids})
}

#[tokio::test(flavor = "current_thread")]
async fn browser_session_return_paths_keep_constructor_origin_and_getter_rules() {
    let values = [
        None,
        Some(""),
        Some("/ivy"),
        Some("/ivy/"),
        Some("/ivy?"),
        Some("/ivy#"),
        Some("/ivy?#"),
        Some("/ivy?x=1#fragment?more"),
        Some("/ivy?x=1??#a#b"),
        Some("/ivy/../outside"),
        Some("/ivy/%2e%2e/outside"),
        Some("/ivy2/path"),
        Some("//example.test/ivy"),
        Some("https://example.test/ivy"),
        Some("/ivy/\\x"),
        Some("/ivy/%5Cx"),
        Some("/\t/example.test/ivy"),
        Some("/\t/user@example.test/ivy?x=1#f"),
        Some("/\n/other.test/ivy"),
        Some("/ivy/%00?x=a\tb#c"),
        Some("/ivy/🙂?x=ß#fragment"),
    ];
    let mut steps = values
        .into_iter()
        .enumerate()
        .map(|(i, value)| json!({"op":"returnPath","id":format!("path-{i}"),"value":value}))
        .collect::<Vec<_>>();
    steps.push(json!({"op":"returnPath","id":"lone","value":{"$units":[47,105,118,121,47,55296,63,120,61,56320,35,55296]}}));
    compare_session(
        "return-path-origin-prefix-query-fragment-and-usv",
        json!({"steps":steps}),
    )
    .await;
    compare_session("root-return-path",json!({"basePath":"","steps":[{"op":"returnPath","id":"empty","value":null},{"op":"returnPath","id":"path","value":"/any?x=1#hash"}]})).await;
}

#[tokio::test(flavor = "current_thread")]
async fn browser_session_login_validation_reply_shapes_and_response_failures_match_original() {
    let mut steps = vec![connect()];
    for (i, token) in [
        json!(""),
        json!("x".repeat(4097)),
        json!("🙂".repeat(2049)),
        json!({"$units":[55296]}),
        json!("x".repeat(4096)),
    ]
    .into_iter()
    .enumerate()
    {
        let mut step = login(&format!("token-{i}"));
        step["form"][0][1] = token;
        steps.push(step);
        steps.push(join(&[&format!("token-{i}")]));
    }
    let mut absent = login("missing");
    absent["form"] = json!([]);
    steps.push(absent);
    steps.push(join(&["missing"]));
    let mut path = login("bad-path");
    path["form"][1][1] = "/elsewhere".into();
    steps.push(path);
    steps.push(join(&["bad-path"]));
    let mut duplicate = login("first-duplicate");
    duplicate["form"]
        .as_array_mut()
        .unwrap()
        .push(json!(["token", "ignored"]));
    steps.push(duplicate);
    compare_session(
        "login-token-return-path-duplicates-and-utf16-bounds",
        json!({"steps":steps}),
    )
    .await;
    for (i, reply) in [
        json!(null),
        json!({}),
        json!(13),
        json!({"sessionToken":null}),
        json!({"sessionToken":false}),
        json!({"sessionToken":[1,null]}),
        json!({"sessionToken":{"toString":"blocked"}}),
        json!({"sessionToken":{"$units":[55296,65]}}),
    ]
    .into_iter()
    .enumerate()
    {
        compare_session(
            &format!("login-worker-shape-{i}"),
            json!({"workerReplies":[{"value":reply}],"steps":[connect(),login("login")]}),
        )
        .await;
    }
    for (i, phase) in ["cookie", "redirect.head", "end"].into_iter().enumerate() {
        let mut step = login("login");
        step["throwAt"] = phase.into();
        compare_session(&format!("login-success-callback-failure-{i}"),json!({"steps":[connect(),step,json!({"op":"probe","id":"released","socket":"socket"})]})).await;
    }
    for (i, phase) in [None, Some("rejected.head"), Some("page"), Some("end")]
        .into_iter()
        .enumerate()
    {
        let mut step = login("login");
        if let Some(phase) = phase {
            step["throwAt"] = phase.into();
        }
        compare_session(&format!("login-rejected-callback-{i}"),json!({"workerReplies":[{"error":"unauthenticated"}],"steps":[connect(),step,join(&["login"]),json!({"op":"probe","id":"released","socket":"socket"})]})).await;
    }
    compare_session("login-worker-unavailable-release",json!({"workerReplies":[{"error":"service_unavailable"}],"steps":[connect(),login("login"),join(&["login"]),json!({"op":"probe","id":"released","socket":"socket"})]})).await;
}

#[tokio::test(flavor = "current_thread")]
async fn browser_session_login_shares_retained_auth_slots_and_finally_order() {
    for rejected in [false, true] {
        let mut replies = Vec::new();
        let mut steps = vec![connect()];
        for i in 0..64 {
            let id = format!("login-{i}");
            let gate = format!("gate-{i}");
            replies.push(if i == 63 && rejected {
                json!({"gate":gate,"error":"unauthenticated"})
            } else {
                json!({"gate":gate})
            });
            let mut step = login(&id);
            if i == 63 {
                step["probeAt"] = if rejected { "rejected.head" } else { "cookie" }.into();
            }
            steps.push(step);
        }
        steps.push(json!({"op":"auth","id":"auth-full","socket":"socket","method":"GET","headers":{"authorization":"Bearer credential"}}));
        steps.push(join(&["auth-full"]));
        steps.extend([
            json!({"op":"release","gate":"gate-63"}),
            join(&["login-63"]),
            json!({"op":"probe","id":"after-finally","socket":"socket"}),
        ]);
        for i in 0..63 {
            steps.push(json!({"op":"release","gate":format!("gate-{i}")}));
        }
        steps.push(json!({"op":"auth","id":"auth-recovered","socket":"socket","method":"GET","headers":{"authorization":"Bearer credential"}}));
        compare_session(
            if rejected {
                "shared64-slots-rejection-renders-before-release"
            } else {
                "shared64-slots-success-cookie-after-release"
            },
            json!({"workerReplies":replies,"steps":steps}),
        )
        .await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn browser_session_logout_cookie_origin_peer_selection_and_live_iteration_match_original() {
    let peers = json!([
        {"id":"matching","context":{"sessionDigest":"other-session","credentialDigest":"credential"}},
        {"id":"static","context":{"credentialDigest":"credential"}},
        {"id":"different","context":{"sessionDigest":"session","credentialDigest":"different"}},
        {"id":"empty","context":{"sessionDigest":"","credentialDigest":"credential"}},
        {"id":"later","context":{"sessionDigest":true,"credentialDigest":"credential"}}
    ]);
    compare_session(
        "logout-refresh-clear-cookie-and-credential-peer-selection",
        json!({"peers":peers,"steps":[connect(),logout("logout")]}),
    )
    .await;
    for (i, cookie) in [
        None,
        Some("ivy_session="),
        Some("ivy_session=a;ivy_session=b"),
        Some("other=x"),
        Some(" ivy_session=raw-token\u{a0}"),
        Some("\u{feff}ivy_session=bom-token\u{feff}"),
        Some("\u{85}ivy_session=nel-token\u{85}"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut step = logout("logout");
        if let Some(cookie) = cookie {
            step["headers"]["cookie"] = cookie.into();
        } else {
            step["headers"]
                .as_object_mut()
                .unwrap()
                .shift_remove("cookie");
        }
        compare_session(
            &format!("logout-cookie-prefix-{i}"),
            json!({"steps":[connect(),step]}),
        )
        .await;
    }
    for (i, origin) in [
        Value::Null,
        json!("null"),
        json!("https://example.test/"),
        json!(["https://example.test"]),
    ]
    .into_iter()
    .enumerate()
    {
        let mut step = logout("logout");
        step["headers"]["origin"] = origin;
        compare_session(
            &format!("logout-origin-prefix-before-refusal-{i}"),
            json!({"steps":[connect(),step]}),
        )
        .await;
    }
    compare_session("logout-live-set-addition-removal",json!({"peers":[{"id":"first","context":{"sessionDigest":"yes","credentialDigest":"credential"},"remove":["unvisited"],"append":[{"id":"appended","context":{"sessionDigest":"yes","credentialDigest":"credential"}}]},{"id":"unvisited","context":{"sessionDigest":"yes","credentialDigest":"credential"}}],"steps":[connect(),logout("logout")]})).await;
    compare_session("logout-peer-error-stops-before-cookie-clear",json!({"peers":[{"id":"first","context":{"sessionDigest":"yes","credentialDigest":"credential"},"error":"internal_error"},{"id":"later","context":{"sessionDigest":"yes","credentialDigest":"credential"}}],"steps":[connect(),logout("logout")]})).await;
}

#[tokio::test(flavor = "current_thread")]
async fn browser_session_logout_requires_a_typed_credential_before_posting() {
    // These contexts cannot come from successful authentication. The old fake
    // worker accepted them, although the real storage action required a string.
    for context in [
        Value::Null,
        json!({}),
        json!({"sessionDigest":true}),
        json!({"sessionDigest":"s","credentialDigest":null}),
        json!({"sessionDigest":"s","credentialDigest":{"a":1}}),
    ]
    .into_iter()
    {
        let refreshed = context.get("sessionDigest").is_some();
        let mut step = logout("logout");
        step["context"] = context;
        let result = tokio::task::LocalSet::new().run_until(native_session(&json!({
            "peers":[{"id":"missing","context":{"sessionDigest":"s"}}],
            "steps":[connect(),step],
        }))).await;
        let snapshot = result["snapshots"].as_array().unwrap().last().unwrap();
        assert_eq!(snapshot["results"]["logout"]["error"]["data"]["code"], "internal_error");
        let events = snapshot["events"].as_array().unwrap();
        assert_eq!(events.len(), usize::from(refreshed));
        assert!(events.iter().all(|event| event["kind"] == "header"
            && event["valueEncoded"].as_str().unwrap().contains("ivy_session=token;")));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn browser_session_logout_worker_completion_controls_cache_reset_and_sql_session_scope() {
    let digest = ivy_contracts::hash::digest("credential");
    let a = ivy_contracts::hash::digest("session-a");
    let b = ivy_contracts::hash::digest("session-b");
    let seed = format!(
        "INSERT OR IGNORE INTO principals VALUES('alpha');INSERT INTO credentials VALUES('{digest}','alpha',0);INSERT INTO browser_sessions VALUES('{a}','{digest}','2026-01-01'),('{b}','{digest}','2026-01-01');"
    );
    let auth = |id: &str, token: &str| json!({"op":"auth","id":id,"socket":"socket","method":"GET","headers":{"cookie":format!("ivy_session={token}")}});
    for failed in [false, true] {
        let mut out = logout("logout");
        out["headers"]["cookie"] = "ivy_session=session-a".into();
        out["context"] = json!({"credentialDigest":digest,"sessionDigest":a,"principalId":"alpha","transport":"http"});
        let steps = vec![
            connect(),
            auth("before", "session-a"),
            join(&["before"]),
            out,
            json!({"op":"snapshot","name":"worker pending"}),
            auth("while-pending", "session-a"),
            join(&["while-pending"]),
            json!({"op":"release","gate":"end"}),
            join(&["logout"]),
            auth("after", "session-a"),
            join(&["after"]),
            auth("other-session", "session-b"),
        ];
        let result=compare_session(if failed {"logout-worker-failure-preserves-cache-and-sql"}else{"logout-deletes-one-session-resets-cache-and-terminates-other-browser-peer"},json!({"realStore":true,"seedSql":seed,"keys":{"publicKey":"existing","privateKey":"existing"},"workerReplies":[{},if failed{json!({"gate":"end","error":"storage_unavailable"})}else{json!({"gate":"end"})}],"peers":[{"id":"other-browser","context":{"credentialDigest":digest,"sessionDigest":b}},{"id":"static","context":{"credentialDigest":digest}}],"steps":steps})).await;
        assert_eq!(
            result["store"]["sessions"].as_array().unwrap().len(),
            if failed { 2 } else { 1 }
        );
    }
}
