use super::{AuthenticatedContext, LedgerResult, Root, Store, native, reference as oracle};
use ivy_contracts::{
    IvyJson,
    hash::hash_json,
    limits::{
        MUTATION_RECEIPT_COUNT, MUTATION_RECEIPT_JOURNAL_BYTES, MUTATION_RECEIPT_RESULT_BYTES,
    },
};
use serde_json::{Value, json};

fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn context() -> Value {
    json!({"principalId":"alpha","credentialDigest":"fixture"})
}
fn mutation(id: &str, value: Value) -> Value {
    json!({"op":"mutate","context":context(),"method":"fixture","params":{"mutationId":id,"value":1},"now":100_000,"value":value,"sql":"INSERT INTO metadata VALUES('effect','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1","scope":"effect"})
}
fn setup() -> Value {
    sql(
        "UPDATE metadata SET value='epoch' WHERE key='runtime_epoch'; INSERT INTO principals VALUES('alpha'),('beta');",
    )
}
fn compare(steps: &[Value]) {
    let root = Root::new();
    let expected = oracle(&root.0.join("reference.sqlite"), steps);
    let actual = native(&root.0.join("native.sqlite"), steps);
    assert_eq!(actual["rows"].as_array().unwrap().len(), steps.len());
    assert_eq!(expected["rows"].as_array().unwrap().len(), steps.len());
    for (index, (left, right)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(left, right, "step {index}: {}", steps[index]);
    }
    assert_eq!(actual["metadata"], expected["metadata"]);
    assert_eq!(actual["definitions"], expected["definitions"]);
}

#[test]
fn mutation_replay_conflicts_contract_reference_and_failure_rollback_match_original() {
    let a = mutation("epoch:1000:a", json!({"accepted":"original"}));
    let mut replay = a.clone();
    replay["value"] = json!({"accepted":"replayed work must not run"});
    replay["fail"] = json!(true);
    let mut conflict = a.clone();
    conflict["params"]["value"] = json!(2);
    let mut other = a.clone();
    other["context"]["principalId"] = json!("beta");
    let hash = hash_json(
        &IvyJson::from_serde(&json!({"method":"fixture","arguments":{"value":1}})).unwrap(),
    )
    .unwrap();
    let mut work_failure = mutation("epoch:1001:fail", Value::Null);
    work_failure["fail"] = json!(true);
    let mut contract = mutation("epoch:1002:contract", json!({"fresh":true}));
    contract["kind"] = json!("contract");
    contract["reference"] = json!({"key":"fixture/value","version":"1.0.0"});
    let mut null_reference = mutation("epoch:1003:null-ref", json!(false));
    null_reference["reference"] = Value::Null;
    let mut empty_reference = mutation("epoch:1004:empty-ref", json!("first"));
    empty_reference["reference"] = json!(false);
    let mut event = mutation("epoch:1005:event", json!({"sequence":9}));
    event["kind"] = json!("event");
    let mut steps = vec![
        setup(),
        a.clone(),
        replay.clone(),
        conflict,
        other,
        json!({"op":"receipt","context":context(),"mutationId":"epoch:1000:a","hash":hash}),
        json!({"op":"receipt","context":context(),"mutationId":"epoch:1000:a","hash":"wrong"}),
        json!({"op":"receipt","context":context(),"mutationId":"epoch:1000:absent","hash":"wrong"}),
        mutation("other:1000:a", Value::Null),
        mutation("epoch:160001:future", Value::Null),
        mutation("epoch:0:zero", Value::Null),
        mutation("malformed", Value::Null),
        work_failure,
        sql(
            "INSERT INTO contract_families VALUES('fixture/value','alpha','application/json','retain'); INSERT INTO contracts VALUES('fixture/value','1.0.0','{\"key\":\"fixture/value\",\"version\":\"1.0.0\",\"nested\":{\"value\":1}}');",
        ),
        contract.clone(),
        contract,
        null_reference.clone(),
        null_reference,
        empty_reference.clone(),
        empty_reference,
        event.clone(),
        event,
        json!({"op":"receipt","context":context(),"mutationId":"epoch:1005:event","hash":hash}),
        mutation(
            "epoch:1006:large",
            json!("x".repeat(MUTATION_RECEIPT_RESULT_BYTES - 1)),
        ),
        sql("UPDATE metadata SET value='1000' WHERE key='mutations_expired_before';"),
        replay,
        mutation("epoch:1000:absent", Value::Null),
        json!({"op":"reopen"}),
        a,
        json!({"op":"contract","key":"fixture/value","version":"missing"}),
        sql(
            "INSERT INTO retention_families VALUES('fixture/value','hash',0,'{\"objectCount\":2,\"revisionCount\":3,\"byteLength\":10}');",
        ),
        json!({"op":"adjust","key":"fixture/value","objects":-5,"revisions":4,"bytes":-3}),
        sql(
            "UPDATE retention_families SET summary_json='{\"objectCount\":\"2\",\"revisionCount\":3,\"byteLength\":10}';",
        ),
        json!({"op":"adjust","key":"fixture/value","objects":1,"revisions":1,"bytes":1}),
    ];
    let mut failed_commit = mutation("epoch:1007:commit-fail", Value::Null);
    failed_commit["sql"] =
        json!("PRAGMA defer_foreign_keys=ON; INSERT INTO credentials VALUES('bad','missing',0);");
    steps.push(failed_commit);
    compare(&steps);
}

#[test]
fn receipt_fifo_count_byte_batches_and_eviction_failure_match_original() {
    let original = mutation("epoch:1000:original", json!({"original":true}));
    let newest = mutation("epoch:999:newest", json!({"admitted":true}));
    let mut steps = vec![
        setup(),
        original.clone(),
        sql(format!(
            "WITH RECURSIVE seq(n) AS (SELECT 0 UNION ALL SELECT n+1 FROM seq WHERE n<{}) INSERT INTO mutations SELECT 'alpha','bulk-'||n,'hash',2000,2000,'snapshot','{{}}',128 FROM seq; UPDATE metadata SET value=(SELECT SUM(receipt_bytes) FROM mutations) WHERE key='mutation_receipt_bytes';",
            MUTATION_RECEIPT_COUNT - 2
        )),
        json!({"op":"cleanup"}),
        original.clone(),
        sql(
            "CREATE TRIGGER fail_receipt BEFORE INSERT ON mutations BEGIN SELECT RAISE(ABORT,'injected'); END;",
        ),
        newest.clone(),
        sql("DROP TRIGGER fail_receipt;"),
        newest.clone(),
        original,
        newest,
        json!({"op":"reopen"}),
        mutation("epoch:1000:original", Value::Null),
        sql(
            "DELETE FROM mutations; UPDATE metadata SET value='0' WHERE key IN ('mutation_receipt_bytes','mutations_expired_before');",
        ),
        sql(format!(
            "WITH RECURSIVE seq(n) AS (SELECT 0 UNION ALL SELECT n+1 FROM seq WHERE n<999) INSERT INTO mutations SELECT 'alpha','bytes-'||n,'hash',1000,1000,'snapshot','{{}}',100 FROM seq; UPDATE mutations SET receipt_bytes={} WHERE mutation_id='bytes-999'; UPDATE metadata SET value='{}' WHERE key='mutation_receipt_bytes';",
            MUTATION_RECEIPT_JOURNAL_BYTES - 32 - 999 * 100,
            MUTATION_RECEIPT_JOURNAL_BYTES - 32
        )),
        mutation(
            "epoch:2001:bytes",
            json!("x".repeat(MUTATION_RECEIPT_RESULT_BYTES - 2)),
        ),
        json!({"op":"cleanup","bytes":MUTATION_RECEIPT_JOURNAL_BYTES+1}),
        sql(
            "DELETE FROM mutations; UPDATE metadata SET value='268435457' WHERE key='mutation_receipt_bytes';",
        ),
        json!({"op":"cleanup"}),
    ];
    // Retained replay is checked before the time fence, even for out-of-order issue times.
    steps.insert(11, mutation("epoch:999:newest", Value::Null));
    compare(&steps);
}

fn event(operation: &str, key: &str) -> Value {
    json!({"op":"event","principal":{"principalId":"alpha"},"objectId":"object","revision":1,"operation":operation,"mutationId":"epoch:1000:event","now":"2026-01-01T00:00:00.000Z","key":key})
}
fn fill_events(count: i64) -> Value {
    sql(format!(
        "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<{count}) INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) SELECT 'hive.object.changed','1.0.0','fixture','time','id','{{}}' FROM seq;"
    ))
}
#[test]
fn event_capacity_durable_acknowledgement_and_consumer_reserve_match_original() {
    compare(&[
        setup(),
        event("write", "fixture/value"),
        event("delete", "fixture/value"),
        event("move", "fixture/value"),
        event("write", "secretary/execution"),
        event("write", "secretary/schedule-progress"),
        event("write", "secretary/event-progress"),
        sql(
            "DELETE FROM events; DELETE FROM sqlite_sequence WHERE name='events'; INSERT INTO service_nodes VALUES('consumer','alpha','host','fixture',1,'{}'); INSERT INTO subscriptions VALUES('consumer','durable','{}',0,0,1,'{}'),('consumer','ordinary','{}',0,0,1,'{}'); INSERT INTO durable_subscriptions VALUES('consumer','durable');",
        ),
        fill_events(10_000),
        json!({"op":"durableBoundary"}),
        json!({"op":"admit"}),
        event("write", "fixture/value"),
        event("write", "secretary/execution"),
        sql("UPDATE service_nodes SET service_name='secretary' WHERE id='consumer';"),
        event("write", "fixture/value"),
        sql("UPDATE subscriptions SET acknowledged_sequence=500 WHERE name='durable';"),
        json!({"op":"durableBoundary"}),
        json!({"op":"admit"}),
        sql(
            "DELETE FROM events; DELETE FROM sqlite_sequence WHERE name='events'; UPDATE metadata SET value='0' WHERE key='events_pruned_through'; UPDATE subscriptions SET acknowledged_sequence=0,batch_json='{}';",
        ),
        fill_events(11_024),
        json!({"op":"admit","reserve":true}),
        sql("DELETE FROM durable_subscriptions;"),
        json!({"op":"durableBoundary"}),
        json!({"op":"admit"}),
        sql(
            "CREATE TRIGGER fail_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'injected'); END;",
        ),
        event("archive", "fixture/value"),
    ]);
}

#[test]
fn ledger_integer_bounds_reject_corrupt_usage_and_keep_replay_fences() {
    let root = Root::new();
    let path = root.0.join("native-integers.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .connection()
        .execute_batch("INSERT INTO principals VALUES('alpha');")
        .unwrap();
    store.set_metadata("runtime_epoch", "epoch").unwrap();
    let maximum = MUTATION_RECEIPT_JOURNAL_BYTES as i64;
    let safe = 9_007_199_254_740_991_i64;
    store
        .connection()
        .execute(
            "INSERT INTO mutations VALUES('alpha','prior','hash',?,?, 'snapshot','{}',?)",
            rusqlite::params![safe - 1, safe - 1, maximum + 2],
        )
        .unwrap();
    store
        .set_metadata("mutation_receipt_bytes", &(maximum + 1).to_string())
        .unwrap();
    let error = store
        .transaction(|store| store.cleanup_mutation_receipts(0, 0))
        .unwrap_err();
    assert_eq!(error.code, "storage_invalid");
    assert_eq!(store.mutation_receipt_bytes().unwrap(), maximum + 1);
    assert_eq!(
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM mutations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .metadata("mutations_expired_before")
            .unwrap()
            .as_deref(),
        Some("0")
    );
    store
        .connection()
        .execute("UPDATE mutations SET receipt_bytes=?", [maximum + 1])
        .unwrap();
    store
        .transaction(|store| store.cleanup_mutation_receipts(0, 0))
        .unwrap();
    assert_eq!(store.mutation_receipt_bytes().unwrap(), 0);
    assert_eq!(
        store.metadata("mutations_expired_before").unwrap(),
        Some((safe - 1).to_string())
    );
    let context = AuthenticatedContext {
        principal_id: "alpha".into(),
        credential_digest: "fixture".into(),
    };
    let request =
        |issued, nonce| IvyJson::object([("mutationId", format!("epoch:{issued}:{nonce}").into())]);
    for (parameters, now) in [
        (request(safe - 1, "expired"), safe as u64),
        (request(safe, "future"), (safe - 60_001) as u64),
    ] {
        let error = store
            .mutate(&context, &"fixture".into(), &parameters, now, |_, _| {
                panic!("Invalid identity ran work")
            })
            .unwrap_err();
        assert_eq!(error.code, "mutation_expired");
    }
    let parameters = request(safe, "accepted");
    let value = IvyJson::object([("retained", true.into())]);
    assert_eq!(
        store
            .mutate(
                &context,
                &"fixture".into(),
                &parameters,
                safe as u64,
                |_, _| Ok(LedgerResult::snapshot(value.clone()))
            )
            .unwrap(),
        value
    );
    let bytes: i64 = store
        .connection()
        .query_row("SELECT SUM(receipt_bytes) FROM mutations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(store.mutation_receipt_bytes().unwrap(), bytes);
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .mutate(&context, &"fixture".into(), &parameters, 0, |_, _| panic!(
                "Replay reran work"
            ))
            .unwrap(),
        value
    );
    assert_eq!(store.mutation_receipt_bytes().unwrap(), bytes);
}
