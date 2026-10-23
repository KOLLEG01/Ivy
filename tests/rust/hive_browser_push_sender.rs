use super::*;
use ivy_hive::{
    browser_push::{BrowserPush, RequestFuture, SendFuture, Sender, VapidDetails, Worker},
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
#[path = "hive_push_original.rs"]
mod original;
use original::original_output;

type Posted = (IvyJson, oneshot::Sender<IvyResult<IvyJson>>);
static CORPUS: Mutex<()> = Mutex::new(());
fn carrier(value: &IvyJson) -> Value {
    serde_json::from_str(&stringify(value, usize::MAX).unwrap()).unwrap()
}
fn send_options(details: VapidDetails) -> IvyJson {
    IvyJson::object([
        (
            "vapidDetails",
            IvyJson::object([
                ("publicKey", details.public_key.into()),
                ("privateKey", details.private_key.into()),
                ("subject", details.subject.into()),
            ]),
        ),
        ("TTL", IvyJson::Number(3600.0)),
        ("timeout", IvyJson::Number(10000.0)),
    ])
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
    fn send(&self, subscription: IvyJson, payload: String, details: VapidDetails) -> SendFuture {
        let options = send_options(details);
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
            if spec["error"] == true {
                Err(IvyError::internal())
            } else {
                Ok(u16::try_from(spec["status"].as_u64().unwrap()).unwrap())
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
async fn run_sender(input: Value) -> Value {
    tokio::task::LocalSet::new()
        .run_until(native_sender(&input))
        .await
}
fn job(id: u64) -> Value {
    json!({"id":id,"attempts":0,"subscription":{"endpoint":format!("https://fcm.googleapis.com/fcm/send/fixture-{id}"),"keys":{"auth":"fixture","p256dh":"fixture"}},"notice":{"10":"ten","2":"two","title":"O'Case é 🙂","body":"Body","tag":format!("tag-{id}"),"url":"https://example.test/ivy/ui/"}})
}
fn batch(jobs: &[u64], more: bool) -> Value {
    json!({"value":{"deliveries":jobs.iter().map(|id|job(*id)).collect::<Vec<_>>(),"hasMore":more}})
}
fn script(next: Vec<Value>, sends: Vec<Value>, steps: Value) -> Value {
    json!({"keys":{"publicKey":"fixture-public","subject":"overridden","privateKey":"fixture-private","extra":"ignored"},"subject":"mailto:operator@example.invalid","next":next,"sends":sends,"steps":steps})
}
fn trace(result: &Value) -> &[Value] {
    result["trace"].as_array().unwrap()
}
fn requests<'a>(events: &'a [Value], action: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["kind"] == "request" && event["payload"]["action"] == action)
        .collect()
}
fn marked<'a>(result: &'a Value, name: &str) -> &'a [Value] {
    let events = trace(result);
    &events[..events
        .iter()
        .position(|event| event["kind"] == "mark" && event["name"] == name)
        .unwrap()]
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn browser_push_sender_preserves_payload_status_and_empty_batches() {
    let result = run_sender(script(
        vec![batch(&[1, 2, 3, 4], false)],
        vec![
            json!({"status":201}),
            json!({"status":503}),
            json!({"error":true}),
            json!({"status":410}),
        ],
        json!([{"op":"wake"},{"op":"close"}]),
    ))
    .await;
    let events = trace(&result);
    let sends = events
        .iter()
        .filter(|event| event["kind"] == "send")
        .collect::<Vec<_>>();
    assert_eq!(sends.len(), 4);
    for (index, sent) in sends.iter().enumerate() {
        assert_eq!(sent["subscription"], job(index as u64 + 1)["subscription"]);
        assert_eq!(
            serde_json::from_str::<Value>(sent["payload"].as_str().unwrap()).unwrap(),
            job(index as u64 + 1)["notice"]
        );
        assert_eq!(
            sent["options"],
            json!({"TTL":3600,"timeout":10000,"vapidDetails":{
            "publicKey":"fixture-public","privateKey":"fixture-private","subject":"mailto:operator@example.invalid"}})
        );
    }
    let complete = requests(events, "browserPush.complete");
    assert_eq!(
        complete
            .iter()
            .map(|event| event["payload"]["status"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![201, 503, 0, 410]
    );
    assert_eq!(requests(events, "browserPush.keys").len(), 1);
    assert_eq!(requests(events, "browserPush.next").len(), 2);
    assert_eq!(events.last().unwrap()["kind"], "closed");
    let empty = run_sender(script(
        vec![batch(&[], true), batch(&[], false)],
        vec![],
        json!([{"op":"wake"},{"op":"close"}]),
    ))
    .await;
    assert_eq!(requests(trace(&empty), "browserPush.next").len(), 2);
    assert!(requests(trace(&empty), "browserPush.keys").is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn browser_push_sender_coalesces_wakes_and_owns_one_retry_timer() {
    let coalesced = run_sender(script(vec![json!({"gate":"first","value":{"deliveries":[],"hasMore":false}})],vec![],
        json!([{"op":"wake","count":3},{"op":"mark","name":"pending"},{"op":"release","gate":"first"},{"op":"close"}]))).await;
    assert_eq!(
        requests(marked(&coalesced, "pending"), "browserPush.next").len(),
        1
    );
    assert_eq!(requests(trace(&coalesced), "browserPush.next").len(), 2);
    let timed = run_sender(script(
        vec![json!({"error":true})],
        vec![],
        json!([
        {"op":"start"},{"op":"start"},{"op":"mark","name":"initial"},
        {"op":"advance","ms":14999},{"op":"mark","name":"before timer"},
        {"op":"advance","ms":1},{"op":"mark","name":"after timer"},
        {"op":"close"},{"op":"advance","ms":15000},{"op":"start"},{"op":"wake"}]),
    ))
    .await;
    for (mark, count) in [("initial", 1), ("before timer", 1), ("after timer", 2)] {
        assert_eq!(
            requests(marked(&timed, mark), "browserPush.next").len(),
            count,
            "{mark}"
        );
    }
    assert_eq!(requests(trace(&timed), "browserPush.next").len(), 2);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn browser_push_sender_rejects_invalid_keys_and_batches_until_next_poll() {
    for invalid in [
        Value::Null,
        json!(false),
        json!(12),
        json!(["public", "private"]),
        json!("keys"),
        json!({}),
        json!({"publicKey":1,"privateKey":"private"}),
        json!({"publicKey":"public"}),
    ] {
        let mut input = script(
            vec![batch(&[1], true), batch(&[1], false)],
            vec![json!({"status":201})],
            json!([{"op":"start"},{"op":"mark","name":"invalid"},{"op":"advance","ms":15000},{"op":"close"}]),
        );
        input["keyReplies"] = json!([{"value":invalid},{"value":input["keys"]}]);
        let result = run_sender(input).await;
        let before = marked(&result, "invalid");
        assert_eq!(requests(before, "browserPush.next").len(), 1);
        assert_eq!(requests(before, "browserPush.keys").len(), 1);
        assert!(!before.iter().any(|event| event["kind"] == "send"));
        assert!(requests(before, "browserPush.complete").is_empty());
        assert_eq!(requests(trace(&result), "browserPush.complete").len(), 1);
        assert_eq!(requests(trace(&result), "browserPush.next").len(), 3);
    }
    let mut invalid = job(2);
    invalid.as_object_mut().unwrap().remove("notice");
    let result = run_sender(script(
        vec![json!({"value":{"deliveries":[job(1),invalid],"hasMore":false}})],
        vec![],
        json!([{"op":"wake"},{"op":"close"}]),
    ))
    .await;
    assert!(!trace(&result).iter().any(|event| event["kind"] == "send"));
    assert!(requests(trace(&result), "browserPush.complete").is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn browser_push_sender_close_drains_started_jobs_despite_completion_failure() {
    let abandoned = run_sender(script(vec![json!({"gate":"next","value":{"deliveries":[job(1)],"hasMore":false}})],
        vec![json!({"status":201})],json!([{"op":"wake"},{"op":"abandonClose"},{"op":"release","gate":"next"},{"op":"wake"},{"op":"start"}]))).await;
    assert_eq!(requests(trace(&abandoned), "browserPush.next").len(), 1);
    assert_eq!(requests(trace(&abandoned), "browserPush.complete").len(), 1);
    for requested in [false, true] {
        let mut steps = vec![json!({"op":"wake"})];
        if requested {
            steps.push(json!({"op":"wake"}));
        }
        steps.extend([
            json!({"op":"close"}),
            json!({"op":"release","gate":"ack"}),
            json!({"op":"mark","name":"draining"}),
            json!({"op":"release","gate":"late"}),
            json!({"op":"wake"}),
        ]);
        let mut input = script(
            vec![batch(&[1, 2], false)],
            vec![json!({"status":201}), json!({"status":201,"gate":"late"})],
            json!(steps),
        );
        input["completions"] = json!({"1":{"gate":"ack","error":true}});
        let result = run_sender(input).await;
        assert!(
            !marked(&result, "draining")
                .iter()
                .any(|event| event["kind"] == "closed")
        );
        assert_eq!(requests(trace(&result), "browserPush.complete").len(), 2);
        assert_eq!(trace(&result).last().unwrap()["kind"], "closed");
        assert_eq!(requests(trace(&result), "browserPush.next").len(), 1);
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn browser_push_sender_integrates_actual_sql_queue_completions_and_backoff() {
    let mut value = script(
        vec![],
        vec![
            json!({"status":201}),
            json!({"status":503}),
            json!({"status":404}),
            json!({"status":410}),
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
    let result = run_sender(value).await;
    let last = trace(&result).last().unwrap();
    assert_eq!(last["subscriptions"].as_array().unwrap().len(), 2);
    assert_eq!(last["outbox"].as_array().unwrap().len(), 1);
    assert_eq!(last["outbox"][0]["id"], 2);
    assert_eq!(last["outbox"][0]["attempts"], 1);
    assert_eq!(last["outbox"][0]["next_attempt"], 130000);
}
