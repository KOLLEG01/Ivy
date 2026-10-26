use super::*;
use ivy_hive::{
    http_auth::{self, Headers, Request},
    server_constructor::Constructed,
};
use serde_json::{Value, json};
use std::sync::atomic::AtomicU64;
use tokio::sync::oneshot;

type Observations = Arc<Mutex<serde_json::Map<String, Value>>>;
struct AuthWorker {
    credential: JsString,
    events: Arc<Mutex<Vec<Value>>>,
    replies: Mutex<HashMap<String, VecDeque<Value>>>,
    gates: Mutex<HashMap<String, oneshot::Sender<()>>>,
}
impl AuthWorker {
    fn post(&self, payload: IvyJson) -> Reply {
        self.events
            .lock()
            .unwrap()
            .push(json!({"kind":"worker","payload":payload.to_serde().unwrap()}));
        let action = payload.get("action").unwrap().as_str().unwrap();
        let spec = self
            .replies
            .lock()
            .unwrap()
            .get_mut(&action)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Value::Null);
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
        let fallback = match action.as_str() {
            "authenticate" => obj([("principalId", "a".into())]),
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
        Box::pin(async move {
            if let Some(waiting) = waiting {
                waiting.await.unwrap();
            }
            if let Some(code) = spec["error"].as_str() {
                return Err(IvyError::new(
                    code,
                    spec["message"]
                        .as_str()
                        .unwrap_or("Isolated storage failure."),
                ));
            }
            Ok(spec
                .get("value")
                .map(|v| IvyJson::from_serde(v).unwrap())
                .unwrap_or(fallback))
        })
    }
}
impl Worker for AuthWorker {
    fn request(&self, payload: IvyJson) -> browser_push::RequestFuture {
        self.post(payload)
    }
}
impl Storage for AuthWorker {
    fn request(&self, payload: IvyJson, _: RequestOptions) -> Reply {
        self.post(payload)
    }
}
impl PackageStorage for AuthWorker {
    fn request(&self, payload: IvyJson) -> PackageRequest {
        self.post(payload)
    }
}
impl StorageLifecycle for AuthWorker {
    fn start(&self) -> Reply<()> {
        panic!("No storage launch in authentication composition")
    }
    fn close(&self) -> Reply<()> {
        panic!("No storage shutdown in authentication composition")
    }
}
fn text(v: &Value, name: &str) -> Option<JsString> {
    v.get(name).map(|v| v.as_str().unwrap().into())
}
fn credentials(v: &Value) -> Vec<Credential> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|v| Credential {
            principal_id: text(v, "principalId").unwrap(),
            digest: text(v, "digest").unwrap(),
        })
        .collect()
}
fn headers(v: &Value) -> Headers {
    Headers {
        authorization: text(v, "authorization"),
        cookie: text(v, "cookie"),
        origin: v.get("origin").map(|v| IvyJson::from_serde(v).unwrap()),
        forwarded_client_ip: v
            .get("x-ivy-client-ip")
            .map(|v| IvyJson::from_serde(v).unwrap()),
    }
}
fn outcome(v: IvyResult<IvyJson>) -> Value {
    match v {
        Ok(v) => json!({"encoded":encode(&v)}),
        Err(e) => json!({"error":e.to_wire().to_serde().unwrap()}),
    }
}
fn record(results: &Observations, id: &str, v: IvyResult<IvyJson>) {
    results.lock().unwrap().insert(id.into(), outcome(v));
}
async fn mint(server: &Constructed<AuthWorker>, credential: JsString) -> IvyResult<IvyJson> {
    let registered = server.oauth.register(&super::json(
        r#"{"redirect_uris":["https://client.invalid/callback"]}"#,
    ))?;
    let client = string(&registered, "client_id");
    let verifier = "v".repeat(43);
    let challenge = {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        use sha2::{Digest, Sha256};
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    };
    let pending = server.oauth.begin(&parameters([
        ("response_type", "code".into()),
        ("client_id", client.clone()),
        ("redirect_uri", "https://client.invalid/callback".into()),
        ("state", "fixture".into()),
        ("code_challenge", challenge.into()),
        ("code_challenge_method", "S256".into()),
        ("resource", server.oauth.resource().clone()),
    ]))?;
    let redirect = server
        .oauth
        .authorize(&string(&pending, "transaction"), &credential)?;
    let code = Parameters::parse_form(&redirect_query(&redirect))
        .get("code")
        .unwrap()
        .clone();
    server
        .oauth
        .token(&parameters([
            ("grant_type", "authorization_code".into()),
            ("client_id", client),
            ("resource", server.oauth.resource().clone()),
            ("redirect_uri", "https://client.invalid/callback".into()),
            ("code", code),
            ("code_verifier", verifier.into()),
        ]))
        .await
}
async fn flush() {
    for _ in 0..128 {
        tokio::task::yield_now().await;
    }
}
async fn native(trace: &Value) -> Value {
    let events = Arc::new(Mutex::new(vec![]));
    let worker = Arc::new(AuthWorker {
        credential: trace["options"]["credentials"][0]["digest"]
            .as_str()
            .unwrap()
            .into(),
        events: events.clone(),
        gates: Mutex::default(),
        replies: Mutex::new(
            trace["replies"]
                .as_object()
                .map(|v| {
                    v.iter()
                        .map(|(k, v)| (k.clone(), v.as_array().unwrap().iter().cloned().collect()))
                        .collect()
                })
                .unwrap_or_default(),
        ),
    });
    let now = Arc::new(AtomicU64::new(100000.0f64.to_bits()));
    let mut dependencies = dependencies(Events::default());
    dependencies.now = {
        let now = now.clone();
        Arc::new(move || f64::from_bits(now.load(AtomicOrdering::SeqCst)))
    };
    let server = Prepared::new(options(&IvyJson::from_serde(&trace["options"]).unwrap()))
        .unwrap()
        .bind(worker.clone(), dependencies, Admission::default())
        .unwrap();
    server
        .oauth
        .initialize(&oauth::Secrets {
            signing_key: "11".repeat(32).into(),
            reference_key: "22".repeat(32).into(),
        })
        .unwrap();
    assert!(events.lock().unwrap().is_empty());
    let mut sockets = HashMap::new();
    let results = Observations::default();
    let mut running = HashMap::new();
    let mut snapshots = vec![];
    let mut tokens: HashMap<String, JsString> = HashMap::new();
    for step in trace["steps"].as_array().unwrap() {
        let op = step["op"].as_str().unwrap();
        let id = step["id"].as_str().unwrap_or("");
        match op {
            "connect"=>{assert!(sockets.insert(id.to_owned(),server.authentication.connection(text(step,"remoteAddress"))).is_none());}
            "auth"|"abandon"=>{
                let mut headers=headers(step.get("headers").unwrap_or(&Value::Null));
                if let Some(token)=step["token"].as_str(){let mut authorization=JsString::from("Bearer ");authorization.0.extend(&tokens[token].0);headers.authorization=Some(authorization);}
                let work=sockets[step["socket"].as_str().unwrap()].authenticate(Request{method:Some(step["method"].as_str().unwrap_or("GET").into()),headers,transport:step["transport"].as_str().unwrap_or("http").into(),resource:text(step,"resource")});
                if op=="auth" {let results=results.clone();let id=id.to_owned();running.insert(id.clone(),tokio::spawn(async move{record(&results,&id,work.await);}));}else{drop(work);}
            }
            "configure"|"abandonConfigure"=>{
                let work=server.network.lifecycle.set_credentials(credentials(&step["credentials"]));
                if op=="configure" {let results=results.clone();let id=id.to_owned();running.insert(id.clone(),tokio::spawn(async move{record(&results,&id,work.await.map(|()|IvyJson::Null));}));}else{drop(work);}
            }
            "join"=>{for id in step["ids"].as_array().unwrap(){running.remove(id.as_str().unwrap()).expect("Missing observation").await.unwrap();}}
            "release"=>{worker.gates.lock().unwrap().remove(step["gate"].as_str().unwrap()).expect("Missing gate").send(()).unwrap();}
            "failure"=>{server.network.lifecycle.event_sink()(Event::Failure(IvyError::new("storage_unavailable","Isolated worker failure.")));}
            "clock"=>now.store(step["now"].as_f64().unwrap().to_bits(),AtomicOrdering::SeqCst),
            "close"=>sockets[step["socket"].as_str().unwrap()].close(),
            "snapshot"=>snapshots.push(json!({"name":step["name"],"events":std::mem::take(&mut *events.lock().unwrap()),"results":*results.lock().unwrap()})),
            _=>{
                let value=match op {
                    "mint"=>{
                        let value=mint(&server,text(step,"digest").unwrap_or_else(||trace["options"]["credentials"][0]["digest"].as_str().unwrap().into())).await;
                        if let Ok(value)=&value {tokens.insert(id.into(),string(value,"access_token"));}
                        value
                    }
                    "cookie"=>Ok(IvyJson::String(server.authentication.cookie(&text(step,"value").unwrap(),step["clear"]==true))),
                    "address"=>sockets[step["socket"].as_str().unwrap()].client_address(&headers(&step["headers"])).map(IvyJson::String),
                    "origin"=>http_auth::same_origin(server.authentication.canonical_origin(),headers(&step["headers"]).origin.as_ref()).map(|()|IvyJson::Null),
                    "mcpOrigin"=>http_auth::mcp_origin(server.authentication.canonical_origin(),headers(&step["headers"]).origin.as_ref()).map(|()|IvyJson::Null),
                    _=>panic!("Unknown composed authentication command {op}"),
                };
                record(&results,id,value);
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
    json!({"snapshots":snapshots})
}
fn compare(group: &str, traces: Vec<Value>) -> Vec<Value> {
    let original = lifetime_reference("authentication", &traces);
    assert_eq!(original.len(), traces.len());
    let mut observed = vec![];
    for (input, original) in traces.into_iter().zip(original) {
        let native = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(native(&input));
        let snapshots = input["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|step| step["op"] == "snapshot")
            .count()
            + 1;
        assert_eq!(original["snapshots"].as_array().unwrap().len(), snapshots);
        assert_eq!(native["snapshots"].as_array().unwrap().len(), snapshots);
        assert_eq!(
            native, original,
            "Authentication composition: {group}, {input}"
        );
        observed.push(native);
    }
    observed
}
fn trace(name: &str, steps: Vec<Value>) -> Value {
    json!({"name":name,"options":{"filename":"state/hive.sqlite","publicBaseUrl":"https://EXAMPLE.test:443/ivy/","version":"1.2.3","buildId":"build:test","credentials":[{"principalId":"a","digest":digest("initial")}],"trustedProxyAddresses":["127.0.0.1"]},"steps":steps})
}
fn connect() -> Value {
    json!({"op":"connect","id":"s","remoteAddress":"::ffff:127.0.0.1"})
}
fn bearer(id: &str) -> Value {
    json!({"op":"auth","id":id,"socket":"s","headers":{"authorization":"Bearer initial"}})
}
fn access(id: &str) -> Value {
    json!({"op":"auth","id":id,"socket":"s","token":"access"})
}
fn configure(op: &str) -> Value {
    json!({"op":op,"id":"configure","credentials":[{"principalId":"b","digest":digest("rotated")}]})
}
fn final_results(value: &Value) -> &Value {
    &value["snapshots"].as_array().unwrap().last().unwrap()["results"]
}

#[test]
fn original_composed_authentication_origin_resource_cookie_and_transport() {
    let mut traces = vec![];
    for (base, origin, path) in [
        (
            "https://EXAMPLE.test:443/ivy/",
            "https://example.test",
            "/ivy",
        ),
        ("https://example.test", "https://example.test", ""),
        ("http://127.1:8080/a//", "http://127.0.0.1:8080", "/a/"),
        (
            "https://bücher.test/路径/",
            "https://xn--bcher-kva.test",
            "/%E8%B7%AF%E5%BE%84",
        ),
    ] {
        let mut second = bearer("cached-transport");
        second["transport"] = json!("ws");
        let cookie = json!({"op":"auth","id":"session","socket":"s","headers":{"cookie":"ivy_session=session"}});
        let mut cookie_post = cookie.clone();
        cookie_post["id"] = json!("cached-cookie-origin");
        cookie_post["method"] = json!("POST");
        cookie_post["headers"]["origin"] = json!(origin);
        let mut bad = cookie_post.clone();
        bad["id"] = json!("bad-origin");
        bad["headers"]["origin"] = json!("https://elsewhere.invalid");
        let mut resource = origin.to_string();
        resource.push_str(path);
        resource.push_str("/mcp");
        let mut oauth = access("oauth-resource");
        oauth["transport"] = json!("mcp");
        oauth["resource"] = json!(resource);
        let mut cached = oauth.clone();
        cached["id"] = json!("oauth-cached-transport");
        cached["transport"] = json!("ws");
        let mut wrong = oauth.clone();
        wrong["id"] = json!("wrong-resource");
        wrong["resource"] = json!(format!("{origin}{path}/mcp-dev"));
        let mut expired = cached.clone();
        expired["id"] = json!("expired-cached");
        let steps = vec![
            connect(),
            json!({"op":"address","id":"forwarded","socket":"s","headers":{"x-ivy-client-ip":"::ffff:192.0.2.1"}}),
            json!({"op":"cookie","id":"cookie","value":"session"}),
            json!({"op":"cookie","id":"clear-cookie","value":"","clear":true}),
            json!({"op":"origin","id":"origin","headers":{"origin":origin}}),
            json!({"op":"mcpOrigin","id":"mcp-origin","headers":{"origin":format!("{origin}/")}}),
            bearer("raw"),
            second,
            cookie,
            cookie_post,
            bad,
            json!({"op":"mint","id":"access"}),
            oauth,
            cached,
            wrong,
            json!({"op":"clock","now":1000000}),
            expired,
        ];
        let mut t = trace(base, steps);
        t["options"]["publicBaseUrl"] = json!(base);
        traces.push(t);
    }
    let checked = compare("canonical-authentication-bindings", traces);
    for row in checked {
        let results = final_results(&row);
        assert!(results["oauth-resource"]["encoded"].is_string());
        assert_eq!(
            results["wrong-resource"]["error"]["data"]["code"],
            "unauthenticated"
        );
        assert_eq!(
            results["expired-cached"]["error"]["data"]["code"],
            "unauthenticated"
        );
    }
}

#[test]
fn original_composed_credential_commit_failure_and_abandonment_reset() {
    let mut traces = vec![];
    for op in ["configure", "abandonConfigure"] {
        let mut steps = vec![
            connect(),
            json!({"op":"mint","id":"access"}),
            access("oauth-before"),
            bearer("raw-before"),
            configure(op),
            access("oauth-during"),
            bearer("raw-during"),
            json!({"op":"snapshot","name":"pending-configure"}),
            json!({"op":"release","gate":"configure"}),
        ];
        if op == "configure" {
            steps.push(json!({"op":"join","ids":["configure"]}));
        }
        steps.extend([access("oauth-after"),bearer("raw-after"),json!({"op":"mint","id":"rotated","digest":digest("rotated")}),json!({"op":"auth","id":"rotated-auth","socket":"s","token":"rotated"}),json!({"op":"auth","id":"new-raw","socket":"s","headers":{"authorization":"Bearer rotated"}})]);
        let mut t = trace(op, steps);
        t["replies"] = json!({"credentials.configure":[{"gate":"configure"}],"authenticate":[{"value":{"principalId":"a"}},{"error":"unauthenticated","message":"A current Hive credential is required."},{"value":{"principalId":"b"}}]});
        traces.push(t);
    }
    let mut failed = trace(
        "failed-configuration-preserves-both-owners",
        vec![
            connect(),
            json!({"op":"mint","id":"access"}),
            access("oauth-before"),
            bearer("raw-before"),
            configure("configure"),
            json!({"op":"join","ids":["configure"]}),
            access("oauth-after"),
            bearer("raw-after"),
        ],
    );
    failed["replies"] = json!({"credentials.configure":[{"error":"fixture_configuration"}]});
    traces.push(failed);
    let checked = compare("credential-commit-and-abandonment", traces);
    for row in &checked[..2] {
        let results = final_results(row);
        assert_eq!(
            results["oauth-after"]["error"]["data"]["code"],
            "unauthenticated"
        );
        assert!(
            results["rotated-auth"]["encoded"]
                .as_str()
                .unwrap()
                .contains("\"principalId\":\"b\"")
        );
        assert_eq!(
            results["raw-after"]["error"]["data"]["code"],
            "unauthenticated"
        );
        assert!(
            results["new-raw"]["encoded"]
                .as_str()
                .unwrap()
                .contains("\"principalId\":\"b\"")
        );
    }
    assert_eq!(
        final_results(&checked[2])["raw-before"],
        final_results(&checked[2])["raw-after"]
    );
    assert!(final_results(&checked[2])["oauth-after"]["encoded"].is_string());
}

#[test]
fn original_composed_failure_and_credential_reset_preserve_pending_cache_epochs() {
    let mut traces = vec![];
    for reset in ["failure", "configure"] {
        for fail_pending in [false, true] {
            let mut first = bearer("pending-old");
            first["noFlush"] = json!(true);
            let mut steps = vec![
                connect(),
                json!({"op":"mint","id":"access"}),
                access("oauth-before"),
                first,
            ];
            let mut configuration = configure("configure");
            configuration["credentials"] = json!([{"principalId":"a","digest":digest("initial")}]);
            steps.push(if reset == "failure" {
                json!({"op":"failure"})
            } else {
                configuration
            });
            if reset == "configure" {
                steps.push(json!({"op":"join","ids":["configure"]}));
            }
            steps.extend([
                bearer("new-epoch"),
                access("oauth-after"),
                json!({"op":"release","gate":"old"}),
                json!({"op":"join","ids":["pending-old","new-epoch"]}),
                bearer("cached-new"),
                json!({"op":"close","socket":"s"}),
                bearer("after-close"),
            ]);
            let mut t = trace(&format!("{reset}-pending-error-{fail_pending}"), steps);
            let mut pending = json!({"gate":"old","value":{"principalId":"a"}});
            if fail_pending {
                pending = json!({"gate":"old","error":"storage_unavailable"});
            }
            t["replies"] = json!({"authenticate":[pending,{},{}]});
            traces.push(t);
        }
    }
    let mut failure = trace(
        "failure-clears-bearer-session-and-retains-oauth",
        vec![
            connect(),
            json!({"op":"mint","id":"access"}),
            access("oauth-before"),
            bearer("raw-before"),
            json!({"op":"auth","id":"session-before","socket":"s","headers":{"cookie":"ivy_session=session"}}),
            json!({"op":"failure"}),
            access("oauth-after"),
            bearer("raw-after"),
            json!({"op":"auth","id":"session-after","socket":"s","headers":{"cookie":"ivy_session=session"}}),
        ],
    );
    failure["replies"] = json!({});
    traces.push(failure);
    let checked = compare("failure-and-pending-epochs", traces);
    for (index, row) in checked[..4].iter().enumerate() {
        let results = final_results(row);
        if index % 2 == 0 {
            assert!(results["pending-old"]["encoded"].is_string());
        } else {
            assert_eq!(
                results["pending-old"]["error"]["data"]["code"],
                "storage_unavailable"
            );
        }
        assert_eq!(results["new-epoch"], results["cached-new"]);
        assert!(results["after-close"]["encoded"].is_string());
    }
    assert_eq!(
        final_results(&checked[0])["oauth-before"],
        final_results(&checked[0])["oauth-after"]
    );
    assert_eq!(
        final_results(&checked[2])["oauth-before"],
        final_results(&checked[2])["oauth-after"]
    );
}
