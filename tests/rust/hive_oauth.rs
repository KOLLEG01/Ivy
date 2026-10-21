use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_contracts::{
    hash::{digest, hmac_sha256},
    json::parse,
};
use ivy_hive::{
    oauth::{Dependencies, Entropy, HiveOAuth, Parameters, Redirects, Secrets, WorkerFamilies},
    worker_identity,
};
use sha2::Digest;
use std::sync::atomic::{AtomicU64, Ordering};

struct Providers {
    events: Arc<Mutex<Vec<Value>>>,
    random: AtomicU64,
    actor: mpsc::UnboundedSender<Posted>,
    specs: Mutex<VecDeque<Value>>,
    gates: Mutex<HashMap<String, oneshot::Sender<()>>>,
}
impl Entropy for Providers {
    fn bytes(&self, length: usize) -> IvyResult<Vec<u8>> {
        let number = self.random.fetch_add(1, Ordering::Relaxed);
        let mut bytes = (0..length)
            .map(|i| (number * 17 + i as u64) as u8)
            .collect::<Vec<_>>();
        bytes[..8].copy_from_slice(&number.to_le_bytes());
        let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        self.events
            .lock()
            .unwrap()
            .push(json!({"kind":"random","length":length,"hex":hex}));
        Ok(bytes)
    }
}
impl Redirects for Providers {
    fn set_parameters(
        &self,
        uri: &JsString,
        entries: &[(JsString, JsString)],
    ) -> IvyResult<JsString> {
        let mut events = self.events.lock().unwrap();
        events.push(json!({"kind":"redirect","uri":carrier(&IvyJson::String(uri.clone()))}));
        for (key, value) in entries {
            events.push(json!({"kind":"redirect.set","key":carrier(&IvyJson::String(key.clone())),"value":carrier(&IvyJson::String(value.clone()))}));
        }
        // The test observes the URL owner call and code. It deliberately does
        // not implement a second URLSearchParams serializer inside this fixture.
        Ok(entries[0].1.clone())
    }
}
impl Worker for Providers {
    fn request(&self, payload: IvyJson) -> RequestFuture {
        self.events
            .lock()
            .unwrap()
            .push(json!({"kind":"worker","encoded":stringify(&payload,usize::MAX).unwrap()}));
        let spec = self.specs.lock().unwrap().pop_front().unwrap_or(json!({}));
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
        let actor = self.actor.clone();
        Box::pin(async move {
            if let Some(waiting) = waiting {
                waiting.await.unwrap();
            }
            if let Some(code) = spec["error"].as_str() {
                return Err(IvyError::new(code, "Isolated family failure."));
            }
            let (send, receive) = oneshot::channel();
            actor.send((payload, send)).unwrap();
            receive.await.unwrap()
        })
    }
}
async fn raw(actor: &mpsc::UnboundedSender<Posted>, payload: IvyJson) -> IvyResult<IvyJson> {
    let (send, receive) = oneshot::channel();
    actor.send((payload, send)).unwrap();
    receive.await.unwrap()
}
fn credentials(value: &IvyJson) -> Vec<Credential> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| Credential {
            principal_id: v
                .get("principalId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            digest: v.get("digest").unwrap().as_js_string().unwrap().clone(),
        })
        .collect()
}
fn start_actor(clock: Arc<AtomicU64>, initial: &IvyJson) -> mpsc::UnboundedSender<Posted> {
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    store
        .set_metadata("cursor_secret", &"33".repeat(32))
        .unwrap();
    store.configure_credentials(&credentials(initial)).unwrap();
    let (send, mut receive) = mpsc::unbounded_channel::<Posted>();
    tokio::task::spawn_local(async move {
        while let Some((payload, reply)) = receive.recv().await {
            let now = clock.load(Ordering::Relaxed);
            let result = match payload.get("action").unwrap().as_str().unwrap().as_str() {
                "oauth.initialize" => worker_identity::oauth_secrets(&store),
                "oauth.family.create" => worker_identity::create_oauth_family(
                    &mut store,
                    payload.get("state").unwrap(),
                    &payload
                        .get("activeCredentialReferences")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_js_string().unwrap().clone())
                        .collect::<Vec<_>>(),
                    now,
                ),
                "oauth.family.rotate" => worker_identity::rotate_oauth_family(
                    &mut store,
                    payload.get("rotation").unwrap(),
                    now,
                ),
                "fixture.snapshot" => Ok(IvyJson::from_serde(&rows(
                    &store,
                    "SELECT key,value FROM metadata WHERE key LIKE 'oauth_refresh:%' ORDER BY key",
                ))
                .unwrap()),
                "fixture.sql" => {
                    store
                        .connection()
                        .execute_batch(&payload.get("sql").unwrap().as_str().unwrap())
                        .unwrap();
                    Ok(IvyJson::Null)
                }
                _ => panic!("Unexpected OAuth actor request"),
            };
            let _ = reply.send(result);
        }
    });
    send
}
fn resolved(value: &Value, values: &HashMap<String, IvyJson>) -> IvyJson {
    if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        let mut parts = reference.split('.');
        let mut value = &values[parts.next().unwrap()];
        for key in parts {
            value = value.get(key).unwrap();
        }
        return value.clone();
    }
    if let Some(units) = value.get("$units").and_then(Value::as_array) {
        return IvyJson::String(JsString(
            units.iter().map(|v| v.as_u64().unwrap() as u16).collect(),
        ));
    }
    match value {
        Value::Array(v) => IvyJson::Array(v.iter().map(|v| resolved(v, values)).collect()),
        Value::Object(v) => {
            IvyJson::object(v.iter().map(|(k, v)| (k.as_str(), resolved(v, values))))
        }
        _ => IvyJson::from_serde(value).unwrap(),
    }
}
fn field<'a>(value: &'a IvyJson, name: &str) -> &'a JsString {
    value.get(name).unwrap().as_js_string().unwrap()
}
fn parameters(step: &IvyJson) -> Parameters {
    Parameters::new(
        step.get("form")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let entry = entry.as_array().unwrap();
                (
                    entry[0].as_js_string().unwrap().clone(),
                    entry[1].as_js_string().unwrap().clone(),
                )
            }),
    )
}
type Results = Arc<Mutex<serde_json::Map<String, Value>>>;
type Values = Arc<Mutex<HashMap<String, IvyJson>>>;
fn record(id: &str, result: IvyResult<IvyJson>, results: &Results, values: &Values) {
    let value = match result {
        Ok(value) => {
            let encoded = stringify(&value, usize::MAX).unwrap();
            values.lock().unwrap().insert(id.into(), value);
            json!({"encoded":encoded})
        }
        Err(error) => json!({"error":carrier(&error.to_wire())}),
    };
    results.lock().unwrap().insert(id.into(), value);
}
async fn construct(
    input: &Value,
    current: &IvyJson,
    clock: &Arc<AtomicU64>,
    provider: &Arc<Providers>,
) -> HiveOAuth {
    let reading = clock.clone();
    let owner = HiveOAuth::new(
        &input["base"]
            .as_str()
            .unwrap_or("https://example.test/ivy/")
            .into(),
        credentials(current),
        Dependencies {
            families: Arc::new(WorkerFamilies(provider.clone())),
            redirects: provider.clone(),
            entropy: provider.clone(),
            clock: Arc::new(move || reading.load(Ordering::Relaxed) as f64),
        },
    )
    .unwrap();
    if input["initialize"] != false {
        let keys = raw(
            &provider.actor,
            IvyJson::object([("action", "oauth.initialize".into())]),
        )
        .await
        .unwrap();
        owner
            .initialize(&Secrets {
                signing_key: field(&keys, "signingKey").clone(),
                reference_key: field(&keys, "referenceKey").clone(),
            })
            .unwrap();
    }
    owner
}
async fn native_oauth(input: &Value) -> Value {
    let clock = Arc::new(AtomicU64::new(100000));
    let mut current = IvyJson::from_serde(
        &input
            .get("credentials")
            .cloned()
            .unwrap_or(json!([{"principalId":"alpha","digest":digest("credential")}])),
    )
    .unwrap();
    let actor = start_actor(clock.clone(), &current);
    let events = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(Providers {
        events: events.clone(),
        random: AtomicU64::new(0),
        actor,
        specs: Mutex::new(
            input["familyReplies"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into(),
        ),
        gates: Mutex::new(HashMap::new()),
    });
    let mut owner = construct(input, &current, &clock, &provider).await;
    let results: Results = Default::default();
    let values: Values = Default::default();
    let mut running = HashMap::new();
    let mut snapshots = Vec::new();
    for raw_step in input["steps"].as_array().unwrap() {
        let step = resolved(raw_step, &values.lock().unwrap());
        let op = field(&step, "op").to_utf8().unwrap();
        let id = step
            .get("id")
            .and_then(IvyJson::as_js_string)
            .map(|v| v.to_utf8().unwrap())
            .unwrap_or_default();
        match op.as_str() {
            "clock" => clock.store(
                step.get("now").unwrap().as_f64().unwrap() as u64,
                Ordering::Relaxed,
            ),
            "snapshot" => {
                let families = raw(
                    &provider.actor,
                    IvyJson::object([("action", "fixture.snapshot".into())]),
                )
                .await
                .unwrap();
                snapshots.push(json!({"name":field(&step,"name").to_utf8().unwrap(),"events":std::mem::take(&mut*events.lock().unwrap()),"results":results.lock().unwrap().clone(),"families":carrier(&families)}));
            }
            "release" => {
                provider
                    .gates
                    .lock()
                    .unwrap()
                    .remove(&field(&step, "gate").to_utf8().unwrap())
                    .unwrap()
                    .send(())
                    .unwrap();
            }
            "join" => {
                for id in step.get("ids").unwrap().as_array().unwrap() {
                    let work: tokio::task::JoinHandle<()> =
                        running.remove(&id.as_str().unwrap()).unwrap();
                    work.await.unwrap();
                }
            }
            "sql" => {
                raw(
                    &provider.actor,
                    IvyJson::object([
                        ("action", "fixture.sql".into()),
                        ("sql", step.get("sql").unwrap().clone()),
                    ]),
                )
                .await
                .unwrap();
            }
            "restart" => owner = construct(input, &current, &clock, &provider).await,
            "setCredentials" => {
                current = step.get("credentials").unwrap().clone();
                owner.set_credentials(credentials(&current));
            }
            "token" | "spawnToken" | "abandon" => {
                let work = owner.token(&parameters(&step));
                if op == "abandon" {
                    drop(work);
                } else if op == "token" {
                    record(&id, work.await, &results, &values);
                } else {
                    let results = results.clone();
                    let values = values.clone();
                    let key = id.clone();
                    running.insert(
                        id,
                        tokio::spawn(async move { record(&key, work.await, &results, &values) }),
                    );
                }
            }
            _ => {
                let result = match op.as_str() {
                    "register" => owner.register(step.get("body").unwrap()),
                    "begin" => owner.begin(&parameters(&step)),
                    "transaction" => owner.transaction(field(&step, "transaction")),
                    "authorize" => owner
                        .authorize(
                            field(&step, "transaction"),
                            field(&step, "credentialDigest"),
                        )
                        .map(IvyJson::String),
                    "auth" => ivy_hive::http_auth::OAuth::authenticate(
                        &owner,
                        field(&step, "token"),
                        step.get("resource").and_then(IvyJson::as_js_string),
                    )
                    .map(|v| v.unwrap_or(IvyJson::Null)),
                    "metadata" => owner.protected_resource_metadata(
                        step.get("resource").and_then(IvyJson::as_js_string),
                    ),
                    "serverMetadata" => owner.authorization_server_metadata(
                        step.get("basePath")
                            .and_then(IvyJson::as_js_string)
                            .unwrap_or(&"/ivy".into()),
                    ),
                    "initialize" => {
                        let keys = step.get("secrets").unwrap();
                        owner
                            .initialize(&Secrets {
                                signing_key: field(keys, "signingKey").clone(),
                                reference_key: field(keys, "referenceKey").clone(),
                            })
                            .map(|_| IvyJson::Null)
                    }
                    "payload" => {
                        let text = field(&step, "token").to_utf8().unwrap();
                        let part = text.split('.').nth(1).unwrap();
                        parse(
                            std::str::from_utf8(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap(),
                            usize::MAX,
                        )
                    }
                    "mutate" => {
                        let token = field(&step, "token");
                        let parts = token
                            .0
                            .split(|u| *u == 46)
                            .map(|v| JsString(v.to_vec()))
                            .collect::<Vec<_>>();
                        let mut encoded = parts[1].clone();
                        let mut signature = parts[2].clone();
                        match field(&step, "mode").to_utf8().unwrap().as_str() {
                            "signature-padding" => signature.0.extend("=ignored".encode_utf16()),
                            "signature-noise" => {
                                let mut units = Vec::new();
                                for (i, u) in signature.0.iter().enumerate() {
                                    if i > 0 {
                                        units.extend(" !\t".encode_utf16());
                                    }
                                    units.push(*u);
                                }
                                signature = JsString(units);
                            }
                            "signature-low-bytes" => signature.0.iter_mut().for_each(|u| *u += 256),
                            "signature-unused-bits" => {
                                let alphabet=b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
                                let last = alphabet
                                    .iter()
                                    .position(|b| *b as u16 == *signature.0.last().unwrap())
                                    .unwrap();
                                *signature.0.last_mut().unwrap() = alphabet[last ^ 1] as u16;
                            }
                            "payload-low-bytes" => {
                                encoded.0.iter_mut().for_each(|u| *u += 256);
                                let keys = raw(
                                    &provider.actor,
                                    IvyJson::object([("action", "oauth.initialize".into())]),
                                )
                                .await
                                .unwrap();
                                let hex = field(&keys, "signingKey").to_utf8().unwrap();
                                let key = (0..hex.len())
                                    .step_by(2)
                                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                                    .collect::<Vec<_>>();
                                signature = URL_SAFE_NO_PAD
                                    .encode(hmac_sha256(
                                        &key,
                                        String::from_utf16_lossy(&encoded.0).as_bytes(),
                                    ))
                                    .into();
                            }
                            _ => panic!("Unknown token mutation"),
                        }
                        let mut units = JsString::from("ivyoa2.").0;
                        units.extend(encoded.0);
                        units.push(46);
                        units.extend(signature.0);
                        Ok(IvyJson::String(JsString(units)))
                    }
                    "forge" => {
                        let keys = raw(
                            &provider.actor,
                            IvyJson::object([("action", "oauth.initialize".into())]),
                        )
                        .await
                        .unwrap();
                        let mut payload = step.get("payload").cloned().unwrap_or(IvyJson::Null);
                        if let Some(patch) = step.get("patch").and_then(IvyJson::as_object) {
                            for (k, v) in patch {
                                payload.insert(k.clone(), v.clone());
                            }
                        }
                        if let Some(omit) = step.get("omit").and_then(IvyJson::as_array) {
                            for key in omit {
                                payload.remove(&key.as_str().unwrap());
                            }
                        }
                        let encoded = step
                            .get("encoded")
                            .and_then(IvyJson::as_js_string)
                            .cloned()
                            .unwrap_or_else(|| {
                                URL_SAFE_NO_PAD
                                    .encode(stringify(&payload, usize::MAX).unwrap())
                                    .into()
                            });
                        let text = String::from_utf16_lossy(&encoded.0);
                        let hex = field(&keys, "signingKey").to_utf8().unwrap();
                        let key = (0..hex.len())
                            .step_by(2)
                            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                            .collect::<Vec<_>>();
                        let signature = URL_SAFE_NO_PAD.encode(hmac_sha256(&key, text.as_bytes()));
                        let mut units = JsString::from("ivyoa2.").0;
                        units.extend(encoded.0);
                        units.extend(format!(".{signature}").encode_utf16());
                        Ok(IvyJson::String(JsString(units)))
                    }
                    _ => panic!("Unknown OAuth command {op}"),
                };
                record(&id, result, &results, &values);
            }
        }
        if step.get("noFlush") != Some(&true.into()) {
            flush().await;
        }
    }
    for (_, work) in running {
        work.await.unwrap();
    }
    flush().await;
    let families = raw(
        &provider.actor,
        IvyJson::object([("action", "fixture.snapshot".into())]),
    )
    .await
    .unwrap();
    snapshots.push(json!({"name":"final","events":*events.lock().unwrap(),"results":*results.lock().unwrap(),"families":carrier(&families)}));
    carrier(&IvyJson::from_serde(&json!({"snapshots":snapshots})).unwrap())
}
async fn compare_oauth(name: &str, input: Value) -> Value {
    let original = original_output("tests/fixtures/hive-oauth-oracle.mjs", &input);
    let native = tokio::task::LocalSet::new()
        .run_until(native_oauth(&input))
        .await;
    if let Ok(path) = std::env::var("IVY_HIVE_OAUTH_CORPUS") {
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
    assert_eq!(native, original, "OAuth: {name}");
    native
}
fn r(path: &str) -> Value {
    json!({"$ref":path})
}
fn registration() -> Value {
    json!({"op":"register","id":"client","body":{"client_name":"Fixture","redirect_uris":["http://127.0.0.1/callback?a=%20&state=old&code=old&iss=old"]}})
}
fn begin(id: &str) -> Value {
    let verifier = "v".repeat(64);
    let hash = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    json!({"op":"begin","id":id,"form":[["response_type","code"],["client_id",r("client.client_id")],["redirect_uri","http://127.0.0.1/callback?a=%20&state=old&code=old&iss=old"],["state","state with + &= :"],["code_challenge",hash],["code_challenge_method","S256"],["resource","https://example.test/ivy/mcp"],["scope","hive"]]})
}
fn authorize(id: &str, transaction: &str) -> Value {
    json!({"op":"authorize","id":id,"transaction":r(&format!("{transaction}.transaction")),"credentialDigest":digest("credential")})
}
fn exchange(id: &str, code: &str) -> Value {
    json!({"op":"token","id":id,"form":[["grant_type","authorization_code"],["client_id",r("client.client_id")],["resource","https://example.test/ivy/mcp"],["code",r(code)],["redirect_uri","http://127.0.0.1/callback?a=%20&state=old&code=old&iss=old"],["code_verifier","v".repeat(64)]]})
}
fn refresh(id: &str, token: &str) -> Value {
    json!({"op":"token","id":id,"form":[["grant_type","refresh_token"],["client_id",r("client.client_id")],["resource","https://example.test/ivy/mcp"],["refresh_token",r(token)]]})
}
fn flow() -> Vec<Value> {
    vec![
        registration(),
        begin("transaction"),
        authorize("code", "transaction"),
        exchange("tokens", "code"),
    ]
}
fn result<'a>(value: &'a Value, key: &str) -> &'a Value {
    &value["snapshots"].as_array().unwrap().last().unwrap()["results"][key]
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_native_grants_refresh_reuse_and_resource_binding_match_original() {
    let mut steps = flow();
    steps.extend([
        json!({"op":"auth","id":"access","token":r("tokens.access_token"),"resource":"https://example.test/ivy/mcp"}),
        json!({"op":"auth","id":"wrongResource","token":r("tokens.access_token"),"resource":"https://example.test/ivy/mcp-dev"}),
        json!({"op":"auth","id":"refreshAsAccess","token":r("tokens.refresh_token")}),
        json!({"op":"auth","id":"static","token":"static"}),
        exchange("consumed","code"),refresh("rotated","tokens.refresh_token"),
        json!({"op":"restart"}),refresh("afterRestart","rotated.refresh_token"),refresh("reused","tokens.refresh_token"),refresh("revoked","afterRestart.refresh_token"),
    ]);
    let value = compare_oauth(
        "native-family-issue-rotation-restart-reuse-and-resources",
        json!({"steps":steps}),
    )
    .await;
    assert!(
        result(&value, "access")["encoded"]
            .as_str()
            .unwrap()
            .contains("alpha")
    );
    assert_eq!(
        result(&value, "revoked")["error"]["data"]["code"],
        "unauthenticated"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_native_registration_metadata_client_and_redirect_guards_match_original() {
    let mut steps = vec![
        json!({"op":"metadata","id":"metadata"}),
        json!({"op":"metadata","id":"development","resource":"https://example.test/ivy/mcp-dev"}),
        json!({"op":"metadata","id":"unknown","resource":"wrong"}),
        json!({"op":"serverMetadata","id":"server"}),
    ];
    for (i,body) in [json!(null),json!([]),json!({}),json!({"redirect_uris":[]}),json!({"redirect_uris":[1]}),json!({"redirect_uris":["https://example.test"],"token_endpoint_auth_method":"client_secret_basic"}),json!({"redirect_uris":["https://example.test"],"client_name":""}),json!({"redirect_uris":["https://EXAMPLE.test:443/../cb?x=%20#"]}),json!({"redirect_uris":vec!["https://example.test";17]}),json!({"redirect_uris":["https://example.test"],"client_name":"x".repeat(257)})].into_iter().enumerate() {steps.push(json!({"op":"register","id":format!("body-{i}"),"body":body}));}
    for (i, uri) in [
        "",
        "not a URL",
        "http://localhost/cb",
        "http://LOCALHOST:80/cb",
        "http://127.1/cb",
        "http://[::1]/cb",
        "http://localhost./cb",
        "http://sub.localhost/cb",
        "http://[::ffff:127.0.0.1]/cb",
        "https://u:p@example.test/cb",
        "https://example.test/#fragment",
        "https://example.test/\\cb",
        "https://example.test/%5Ccb",
        "https://example.test/?x=\\",
        "data:text/plain,cb",
        "https://example.test/cb?x=one",
    ]
    .into_iter()
    .enumerate()
    {
        steps.push(
            json!({"op":"register","id":format!("redirect-{i}"),"body":{"redirect_uris":[uri]}}),
        );
    }
    steps.extend([
        json!({"op":"register","id":"lone-name","body":{"redirect_uris":["https://example.test/cb"],"client_name":{"$units":[55296,65,56320]}}}),
        json!({"op":"register","id":"astral-name","body":{"redirect_uris":["https://example.test/cb"],"client_name":"🙂".repeat(128)}}),
        json!({"op":"register","id":"uri-too-long","body":{"redirect_uris":[format!("https://example.test/{}","a".repeat(2048))]}}),
        json!({"op":"register","id":"registration-too-large","body":{"redirect_uris":vec![format!("https://example.test/{}","a".repeat(220));16]}}),
    ]);
    steps.push(registration());
    steps.push(begin("pending"));
    steps.push(
        json!({"op":"transaction","id":"transaction","transaction":r("pending.transaction")}),
    );
    compare_oauth(
        "registration-metadata-client-and-redirect-policy",
        json!({"steps":steps}),
    )
    .await;
    compare_oauth("root-resource-and-metadata-base",json!({"base":"https://example.test/","steps":[{"op":"metadata","id":"root"},{"op":"serverMetadata","id":"server","basePath":""}]})).await;
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_native_code_consumption_failure_and_expiry_match_original() {
    for (name,field,value) in [("pkce-failure","code_verifier",json!("x".repeat(64))),("redirect-failure","redirect_uri",json!("https://other.test/cb")),("redirect-syntax","redirect_uri",json!("not a URL")),("client-mismatch","client_id",json!(format!("ivyc1.{}",URL_SAFE_NO_PAD.encode(r#"{"version":1,"redirectUris":["http://127.0.0.1/callback"],"clientName":"other"}"#)))),("resource-mismatch","resource",json!("https://example.test/ivy/mcp-dev"))] {
        let mut steps=vec![registration(),begin("transaction"),authorize("code","transaction")];let mut bad=exchange("bad","code");
        for pair in bad["form"].as_array_mut().unwrap() {if pair[0]==field {pair[1]=value.clone();}}
        steps.extend([bad,exchange("retry","code")]);
        compare_oauth(name,json!({"steps":steps})).await;
    }
    let mut steps = vec![registration(), begin("expired")];
    steps.extend([
        json!({"op":"clock","now":1_000_000}),
        json!({"op":"transaction","id":"expiredAtBoundary","transaction":r("expired.transaction")}),
        begin("new"),
        authorize("code", "new"),
        json!({"op":"clock","now":1_900_000}),
        exchange("expiredCode", "code"),
        exchange("consumedExpired", "code"),
    ]);
    compare_oauth(
        "exact-transaction-and-code-expiry-boundaries",
        json!({"steps":steps}),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_native_pending_and_code_capacity_reclaim_without_consuming_retry() {
    let mut steps = vec![registration()];
    for i in 0..1024 {
        steps.push(begin(&format!("pending-{i}")));
    }
    steps.extend([
        begin("full"),
        json!({"op":"clock","now":1_000_000}),
        begin("replacement"),
        json!({"op":"transaction","id":"reclaimed","transaction":r("replacement.transaction")}),
    ]);
    let value = compare_oauth(
        "1024-pending-bound-and-expiry-reclamation",
        json!({"steps":steps}),
    )
    .await;
    assert_eq!(
        result(&value, "full")["error"]["data"]["code"],
        "limit_exceeded"
    );
    let mut steps = vec![registration()];
    for i in 0..1024 {
        let id = format!("pending-{i}");
        steps.extend([begin(&id), authorize(&format!("code-{i}"), &id)]);
    }
    steps.extend([
        json!({"op":"clock","now":160000}),
        begin("retry"),
        authorize("full", "retry"),
        json!({"op":"transaction","id":"retained","transaction":r("retry.transaction")}),
        json!({"op":"clock","now":1_000_000}),
        authorize("reclaimed", "retry"),
        exchange("token", "reclaimed"),
    ]);
    let value = compare_oauth(
        "1024-code-bound-preserves-pending-and-reclaims-expiry",
        json!({"steps":steps}),
    )
    .await;
    assert_eq!(
        result(&value, "full")["error"]["data"]["code"],
        "limit_exceeded"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_native_async_family_failures_credentials_and_abandoned_exchange_match_original() {
    let mut steps = vec![
        registration(),
        begin("transaction"),
        authorize("code", "transaction"),
    ];
    let mut started = exchange("pending", "code");
    started["op"] = "spawnToken".into();
    steps.extend([
        started,
        json!({"op":"snapshot","name":"family pending"}),
        json!({"op":"setCredentials","credentials":[]}),
        json!({"op":"release","gate":"family"}),
        json!({"op":"join","ids":["pending"]}),
        exchange("consumed", "code"),
    ]);
    let value = compare_oauth(
        "credential-revoked-during-pending-family-write",
        json!({"familyReplies":[{"gate":"family"}],"steps":steps}),
    )
    .await;
    assert_eq!(
        result(&value, "pending")["error"]["data"]["code"],
        "unauthenticated"
    );
    let mut steps = vec![
        registration(),
        begin("transaction"),
        authorize("code", "transaction"),
    ];
    let mut abandoned = exchange("abandoned", "code");
    abandoned["op"] = "abandon".into();
    steps.extend([
        abandoned,
        json!({"op":"snapshot","name":"abandoned family pending"}),
        json!({"op":"release","gate":"family"}),
        json!({"op":"snapshot","name":"family completed"}),
        exchange("consumed", "code"),
    ]);
    compare_oauth(
        "abandoned-exchange-retains-family-and-access-continuation",
        json!({"familyReplies":[{"gate":"family"}],"steps":steps}),
    )
    .await;
    let mut steps = vec![
        registration(),
        begin("transaction"),
        authorize("code", "transaction"),
        exchange("failure", "code"),
        exchange("consumed", "code"),
    ];
    steps.extend([
        begin("next"),
        authorize("nextCode", "next"),
        exchange("recovered", "nextCode"),
    ]);
    compare_oauth(
        "failed-family-create-still-consumes-code",
        json!({"familyReplies":[{"error":"storage_unavailable"}],"steps":steps}),
    )
    .await;
    let mut steps = flow();
    let mut left = refresh("left", "tokens.refresh_token");
    left["op"] = "spawnToken".into();
    let mut right = refresh("right", "tokens.refresh_token");
    right["op"] = "spawnToken".into();
    steps.extend([
        left,
        right,
        json!({"op":"join","ids":["left","right"]}),
        refresh("revoked", "left.refresh_token"),
    ]);
    compare_oauth(
        "concurrent-refresh-predecessor-reuse-revokes-successor",
        json!({"steps":steps}),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_native_signed_payload_validation_and_reference_rebinding_match_original() {
    let mut steps = flow();
    steps.push(json!({"op":"payload","id":"claims","token":r("tokens.access_token")}));
    for (i, mode) in [
        "signature-padding",
        "signature-noise",
        "signature-low-bytes",
        "signature-unused-bits",
        "payload-low-bytes",
    ]
    .into_iter()
    .enumerate()
    {
        steps.extend([json!({"op":"mutate","id":format!("mutation-{i}"),"mode":mode,"token":r("tokens.access_token")}),json!({"op":"auth","id":format!("mutated-{i}"),"token":r(&format!("mutation-{i}"))})]);
    }
    let patches = vec![
        json!({"version":1}),
        json!({"type":"refresh"}),
        json!({"credentialReference":"ivyr2.unknown"}),
        json!({"clientId":""}),
        json!({"clientId":"x".repeat(4097)}),
        json!({"scope":"hive hive"}),
        json!({"issuedAt":160001}),
        json!({"issuedAt":160000}),
        json!({"expiresAt":100000}),
        json!({"expiresAt":100001}),
        json!({"issuedAt":1.5}),
        json!({"expiresAt":9_007_199_254_740_992_f64}),
        json!({"nonce":false}),
        json!({"nonce":""}),
        json!({"resource":"https://other.test/mcp"}),
        json!({"clientId":"self-asserted-client","extra":{"accepted":true}}),
    ];
    for (i, patch) in patches.into_iter().enumerate() {
        steps.extend([
            json!({"op":"forge","id":format!("token-{i}"),"payload":r("claims"),"patch":patch}),
            json!({"op":"auth","id":format!("auth-{i}"),"token":r(&format!("token-{i}"))}),
        ]);
    }
    for (i, key) in [
        "version",
        "type",
        "credentialReference",
        "clientId",
        "scope",
        "issuedAt",
        "expiresAt",
        "nonce",
        "resource",
    ]
    .into_iter()
    .enumerate()
    {
        steps.extend([
            json!({"op":"forge","id":format!("omitted-{i}"),"payload":r("claims"),"omit":[key]}),
            json!({"op":"auth","id":format!("missing-{i}"),"token":r(&format!("omitted-{i}"))}),
        ]);
    }
    steps.extend([json!({"op":"setCredentials","credentials":[{"principalId":"beta","digest":digest("credential")},{"principalId":"gamma","digest":digest("credential")}]}),json!({"op":"auth","id":"rebound","token":r("tokens.access_token")}),json!({"op":"setCredentials","credentials":[]}),json!({"op":"auth","id":"removed","token":r("tokens.access_token")})]);
    let value = compare_oauth(
        "signed-payload-validation-and-current-reference-rebinding",
        json!({"steps":steps}),
    )
    .await;
    assert!(
        result(&value, "rebound")["encoded"]
            .as_str()
            .unwrap()
            .contains("gamma")
    );
    let mut steps = flow();
    steps.push(json!({"op":"payload","id":"claims","token":r("tokens.refresh_token")}));
    for (i, patch) in [
        json!({"familyId":"a".repeat(42)}),
        json!({"familyId":format!("{}\n","a".repeat(43))}),
        json!({"generation":0}),
        json!({"generation":1.5}),
        json!({"generation":"1"}),
        json!({"generation":9_007_199_254_740_991_f64}),
    ]
    .into_iter()
    .enumerate()
    {
        steps.extend([
            json!({"op":"forge","id":format!("token-{i}"),"payload":r("claims"),"patch":patch}),
            refresh(&format!("refresh-{i}"), &format!("token-{i}")),
        ]);
    }
    compare_oauth(
        "refresh-shape-safe-integer-and-family-validation",
        json!({"steps":steps}),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_native_form_duplicate_state_scope_and_self_asserted_client_guards_match_original() {
    let mut steps = vec![registration()];
    for (i, (key, value)) in [
        ("response_type", json!("token")),
        ("state", json!("")),
        ("state", json!("s".repeat(4097))),
        ("code_challenge", json!("a".repeat(42))),
        ("code_challenge", json!("a".repeat(129))),
        ("code_challenge", json!(format!("{}\n", "a".repeat(43)))),
        ("code_challenge", json!("+".repeat(43))),
        ("code_challenge_method", json!("plain")),
        ("resource", json!("https://example.test/ivy/mcp/")),
        ("scope", json!("hive hive")),
        ("scope", json!(" ")),
        ("scope", json!("hive\u{85}")),
        ("scope", json!("\u{feff}\t hive\u{a0}")),
        ("scope", json!("")),
        ("client_id", json!("ivyc1.not-json")),
        (
            "client_id",
            json!(format!("ivyc1.{}", URL_SAFE_NO_PAD.encode("[]"))),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut step = begin(&format!("guard-{i}"));
        for pair in step["form"].as_array_mut().unwrap() {
            if pair[0] == key {
                pair[1] = value.clone();
            }
        }
        steps.push(step);
    }
    let mut duplicate = begin("firstDuplicate");
    duplicate["form"]
        .as_array_mut()
        .unwrap()
        .push(json!(["resource", "wrong"]));
    steps.push(duplicate);
    steps.push(authorize("duplicateCode", "firstDuplicate"));
    let mut verifier = exchange("newlineVerifier", "duplicateCode");
    for pair in verifier["form"].as_array_mut().unwrap() {
        if pair[0] == "code_verifier" {
            pair[1] = format!("{}\n", "v".repeat(64)).into();
        }
    }
    steps.push(verifier);
    steps.push(exchange("consumed", "duplicateCode"));
    let client = json!({"version":1,"redirectUris":["http://127.0.0.1/callback?a=%20&state=old&code=old&iss=old"],"clientName":"Self asserted","extra":{"kept":true},"scope":"untrusted","state":"untrusted"});
    let id = format!("ivyc1.{}", URL_SAFE_NO_PAD.encode(client.to_string()));
    let mut self_asserted = begin("selfAsserted");
    for pair in self_asserted["form"].as_array_mut().unwrap() {
        if pair[0] == "client_id" {
            pair[1] = id.clone().into();
        }
    }
    steps.extend([self_asserted,json!({"op":"transaction","id":"retainedExtra","transaction":r("selfAsserted.transaction")}),authorize("selfCode","selfAsserted")]);
    compare_oauth(
        "form-state-scope-pkce-duplicates-and-self-asserted-clients",
        json!({"steps":steps}),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_native_initialization_and_loose_base64_object_parsing_match_original() {
    let mut steps = vec![
        registration(),
        begin("transaction"),
        authorize("code", "transaction"),
        exchange("uninitialized", "code"),
    ];
    for (i, signing) in [
        "11".repeat(31),
        "AA".repeat(32),
        format!("{}\n", "11".repeat(32)),
        "gg".repeat(32),
        "22".repeat(32),
    ]
    .into_iter()
    .enumerate()
    {
        steps.push(json!({"op":"initialize","id":format!("invalid-{i}"),"secrets":{"signingKey":signing,"referenceKey":"22".repeat(32)}}));
    }
    compare_oauth(
        "explicit-key-initialization-and-invalid-secrets",
        json!({"initialize":false,"steps":steps}),
    )
    .await;
    let mut steps = flow();
    steps.push(json!({"op":"payload","id":"claims","token":r("tokens.access_token")}));
    for (i, encoded) in [
        "",
        "bnVsbA",
        "W10",
        "e30",
        "eA",
        "77u/e30=",
        "e30=ignored",
        "e!3 0====",
        "eyJ2ZXJzaW9uIjoyLCJ0eXBlIjoiYWNjZXNzIn0",
    ]
    .into_iter()
    .enumerate()
    {
        steps.extend([
            json!({"op":"forge","id":format!("forged-{i}"),"encoded":encoded}),
            json!({"op":"auth","id":format!("auth-{i}"),"token":r(&format!("forged-{i}"))}),
        ]);
    }
    for (i, token) in [
        "ivyoa2",
        "ivyoa2.",
        "ivyoa2...",
        "ivyoa2.e30.bad",
        "ivyoa1.old",
        "ivyoa2..",
        &format!("ivyoa2.{}.{}", "a".repeat(8192), "b"),
    ]
    .into_iter()
    .enumerate()
    {
        steps.push(json!({"op":"auth","id":format!("syntax-{i}"),"token":token}));
    }
    compare_oauth(
        "signed-loose-base64-objects-and-token-syntax",
        json!({"steps":steps}),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_native_rolling_refresh_and_family_capacity_match_original() {
    let year = ivy_hive::oauth::REFRESH_TOKEN_INACTIVITY_LIFETIME_MS as u64;
    let mut steps = flow();
    steps.extend([
        json!({"op":"clock","now":100000+year-1000}),
        refresh("renewed", "tokens.refresh_token"),
        json!({"op":"restart"}),
        json!({"op":"clock","now":100000+year+1000}),
        refresh("beyondOriginal", "renewed.refresh_token"),
        json!({"op":"clock","now":100000+year+1000+year}),
        refresh("expired", "beyondOriginal.refresh_token"),
    ]);
    compare_oauth(
        "rolling-refresh-renews-inactivity-across-original-expiry-and-restart",
        json!({"steps":steps}),
    )
    .await;
    let root = Store::open(Path::new(":memory:")).unwrap();
    root.set_metadata("cursor_secret", &"33".repeat(32))
        .unwrap();
    let reference = worker_identity::credential_reference(&root, &digest("credential").into())
        .unwrap()
        .to_utf8()
        .unwrap();
    let mut seed = String::new();
    for i in 0..1023 {
        let state = json!({"familyDigest":format!("filler-{i}"),"credentialReference":reference,"clientId":"filler","resource":"https://example.test/ivy/mcp","scope":"hive","generation":1,"currentTokenDigest":format!("token-{i}"),"expiresAt":100000+year+10000,"revoked":false});
        seed.push_str(&format!(
            "INSERT INTO metadata VALUES('oauth_refresh:filler-{i}','{}');",
            state.to_string().replace('\'', "''")
        ));
    }
    let mut steps = flow();
    steps.extend([json!({"op":"sql","sql":seed}),refresh("atCapacity","tokens.refresh_token"),begin("next"),authorize("nextCode","next"),exchange("full","nextCode"),exchange("consumed","nextCode"),json!({"op":"sql","sql":"UPDATE metadata SET value=json_set(value,'$.expiresAt',100000) WHERE key='oauth_refresh:filler-0';"}),begin("reclaim"),authorize("reclaimCode","reclaim"),exchange("reclaimed","reclaimCode")]);
    let value = compare_oauth(
        "1024-family-bound-rotation-at-capacity-and-expired-reclamation",
        json!({"steps":steps}),
    )
    .await;
    assert_eq!(
        result(&value, "full")["error"]["data"]["code"],
        "limit_exceeded"
    );
    assert_eq!(
        value["snapshots"].as_array().unwrap().last().unwrap()["families"]
            .as_array()
            .unwrap()
            .len(),
        1024
    );
}
