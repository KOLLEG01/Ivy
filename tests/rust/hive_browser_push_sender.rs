use super::*;
use ivy_hive::{
    browser_push::{BrowserPush, RequestFuture, SendFuture, SendOutcome, Sender, Worker},
    browser_push_store::{BrowserPushStore, Crypto},
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};
use tokio::sync::{mpsc, oneshot};
#[path = "hive_http_auth.rs"]
mod authentication;
#[path = "hive_browser_push_http.rs"]
mod http;
#[path = "hive_oauth.rs"]
mod oauth;
#[path = "hive_push_crypto.rs"]
mod native_crypto;

type Posted = (IvyJson, oneshot::Sender<IvyResult<IvyJson>>);
static CORPUS: Mutex<()> = Mutex::new(());
fn carrier(value: &IvyJson) -> Value {
    serde_json::from_str(&stringify(value, usize::MAX).unwrap()).unwrap()
}
struct Fixture {
    trace: Arc<Mutex<Vec<Value>>>,
    gates: Mutex<HashMap<String, oneshot::Sender<()>>>,
    next: Mutex<VecDeque<Value>>,
    keys: Value,
    key_replies: Mutex<VecDeque<Value>>,
    sends: Mutex<VecDeque<Value>>,
    completions: Value,
    actor: Option<mpsc::UnboundedSender<Posted>>,
}
impl Fixture {
    fn gate(&self, spec: &Value) -> Option<oneshot::Receiver<()>> {
        spec["gate"].as_str().map(|key| {
            let (send, receive) = oneshot::channel();
            assert!(
                self.gates
                    .lock()
                    .unwrap()
                    .insert(key.into(), send)
                    .is_none()
            );
            receive
        })
    }
    fn raw(&self, payload: IvyJson) -> RequestFuture {
        let (send, receive) = oneshot::channel();
        self.actor.as_ref().unwrap().send((payload, send)).unwrap();
        Box::pin(async move { receive.await.unwrap() })
    }
}
impl Worker for Fixture {
    fn request(&self, payload: IvyJson) -> RequestFuture {
        self.trace
            .lock()
            .unwrap()
            .push(json!({"kind":"request","payload":carrier(&payload)}));
        if self.actor.is_some() {
            return self.raw(payload);
        }
        let spec = match payload.get("action").unwrap().as_str().unwrap().as_str() {
            "browserPush.next" => self
                .next
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| json!({"value":{"deliveries":[],"hasMore":false}})),
            "browserPush.keys" => self
                .key_replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| json!({"value":self.keys})),
            "browserPush.complete" => {
                self.completions[payload.get("id").unwrap().as_f64().unwrap().to_string()].clone()
            }
            _ => panic!("Unexpected worker action"),
        };
        let waiting = self.gate(&spec);
        Box::pin(async move {
            if let Some(waiting) = waiting {
                waiting.await.unwrap();
            }
            if spec["error"] == true {
                Err(IvyError::internal())
            } else {
                Ok(IvyJson::from_serde(&spec["value"]).unwrap())
            }
        })
    }
}
impl Sender for Fixture {
    fn send(&self, subscription: IvyJson, payload: String, options: IvyJson) -> SendFuture {
        self.trace.lock().unwrap().push(json!({"kind":"send","subscription":carrier(&subscription),"payload":payload,"options":carrier(&options)}));
        let spec = self
            .sends
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected fixture delivery");
        let waiting = self.gate(&spec);
        Box::pin(async move {
            if let Some(waiting) = waiting {
                waiting.await.unwrap();
            }
            let status = spec.get("status").map(|v| IvyJson::from_serde(v).unwrap());
            match spec["kind"].as_str() {
                Some("nullish") => SendOutcome::UninspectableFailure,
                Some("error") => SendOutcome::Rejected(status),
                _ => SendOutcome::Response(status),
            }
        })
    }
}
struct NoCrypto;
impl Crypto for NoCrypto {
    fn generate_vapid_keys(&self) -> IvyResult<IvyJson> {
        panic!("Existing keys only; no encryption qualification")
    }
    fn validate_p256_public_key(&self, _: &[u8]) -> IvyResult<()> {
        panic!("Existing subscriptions only")
    }
}
fn actor(input: &Value) -> Option<mpsc::UnboundedSender<Posted>> {
    if input["realStore"] != true {
        return None;
    }
    let store = Store::open(Path::new(":memory:")).unwrap();
    let push = BrowserPushStore::new(
        &store,
        &"https://example.test/ivy/".into(),
        Rc::new(NoCrypto),
        Rc::new(|| 100000.0),
    )
    .unwrap();
    store
        .set_metadata("browser_push_keys", &input["keys"].to_string())
        .unwrap();
    store
        .connection()
        .execute_batch(input["seedSql"].as_str().unwrap())
        .unwrap();
    let (send, mut receive) = mpsc::unbounded_channel::<Posted>();
    tokio::task::spawn_local(async move {
        let mut store = store;
        while let Some((request, reply)) = receive.recv().await {
            let result = match request.get("action").unwrap().as_str().unwrap().as_str() {
                "authenticate" => {
                    let context = request.get("context").unwrap();
                    store
                        .authenticate(&ivy_hive::store::AuthContext {
                            credential_digest: context
                                .get("credentialDigest")
                                .unwrap()
                                .as_js_string()
                                .unwrap()
                                .clone(),
                            session_digest: context
                                .get("sessionDigest")
                                .and_then(IvyJson::as_js_string)
                                .cloned(),
                        })
                        .map(|principal| {
                            IvyJson::object([
                                ("principalId", IvyJson::String(principal.principal_id)),
                                (
                                    "credentialDigest",
                                    IvyJson::String(principal.credential_digest),
                                ),
                            ])
                        })
                }
                "session.resolve" => ivy_hive::worker_identity::resolve_session(
                    &store,
                    request
                        .get("sessionDigest")
                        .unwrap()
                        .as_js_string()
                        .unwrap(),
                ),
                "session.end" => {
                    let context = request.get("context").unwrap();
                    ivy_hive::worker_identity::end_session(
                        &store,
                        &ivy_hive::store::AuthenticatedContext {
                            credential_digest: context
                                .get("credentialDigest")
                                .unwrap()
                                .as_js_string()
                                .unwrap()
                                .clone(),
                            principal_id: context
                                .get("principalId")
                                .unwrap()
                                .as_js_string()
                                .unwrap()
                                .clone(),
                        },
                        context.get("sessionDigest").and_then(IvyJson::as_js_string),
                    )
                }
                "fixture.sessions" => Ok(IvyJson::from_serde(&rows(
                    &store,
                    "SELECT digest,credential_digest FROM browser_sessions ORDER BY digest",
                ))
                .unwrap()),
                "fixture.sql" => {
                    store
                        .connection()
                        .execute_batch(&request.get("sql").unwrap().as_str().unwrap())
                        .unwrap();
                    Ok(IvyJson::Null)
                }
                "browserPush.request" => push.request(
                    &mut store,
                    request.get("context").unwrap(),
                    request.get("request").unwrap(),
                ),
                "browserPush.next" => push.next(&mut store),
                "browserPush.keys" => push.keys(&store),
                "browserPush.complete" => push
                    .complete(
                        &store,
                        request.get("id").unwrap().as_f64().unwrap(),
                        request.get("status").unwrap().as_f64().unwrap(),
                    )
                    .map(|_| IvyJson::Null),
                "fixture.snapshot" => {
                    let subscriptions = rows(
                        &store,
                        "SELECT endpoint,last_error FROM browser_push ORDER BY endpoint",
                    );
                    let outbox = rows(
                        &store,
                        "SELECT id,endpoint,attempts,next_attempt FROM browser_push_outbox ORDER BY id",
                    );
                    Ok(IvyJson::from_serde(
                        &json!({"kind":"store","subscriptions":subscriptions,"outbox":outbox}),
                    )
                    .unwrap())
                }
                _ => panic!("Unexpected real storage request"),
            };
            let _ = reply.send(result);
        }
    });
    Some(send)
}
async fn flush() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}
async fn native_sender(input: &Value) -> Value {
    let queue = |key: &str| input[key].as_array().cloned().unwrap_or_default().into();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let fixture = Arc::new(Fixture {
        trace: trace.clone(),
        gates: Mutex::new(HashMap::new()),
        next: Mutex::new(queue("next")),
        keys: input["keys"].clone(),
        key_replies: Mutex::new(queue("keyReplies")),
        sends: Mutex::new(queue("sends")),
        completions: input["completions"].clone(),
        actor: actor(input),
    });
    let coordinator = BrowserPush::new(
        fixture.clone(),
        input["subject"].as_str().unwrap().into(),
        fixture.clone(),
    );
    let mut closing = Vec::new();
    for step in input["steps"].as_array().unwrap() {
        match step["op"].as_str().unwrap() {
            "wake" => {
                for _ in 0..step["count"].as_u64().unwrap_or(1) {
                    coordinator.wake();
                }
            }
            "start" => coordinator.start(),
            "advance" => {
                tokio::time::advance(Duration::from_millis(step["ms"].as_u64().unwrap())).await
            }
            "release" => fixture
                .gates
                .lock()
                .unwrap()
                .remove(step["gate"].as_str().unwrap())
                .expect("Unknown fixture gate")
                .send(())
                .unwrap(),
            "close" => {
                let done = coordinator.close();
                let trace = trace.clone();
                closing.push(tokio::spawn(async move {
                    done.await;
                    trace.lock().unwrap().push(json!({"kind":"closed"}));
                }));
            }
            "abandonClose" => drop(coordinator.close()),
            "mark" => trace
                .lock()
                .unwrap()
                .push(json!({"kind":"mark","name":step["name"]})),
            _ => panic!("Unknown fixture command"),
        }
        flush().await;
    }
    for done in closing {
        done.await.unwrap();
    }
    flush().await;
    if fixture.actor.is_some() {
        let value = fixture
            .raw(IvyJson::object([("action", "fixture.snapshot".into())]))
            .await
            .unwrap();
        trace.lock().unwrap().push(carrier(&value));
    }
    let result = json!({"trace":*trace.lock().unwrap()});
    result
}
fn original_sender(input: &Value) -> Value {
    original_output("tests/fixtures/hive-browser-push-sender-oracle.mjs", input)
}
fn original_output(fixture: &str, input: &Value) -> Value {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../..")
                    .join(fixture),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "Original sender: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}
async fn compare_sender(name: &str, input: Value) -> Value {
    let native = tokio::task::LocalSet::new()
        .run_until(native_sender(&input))
        .await;
    let original = original_sender(&input);
    if let Some(file) = std::env::var_os("IVY_HIVE_PUSH_SENDER_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .unwrap(),
            "{}",
            json!({"case":name,"input":input,"original":original,"native":native})
        )
        .unwrap();
    }
    assert_eq!(native["trace"], original["trace"], "{name}");
    native
}
fn job(id: u64) -> Value {
    json!({"id":id,"attempts":0,"subscription":{"endpoint":format!("https://fcm.googleapis.com/fcm/send/fixture-{id}"),"keys":{"auth":"fixture","p256dh":"fixture"}},"notice":{"10":"ten","2":"two","title":"O'Case é 🙂","body":"Body","tag":format!("tag-{id}"),"url":"https://example.test/ivy/ui/"}})
}
fn batch(jobs: &[u64], more: bool) -> Value {
    json!({"value":{"deliveries":jobs.iter().map(|id|job(*id)).collect::<Vec<_>>(),"hasMore":more}})
}
fn script(next: Vec<Value>, sends: Vec<Value>, steps: Value) -> Value {
    json!({"keys":{"publicKey":"fixture-public","subject":"overridden","privateKey":"fixture-private","extra":"preserved"},"subject":"mailto:operator@example.invalid","next":next,"sends":sends,"steps":steps})
}
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn browser_push_sender_payload_options_status_and_empty_batches_match_original() {
    compare_sender(
        "four-statuses",
        script(
            vec![batch(&[1, 2, 3, 4], false)],
            vec![
                json!({"status":201}),
                json!({"kind":"error","status":503}),
                json!({"kind":"error"}),
                json!({"kind":"error","status":null}),
            ],
            json!([{"op":"wake"},{"op":"close"}]),
        ),
    )
    .await;
    compare_sender(
        "empty-has-more",
        script(
            vec![batch(&[], true), batch(&[], false)],
            vec![],
            json!([{"op":"wake"},{"op":"close"}]),
        ),
    )
    .await;
    let mut value = script(
        vec![batch(&[1, 2], false)],
        vec![json!({}), json!({"status":null})],
        json!([{"op":"wake"},{"op":"close"}]),
    );
    value["keys"] = json!(["public", "private"]);
    compare_sender("missing-and-null-success-status", value).await;
}
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn browser_push_sender_coalesces_wakes_and_retries_storage_through_requested_or_timer() {
    compare_sender("coalesced-wake",script(vec![json!({"gate":"first","value":{"deliveries":[],"hasMore":false}})],vec![],json!([{"op":"wake","count":3},{"op":"mark","name":"one pending next"},{"op":"release","gate":"first"},{"op":"close"}]))).await;
    let mut value = script(
        vec![batch(&[1], true)],
        vec![],
        json!([{"op":"wake"},{"op":"close"}]),
    );
    value["keyReplies"] = json!([{"error":true}]);
    compare_sender("has-more-retries-key-failure", value).await;
    compare_sender("timer-storage-recovery",script(vec![json!({"error":true})],vec![],json!([{"op":"start"},{"op":"mark","name":"failed storage"},{"op":"advance","ms":14999},{"op":"mark","name":"before timer"},{"op":"advance","ms":1},{"op":"close"},{"op":"advance","ms":15000}]))).await;
    compare_sender(
        "repeated-start-retains-only-latest-timer",
        script(
            vec![],
            vec![],
            json!([{"op":"start"},{"op":"start"},{"op":"close"},{"op":"advance","ms":15000}]),
        ),
    )
    .await;
}
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn browser_push_sender_close_and_early_completion_failures_keep_other_jobs_alive() {
    compare_sender("abandoned-close-still-stops-admission",script(vec![json!({"gate":"next","value":{"deliveries":[job(1)],"hasMore":false}})],vec![json!({"status":201})],json!([{"op":"wake"},{"op":"abandonClose"},{"op":"release","gate":"next"},{"op":"mark","name":"finished without another next"},{"op":"wake"}]))).await;
    compare_sender("close-waits-current-batch",script(vec![json!({"gate":"next","value":{"deliveries":[job(1),job(2)],"hasMore":false}})],vec![json!({"status":201}),json!({"status":201,"gate":"send"})],json!([{"op":"wake"},{"op":"close"},{"op":"release","gate":"next"},{"op":"mark","name":"sending after stop"},{"op":"release","gate":"send"},{"op":"wake"}]))).await;
    for requested in [false, true] {
        let steps = if requested {
            json!([{"op":"wake"},{"op":"wake"},{"op":"release","gate":"ack"},{"op":"close"},{"op":"mark","name":"closed before late completion"},{"op":"release","gate":"late"}])
        } else {
            json!([{"op":"wake"},{"op":"release","gate":"ack"},{"op":"close"},{"op":"mark","name":"closed before late completion"},{"op":"release","gate":"late"}])
        };
        let mut value = script(
            vec![batch(&[1, 2], false)],
            vec![json!({"status":201}), json!({"status":201,"gate":"late"})],
            steps,
        );
        value["completions"] = json!({"1":{"gate":"ack","error":true}});
        compare_sender(
            if requested {
                "early-completion-failure-with-pending-wake"
            } else {
                "early-completion-failure-without-wake"
            },
            value,
        )
        .await;
    }
    compare_sender("nullish-throw-does-not-cancel-other-job",script(vec![batch(&[1,2],false)],vec![json!({"kind":"nullish"}),json!({"status":201,"gate":"late"})],json!([{"op":"wake"},{"op":"close"},{"op":"mark","name":"no first completion"},{"op":"release","gate":"late"}]))).await;
}
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn browser_push_sender_integrates_actual_sql_queue_completions_and_backoff() {
    let mut value = script(
        vec![],
        vec![
            json!({"status":201}),
            json!({"kind":"error","status":503}),
            json!({"kind":"error","status":404}),
            json!({"kind":"error","status":410}),
        ],
        json!([{"op":"wake"},{"op":"close"}]),
    );
    let digest = format!("sha256:{}", "a".repeat(64));
    let mut seed = format!(
        "INSERT OR IGNORE INTO principals VALUES('alpha');INSERT INTO credentials VALUES('{digest}','alpha',0);INSERT INTO browser_sessions VALUES('session','{digest}','2026-01-01');"
    );
    for id in 1..=4 {
        let row = job(id);
        let endpoint = row["subscription"]["endpoint"].as_str().unwrap();
        let quote = |value: &Value| value.to_string().replace('\'', "''");
        seed.push_str(&format!("INSERT INTO browser_push VALUES('{endpoint}','session','{}',NULL,0);INSERT INTO browser_push_outbox(endpoint,tag,notice_json) VALUES('{endpoint}','tag-{id}','{}');",quote(&row["subscription"]),quote(&row["notice"])));
    }
    value["realStore"] = true.into();
    value["seedSql"] = seed.into();
    let result = compare_sender("actual-store-queue-and-completion", value).await;
    let last = result["trace"].as_array().unwrap().last().unwrap();
    assert_eq!(last["subscriptions"].as_array().unwrap().len(), 2);
    assert_eq!(last["outbox"][0]["id"], 2);
    assert_eq!(last["outbox"][0]["attempts"], 1);
    assert_eq!(last["outbox"][0]["next_attempt"], 130000);
}
