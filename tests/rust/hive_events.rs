use super::{AuthContext, AuthenticatedContext, IvyError, IvyJson, JsString, Store};
use ivy_contracts::IvyResult;
use ivy_hive::events;
use serde_json::{Value, json};
use std::time::Duration;

pub(super) fn native_event(store: &mut Store, step: &Value) -> IvyResult<IvyJson> {
    let principal = if step["context"].is_object() {
        Some(store.authenticate(&AuthContext {
            credential_digest: step["context"]["credentialDigest"].as_str().unwrap().into(),
            session_digest: None,
        })?)
    } else {
        None
    };
    let context = principal.map(|p| AuthenticatedContext {
        principal_id: p.principal_id,
        credential_digest: p.credential_digest,
    });
    let node = step["context"]["serviceNodeId"]
        .as_str()
        .map(JsString::from);
    let mut params = IvyJson::from_serde(&step["params"]).unwrap();
    super::object_write::apply_units(&mut params, step);
    store.with_budget_mut(
        if step["overdue"] == true {
            Duration::ZERO
        } else {
            Duration::from_secs(10)
        },
        |store| match step["method"].as_str().unwrap() {
            "registerTopic" => {
                events::register_topic(store, &params, &step["owner"].as_str().unwrap().into())
                    .map(|_| IvyJson::Null)
            }
            "publish" => events::publish(
                store,
                context.as_ref().unwrap(),
                node.as_ref().unwrap(),
                &params,
                step["now"].as_u64().unwrap(),
                &step["nowText"].as_str().unwrap().into(),
            ),
            "subscribe" => events::subscribe(store, node.as_ref().unwrap(), &params),
            "ack" => events::ack(store, node.as_ref().unwrap(), &params),
            "unsubscribe" => events::unsubscribe(
                store,
                context.as_ref().unwrap(),
                node.as_ref().unwrap(),
                &params,
                step["now"].as_u64().unwrap(),
            ),
            "read" => events::read(store, &params),
            "head" => events::head(store),
            _ => Err(IvyError::internal()),
        },
    )
}
fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn event(method: &str, node: Option<&str>, params: Value) -> Value {
    let mut step = json!({"op":"events","method":method,"params":params,"now":100000,"nowText":"1970-01-01T00:01:40.000Z"});
    if let Some(node) = node {
        step["context"] =
            json!({"credentialDigest":format!("sha256:{}","a".repeat(64)),"serviceNodeId":node});
    }
    step
}
fn topic() -> Value {
    let mut step = event(
        "registerTopic",
        None,
        json!({"topic":"fixture.changed","version":"1.0.0","payloadSchema":{"type":"object","required":["n"],"properties":{"n":{"type":"integer"}},"additionalProperties":false}}),
    );
    step["owner"] = "service:fixture-publisher".into();
    step
}
fn publish(nonce: &str, n: i64) -> Value {
    event(
        "publish",
        Some("publisher"),
        json!({"topic":"fixture.changed","topicVersion":"1.0.0","payload":{"n":n},"mutationId":format!("epoch:1000:{nonce}")}),
    )
}
fn subscribe(node: &str, name: &str, limit: u64) -> Value {
    event(
        "subscribe",
        Some(node),
        json!({"name":name,"filter":{},"limit":limit}),
    )
}
fn ack(node: &str, name: &str, through: u64) -> Value {
    event(
        "ack",
        Some(node),
        json!({"name":name,"throughSequence":through}),
    )
}
fn unsubscribe(node: &str, name: &str, nonce: &str) -> Value {
    event(
        "unsubscribe",
        Some(node),
        json!({"name":name,"mutationId":format!("epoch:1000:{nonce}")}),
    )
}
fn setup() -> Vec<Value> {
    vec![
        sql("UPDATE metadata SET value='epoch' WHERE key='runtime_epoch';"),
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":format!("sha256:{}","a".repeat(64))},{"principalId":"beta","digest":format!("sha256:{}","b".repeat(64))}]}),
        sql(
            "INSERT INTO service_nodes VALUES('publisher','alpha','fixture-host','fixture-publisher',1,'{}'),('peer','alpha','fixture-host','fixture-publisher',1,'{}'),('secretary','alpha','fixture-host','secretary',1,'{}'),('other','alpha','fixture-host','other-service',1,'{}'); INSERT INTO registries VALUES('publisher','{\"namespaces\":[{\"topics\":[{\"topic\":\"fixture.changed\",\"version\":\"1.0.0\"}]}]}');",
        ),
        topic(),
    ]
}
fn journal(count: u64) -> Value {
    sql(format!(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{count}) INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) SELECT 'hive.object.changed','1.0.0',CASE WHEN i%2=0 THEN 'source:even' ELSE 'source:odd' END,'2026-01-01','event-'||i,json_object('objectId','object-'||i,'revision',1,'operation','write') FROM n;"
    ))
}
fn compare(steps: &[Value]) -> Value {
    super::object_mutations::compare(steps)
}
fn state(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["state"]
}
fn success(rows: &Value, index: usize) {
    assert!(
        rows["rows"][index]["error"].is_null(),
        "Event trace {index}: {}",
        rows["rows"][index]["error"]
    );
}
fn no_effect(rows: &Value, index: usize, code: &str) {
    assert_eq!(rows["rows"][index]["error"]["data"]["code"], code);
    assert_eq!(state(rows, index), state(rows, index - 1));
}
fn value(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["value"]
}

#[test]
fn topic_registration_and_service_publication_preserve_schema_ownership_registry_and_receipt_order()
{
    for (encoded, expected) in [
        ("null", "registry_invalid"),
        ("{}", "internal_error"),
        ("{", "internal_error"),
    ] {
        let mut steps = setup();
        steps.push(sql(format!(
            "UPDATE registries SET registry_json='{encoded}' WHERE node_id='publisher';"
        )));
        let failed = steps.len();
        steps.push(publish("malformed-registry", 1));
        let rows = compare(&steps);
        no_effect(&rows, failed, expected);
    }
    let mut steps = setup();
    steps.push(topic());
    let mut owner = topic();
    owner["owner"] = "service:other-service".into();
    let wrong_owner = steps.len();
    steps.push(owner);
    let mut changed = topic();
    changed["params"]["payloadSchema"] = json!({"type":"string"});
    let changed_schema = steps.len();
    steps.push(changed);
    let mut invalid = topic();
    invalid["params"]["payloadSchema"] = json!({"type":"not-a-type"});
    let invalid_schema = steps.len();
    steps.push(invalid);
    let first = steps.len();
    let pending = publish("first", 1);
    steps.push(pending.clone());
    let mut wrong = publish("owner", 1);
    wrong["context"]["serviceNodeId"] = "other".into();
    let denied = steps.len();
    steps.push(wrong);
    let mut bad = publish("invalid", 1);
    bad["params"]["payload"] = json!({"n":"bad"});
    let payload = steps.len();
    steps.push(bad);
    steps.push(sql("DELETE FROM registries WHERE node_id='publisher';"));
    let replay = steps.len();
    steps.push(pending.clone());
    let withdrawn = steps.len();
    steps.push(publish("withdrawn", 2));
    let mut peer = pending.clone();
    peer["context"]["serviceNodeId"] = "peer".into();
    let same_principal = steps.len();
    steps.push(peer);
    steps.push(sql("DELETE FROM service_nodes WHERE id='publisher';"));
    let missing = steps.len();
    steps.push(pending);
    let rows = compare(&steps);
    for index in [
        wrong_owner,
        changed_schema,
        invalid_schema,
        denied,
        withdrawn,
    ] {
        no_effect(&rows, index, "registry_invalid");
    }
    no_effect(&rows, payload, "invalid_arguments");
    no_effect(&rows, missing, "not_found");
    for index in [first, replay, same_principal] {
        success(&rows, index);
        assert_eq!(value(&rows, index), value(&rows, first));
    }
    assert_eq!(
        value(&rows, first),
        &json!({"sequence":1,"topic":"fixture.changed","publishedAt":"1970-01-01T00:01:40.000Z"})
    );
    assert_eq!(
        rows["rows"][denied]["error"]["message"],
        "The service does not own this topic version."
    );
    assert_eq!(
        rows["rows"][withdrawn]["error"]["message"],
        "Topic is not in this node's current registry."
    );
}

#[test]
fn event_reads_keep_fixed_scan_filter_boundaries_work_budgets_gaps_and_deliverable_byte_limits() {
    let mut legacy = setup();
    legacy.push(sql("INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES('hive.object.changed','1.0.0','source','now','id','null');"));
    let skipped = legacy.len();
    legacy.push(event(
        "read",
        None,
        json!({"afterSequence":0,"filter":{"topics":[],"objectIds":["x"]}}),
    ));
    let malformed = legacy.len();
    legacy.push(event(
        "read",
        None,
        json!({"afterSequence":0,"filter":{"objectIds":["x"]}}),
    ));
    let rows = compare(&legacy);
    success(&rows, skipped);
    no_effect(&rows, malformed, "internal_error");
    let mut steps = setup();
    steps.push(journal(205));
    let start = steps.len();
    steps.push(event(
        "read",
        None,
        json!({"afterSequence":0,"filter":{"topics":[]}}),
    ));
    steps.push(event("read",None,json!({"afterSequence":100,"filter":{"sources":["source:even"],"objectIds":["object-150"]},"limit":1})));
    steps.push(event(
        "read",
        None,
        json!({"afterSequence":150,"filter":{},"limit":2}),
    ));
    let overdue = steps.len();
    let mut expired = event("read", None, json!({"afterSequence":0,"filter":{}}));
    expired["overdue"] = true.into();
    steps.push(expired);
    let empty = steps.len();
    let mut tail = event("read", None, json!({"afterSequence":205,"filter":{}}));
    tail["overdue"] = true.into();
    steps.push(tail);
    steps.push(sql(
        "UPDATE metadata SET value='100' WHERE key='events_pruned_through';",
    ));
    let gap = steps.len();
    let mut gap_read = event(
        "read",
        None,
        json!({"afterSequence":99,"filter":{"topics":[]}}),
    );
    gap_read["overdue"] = true.into();
    steps.push(gap_read);
    steps.push(event("head", None, json!({})));
    steps.push(sql(
        "UPDATE metadata SET value='500' WHERE key='events_pruned_through';",
    ));
    let head = steps.len();
    steps.push(event("head", None, json!({})));
    let rows = compare(&steps);
    for index in [start, start + 1, start + 2, empty, gap, gap + 1, head] {
        success(&rows, index);
    }
    assert_eq!(
        value(&rows, start),
        &json!({"items":[],"throughSequence":100,"hasMore":true,"gap":null})
    );
    assert_eq!(value(&rows, start + 1)["throughSequence"], 150);
    assert_eq!(
        value(&rows, start + 1)["items"].as_array().unwrap().len(),
        1
    );
    assert_eq!(value(&rows, start + 2)["throughSequence"], 152);
    no_effect(&rows, overdue, "limit_exceeded");
    assert_eq!(
        value(&rows, empty),
        &json!({"items":[],"throughSequence":205,"hasMore":false,"gap":null})
    );
    assert_eq!(
        value(&rows, gap),
        &json!({"items":[],"throughSequence":100,"hasMore":true,"gap":{"prunedThroughSequence":100,"resumeAfterSequence":100}})
    );
    assert_eq!(value(&rows, head)["throughSequence"], 500);

    let mut steps = setup();
    let data = "x".repeat(400_000);
    steps.push(sql(format!("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<3) INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) SELECT 'hive.object.changed','1.0.0','source','now','id',json_quote('{data}') FROM n;")));
    let limited = steps.len();
    steps.push(event("read", None, json!({"afterSequence":0,"filter":{}})));
    steps.push(event("read", None, json!({"afterSequence":2,"filter":{}})));
    let rows = compare(&steps);
    success(&rows, limited);
    success(&rows, limited + 1);
    assert_eq!(value(&rows, limited)["items"].as_array().unwrap().len(), 2);
    assert_eq!(value(&rows, limited)["throughSequence"], 2);
    assert_eq!(value(&rows, limited)["hasMore"], true);
    assert_eq!(value(&rows, limited + 1)["throughSequence"], 3);
    let overhead=json!({"sequence":1,"topic":"hive.object.changed","topicVersion":"1.0.0","source":"source","occurredAt":"now","mutationId":"id","payload":""}).to_string().len();
    let data = "x".repeat(1024 * 1024 - 1024 - 128 + 1 - overhead);
    let mut steps = setup();
    steps.push(sql(format!("INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES('hive.object.changed','1.0.0','source','now','id',json_quote('{data}'));")));
    let oversized = steps.len();
    steps.push(event("read", None, json!({"afterSequence":0,"filter":{}})));
    let rows = compare(&steps);
    no_effect(&rows, oversized, "result_too_large");
    assert_eq!(
        rows["rows"][oversized]["error"]["message"],
        "Journal event cannot fit a deliverable batch."
    );
}

#[test]
fn subscriptions_freeze_issued_batches_and_acknowledge_exact_empty_and_idempotent_boundaries() {
    let mut steps = setup();
    steps.push(journal(3));
    let first = steps.len();
    let mut original = subscribe("secretary", "work", 1);
    original["params"]["durable"] = true.into();
    steps.push(original.clone());
    steps.push(journal(1));
    let repeated = steps.len();
    let mut retry = original.clone();
    retry["params"]["limit"] = 3.into();
    retry["overdue"] = true.into();
    steps.push(retry);
    let conflict = steps.len();
    let mut changed = original.clone();
    changed["params"]["filter"] = json!({"topics":[]});
    steps.push(changed);
    let invalid = steps.len();
    steps.push(ack("secretary", "work", 2));
    let null_gap = steps.len();
    let mut bad = ack("secretary", "work", 1);
    bad["params"]["gapThroughSequence"] = Value::Null;
    steps.push(bad);
    let accepted = steps.len();
    steps.push(ack("secretary", "work", 1));
    steps.push(ack("secretary", "work", 1));
    let next = steps.len();
    steps.push(subscribe("secretary", "work", 2));
    let old = steps.len();
    steps.push(ack("secretary", "work", 1));
    steps.push(ack("secretary", "work", 3));
    steps.push(subscribe("secretary", "work", 1));
    steps.push(ack("secretary", "work", 4));
    let empty = steps.len();
    steps.push(subscribe("secretary", "work", 1));
    let clear_empty = steps.len();
    steps.push(ack("secretary", "work", 4));
    steps.push(journal(1));
    let last = steps.len();
    steps.push(subscribe("secretary", "work", 1));
    let rows = compare(&steps);
    for index in [
        first,
        repeated,
        accepted,
        accepted + 1,
        next,
        old,
        empty,
        clear_empty,
        last,
    ] {
        success(&rows, index);
    }
    assert_eq!(value(&rows, first), value(&rows, repeated));
    no_effect(&rows, conflict, "subscription_filter_conflict");
    no_effect(&rows, invalid, "invalid_ack");
    no_effect(&rows, null_gap, "invalid_ack");
    assert_eq!(value(&rows, next)["throughSequence"], 3);
    assert_eq!(value(&rows, next)["hasMore"], true);
    assert_eq!(state(&rows, old), state(&rows, old - 1));
    assert_eq!(
        value(&rows, empty),
        &json!({"items":[],"throughSequence":4,"hasMore":false,"gap":null})
    );
    assert!(state(&rows, clear_empty)["subscriptions"][0]["batch_json"].is_null());
    assert_eq!(value(&rows, last)["items"][0]["sequence"], 5);
    assert_eq!(
        state(&rows, last)["durable"],
        json!([{"node_id":"secretary","name":"work"}])
    );
}

#[test]
fn missing_issued_rows_restart_gaps_capacity_and_durable_pruning_preserve_delivery_state() {
    let mut steps = setup();
    steps.push(journal(3));
    steps.push(subscribe("secretary", "work", 3));
    steps.push(sql("DELETE FROM events WHERE sequence=2;"));
    let budget = steps.len();
    let mut expired = subscribe("secretary", "work", 3);
    expired["overdue"] = true.into();
    steps.push(expired);
    let rebuilt = steps.len();
    steps.push(subscribe("secretary", "work", 3));
    steps.push(sql("DELETE FROM events WHERE sequence<=3; UPDATE metadata SET value='3' WHERE key='events_pruned_through';"));
    let gap = steps.len();
    steps.push(subscribe("secretary", "work", 3));
    let wrong = steps.len();
    steps.push(ack("secretary", "work", 3));
    let accepted = steps.len();
    let mut acknowledge = ack("secretary", "work", 3);
    acknowledge["params"]["gapThroughSequence"] = 3.into();
    steps.push(acknowledge);
    steps.push(journal(2));
    steps.push(subscribe("secretary", "work", 1));
    steps.push(json!({"op":"reopen"}));
    let restart = steps.len();
    steps.push(subscribe("secretary", "work", 2));
    let rows = compare(&steps);
    no_effect(&rows, budget, "limit_exceeded");
    success(&rows, rebuilt);
    success(&rows, gap);
    no_effect(&rows, wrong, "invalid_ack");
    success(&rows, accepted);
    success(&rows, restart);
    assert_eq!(
        value(&rows, rebuilt)["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["sequence"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(value(&rows, gap)["gap"]["prunedThroughSequence"], 3);
    assert_eq!(value(&rows, restart)["throughSequence"], 5);

    let mut steps = setup();
    steps.push(journal(10_000));
    let mut durable = subscribe("secretary", "backlog", 1);
    durable["params"]["durable"] = true.into();
    durable["params"]["filter"] = json!({"topics":[]});
    let issued = steps.len();
    steps.push(durable);
    let full = steps.len();
    steps.push(publish("full", 1));
    let acknowledged = steps.len();
    steps.push(ack("secretary", "backlog", 100));
    let published = steps.len();
    steps.push(publish("admitted", 2));
    steps.push(journal(99));
    let removed = steps.len();
    let original = unsubscribe("secretary", "backlog", "unsubscribe");
    steps.push(original.clone());
    steps.push(subscribe("secretary", "backlog", 1));
    let replay = steps.len();
    steps.push(original);
    let rows = compare(&steps);
    success(&rows, issued);
    no_effect(&rows, full, "capacity_exceeded");
    for index in [acknowledged, published, removed, replay] {
        success(&rows, index);
    }
    assert_eq!(value(&rows, issued)["throughSequence"], 100);
    assert_eq!(value(&rows, published)["sequence"], 10001);
    assert_eq!(value(&rows, removed)["removed"], true);
    assert!(
        state(&rows, removed)["durable"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(state(&rows, replay), state(&rows, replay - 1));
    let metadata = state(&rows, removed)["metadata"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == "events_pruned_through")
        .unwrap();
    assert_eq!(metadata["value"], "1124");
    let mut steps = setup();
    steps.push(sql("WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i<63) INSERT INTO subscriptions(node_id,name,filter_json,initial_sequence,acknowledged_sequence,batch_json) SELECT 'other','sub-'||i,'{}',0,0,NULL FROM n;"));
    let capacity = steps.len();
    steps.push(subscribe("other", "new", 1));
    let existing = steps.len();
    steps.push(subscribe("other", "sub-0", 1));
    let forbidden = steps.len();
    let mut wrong = subscribe("other", "new", 1);
    wrong["params"]["durable"] = true.into();
    steps.push(wrong);
    let rows = compare(&steps);
    no_effect(&rows, capacity, "capacity_exceeded");
    success(&rows, existing);
    no_effect(&rows, forbidden, "forbidden");
}

#[test]
fn event_mutation_subscription_and_ack_failures_roll_back_work_pruning_receipts_and_retries() {
    for trigger in [
        "CREATE TRIGGER reject_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'fixture_event'); END;",
        "CREATE TRIGGER reject_event BEFORE INSERT ON mutations BEGIN SELECT RAISE(ABORT,'fixture_event'); END;",
        "CREATE TRIGGER reject_event AFTER INSERT ON mutations BEGIN INSERT INTO durable_subscriptions VALUES('missing','missing'); END; PRAGMA defer_foreign_keys=ON;",
    ] {
        let mut steps = setup();
        steps.push(sql(trigger));
        let failed = steps.len();
        let original = publish("retry", 1);
        steps.push(original.clone());
        steps.push(sql("DROP TRIGGER reject_event;"));
        let retried = steps.len();
        steps.push(original.clone());
        steps.push(original);
        let rows = compare(&steps);
        no_effect(&rows, failed, "internal_error");
        success(&rows, retried);
        success(&rows, retried + 1);
        assert_eq!(value(&rows, retried), value(&rows, retried + 1));
    }
    for trigger in [
        "CREATE TRIGGER reject_event BEFORE INSERT ON subscriptions BEGIN SELECT RAISE(ABORT,'fixture_event'); END;",
        "CREATE TRIGGER reject_event BEFORE INSERT ON durable_subscriptions BEGIN SELECT RAISE(ABORT,'fixture_event'); END;",
        "CREATE TRIGGER reject_event BEFORE UPDATE OF batch_json ON subscriptions BEGIN SELECT RAISE(ABORT,'fixture_event'); END;",
    ] {
        let mut steps = setup();
        steps.push(journal(1));
        steps.push(sql(trigger));
        let failed = steps.len();
        let mut original = subscribe("secretary", "work", 1);
        original["params"]["durable"] = true.into();
        steps.push(original.clone());
        steps.push(sql("DROP TRIGGER reject_event;"));
        let retried = steps.len();
        steps.push(original);
        let rows = compare(&steps);
        no_effect(&rows, failed, "internal_error");
        success(&rows, retried);
    }
    let mut steps = setup();
    steps.push(journal(10_000));
    let mut durable = subscribe("secretary", "work", 1);
    durable["params"]["durable"] = true.into();
    steps.push(durable);
    steps.push(sql("CREATE TRIGGER reject_event BEFORE UPDATE OF value ON metadata WHEN OLD.key='events_pruned_through' BEGIN SELECT RAISE(ABORT,'fixture_event'); END;"));
    let failed = steps.len();
    steps.push(ack("secretary", "work", 1));
    steps.push(sql("DROP TRIGGER reject_event;"));
    let retried = steps.len();
    steps.push(ack("secretary", "work", 1));
    steps.push(sql("CREATE TRIGGER reject_event BEFORE INSERT ON mutations BEGIN SELECT RAISE(ABORT,'fixture_event'); END;"));
    let unsubscribe_failed = steps.len();
    let original = unsubscribe("secretary", "work", "retry-unsubscribe");
    steps.push(original.clone());
    steps.push(sql("DROP TRIGGER reject_event;"));
    let removed = steps.len();
    steps.push(original);
    let rows = compare(&steps);
    no_effect(&rows, failed, "internal_error");
    success(&rows, retried);
    no_effect(&rows, unsubscribe_failed, "internal_error");
    success(&rows, removed);
}

#[test]
fn event_strings_keep_raw_utf16_payloads_filters_and_sql_topic_identity_boundaries() {
    let mut steps = setup();
    let mut raw = topic();
    raw["params"]["payloadSchema"] = json!({"type":"string"});
    raw["parameterUnits"] = json!({"/topic":[0xd800]});
    let registered = steps.len();
    steps.push(raw.clone());
    let mut collision = raw;
    collision["parameterUnits"] = json!({"/topic":[0xfffd]});
    let refused = steps.len();
    steps.push(collision);
    steps.push(sql("UPDATE registries SET registry_json='{\"namespaces\":[{\"topics\":[{\"topic\":\"\\ud800\",\"version\":\"1.0.0\"}]}]}' WHERE node_id='publisher';"));
    let mut publication = publish("raw", 1);
    publication["params"]["payload"] = "unused".into();
    publication["parameterUnits"] = json!({"/topic":[0xd800],"/payload":[0xdfff]});
    publication["lossless"] = true.into();
    let published = steps.len();
    steps.push(publication);
    let read = steps.len();
    let mut lossless = event(
        "read",
        None,
        json!({"afterSequence":0,"filter":{"topics":["�"]}}),
    );
    lossless["lossless"] = true.into();
    steps.push(lossless);
    let not_matching = steps.len();
    steps.push(event(
        "read",
        None,
        json!({"afterSequence":0,"filter":{"topics":[]}}),
    ));
    let rows = compare(&steps);
    success(&rows, registered);
    no_effect(&rows, refused, "registry_invalid");
    success(&rows, published);
    success(&rows, read);
    success(&rows, not_matching);
    assert!(
        value(&rows, published)["encoded"]
            .as_str()
            .unwrap()
            .contains("\\ud800")
    );
    assert!(
        value(&rows, read)["encoded"]
            .as_str()
            .unwrap()
            .contains("\\udfff")
    );
    assert_eq!(value(&rows, not_matching)["throughSequence"], 1);
    assert!(
        value(&rows, not_matching)["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
