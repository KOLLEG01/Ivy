use super::*;
#[path = "hive_browser_session.rs"]
mod browser_session;
use ivy_hive::http_auth::{
    self, Authentication, Headers as AuthHeaders, OAuth, Options as AuthOptions,
    Request as AuthRequest,
};
use std::sync::atomic::{AtomicU64, Ordering};

struct Providers {
    events: Arc<Mutex<Vec<Value>>>,
    gates: Mutex<HashMap<String, oneshot::Sender<()>>>,
    worker: Mutex<VecDeque<Value>>,
    oauth: Mutex<VecDeque<Value>>,
    actor: Option<mpsc::UnboundedSender<Posted>>,
}
fn isolated(code: &str) -> IvyError {
    if code == "internal_error" {
        IvyError::internal()
    } else {
        IvyError::new(code, "Isolated provider failure.")
    }
}
impl Worker for Providers {
    fn request(&self, payload: ivy_hive::storage::Payload) -> RequestFuture {
        let payload = crate::payload_fixture::json(payload);
        self.events
            .lock()
            .unwrap()
            .push(json!({"kind":"worker","encoded":stringify(&payload,usize::MAX).unwrap()}));
        if let Some(actor) = &self.actor {
            let (send, receive) = oneshot::channel();
            actor.send((payload, send)).unwrap();
            return Box::pin(async move { receive.await.unwrap() });
        }
        let spec=self.worker.lock().unwrap().pop_front().unwrap_or_else(|| {
            if payload.get("action")==Some(&"authenticate".into()) {json!({"value":{"principalId":"alpha"}})}
            else {json!({"value":{"credentialDigest":"session-credential","sessionDigest":carrier(payload.get("sessionDigest").unwrap()),"principalId":"alpha"}})}
        });
        let waiting = spec["gate"].as_str().map(|gate| {
            let (send, receive) = oneshot::channel();
            assert!(
                self.gates
                    .lock()
                    .unwrap()
                    .insert(gate.into(), send)
                    .is_none()
            );
            receive
        });
        Box::pin(async move {
            if let Some(waiting) = waiting {
                waiting.await.unwrap();
            }
            if let Some(code) = spec["error"].as_str() {
                Err(isolated(code))
            } else {
                Ok(IvyJson::from_serde(&spec["value"]).unwrap())
            }
        })
    }
}
impl OAuth for Providers {
    fn authenticate(
        &self,
        token: &JsString,
        resource: Option<&JsString>,
    ) -> IvyResult<Option<IvyJson>> {
        self.events.lock().unwrap().push(json!({"kind":"oauth","encoded":stringify(&IvyJson::Array(vec![IvyJson::String(token.clone()),resource.map(|r|IvyJson::String(r.clone())).unwrap_or(IvyJson::Null)]),usize::MAX).unwrap()}));
        let spec = self
            .oauth
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Value::Null);
        if let Some(code) = spec["error"].as_str() {
            Err(isolated(code))
        } else {
            Ok(spec.get("value").map(|v| IvyJson::from_serde(v).unwrap()))
        }
    }
}
fn text(value: &Value, name: &str) -> Option<JsString> {
    if let Some(units) = value.get(format!("{name}Units")) {
        return Some(JsString(
            units
                .as_array()
                .unwrap()
                .iter()
                .map(|v| u16::try_from(v.as_u64().unwrap()).unwrap())
                .collect(),
        ));
    }
    value.get(name).map(|v| v.as_str().unwrap().into())
}
fn headers(value: &Value) -> AuthHeaders {
    AuthHeaders {
        authorization: text(value, "authorization"),
        cookie: text(value, "cookie"),
        origin: value.get("origin").map(|v| IvyJson::from_serde(v).unwrap()),
        forwarded_client_ip: value
            .get("x-ivy-client-ip")
            .map(|v| IvyJson::from_serde(v).unwrap()),
    }
}
fn outcome(result: IvyResult<IvyJson>) -> Value {
    match result {
        Ok(value) => json!({"encoded":stringify(&value,usize::MAX).unwrap()}),
        Err(error) => json!({"error":carrier(&error.to_wire())}),
    }
}
fn options(input: &Value, trusted: Vec<JsString>) -> AuthOptions {
    AuthOptions {
        canonical_origin: "https://example.test".into(),
        base_path: input["basePath"].as_str().unwrap_or("/ivy").into(),
        trusted_proxy_addresses: trusted,
    }
}
async fn native_auth(input: &Value) -> Value {
    let events = Arc::new(Mutex::new(Vec::new()));
    let queue = |name| input[name].as_array().cloned().unwrap_or_default().into();
    let providers = Arc::new(Providers {
        events: events.clone(),
        gates: Mutex::default(),
        worker: Mutex::new(queue("workerReplies")),
        oauth: Mutex::new(queue("oauthReplies")),
        actor: actor(input),
    });
    let now = Arc::new(AtomicU64::new(100000.0f64.to_bits()));
    let clock = {
        let now = now.clone();
        Arc::new(move || f64::from_bits(now.load(Ordering::SeqCst)))
    };
    if input["mode"] == "ip" {
        let rows = input["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|candidate| {
                match Authentication::new(
                    options(input, vec![candidate.as_str().unwrap().into()]),
                    providers.clone(),
                    providers.clone(),
                    clock.clone(),
                ) {
                    Ok(_) => json!({"valid":true}),
                    Err(error) => json!({"error":carrier(&error.to_wire())}),
                }
            })
            .collect::<Vec<_>>();
        return json!({"rows":rows});
    }
    let trusted = input["trusted"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| v.as_str().unwrap().into())
        .collect();
    let authentication = match Authentication::new(
        options(input, trusted),
        providers.clone(),
        providers.clone(),
        clock,
    ) {
        Ok(value) => value,
        Err(error) => return json!({"constructionError":carrier(&error.to_wire())}),
    };
    let mut sockets = HashMap::new();
    let results = Arc::new(Mutex::new(serde_json::Map::new()));
    let mut running = HashMap::new();
    let mut snapshots = vec![];
    for step in input["steps"].as_array().unwrap() {
        let operation = step["op"].as_str().unwrap();
        match operation {
            "connect"=>{assert!(sockets.insert(step["id"].as_str().unwrap().to_owned(),authentication.connection(text(step,"remoteAddress"))).is_none());}
            "auth"|"abandon"=>{
                let request=AuthRequest {method:Some(step["method"].as_str().unwrap_or("GET").into()),headers:headers(&step["headers"]),transport:step["transport"].as_str().unwrap_or("http").into(),resource:text(step,"resource")};
                let work=sockets.get(step["socket"].as_str().unwrap()).unwrap().authenticate(request);
                if operation=="auth" {
                    let held=results.clone();let id=step["id"].as_str().unwrap().to_owned();
                    assert!(running.insert(id.clone(),tokio::task::spawn_local(async move {let result=work.await;held.lock().unwrap().insert(id,outcome(result));})).is_none());
                }else{drop(work);}
            }
            "join"=>{for id in step["ids"].as_array().unwrap(){running.remove(id.as_str().unwrap()).unwrap().await.unwrap();}}
            "release"=>providers.gates.lock().unwrap().remove(step["gate"].as_str().unwrap()).unwrap().send(()).unwrap(),
            "clock"=>now.store(step["now"].as_f64().unwrap().to_bits(),Ordering::SeqCst),
            "reset"=>authentication.reset(),
            "close"=>sockets.get(step["socket"].as_str().unwrap()).unwrap().close(),
            "sql"=>{let(send,receive)=oneshot::channel();providers.actor.as_ref().unwrap().send((IvyJson::object([("action","fixture.sql".into()),("sql",step["sql"].as_str().unwrap().into())]),send)).unwrap();receive.await.unwrap().unwrap();}
            "snapshot"=>snapshots.push(json!({"name":step["name"],"events":std::mem::take(&mut *events.lock().unwrap()),"results":*results.lock().unwrap()})),
            _=>{
                let value=match operation {
                    "token"=>Ok(http_auth::session_token(headers(&step["headers"]).cookie.as_ref()).map(IvyJson::String).unwrap_or(IvyJson::Null)),
                    "cookie"=>Ok(IvyJson::String(authentication.cookie(&text(step,"value").unwrap(),step["clear"]==true))),
                    "address"=>sockets.get(step["socket"].as_str().unwrap()).unwrap().client_address(&headers(&step["headers"])).map(IvyJson::String),
                    "origin"=>http_auth::same_origin(&"https://example.test".into(),headers(&step["headers"]).origin.as_ref()).map(|_|IvyJson::Null),
                    "mcpOrigin"=>http_auth::mcp_origin(&"https://example.test".into(),headers(&step["headers"]).origin.as_ref()).map(|_|IvyJson::Null),
                    _=>panic!("Unknown fixture command"),
                };
                results.lock().unwrap().insert(step["id"].as_str().unwrap().into(),outcome(value));
            }
        }
        if step["noFlush"] != true {
            flush().await;
        }
    }
    for (_, work) in running {
        work.await.unwrap();
    }
    flush().await;
    snapshots.push(
        json!({"name":"final","events":*events.lock().unwrap(),"results":*results.lock().unwrap()}),
    );
    carrier(&IvyJson::from_serde(&json!({"snapshots":snapshots})).unwrap())
}
async fn compare_auth(name: &str, input: Value) -> Value {
    let original = auth_reference("http-auth", &input);
    let native = tokio::task::LocalSet::new()
        .run_until(native_auth(&input))
        .await;
    assert_eq!(native, original, "HTTP authentication: {name}");
    native
}
fn connect(id: &str, remote: &str) -> Value {
    json!({"op":"connect","id":id,"remoteAddress":remote})
}
fn auth(id: &str, socket: &str, headers: Value) -> Value {
    json!({"op":"auth","id":id,"socket":socket,"headers":headers})
}
fn bearer(id: &str, socket: &str, token: &str) -> Value {
    auth(
        id,
        socket,
        json!({"authorization":format!("Bearer {token}")}),
    )
}
fn snapshot(name: &str) -> Value {
    json!({"op":"snapshot","name":name})
}
fn final_results(value: &Value) -> &serde_json::Map<String, Value> {
    value["snapshots"].as_array().unwrap().last().unwrap()["results"]
        .as_object()
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn http_auth_cookie_headers_origin_proxy_and_ip_policy_match_original() {
    let mut steps = vec![
        connect("peer", "::ffff:127.0.0.1"),
        connect("external", "192.0.2.10"),
        json!({"op":"connect","id":"local"}),
    ];
    for (i, cookie) in [
        None,
        Some(""),
        Some("unrelated=x"),
        Some("ivy_session="),
        Some(" ivy_session=one\u{a0};other=x"),
        Some("ivy_session=one;ivy_session=two"),
        Some("ivy_session=one; IVY_session=two"),
        Some("ivy_session=one=two"),
        Some("ivy_session=one;ivy_session="),
    ]
    .into_iter()
    .enumerate()
    {
        steps.push(json!({"op":"token","id":format!("token-{i}"),"headers":cookie.map(|value|json!({"cookie":value})).unwrap_or(json!({}))}));
    }
    steps.push(json!({"op":"token","id":"token-utf16","headers":{"cookieUnits":[105,118,121,95,115,101,115,115,105,111,110,61,55296]}}));
    for (clear, id) in [(false, "set"), (true, "clear")] {
        steps.push(json!({"op":"cookie","id":id,"value":"opaque-session","clear":clear}));
    }
    for (i, origin) in [
        None,
        Some(json!(null)),
        Some(json!(["https://example.test"])),
        Some(json!("null")),
        Some(json!("https://example.test")),
        Some(json!("https://example.test/")),
        Some(json!("https://EXAMPLE.test:443")),
        Some(json!("https://example.test/#")),
        Some(json!("https://user@example.test")),
        Some(json!("https://example.test/..")),
        Some(json!("http://example.test")),
        Some(json!("broken")),
    ]
    .into_iter()
    .enumerate()
    {
        for op in ["origin", "mcpOrigin"] {
            steps.push(json!({"op":op,"id":format!("{op}-{i}"),"headers":origin.as_ref().map(|v|json!({"origin":v})).unwrap_or(json!({}))}));
        }
    }
    for (i, forwarded) in [
        None,
        Some(json!("192.0.2.1")),
        Some(json!("::ffff:192.0.2.1")),
        Some(json!("fe80::1%eth0")),
        Some(json!("fe80::1%bad_")),
        Some(json!("192.0.2.1, 192.0.2.2")),
        Some(json!(["192.0.2.1"])),
        Some(json!(" 192.0.2.1")),
    ]
    .into_iter()
    .enumerate()
    {
        for socket in ["peer", "external", "local"] {
            steps.push(json!({"op":"address","id":format!("address-{socket}-{i}"),"socket":socket,"headers":forwarded.as_ref().map(|v|json!({"x-ivy-client-ip":v})).unwrap_or(json!({}))}));
        }
    }
    compare_auth(
        "cookie-origin-mcp-and-exact-proxy-boundaries",
        json!({"trusted":["127.0.0.1"],"steps":steps}),
    )
    .await;
    compare_auth(
        "root-cookie-path",
        json!({"basePath":"","steps":[{"op":"cookie","id":"root","value":"","clear":true}]}),
    )
    .await;
    let mut candidates = vec![
        "0.0.0.0",
        "255.255.255.255",
        "01.2.3.4",
        "1.2.3",
        "1.2.3.4\n",
        "::",
        "::1",
        "[::1]",
        "::ffff:192.0.2.1",
        "::ffff:0192.0.2.1",
        "1:2:3:4:5:6:7:8",
        "1:2:3:4:5:6:7:8:9",
        "fe80::1%zone",
        "fe80::1%zone-.:1",
        "fe80::1%zone_",
        "fe80::1%",
        "::%one%two",
        "example.test",
        "127.0.0.1/32",
        "",
        "::1\n",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    for a in [-1, 0, 1, 9, 10, 99, 100, 199, 200, 249, 250, 255, 256, 999] {
        for b in ["0", "1", "01", "255", "256"] {
            candidates.push(format!("{a}.{b}.2.3"));
        }
    }
    for n in 0..128 {
        for suffix in ["", "%if0", "%bad_", "%1-.:z"] {
            candidates.push(format!("{:x}:db8::{:x}{suffix}", n * 513, n * 17));
        }
    }
    compare_auth(
        "native-ip-leaf-vs-pinned-node-net",
        json!({"mode":"ip","candidates":candidates}),
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn http_auth_bearer_session_and_oauth_decision_order_match_original() {
    let mut steps = vec![connect("s", "192.0.2.1")];
    for (i, value) in [
        "",
        "bearer token",
        "Bearer",
        "Bearer ",
        "Bearer  token",
        " Bearer token",
        "Bearer token ",
        "Bearer a\tb",
        "Bearer a\nb",
        "Bearer a\u{a0}b",
        "Bearer a\u{feff}b",
        "Bearer a\u{200b}b",
        "Bearer a\u{85}b",
    ]
    .into_iter()
    .enumerate()
    {
        steps.push(auth(
            &format!("header-{i}"),
            "s",
            json!({"authorization":value}),
        ));
    }
    steps.push(auth(
        "surrogate-bearer",
        "s",
        json!({"authorizationUnits":[66,101,97,114,101,114,32,55296]}),
    ));
    steps.push(auth("empty-cookie", "s", json!({"cookie":"ivy_session="})));
    steps.push(auth("missing-cookie", "s", json!({})));
    steps.push(json!({"op":"auth","id":"cookie-post-origin","socket":"s","method":"POST","headers":{"cookie":"ivy_session=x","origin":"https://other.test"}}));
    steps.push(json!({"op":"auth","id":"cookie-ws-origin","socket":"s","transport":"ws","headers":{"cookie":"ivy_session=x"}}));
    steps.push(json!({"op":"auth","id":"bearer-ws-origin","socket":"s","transport":"ws","headers":{"authorization":"Bearer static-ws","origin":"https://other.test"}}));
    let result = compare_auth("bearer-cookie-and-origin-selection", json!({"steps":steps})).await;
    assert_eq!(
        final_results(&result)["cookie-post-origin"]["error"]["data"]["code"],
        "unauthenticated"
    );
    let value = json!({"oauthReplies":[{"value":{"principalId":"oauth","credentialDigest":"bound","transport":"overridden","expiresAt":100001}},{"error":"forbidden"},{"value":false}],"steps":[connect("s","192.0.2.1"),bearer("oauth","s","oauth"),bearer("oauth-error","s","bad"),bearer("falsy-fallback","s","static")]});
    compare_auth("required-oauth-before-static-worker", value).await;
}
#[tokio::test(flavor = "current_thread")]
async fn http_auth_pending_identities_reset_close_and_abandoned_observers_match_original() {
    let steps = vec![
        connect("s", "192.0.2.1"),
        bearer("one", "s", "one"),
        json!({"op":"auth","id":"same-ws","socket":"s","transport":"ws","headers":{"authorization":"Bearer one"}}),
        snapshot("coalesced"),
        json!({"op":"reset"}),
        bearer("new-epoch", "s", "one"),
        snapshot("separate-epoch"),
        json!({"op":"close","socket":"s"}),
        bearer("after-close", "s", "one"),
        json!({"op":"release","gate":"old"}),
        json!({"op":"release","gate":"new"}),
        json!({"op":"join","ids":["one","same-ws","new-epoch","after-close"]}),
        snapshot("all-original-waiters-complete"),
    ];
    compare_auth("pending-reset-and-close-do-not-cancel-original-work",json!({"workerReplies":[{"gate":"old","value":{"principalId":"old"}},{"gate":"new","value":{"principalId":"new"}},{"value":{"principalId":"after"}}],"steps":steps})).await;
    let steps = vec![
        connect("s", "192.0.2.1"),
        json!({"op":"abandon","id":"discard","socket":"s","headers":{"authorization":"Bearer token"}}),
        bearer("joined", "s", "token"),
        snapshot("observer-dropped"),
        json!({"op":"release","gate":"failed"}),
        json!({"op":"join","ids":["joined"]}),
        bearer("retry", "s", "token"),
    ];
    compare_auth("abandoned-observation-retains-work-then-evicts-failure",json!({"workerReplies":[{"gate":"failed","error":"unauthenticated"},{"value":{"principalId":"retry"}}],"steps":steps})).await;
    let mut steps = vec![connect("s", "192.0.2.1")];
    for (i, (cookie, resource)) in [
        (None, None),
        (Some("other=x"), None),
        (None, Some("")),
        (None, Some("resource")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut step = bearer(&format!("identity-{i}"), "s", "token");
        if let Some(cookie) = cookie {
            step["headers"]["cookie"] = cookie.into();
        }
        if let Some(resource) = resource {
            step["resource"] = resource.into();
        }
        steps.push(step);
    }
    steps.push(bearer("same-default", "s", "token"));
    compare_auth(
        "raw-cookie-and-resource-key-partition",
        json!({"steps":steps}),
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn http_auth_expiry_failures_and_connection_identity_capacity_match_original() {
    let mut steps = vec![
        connect("s", "192.0.2.1"),
        bearer("valid", "s", "expiring"),
        json!({"op":"clock","now":100001}),
        bearer("expired", "s", "expiring"),
        bearer("expired-again", "s", "expiring"),
    ];
    for i in 0..15 {
        steps.push(bearer(&format!("identity-{i}"), "s", &format!("token-{i}")));
    }
    steps.push(bearer("capacity", "s", "seventeenth"));
    steps.push(json!({"op":"reset"}));
    steps.push(bearer("after-reset", "s", "expiring"));
    compare_auth("cached-expiry-keeps-identity-slot-until-reset",json!({"oauthReplies":[{"value":{"principalId":"oauth","expiresAt":100001}}],"steps":steps})).await;
    for (expiry, name) in [
        (json!(100000), "equal"),
        (json!(99999), "past"),
        (json!(null), "null"),
        (json!("100001"), "string"),
        (json!({"toString":"not callable"}), "coercion-error"),
    ] {
        compare_auth(&format!("initial-expiry-{name}"),json!({"oauthReplies":[{"value":{"principalId":"oauth","expiresAt":expiry}}],"steps":[connect("s","192.0.2.1"),bearer("first","s","token"),bearer("retry","s","token")]})).await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn http_auth_arrival_and_retained_slot_limits_recover_without_cache_poisoning() {
    let mut steps = vec![];
    let mut replies = vec![];
    for i in 0..65 {
        let socket = format!("s{i}");
        steps.push(connect(&socket, "192.0.2.1"));
        steps.push(bearer(&format!("r{i}"), &socket, &format!("t{i}")));
        if i < 64 {
            replies.push(json!({"gate":format!("g{i}"),"value":{"principalId":"alpha"}}));
        }
    }
    steps.push(snapshot("per-client-retained-limit"));
    for i in 0..64 {
        steps.push(json!({"op":"release","gate":format!("g{i}")}));
    }
    steps.push(bearer("recovered", "s64", "new"));
    compare_auth(
        "64-retained-authentications-per-client",
        json!({"workerReplies":replies,"steps":steps}),
    )
    .await;
    let mut steps = vec![];
    for i in 0..257 {
        let socket = format!("s{i}");
        steps.push(connect(&socket, "192.0.2.1"));
        steps.push(auth(
            &format!("bad-{i}"),
            &socket,
            json!({"authorization":"invalid"}),
        ));
    }
    steps.push(json!({"op":"clock","now":100999}));
    steps.push(bearer("before-window", "s256", "valid"));
    steps.push(json!({"op":"clock","now":101000}));
    steps.push(bearer("at-window", "s256", "valid"));
    compare_auth(
        "arrival-rejections-count-and-window-boundary",
        json!({"steps":steps}),
    )
    .await;
    let mut steps = vec![];
    let mut replies = vec![];
    for i in 0..513 {
        let socket = format!("global-{i}");
        steps.push(connect(&socket, &format!("198.18.{}.{}", i / 256, i % 256)));
        if i == 512 {
            steps.push(json!({"op":"clock","now":101000}));
        }
        steps.push(bearer(
            &format!("request-{i}"),
            &socket,
            &format!("token-{i}"),
        ));
        if i < 512 {
            replies
                .push(json!({"gate":format!("global-gate-{i}"),"value":{"principalId":"alpha"}}));
        }
    }
    steps.push(snapshot("global-retained-budget-after-new-arrival-window"));
    for i in 0..512 {
        steps.push(json!({"op":"release","gate":format!("global-gate-{i}")}));
    }
    steps.push(bearer("global-recovered", "global-512", "new"));
    compare_auth(
        "512-retained-global-authentications-across-arrival-window",
        json!({"workerReplies":replies,"steps":steps}),
    )
    .await;
    let mut steps = vec![];
    for i in 0..513 {
        let socket = format!("arrival-{i}");
        steps.push(connect(&socket, &format!("198.19.{}.{}", i / 256, i % 256)));
        steps.push(auth(
            &format!("invalid-{i}"),
            &socket,
            json!({"authorization":"invalid"}),
        ));
    }
    steps.push(json!({"op":"clock","now":101000}));
    steps.push(bearer("global-arrival-recovered", "arrival-512", "valid"));
    compare_auth(
        "512-global-arrivals-then-window-recovery",
        json!({"steps":steps}),
    )
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn http_auth_real_sqlite_credential_session_revocation_and_reset_match_original() {
    let credential = ivy_contracts::hash::digest("static-credential");
    let session = ivy_contracts::hash::digest("session-token");
    let mut cookie = auth(
        "cookie-first",
        "browser",
        json!({"cookie":"ivy_session=session-token"}),
    );
    cookie["method"] = "POST".into();
    cookie["headers"]["origin"] = "https://example.test".into();
    let mut same = cookie.clone();
    same["id"] = "cookie-bound".into();
    let mut rebound = cookie.clone();
    rebound["id"] = "cookie-rebind".into();
    let steps = vec![
        connect("browser", "192.0.2.1"),
        connect("bearer", "192.0.2.1"),
        cookie,
        bearer("static-first", "bearer", "static-credential"),
        json!({"op":"sql","sql":format!("DELETE FROM browser_sessions WHERE digest='{session}';")}),
        same,
        json!({"op":"reset"}),
        rebound,
        json!({"op":"sql","sql":"UPDATE credentials SET revoked=1;"}),
        bearer("revoked", "bearer", "static-credential"),
    ];
    let value = json!({"realStore":true,"keys":{},"seedSql":format!("INSERT OR IGNORE INTO principals VALUES('alpha');INSERT INTO credentials VALUES('{credential}','alpha',0);INSERT INTO browser_sessions VALUES('{session}','{credential}','2026-01-01');"),"steps":steps});
    let result = compare_auth(
        "actual-store-bind-revoke-reset-and-session-resolution",
        value,
    )
    .await;
    assert!(final_results(&result)["cookie-bound"]["encoded"].is_string());
    assert_eq!(
        final_results(&result)["cookie-rebind"]["error"]["data"]["code"],
        "unauthenticated"
    );
}
