use super::{
    AuthContext, AuthenticatedContext, IvyError, IvyJson, JsString, Root, Store,
    native_with_runtime, oracle,
};
use ivy_contracts::IvyResult;
use ivy_hive::retention::{self, Runtime};
use serde_json::{Value, json};
use std::{cmp::Ordering, path::Path, time::Duration};

// Exact Node runtime observations are fixture inputs, never retention decisions.
// Production Shared clock/default-locale binding remains a separate integration.
struct TraceRuntime<'a>(&'a Value);
impl Runtime for TraceRuntime<'_> {
    fn parse_date(&self, value: &JsString) -> f64 {
        let value = value.to_utf8().unwrap();
        let found = self.0["dates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v[0].as_str() == Some(&value))
            .unwrap_or_else(|| panic!("Unobserved Date.parse {value}"));
        found[1].as_f64().unwrap_or(f64::NAN)
    }
    fn iso_date(&self, milliseconds: f64) -> IvyResult<JsString> {
        self.0["iso"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v[0].as_f64() == Some(milliseconds))
            .and_then(|v| v[1].as_str())
            .map(JsString::from)
            .ok_or_else(IvyError::internal)
    }
    fn compare_contract_keys(&self, a: &JsString, b: &JsString) -> Ordering {
        let key = json!([a.to_utf8().unwrap(), b.to_utf8().unwrap()]).to_string();
        let sign = self.0["comparisons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v[0].as_str() == Some(&key))
            .unwrap_or_else(|| panic!("Unobserved locale pair {key}"))[1]
            .as_i64()
            .unwrap();
        sign.cmp(&0)
    }
}
pub(super) fn native_retention(
    store: &mut Store,
    step: &Value,
    profile: &Value,
) -> IvyResult<IvyJson> {
    let runtime = TraceRuntime(profile);
    let now = step["now"].as_f64().unwrap();
    if step["op"] == "pruneRevisions" {
        let principal = store.authenticate(&AuthContext {
            credential_digest: step["context"]["credentialDigest"].as_str().unwrap().into(),
            session_digest: None,
        })?;
        let context = AuthenticatedContext {
            principal_id: principal.principal_id,
            credential_digest: principal.credential_digest,
        };
        let node = step["context"]["serviceNodeId"]
            .as_str()
            .map(JsString::from);
        let parameters = IvyJson::from_serde(&step["params"]).unwrap();
        return store.with_budget_mut(
            if step["overdue"] == true {
                Duration::ZERO
            } else {
                Duration::from_secs(10)
            },
            |store| {
                ivy_hive::objects::prune_revisions(
                    store,
                    &context,
                    node.as_ref(),
                    &parameters,
                    now as u64,
                    &runtime,
                )
            },
        );
    }
    match step["method"].as_str().unwrap() {
        "preview" => retention::preview(store, &runtime, now),
        "status" => retention::status(store, &runtime, now),
        "collect" => {
            let mut diagnostics = |store: &mut Store, _now: f64| {
                store
                    .connection()
                    .execute_batch(step["diagnosticSql"].as_str().unwrap_or(""))
                    .map_err(|e| store.query_error(e))?;
                if step["diagnosticFail"] == true {
                    Err(IvyError::new(
                        "fixture_failure",
                        "Diagnostic collection requested failure.",
                    ))
                } else {
                    Ok(())
                }
            };
            let callback = if step["diagnosticSql"].is_string() || step["diagnosticFail"] == true {
                Some(&mut diagnostics as &mut dyn retention::DiagnosticCollector)
            } else {
                None
            };
            if step["journalFailure"] == true {
                let (result, cause) = store
                    .capture_failure(|store| retention::collect(store, &runtime, now, callback));
                if let Err(error) = &result {
                    retention::record_failure(
                        store,
                        &runtime,
                        &cause.unwrap_or_else(|| error.message_js()),
                        now,
                    )?;
                }
                result
            } else {
                retention::collect(store, &runtime, now, callback)
            }
        }
        "recordFailure" => {
            let message = if let Some(units) = step["messageUnits"].as_array() {
                JsString(units.iter().map(|v| v.as_u64().unwrap() as u16).collect())
            } else {
                step["message"].as_str().unwrap().into()
            };
            retention::record_failure(store, &runtime, &message, now).map(|_| IvyJson::Null)
        }
        _ => Err(IvyError::internal()),
    }
}
fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn seed_sql(text: impl Into<String>) -> Value {
    sql(format!("BEGIN IMMEDIATE;{}COMMIT;", text.into()))
}
fn digest(user: bool) -> String {
    format!("sha256:{}", if user { "a" } else { "b" }.repeat(64))
}
fn contract(key: &str, version: &str, objects: Value, revisions: Value) -> Value {
    let owner = json!({"kind":"service","serviceName":"fixture-service"});
    json!({"op":"register","owner":owner,"definition":{"key":key,"version":version,"owner":owner,"mediaType":"text/plain","retention":{"objects":objects,"revisions":revisions},"specMarkdown":"synthetic retained-data fixture"}})
}
fn setup() -> Vec<Value> {
    vec![
        sql("UPDATE metadata SET value='epoch' WHERE key='runtime_epoch';"),
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":digest(true)},{"principalId":"beta","digest":digest(false)}]}),
        sql(
            "INSERT INTO service_nodes VALUES('owner','alpha','fixture-host','fixture-service',1,'{}'),('wrong-principal','beta','fixture-host','fixture-service',1,'{}'),('wrong-service','alpha','fixture-host','another-service',1,'{}');",
        ),
    ]
}
fn history(id: &str, key: &str, count: usize) -> String {
    let mut value = String::new();
    super::query::insert(
        &mut value,
        id,
        key,
        "1.0.0",
        "NULL",
        false,
        "text",
        "x",
        count as u64,
    );
    value.push_str(&format!("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<{}) INSERT INTO revisions SELECT '{id}',x,'{key}','1.0.0','text','x','sha256:fixture',1,'2026-01-02','{{}}' FROM n;",count-1));
    value.push_str(&format!("INSERT INTO object_fts(rowid,object_id,name,text) SELECT search_id,id,name,'' FROM objects WHERE id='{id}';"));
    value
}
fn action(method: &str) -> Value {
    json!({"op":"retention","method":method,"now":1_800_000_000_000_u64})
}
fn prune(nonce: &str, id: &str, current: usize) -> Value {
    let mut step = super::object_mutations::action(
        "pruneRevisions",
        nonce,
        json!({"objectId":id,"expectedRevision":current,"maximumCount":2,"maximumBytes":1_000_000,"maximumAgeDays":365000}),
    );
    step["context"]["serviceNodeId"] = json!("owner");
    step
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let expected = oracle(&root.0.join("reference.sqlite"), steps);
    let actual = native_with_runtime(
        &root.0.join("native.sqlite"),
        steps,
        Some(&expected["retentionRuntime"]),
    );
    for (index, (left, right)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            left, right,
            "Retention trace step {index} ({}/{})",
            steps[index]["op"], steps[index]["method"]
        );
        if matches!(
            steps[index]["op"].as_str(),
            Some("sql" | "configure" | "register")
        ) {
            assert!(
                right["error"].is_null(),
                "Fixture setup failed at {index}: {}",
                right["error"]
            );
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    expected
}
fn state(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["state"]
}
fn value(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["value"]
}
fn success(rows: &Value, index: usize) {
    assert!(
        rows["rows"][index]["error"].is_null(),
        "Failure at {index}: {}",
        rows["rows"][index]["error"]
    );
}
fn no_effect(rows: &Value, index: usize, code: &str) {
    assert_eq!(rows["rows"][index]["error"]["data"]["code"], code);
    assert_eq!(state(rows, index), state(rows, index - 1));
}
fn family<'a>(rows: &'a Value, index: usize, key: &str) -> &'a Value {
    value(rows, index)["families"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["contractKey"] == key)
        .unwrap()
}
fn summary(rows: &Value, index: usize, key: &str) -> Value {
    serde_json::from_str(
        state(rows, index)["retention"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["contract_key"] == key)
            .unwrap()["summary_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn owning_service_pruning_batches_keep_current_and_distinct_pins_with_replay_and_byte_bounds() {
    let mut steps = setup();
    steps.push(contract(
        "fixture/result",
        "1.0.0",
        json!({"mode":"retain"}),
        json!({"mode":"all"}),
    ));
    let mut seed = history("result", "fixture/result", 1202);
    super::query::insert(
        &mut seed,
        "source",
        "fixture/result",
        "1.0.0",
        "NULL",
        false,
        "text",
        "x",
        1,
    );
    seed.push_str("INSERT INTO revision_references VALUES('source',1,'first','result',100),('source',1,'duplicate','result',100),('source',1,'second','result',200);");
    steps.push(seed_sql(seed));
    steps.push(action("collect"));
    let first = steps.len();
    let request = prune("first", "result", 1202);
    steps.push(request.clone());
    let replay = steps.len();
    steps.push(request.clone());
    steps.push(json!({"op":"reopen"}));
    steps.push(request.clone());
    let second = steps.len();
    steps.push(prune("second", "result", 1202));
    steps.push(prune("third", "result", 1202));
    let bytes = steps.len();
    let mut byte = prune("bytes", "result", 1202);
    byte["params"]["maximumCount"] = json!(10);
    byte["params"]["maximumBytes"] = json!(1);
    steps.push(byte);
    steps.push(json!({"op":"readObject","objectId":"result","revision":2}));
    let rows = compare(&steps);
    for index in [first, replay, replay + 2, second, second + 1, bytes] {
        success(&rows, index);
    }
    assert_eq!(
        value(&rows, first),
        &json!({"deleted":500,"protected":2,"remainingBytes":702})
    );
    assert_eq!(value(&rows, replay), value(&rows, first));
    assert_eq!(state(&rows, replay), state(&rows, first));
    assert_eq!(value(&rows, replay + 2), value(&rows, first));
    assert_eq!(value(&rows, second)["deleted"], 500);
    assert_eq!(value(&rows, second + 1)["deleted"], 198);
    assert_eq!(
        value(&rows, bytes),
        &json!({"deleted":1,"protected":2,"remainingBytes":3})
    );
    assert_eq!(summary(&rows, bytes, "fixture/result")["revisionCount"], 4); // result's pins/current plus source
    assert_eq!(
        rows["rows"].as_array().unwrap().last().unwrap()["error"]["data"]["code"],
        "revision_pruned"
    );
}

#[test]
fn pruning_guard_order_and_every_write_boundary_rollback_before_same_identity_retry() {
    let mut steps = setup();
    steps.push(contract(
        "fixture/result",
        "1.0.0",
        json!({"mode":"retain"}),
        json!({"mode":"all"}),
    ));
    steps.push(seed_sql(history("result", "fixture/result", 12)));
    steps.push(action("collect"));
    let guards = steps.len();
    let mut wrong = prune("missing", "absent", 11);
    wrong["context"]["serviceNodeId"] = json!("missing");
    steps.push(wrong);
    let mut stale = prune("stale", "result", 11);
    stale["context"]["serviceNodeId"] = json!("wrong-service");
    steps.push(stale);
    for node in ["wrong-service", "wrong-principal", "missing"] {
        let mut step = prune(node, "result", 12);
        step["context"]["serviceNodeId"] = json!(node);
        steps.push(step);
    }
    let budget = steps.len();
    let mut expired = prune("budget", "result", 12);
    expired["overdue"] = json!(true);
    steps.push(expired);
    let mut failures = vec![];
    for (nonce, trigger) in [
        (
            "delete",
            "CREATE TRIGGER fixture_failure BEFORE DELETE ON revisions BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        ),
        (
            "summary",
            "CREATE TRIGGER fixture_failure BEFORE UPDATE ON retention_families BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        ),
        (
            "receipt",
            "CREATE TRIGGER fixture_failure BEFORE INSERT ON mutations BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        ),
        (
            "commit",
            "CREATE TABLE fixture_deferred(value TEXT REFERENCES metadata(key) DEFERRABLE INITIALLY DEFERRED) STRICT; CREATE TRIGGER fixture_failure AFTER DELETE ON revisions BEGIN INSERT INTO fixture_deferred VALUES('missing'); END;",
        ),
    ] {
        steps.push(sql(trigger));
        let failed = steps.len();
        steps.push(prune(nonce, "result", 12));
        failures.push(failed);
        steps.push(sql("DROP TRIGGER fixture_failure;"));
    }
    let retry = steps.len();
    let request = prune("commit", "result", 12);
    steps.push(request.clone());
    let conflict = steps.len();
    let mut changed = request;
    changed["params"]["maximumCount"] = json!(1);
    steps.push(changed);
    steps.push(seed_sql("INSERT INTO contracts VALUES('fixture/result','9.0.0','{\"key\":\"fixture/result\",\"version\":\"9.0.0\",\"owner\":null}'); INSERT INTO revisions VALUES('result',13,'fixture/result','9.0.0','text','x','sha256:fixture',1,'2026-01-02','{}'); UPDATE objects SET current_revision=13 WHERE id='result';"));
    let corrupt = steps.len();
    for node in ["missing", "wrong-principal", "owner"] {
        let mut request = prune(&format!("corrupt-{node}"), "result", 13);
        request["context"]["serviceNodeId"] = json!(node);
        steps.push(request);
    }
    let rows = compare(&steps);
    for (offset, code) in [
        "not_found",
        "revision_conflict",
        "forbidden",
        "forbidden",
        "forbidden",
    ]
    .into_iter()
    .enumerate()
    {
        no_effect(&rows, guards + offset, code);
    }
    no_effect(&rows, budget, "limit_exceeded");
    for index in failures {
        no_effect(&rows, index, "internal_error");
    }
    success(&rows, retry);
    assert_eq!(value(&rows, retry)["deleted"], 10);
    no_effect(&rows, conflict, "mutation_conflict");
    for (offset, code) in ["forbidden", "forbidden", "internal_error"]
        .into_iter()
        .enumerate()
    {
        no_effect(&rows, corrupt + offset, code);
    }
}

#[test]
fn changed_family_policies_reapply_parent_owner_and_reference_protection_after_preview_filtering() {
    let mut steps = setup();
    steps.push(contract(
        "fixture/target",
        "1.0.0",
        json!({"mode":"expire","maximumAgeDays":1}),
        json!({"mode":"all"}),
    ));
    steps.push(contract(
        "fixture/source",
        "1.0.0",
        json!({"mode":"retain"}),
        json!({"mode":"all"}),
    ));
    steps.push(contract(
        "fixture/owned",
        "1.0.0",
        json!({"mode":"owned"}),
        json!({"mode":"all"}),
    ));
    let mut seed = String::new();
    for (id, key, parent) in [
        ("target", "fixture/target", "NULL"),
        ("source", "fixture/source", "'target'"),
        ("owner", "fixture/target", "NULL"),
        ("owned", "fixture/owned", "NULL"),
        ("cycle-a", "fixture/target", "NULL"),
        ("cycle-b", "fixture/target", "NULL"),
        ("recent", "fixture/owned", "NULL"),
    ] {
        super::query::insert(&mut seed, id, key, "1.0.0", parent, false, "text", "x", 1);
    }
    // The immutable owner is set in a fresh row, before any update can occur.
    seed.push_str("DELETE FROM revisions WHERE object_id='owned'; DELETE FROM objects WHERE id='owned'; INSERT INTO objects VALUES(NULL,'owned',NULL,'owner','owned','/owned',0,NULL,1,'fixture/owned',1,NULL,0,'2026-01-01','2026-01-02'); INSERT INTO revisions VALUES('owned',1,'fixture/owned','1.0.0','text','x','sha256:fixture',1,'2026-01-02','{}'); INSERT INTO revision_references VALUES('source',1,'target','target',1),('cycle-a',1,'other','cycle-b',1),('cycle-b',1,'other','cycle-a',1); UPDATE objects SET created_at='not a date' WHERE id='recent'; INSERT INTO object_fts(rowid,object_id,name,text) SELECT search_id,id,name,'' FROM objects;");
    steps.push(seed_sql(seed));
    let preview = steps.len();
    steps.push(action("preview"));
    steps.push(action("collect"));
    steps.push(contract(
        "fixture/source",
        "2.0.0",
        json!({"mode":"expire","maximumAgeDays":1}),
        json!({"mode":"all"}),
    ));
    let filtered = steps.len();
    steps.push(action("collect"));
    let deleted = steps.len();
    steps.push(action("collect"));
    let rows = compare(&steps);
    for index in [preview, preview + 1, filtered, deleted] {
        success(&rows, index);
    }
    assert_eq!(
        family(&rows, preview, "fixture/target")["eligibleObjectCount"],
        3
    ); // owner plus the reference cycle
    let ids = |index| {
        state(&rows, index)["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(filtered), vec!["recent", "source", "target"]);
    assert_eq!(ids(deleted), vec!["recent"]);
    assert_eq!(state(&rows, deleted)["references"], json!([]));
    assert_eq!(state(&rows, deleted)["fts"].as_array().unwrap().len(), 1);
}

#[test]
fn bounded_collector_uses_latest_versions_and_cached_legacy_usage_with_per_family_commits() {
    let mut steps = setup();
    for key in ["fixture/a", "fixture/z"] {
        steps.push(contract(
            key,
            "1.0.0",
            json!({"mode":"retain"}),
            json!({"mode":"all"}),
        ));
        steps.push(contract(
            key,
            "2.0.0",
            json!({"mode":"retain"}),
            json!({"mode":"current"}),
        ));
    }
    let mut seed = history("large", "fixture/a", 1202);
    seed.push_str(&history("small", "fixture/z", 3));
    super::query::insert(
        &mut seed,
        "pin",
        "fixture/z",
        "1.0.0",
        "NULL",
        false,
        "text",
        "x",
        1,
    );
    seed.push_str("INSERT INTO revision_references VALUES('pin',1,'pin','large',100);");
    steps.push(seed_sql(seed));
    let preview = steps.len();
    steps.push(action("collect"));
    steps.push(sql("UPDATE retention_families SET summary_json=json_remove(summary_json,'$.objectCount','$.revisionCount','$.byteLength') WHERE contract_key='fixture/a'; CREATE TRIGGER fixture_failure BEFORE INSERT ON retention_families WHEN NEW.contract_key='fixture/z' BEGIN SELECT RAISE(ABORT,'fixture'); END;"));
    let failed = steps.len();
    steps.push(action("collect"));
    steps.push(sql("DROP TRIGGER fixture_failure;"));
    let second = steps.len();
    steps.push(action("collect"));
    steps.push(action("collect"));
    steps.push(action("collect"));
    let scan = steps.len();
    steps.push(action("preview"));
    let rows = compare(&steps);
    success(&rows, preview);
    assert_eq!(
        family(&rows, preview, "fixture/a")["eligibleRevisionCount"],
        1200
    );
    assert_eq!(
        rows["rows"][failed]["error"]["data"]["code"],
        "internal_error"
    );
    assert_eq!(summary(&rows, failed, "fixture/a")["revisionCount"], 702);
    assert_eq!(summary(&rows, failed, "fixture/z")["revisionCount"], 4);
    assert_eq!(summary(&rows, second, "fixture/a")["revisionCount"], 202);
    assert_eq!(summary(&rows, second + 1, "fixture/a")["revisionCount"], 2);
    assert_eq!(
        family(&rows, scan, "fixture/a")["protectedRevisionCount"],
        1
    );
    assert_eq!(family(&rows, scan, "fixture/a")["eligibleRevisionCount"], 0);
}

#[test]
fn journal_cleanup_respects_durable_prefixes_count_age_and_ui_release_pointers() {
    let mut steps = setup();
    steps.push(sql("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10004) INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) SELECT 'hive.object.changed','1.0.0','hive',CASE WHEN x<7 THEN '2026-01-01T00:00:00.000Z' ELSE '2099-01-01T00:00:00.000Z' END,'fixture-'||x,'{}' FROM n; INSERT INTO subscriptions VALUES('owner','durable','{}',0,2,1,'{}'),('owner','temporary','{}',0,1,1,'{}'); INSERT INTO durable_subscriptions VALUES('owner','durable'); INSERT INTO apps VALUES('fixture','{}','current','previous'); INSERT INTO app_releases VALUES('fixture','old','{}'),('fixture','current','{}'),('fixture','previous','{}');"));
    let held = steps.len();
    steps.push(action("collect"));
    steps.push(sql(
        "UPDATE subscriptions SET acknowledged_sequence=10004 WHERE name='durable';",
    ));
    let released = steps.len();
    steps.push(action("collect"));
    let rows = compare(&steps);
    success(&rows, held);
    success(&rows, released);
    assert_eq!(
        value(&rows, held)["events"],
        json!({"count":10002,"byteLength":20004,"prunedThroughSequence":2})
    );
    assert_eq!(
        value(&rows, released)["events"],
        json!({"count":9998,"byteLength":19996,"prunedThroughSequence":6})
    );
    assert_eq!(
        state(&rows, held)["releases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["release_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["current", "previous"]
    );
    assert!(state(&rows, released)["subscriptions"][1]["batch_json"].is_null());
}

#[test]
fn final_cleanup_failure_rolls_back_journals_ui_diagnostics_and_success_but_retains_prior_family_work()
 {
    let mut steps = setup();
    steps.push(contract(
        "fixture/result",
        "1.0.0",
        json!({"mode":"retain"}),
        json!({"mode":"current"}),
    ));
    steps.push(seed_sql(history("result", "fixture/result", 5)));
    steps.push(action("collect"));
    steps.push(sql("INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES('hive.object.changed','1.0.0','hive','2026-01-01','old','{}'); INSERT INTO apps VALUES('fixture','{}',NULL,NULL); INSERT INTO app_releases VALUES('fixture','old','{}');"));
    let failed = steps.len();
    let mut failure = action("collect");
    failure["diagnosticSql"] = json!("INSERT INTO diagnostics VALUES('fixture','{}');");
    failure["diagnosticFail"] = json!(true);
    steps.push(failure);
    let recorded = steps.len();
    let mut message = action("recordFailure");
    message["messageUnits"] = json!([vec![120_u16; 511], vec![0xd83d, 0xde42, 121]].concat());
    steps.push(message);
    let bad = steps.len();
    steps.push(sql(format!(
        "UPDATE metadata SET value='{}' WHERE key='mutation_receipt_bytes';",
        ivy_contracts::limits::MUTATION_RECEIPT_JOURNAL_BYTES + 1
    )));
    steps.push(action("collect"));
    steps.push(sql(
        "UPDATE metadata SET value='0' WHERE key='mutation_receipt_bytes';",
    ));
    let recovered = steps.len();
    steps.push(action("collect"));
    let rows = compare(&steps);
    assert_eq!(
        rows["rows"][failed]["error"]["data"]["code"],
        "fixture_failure"
    );
    assert_eq!(summary(&rows, failed, "fixture/result")["revisionCount"], 1);
    for key in ["events", "releases", "diagnostics", "metadata"] {
        assert_eq!(state(&rows, failed)[key], state(&rows, failed - 1)[key]);
    }
    success(&rows, recorded);
    no_effect(&rows, bad + 1, "storage_invalid");
    success(&rows, recovered);
    assert_eq!(value(&rows, recovered)["lastFailure"], Value::Null);
    assert_eq!(value(&rows, recovered)["events"]["count"], 0);
    assert_eq!(state(&rows, recovered)["releases"], json!([]));
}

#[test]
fn legacy_dates_keep_preview_parsing_and_sql_age_boundaries_distinct_and_usage_is_reinitialized() {
    let mut steps = setup();
    steps.push(contract(
        "fixture/result",
        "1.0.0",
        json!({"mode":"retain"}),
        json!({"mode":"bounded","maximumAgeDays":1}),
    ));
    let mut seed = String::new();
    for id in ["result", "age-result"] {
        super::query::insert(
            &mut seed,
            id,
            "fixture/result",
            "1.0.0",
            "NULL",
            false,
            "text",
            "x",
            4,
        );
        seed.push_str(&format!("INSERT INTO revisions VALUES('{id}',1,'fixture/result','1.0.0','text','x','sha256:fixture',1,'January 2, 2026','{{}}'),('{id}',2,'fixture/result','1.0.0','text','x','sha256:fixture',1,'0000-not-a-date','{{}}'),('{id}',3,'fixture/result','1.0.0','text','x','sha256:fixture',1,'2026-01-02','{{}}');"));
    }
    seed.push_str("INSERT INTO contracts VALUES('fixture/result','0.0.0','{\"key\":\"fixture/result\",\"version\":\"0.0.0\"}');");
    steps.push(seed_sql(seed));
    let age = steps.len();
    let mut request = prune("age", "age-result", 4);
    request["params"]["maximumCount"] = json!(100);
    request["params"]["maximumAgeDays"] = json!(0);
    steps.push(request);
    let initial = steps.len();
    steps.push(action("preview"));
    steps.push(action("collect"));
    let collected = steps.len();
    steps.push(action("collect"));
    steps.push(action("preview"));
    steps.push(sql(
        "UPDATE retention_families SET summary_json='null' WHERE contract_key='fixture/result';",
    ));
    let invalid = steps.len();
    steps.push(action("status"));
    steps.push(sql("UPDATE retention_families SET summary_json='{\"objectCount\":-1,\"revisionCount\":0.5,\"byteLength\":\"4\",\"eligibleObjects\":9007199254740992,\"eligibleRevisions\":2,\"protectedRevisions\":null}' WHERE contract_key='fixture/result';"));
    let cached = steps.len();
    steps.push(action("status"));
    steps.push(action("collect"));
    let rows = compare(&steps);
    success(&rows, age);
    assert_eq!(value(&rows, age)["deleted"], 1);
    assert_eq!(
        family(&rows, initial, "fixture/result")["eligibleRevisionCount"],
        4
    );
    assert_eq!(
        family(&rows, collected, "fixture/result")["eligibleRevisionCount"],
        1
    );
    assert_eq!(
        family(&rows, collected + 1, "fixture/result")["eligibleRevisionCount"],
        2
    );
    no_effect(&rows, invalid, "internal_error");
    success(&rows, cached);
    assert_eq!(family(&rows, cached, "fixture/result")["objectCount"], 0);
    assert_eq!(
        family(&rows, cached + 1, "fixture/result")["objectCount"],
        2
    );
    assert_eq!(
        family(&rows, cached + 1, "fixture/result")["revisionCount"],
        4
    );
}

#[test]
fn byte_prefix_collection_and_pinned_ui_assets_keep_cleanup_atomic() {
    let mut steps = setup();
    steps.push(sql("INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES('hive.object.changed','1.0.0','hive','2099-01-01','large',replace(hex(zeroblob(33554433)),'00','x')),('hive.object.changed','1.0.0','hive','2099-01-01','large-2',replace(hex(zeroblob(33554433)),'00','y')); INSERT INTO subscriptions VALUES('owner','temporary','{}',0,0,1,'{}');"));
    let bytes = steps.len();
    steps.push(action("collect"));
    steps.push(contract(
        "fixture/result",
        "1.0.0",
        json!({"mode":"retain"}),
        json!({"mode":"all"}),
    ));
    let mut seed = String::new();
    super::query::insert(
        &mut seed,
        "asset",
        "fixture/result",
        "1.0.0",
        "NULL",
        false,
        "text",
        "x",
        1,
    );
    seed.push_str("INSERT INTO apps VALUES('fixture','{}',NULL,NULL); INSERT INTO app_releases VALUES('fixture','pinned','{}'); INSERT INTO app_assets VALUES('fixture','pinned','index.txt','asset',1,'text/plain','sha256:fixture'); INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES('hive.object.changed','1.0.0','hive','2026-01-01','old','{}');");
    steps.push(seed_sql(seed));
    let failed = steps.len();
    steps.push(action("collect"));
    steps.push(sql("DELETE FROM app_assets;"));
    let accepted = steps.len();
    steps.push(action("collect"));
    // Compare lengths/identity for large payloads without constructing a second
    // 64 MiB JSON table image merely to observe that retention deletes a prefix.
    for step in &mut steps {
        step["compactEvents"] = json!(true);
    }
    let rows = compare(&steps);
    success(&rows, bytes);
    assert_eq!(
        value(&rows, bytes)["events"],
        json!({"count":1,"byteLength":33554433,"prunedThroughSequence":1})
    );
    assert_eq!(
        rows["rows"][failed]["error"]["data"]["code"],
        "internal_error"
    );
    for key in ["events", "subscriptions", "metadata", "releases"] {
        assert_eq!(state(&rows, failed)[key], state(&rows, failed - 1)[key]);
    }
    success(&rows, accepted);
    assert_eq!(value(&rows, accepted)["events"]["count"], 0);
    assert_eq!(state(&rows, accepted)["releases"], json!([]));
}

fn journaled_collect() -> Value {
    let mut step = action("collect");
    step["journalFailure"] = true.into();
    step
}
fn metadata_at<'a>(rows: &'a Value, index: usize, key: &str) -> &'a str {
    state(rows, index)["metadata"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == key)
        .unwrap()["value"]
        .as_str()
        .unwrap()
}
#[test]
fn retention_failure_journal_preserves_raw_sql_named_and_commit_causes_without_wire_disclosure() {
    let mut steps = setup();
    for key in ["fixture/a", "fixture/z"] {
        steps.push(contract(
            key,
            "1.0.0",
            json!({"mode":"retain"}),
            json!({"mode":"current"}),
        ));
    }
    steps.push(seed_sql(
        history("large", "fixture/a", 5) + &history("small", "fixture/z", 3),
    ));
    let initial = steps.len();
    steps.push(journaled_collect());
    let long = format!("{}🙂private tail", "x".repeat(511));
    let mut checks = vec![];
    for (trigger, message) in [
        (format!("CREATE TRIGGER cause_failure BEFORE INSERT ON retention_families WHEN NEW.contract_key='fixture/z' BEGIN SELECT RAISE(ABORT,'{long}'); END;"), long.clone()),
        ("CREATE TRIGGER cause_failure BEFORE DELETE ON events BEGIN SELECT RAISE(ABORT,'raw event cleanup cause'); END;".into(), "raw event cleanup cause".into()),
        ("CREATE TRIGGER cause_failure BEFORE DELETE ON app_releases BEGIN SELECT RAISE(ABORT,'raw UI SQL cleanup cause'); END;".into(), "raw UI SQL cleanup cause".into()),
        ("CREATE TRIGGER cause_failure BEFORE UPDATE ON metadata WHEN NEW.key='retention_last_success' BEGIN SELECT RAISE(ABORT,'raw success metadata cause'); END;".into(), "raw success metadata cause".into()),
        ("CREATE TABLE cause_deferred(value TEXT REFERENCES metadata(key) DEFERRABLE INITIALLY DEFERRED) STRICT; CREATE TRIGGER cause_failure AFTER UPDATE ON metadata WHEN NEW.key='retention_last_success' BEGIN INSERT INTO cause_deferred VALUES('missing'); END;".into(), "FOREIGN KEY constraint failed".into()),
    ] {
        steps.push(sql("INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES('hive.object.changed','1.0.0','hive','2026-01-01','journal-old','{}'); INSERT OR IGNORE INTO apps VALUES('fixture','{}',NULL,NULL); INSERT OR IGNORE INTO app_releases VALUES('fixture','old','{}');"));
        steps.push(sql(trigger));
        let failed = steps.len();
        steps.push(journaled_collect());
        steps.push(sql("DROP TRIGGER cause_failure;"));
        let recovered = steps.len();
        steps.push(journaled_collect());
        checks.push((failed, recovered, message));
    }
    let prepared = steps.len();
    let mut parse_failure = journaled_collect();
    parse_failure["diagnosticSql"] = "SELECT * FROM missing_retention_table".into();
    steps.push(parse_failure);
    let named = steps.len();
    let mut named_failure = journaled_collect();
    named_failure["diagnosticFail"] = true.into();
    steps.push(named_failure);
    steps.push(sql("UPDATE metadata SET value='prior journal' WHERE key='retention_last_failure'; CREATE TRIGGER cause_failure BEFORE UPDATE ON metadata WHEN NEW.key='retention_last_failure' BEGIN SELECT RAISE(ABORT,'failure journal unavailable'); END;"));
    let journal_failed = steps.len();
    let mut failure = journaled_collect();
    failure["diagnosticSql"] = "SELECT * FROM another_missing_table".into();
    steps.push(failure);
    steps.push(sql("DROP TRIGGER cause_failure;"));
    let recovered = steps.len();
    steps.push(journaled_collect());
    let result = compare(&steps);
    success(&result, initial);
    for (failed, recovered, message) in checks {
        let error = &result["rows"][failed]["error"];
        assert_eq!(error["message"], "The operation failed internally.");
        assert_eq!(error["data"]["outcome"], "unknown");
        let units = message.encode_utf16().take(512).collect::<Vec<_>>();
        let expected = ivy_contracts::json::canonical(
            &IvyJson::object([
                ("at", "2027-01-15T08:00:00.000Z".into()),
                ("message", IvyJson::String(JsString(units))),
            ]),
            usize::MAX,
        )
        .unwrap();
        assert_eq!(
            metadata_at(&result, failed, "retention_last_failure"),
            expected
        );
        assert_eq!(
            metadata_at(&result, failed, "retention_last_success"),
            metadata_at(&result, failed - 1, "retention_last_success")
        );
        assert_eq!(summary(&result, failed, "fixture/a")["revisionCount"], 1);
        success(&result, recovered);
        assert_eq!(
            metadata_at(&result, recovered, "retention_last_failure"),
            ""
        );
        assert_eq!(value(&result, recovered)["lastFailure"], Value::Null);
    }
    for (index, message) in [
        (prepared, "no such table: missing_retention_table"),
        (named, "Diagnostic collection requested failure."),
    ] {
        let failure: Value =
            serde_json::from_str(metadata_at(&result, index, "retention_last_failure")).unwrap();
        assert_eq!(failure["message"], message);
    }
    assert_eq!(
        result["rows"][prepared]["error"]["data"]["code"],
        "internal_error"
    );
    assert_eq!(
        result["rows"][named]["error"]["data"]["code"],
        "fixture_failure"
    );
    assert_eq!(
        result["rows"][journal_failed]["error"]["data"]["code"],
        "internal_error"
    );
    assert_eq!(
        metadata_at(&result, journal_failed, "retention_last_failure"),
        "prior journal"
    );
    success(&result, recovered);
}

#[test]
fn database_failure_capture_is_local_nested_and_restored_after_unwinding() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    let fail = |store: &mut Store, table: &str| -> IvyResult<()> {
        store
            .connection()
            .execute_batch(&format!("SELECT * FROM {table}"))
            .map_err(|e| store.query_error(e))
    };
    let (result, cause) = store.capture_failure(|store| fail(store, "outer_failure"));
    assert_eq!(
        cause.unwrap(),
        JsString::from("no such table: outer_failure")
    );
    assert_eq!(
        result.unwrap_err().to_wire().get("message"),
        Some(&IvyJson::from("The operation failed internally."))
    );
    let (result, cause) = store.capture_failure(|store| {
        let (_, cause) = store.capture_failure(|store| fail(store, "inner_failure"));
        assert_eq!(
            cause.unwrap(),
            JsString::from("no such table: inner_failure")
        );
        Err::<(), _>(IvyError::new(
            "storage_invalid",
            "Named failure after the inner capture.",
        ))
    });
    assert_eq!(result.unwrap_err().code, "storage_invalid");
    assert!(cause.is_none());
    assert!(
        catch_unwind(AssertUnwindSafe(|| store.capture_failure(
            |store| -> IvyResult<()> {
                let _ = fail(store, "unwinding_failure");
                panic!("Fixture unwinding");
            }
        )))
        .is_err()
    );
    let (result, cause) = store
        .capture_failure(|_| Err::<(), _>(IvyError::new("storage_invalid", "Later operation.")));
    assert_eq!(result.unwrap_err().message, "Later operation.");
    assert!(cause.is_none());
    let (result, cause) = store.capture_failure(|store| {
        let _ = fail(store, "handled_failure");
        Ok(())
    });
    result.unwrap();
    assert!(cause.is_none());
}
