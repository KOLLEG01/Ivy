use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_hive::browser_push_store::{BrowserPushStore, Crypto};
use std::collections::HashMap;
pub(super) struct ObservedCrypto {
    keys: Vec<IvyJson>,
    next: Cell<usize>,
    points: HashMap<String, bool>,
}
impl ObservedCrypto {
    pub(super) fn new(profile: &Value) -> Self {
        Self {
            keys: profile["vapid"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| {
                    IvyJson::object([
                        ("publicKey", v["publicKey"].as_str().unwrap().into()),
                        ("privateKey", v["privateKey"].as_str().unwrap().into()),
                    ])
                })
                .collect(),
            next: Cell::new(0),
            points: profile["points"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| (v[0].as_str().unwrap().to_owned(), v[1].as_bool().unwrap()))
                .collect(),
        }
    }
}
impl Crypto for ObservedCrypto {
    fn generate_vapid_keys(&self) -> IvyResult<IvyJson> {
        let i = self.next.get();
        self.next.set(i + 1);
        Ok(self.keys.get(i).expect("Unobserved VAPID entropy").clone())
    }
    fn validate_p256_public_key(&self, bytes: &[u8]) -> IvyResult<()> {
        if *self
            .points
            .get(&URL_SAFE_NO_PAD.encode(bytes))
            .expect("Unobserved P256 primitive")
        {
            Ok(())
        } else {
            Err(IvyError::internal())
        }
    }
}
fn setup() -> Vec<Value> {
    vec![
        json!({"op":"worker","method":"constructor","now":100000,"browserPush":true,"options":{"publicBaseUrl":"https://example.test/ivy/"},"credentials":[{"principalId":"alpha","digest":digest()}],"requireSuccess":true}),
        sql(format!(
            "INSERT INTO browser_sessions VALUES('session-one','{}','2026-01-01'),('session-two','{}','2026-01-01'); INSERT INTO topics VALUES('agent.browser-notification','1.0.0','service:agent','{{}}'),('agent.browser-notification','2.0.0','service:agent','{{}}'),('ignored','1.0.0','service:agent','{{}}');",
            digest(),
            digest()
        )),
    ]
}
fn digest() -> String {
    format!("sha256:{}", "a".repeat(64))
}
fn context(session: &str) -> Value {
    json!({"credentialDigest":digest(),"principalId":"untrusted-unused","sessionDigest":session})
}
fn action(payload: Value) -> Value {
    json!({"op":"worker","method":"dispatch","now":100000,"payload":payload})
}
fn request(session: &str, request: Value) -> Value {
    action(json!({"action":"browserPush.request","context":context(session),"request":request}))
}
fn next() -> Value {
    action(json!({"action":"browserPush.next"}))
}
fn subscription(id: &str) -> Value {
    json!({"endpoint":format!("https://fcm.googleapis.com/fcm/send/{id}"),"keys":{"auth":"AAECAwQFBgcICQoLDA0ODw","p256dh":"BM8nFJjtvfwf51hLUGfy5V-q7JCzuQncYyfm0QCdfSX9Z50FMNgYqikzGgFpb6Zd6dj1eJjmg_tNW5LZFAHyvDk"}})
}
fn subscribe(s: &str, id: &str) -> Value {
    request(
        s,
        json!({"action":"subscribe","subscription":subscription(id)}),
    )
}
fn ok(steps: &mut Vec<Value>, mut step: Value) -> usize {
    step["requireSuccess"] = true.into();
    push(steps, step)
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let observed = oracle(&root.0.join("observed.sqlite"), steps);
    let mut profile = observed["registryRuntime"].clone();
    profile["browserPush"] = observed["browserPushRuntime"].clone();
    let actual = native(&root.0.join("native.sqlite"), steps, &profile);
    let mut bound = steps.to_vec();
    for step in &mut bound {
        if let Some(keys) = observed["browserPushRuntime"]["vapid"]
            .as_array()
            .unwrap()
            .first()
        {
            step["vapidKeys"] = keys.clone();
        }
    }
    let expected = oracle(&root.0.join("reference.sqlite"), &bound);
    for (i, (a, b)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(a, b, "Push step {i}: {}", steps[i]);
        if steps[i]["op"] == "sql" || steps[i]["requireSuccess"] == true {
            assert!(a["error"].is_null(), "Setup at{i}: {}", a["error"]);
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    actual
}
fn value(result: &Value, i: usize) -> &Value {
    &result["rows"][i]["value"]
}
fn state(result: &Value, i: usize) -> &Value {
    &result["rows"][i]["state"]
}
fn notice(tag: &str) -> Value {
    json!({"title":"Notice","body":"Body","tag":tag,"target":{"uiId":"agent-ui","fragment":"#/thread?a=one+two&b=%F0%9F%99%82"}})
}
fn publish(tag: &str, topic: &str, version: &str, source: &str, payload: Value) -> Value {
    let payload = payload.to_string().replace('\'', "''");
    sql(format!(
        "INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES('{topic}','{version}','{source}','2026-01-01','{tag}','{payload}');"
    ))
}
fn event(tag: &str) -> Value {
    publish(
        tag,
        "agent.browser-notification",
        "1.0.0",
        "service:agent",
        notice(tag),
    )
}
fn cursor(result: &Value, i: usize) -> f64 {
    state(result, i)["metadata"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == "browser_push_cursor")
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn browser_push_subscription_guards_ownership_normalization_and_late_completion_match_original() {
    let mut steps = setup();
    let keys = ok(&mut steps, action(json!({"action":"browserPush.keys"})));
    let status = ok(
        &mut steps,
        request("session-one", json!({"action":"status"})),
    );
    let mut missing = request(
        "session-one",
        json!({"action":"subscribe","subscription":{}}),
    );
    missing["payload"]["context"]
        .as_object_mut()
        .unwrap()
        .remove("sessionDigest");
    let forbidden = push(&mut steps, missing);
    let mut bad = request("missing", json!({"action":"unknown"}));
    bad["payload"]["context"]["credentialDigest"] = "sha256:bad".into();
    let auth = push(&mut steps, bad);
    let mut invalid = vec![];
    for endpoint in [
        "broken",
        "http://fcm.googleapis.com/x",
        "https://fcm.googleapis.com:8443/x",
        "https://u:p@fcm.googleapis.com/x",
        "https://fcm.googleapis.com.evil.test/x",
        "https://push.apple.com/x",
        "https://fcm.googleapis.com/x#hash",
        "https://127.0.0.1/x",
    ] {
        let mut sub = subscription("guard");
        sub["endpoint"] = endpoint.into();
        invalid.push(push(
            &mut steps,
            request(
                "session-one",
                json!({"action":"subscribe","subscription":sub}),
            ),
        ));
    }
    for key in [
        json!({}),
        json!({"auth":"x","p256dh":subscription("x")["keys"]["p256dh"]}),
        json!({"auth":subscription("x")["keys"]["auth"],"p256dh":URL_SAFE_NO_PAD.encode([0_u8;65])}),
    ] {
        let mut sub = subscription("guard");
        sub["keys"] = key;
        invalid.push(push(
            &mut steps,
            request(
                "session-one",
                json!({"action":"subscribe","subscription":sub}),
            ),
        ));
    }
    for (i, endpoint) in [
        "https://FCM.GOOGLEAPIS.COM:443/a/../normalized",
        "https://jmt17.google.com/x",
        "https://region.push.apple.com/x",
        "https://region.push.services.mozilla.com/x",
        "https://region.notify.windows.com/x",
    ]
    .iter()
    .enumerate()
    {
        let mut sub = subscription("normalized");
        sub["endpoint"] = (*endpoint).into();
        ok(
            &mut steps,
            request(
                "session-one",
                json!({"action":"subscribe","subscription":sub}),
            ),
        );
        if i == 0 {
            ok(
                &mut steps,
                request(
                    "session-one",
                    json!({"action":"unsubscribe","endpoint":"https://fcm.googleapis.com/normalized"}),
                ),
            );
        }
    }
    let mut noncanonical = subscription("noncanonical");
    noncanonical["keys"]["auth"] = "AAECAwQFBgcICQoLDA0ODx".into();
    ok(
        &mut steps,
        request(
            "session-one",
            json!({"action":"subscribe","subscription":noncanonical}),
        ),
    );
    ok(&mut steps, subscribe("session-one", "same"));
    let queued = ok(
        &mut steps,
        request(
            "session-one",
            json!({"action":"test","endpoint":subscription("same")["endpoint"]}),
        ),
    );
    let original = ok(&mut steps, next());
    ok(&mut steps, subscribe("session-two", "same"));
    let mut changed = subscription("same");
    changed["keys"]["auth"] = "AAAAAAAAAAAAAAAAAAAAAA".into();
    let different = push(
        &mut steps,
        request(
            "session-one",
            json!({"action":"subscribe","subscription":changed}),
        ),
    );
    ok(
        &mut steps,
        request(
            "session-two",
            json!({"action":"test","endpoint":subscription("same")["endpoint"]}),
        ),
    );
    let replacement = ok(&mut steps, next());
    ok(
        &mut steps,
        action(json!({"action":"browserPush.complete","id":1,"status":410})),
    );
    let after = ok(&mut steps, next());
    let mut restart = setup()[0].clone();
    restart["method"] = "reopen".into();
    ok(&mut steps, restart);
    let restarted_keys = ok(&mut steps, action(json!({"action":"browserPush.keys"})));
    let restarted_queue = ok(&mut steps, next());
    let result = compare(&steps);
    assert!(
        result["rows"][status]["encodedValue"]
            .as_str()
            .unwrap()
            .starts_with("{\"publicKey\":")
    );
    assert_eq!(value(&result, restarted_keys), value(&result, keys));
    assert_eq!(value(&result, restarted_queue), value(&result, after));
    error(&result, forbidden, "forbidden");
    error(&result, auth, "unauthenticated");
    for i in invalid {
        error(&result, i, "invalid_arguments");
    }
    error(&result, different, "forbidden");
    assert_eq!(
        value(&result, keys)["publicKey"].as_str().unwrap().len(),
        87
    );
    assert_eq!(value(&result, queued), &json!({"queued":true}));
    assert_eq!(value(&result, original)["deliveries"][0]["id"], 1);
    assert_eq!(value(&result, replacement)["deliveries"][0]["id"], 2);
    assert_eq!(value(&result, after)["deliveries"][0]["id"], 2);
}

#[test]
fn browser_push_event_receipts_cursor_capacity_and_schema_filters_match_original() {
    let mut steps = setup();
    steps.push(event("old"));
    ok(&mut steps, subscribe("session-one", "one"));
    steps.push(event("first"));
    steps.push(event("first"));
    steps.push(publish(
        "future",
        "agent.browser-notification",
        "2.0.0",
        "service:agent",
        notice("future"),
    ));
    steps.push(publish(
        "wrong-source",
        "agent.browser-notification",
        "1.0.0",
        "browser",
        notice("wrong-source"),
    ));
    let mut invalid = notice("invalid");
    invalid["extra"] = true.into();
    steps.push(publish(
        "invalid",
        "agent.browser-notification",
        "1.0.0",
        "service:agent",
        invalid,
    ));
    steps.push(publish(
        "other-source",
        "agent.browser-notification",
        "1.0.0",
        "service:other",
        notice("first"),
    ));
    let mut unicode = notice("unicode");
    unicode["title"] = "🙂".repeat(100).into();
    unicode["body"] = "🙂".repeat(300).into();
    steps.push(publish(
        "unicode",
        "agent.browser-notification",
        "1.0.0",
        "service:agent",
        unicode,
    ));
    let delivered = ok(&mut steps, next());
    steps.push(sql("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2048) INSERT OR IGNORE INTO browser_push_outbox(endpoint,tag,notice_json) SELECT 'https://fcm.googleapis.com/fcm/send/one','pending-'||x,'{}' FROM n; DELETE FROM browser_push_outbox WHERE id IN (SELECT id FROM browser_push_outbox ORDER BY id DESC LIMIT (SELECT COUNT(*)-2048 FROM browser_push_outbox));"));
    steps.push(event("blocked"));
    let blocked = ok(&mut steps, next());
    let capacity = push(
        &mut steps,
        request(
            "session-one",
            json!({"action":"test","endpoint":subscription("one")["endpoint"]}),
        ),
    );
    ok(
        &mut steps,
        action(json!({"action":"browserPush.complete","id":1,"status":201})),
    );
    let admitted = ok(&mut steps, next());
    steps.push(sql("DELETE FROM browser_push_outbox; WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100) INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) SELECT 'ignored','1.0.0','service:agent','2026-01-01','ignored-'||x,'{malformed ignored' FROM n;"));
    let page = ok(&mut steps, next());
    let end = ok(&mut steps, next());
    let result = compare(&steps);
    let deliveries = value(&result, delivered)["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 3);
    assert_eq!(
        deliveries[2]["notice"]["title"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        50
    );
    assert_eq!(
        deliveries[2]["notice"]["body"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        150
    );
    assert_ne!(
        deliveries[0]["notice"]["tag"],
        deliveries[1]["notice"]["tag"]
    );
    assert_eq!(cursor(&result, blocked), cursor(&result, delivered));
    assert_eq!(value(&result, blocked)["hasMore"], false);
    error(&result, capacity, "capacity_exceeded");
    assert!(cursor(&result, admitted) > cursor(&result, blocked));
    assert_eq!(value(&result, page)["hasMore"], true);
    assert_eq!(value(&result, end)["hasMore"], false);
}

#[test]
fn browser_push_retries_retention_revocation_and_transaction_failures_match_original() {
    let mut steps = setup();
    ok(&mut steps, subscribe("session-one", "retry"));
    ok(
        &mut steps,
        request(
            "session-one",
            json!({"action":"test","endpoint":subscription("retry")["endpoint"]}),
        ),
    );
    let mut retries = vec![];
    for attempt in 0..5 {
        steps.push(sql("UPDATE browser_push_outbox SET next_attempt=0"));
        ok(&mut steps, next());
        let i = ok(
            &mut steps,
            action(json!({"action":"browserPush.complete","id":1,"status":503})),
        );
        retries.push((i, attempt));
    }
    ok(&mut steps, subscribe("session-one", "failure"));
    steps.push(event("rollback"));
    steps.push(sql("CREATE TRIGGER push_fault BEFORE INSERT ON browser_push_receipts BEGIN SELECT RAISE(ABORT,'receipt failure'); END;"));
    let rollback = push(&mut steps, next());
    steps.push(sql("DROP TRIGGER push_fault"));
    let retry = ok(&mut steps, next());
    steps.push(sql("CREATE TABLE push_deferred(value TEXT REFERENCES metadata(key) DEFERRABLE INITIALLY DEFERRED) STRICT; CREATE TRIGGER push_fault AFTER UPDATE ON metadata WHEN NEW.key='browser_push_cursor' BEGIN INSERT INTO push_deferred VALUES('missing'); END;"));
    steps.push(event("commit"));
    let commit = push(&mut steps, next());
    steps.push(sql("DROP TRIGGER push_fault"));
    ok(&mut steps, next());
    steps.push(sql("UPDATE browser_push SET last_error='prior error' WHERE endpoint=(SELECT endpoint FROM browser_push_outbox WHERE id=2); CREATE TRIGGER complete_fault BEFORE DELETE ON browser_push_outbox WHEN OLD.id=2 BEGIN SELECT RAISE(ABORT,'complete delete failure'); END;"));
    let partial = push(
        &mut steps,
        action(json!({"action":"browserPush.complete","id":2,"status":201})),
    );
    steps.push(sql("DROP TRIGGER complete_fault;"));
    ok(
        &mut steps,
        action(json!({"action":"browserPush.complete","id":2,"status":201})),
    );
    steps.push(sql("CREATE TRIGGER push_fault BEFORE INSERT ON browser_push WHEN NEW.endpoint LIKE '%failure' BEGIN SELECT RAISE(ABORT,'replace failure'); END;"));
    let replace = push(&mut steps, subscribe("session-two", "failure"));
    steps.push(sql("DROP TRIGGER push_fault"));
    steps.push(sql("INSERT INTO browser_push_receipts VALUES('old',-604700001),('boundary',-604700000); WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10002) INSERT INTO browser_push_receipts SELECT 'retained-'||x,x FROM n;"));
    let retained = ok(&mut steps, next());
    steps.push(sql(
        "DELETE FROM browser_sessions WHERE digest='session-one'",
    ));
    let logout = ok(&mut steps, next());
    ok(&mut steps, subscribe("session-two", "revoked"));
    steps.push(event("revoked"));
    steps.push(sql("UPDATE credentials SET revoked=1"));
    let revoked = ok(&mut steps, next());
    let result = compare(&steps);
    for (i, attempt) in retries {
        let q = state(&result, i)["outbox"].as_array().unwrap();
        if attempt == 4 {
            assert!(q.is_empty());
        } else {
            assert_eq!(q[0]["attempts"], attempt + 1);
            assert_eq!(q[0]["next_attempt"], 100000 + 30000 * (1 << attempt));
        }
    }
    error(&result, partial, "internal_error");
    assert_eq!(
        state(&result, partial)["outbox"],
        state(&result, partial - 1)["outbox"]
    );
    assert_ne!(
        state(&result, partial)["push"],
        state(&result, partial - 1)["push"]
    );
    for i in [rollback, commit, replace] {
        error(&result, i, "internal_error");
        assert_eq!(state(&result, i), state(&result, i - 1));
    }
    assert!(
        !value(&result, retry)["deliveries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        state(&result, retained)["pushReceipts"]
            .as_array()
            .unwrap()
            .len(),
        10000
    );
    assert!(
        state(&result, logout)["push"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        state(&result, revoked)["push"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn browser_push_subscription_limits_apply_after_existing_secret_ownership_and_preserve_rebinds() {
    let mut steps = setup();
    ok(&mut steps, subscribe("session-one", "kept"));
    steps.push(sql("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<15) INSERT INTO browser_push SELECT 'https://fcm.googleapis.com/fill/'||x,'session-one','{}',NULL,0 FROM n;"));
    let session_limit = push(&mut steps, subscribe("session-one", "new"));
    steps.push(sql("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1008) INSERT INTO browser_push SELECT 'https://fcm.googleapis.com/global/'||x,'session-two','{}',NULL,0 FROM n;"));
    let global_limit = push(&mut steps, subscribe("session-two", "global"));
    let rebound = ok(&mut steps, subscribe("session-two", "kept"));
    let mut other = subscription("kept");
    other["keys"]["auth"] = "AAAAAAAAAAAAAAAAAAAAAA".into();
    let ownership = push(
        &mut steps,
        request(
            "session-two",
            json!({"action":"subscribe","subscription":other}),
        ),
    );
    let result = compare(&steps);
    error(&result, session_limit, "limit_exceeded");
    error(&result, global_limit, "limit_exceeded");
    error(&result, ownership, "forbidden");
    assert_eq!(
        result["rows"][session_limit]["error"]["message"],
        "Browser push subscription capacity reached."
    );
    assert_eq!(
        result["rows"][global_limit]["error"]["message"],
        "Push subscription capacity reached."
    );
    let rows = state(&result, rebound)["push"].as_array().unwrap();
    assert_eq!(rows.len(), 1024);
    assert_eq!(
        rows.iter()
            .find(|v| v["endpoint"] == subscription("kept")["endpoint"])
            .unwrap()["session_digest"],
        "session-two"
    );
}

#[test]
fn browser_push_native_integers_preserve_ids_deadlines_and_receipt_cutoff() {
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    store
        .configure_credentials(&[Credential {
            principal_id: "alpha".into(),
            digest: digest().into(),
        }])
        .unwrap();
    let now = 9_007_199_254_740_993_i64;
    let clock = Rc::new(Cell::new(now));
    let observed_clock = clock.clone();
    let push = BrowserPushStore::new(
        &store,
        &"https://example.test/ivy/".into(),
        Rc::new(ObservedCrypto::new(&json!({"vapid":[],"points":[]}))),
        Rc::new(move || observed_clock.get()),
    )
    .unwrap();
    store
        .connection()
        .execute_batch(&format!(
            "INSERT INTO browser_sessions VALUES('session-one','{}','2026-01-01');
         INSERT INTO browser_push VALUES('endpoint','session-one','{{}}',NULL,0);
         INSERT INTO browser_push_outbox(id,endpoint,tag,notice_json)
         VALUES(9007199254740991,'endpoint','native','{{}}');",
            digest()
        ))
        .unwrap();
    let cutoff = now - 7 * 86_400_000;
    for (tag, created_at) in [("expired", cutoff - 1), ("boundary", cutoff)] {
        store
            .connection()
            .execute(
                "INSERT INTO browser_push_receipts VALUES(?,?)",
                rusqlite::params![tag, created_at],
            )
            .unwrap();
    }
    let batch = push.next(&mut store).unwrap().to_serde().unwrap();
    assert_eq!(
        batch["deliveries"][0]["id"].as_u64(),
        Some(9_007_199_254_740_991)
    );
    assert_eq!(
        rows(&store, "SELECT tag FROM browser_push_receipts"),
        json!([{"tag":"boundary"}])
    );
    push.complete(&store, 9_007_199_254_740_991, 503).unwrap();
    let retained: (i64, u32, i64, String) = store
        .connection()
        .query_row(
            "SELECT id,attempts,next_attempt,typeof(next_attempt) FROM browser_push_outbox",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        retained,
        (9_007_199_254_740_991, 1, now + 30_000, "integer".into())
    );
    clock.set(now + 29_999);
    assert!(
        push.next(&mut store)
            .unwrap()
            .get("deliveries")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    clock.set(now + 30_000);
    assert_eq!(
        push.next(&mut store)
            .unwrap()
            .get("deliveries")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    push.complete(&store, 9_007_199_254_740_991, 201).unwrap();
    store
        .connection()
        .execute_batch(
            "INSERT INTO browser_push_outbox(id,endpoint,tag,notice_json)
         VALUES(9007199254740993,'endpoint','unsafe','{}');",
        )
        .unwrap();
    assert_eq!(push.next(&mut store).unwrap_err().code, "storage_invalid");
    assert_eq!(
        store
            .connection()
            .query_row("SELECT id FROM browser_push_outbox", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        9_007_199_254_740_993
    );
    for invalid in ["-0", "-1", "1.5", "9007199254740992"] {
        store.set_metadata("browser_push_cursor", invalid).unwrap();
        assert_eq!(push.next(&mut store).unwrap_err().code, "storage_invalid");
        assert_eq!(
            store.metadata("browser_push_cursor").unwrap().as_deref(),
            Some(invalid)
        );
    }
}

#[test]
fn browser_push_worker_rejects_non_integer_completion_without_mutation() {
    let mut steps = setup();
    ok(&mut steps, subscribe("session-one", "native"));
    ok(
        &mut steps,
        request(
            "session-one",
            json!({"action":"test","endpoint":subscription("native")["endpoint"]}),
        ),
    );
    let root = Root::new();
    let observed = oracle(&root.0.join("observed.sqlite"), &steps);
    let mut profile = observed["registryRuntime"].clone();
    profile["browserPush"] = observed["browserPushRuntime"].clone();
    let mut rejected = vec![];
    for id in [
        json!(1.5),
        json!(-1),
        json!(9_007_199_254_740_992_u64),
        json!("1"),
        Value::Null,
    ] {
        rejected.push(push(
            &mut steps,
            action(json!({"action":"browserPush.complete","id":id,"status":201})),
        ));
    }
    for status in [
        json!(201.5),
        json!(-1),
        json!(65536),
        json!("201"),
        Value::Null,
    ] {
        rejected.push(push(
            &mut steps,
            action(json!({"action":"browserPush.complete","id":1,"status":status})),
        ));
    }
    let retried = ok(
        &mut steps,
        action(json!({"action":"browserPush.complete","id":1,"status":0})),
    );
    let result = native(&root.0.join("native.sqlite"), &steps, &profile);
    for index in rejected {
        error(&result, index, "internal_error");
        assert_eq!(state(&result, index), state(&result, index - 1));
    }
    assert_eq!(state(&result, retried)["outbox"][0]["attempts"], 1);
    assert_eq!(state(&result, retried)["outbox"][0]["next_attempt"], 130000);
}
