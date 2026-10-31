use super::*;
use rusqlite::types::ValueRef;
fn setup() -> Vec<Value> {
    let mut steps = super::setup();
    steps.push(sql("UPDATE metadata SET value='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' WHERE key='cursor_secret';"));
    steps.push(json!({"op":"reopen"}));
    steps
}
pub(super) fn query_sql(store: &Store, query: &str) -> IvyResult<IvyJson> {
    let mut statement = store
        .connection()
        .prepare_cached(query)
        .map_err(|e| store.query_error(e))?;
    let columns: Vec<_> = statement
        .column_names()
        .iter()
        .map(|v| v.to_string())
        .collect();
    let rows = statement
        .query_map([], |row| {
            Ok(IvyJson::object(
                columns
                    .iter()
                    .enumerate()
                    .map(|(index, name)| {
                        let value = match row.get_ref(index)? {
                            ValueRef::Null => IvyJson::Null,
                            ValueRef::Integer(v) => IvyJson::Number(v as f64),
                            ValueRef::Real(v) => IvyJson::Number(v),
                            ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
                            ValueRef::Blob(_) => unreachable!(),
                        };
                        Ok((name.clone(), value))
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            ))
        })
        .map_err(|e| store.query_error(e))?
        .map(|r| r.map_err(|e| store.query_error(e)))
        .collect::<IvyResult<Vec<_>>>()?;
    Ok(IvyJson::Array(rows))
}
fn operation(method: &str, id: Option<&str>, params: Value) -> Value {
    let mut step = action(method, id, params);
    step["op"] = json!("inventory");
    step
}
fn inventory_catalog() -> Value {
    let mut result = catalog("fixture");
    result["namespaces"][0]["inventoryKinds"] = json!([
        {"kind":"item","version":"1.0.0","summarySchema":{"type":"object","properties":{"name":{"type":["string","null"]},"archived":{"type":["boolean","null"]},"recency":{"type":["number","string","boolean","null"]},"nested":{"type":"object"}},"required":["name"],"additionalProperties":false},"searchPointers":["/name"],"archivedPointer":"/archived","recencyPointer":"/recency"},
        {"kind":"plain","version":"1.0.0","summarySchema":true},
    ]);
    result
}
fn ready(steps: &mut Vec<Value>, id: &str, host: &str) {
    push(steps, connect(id, "sample", host, 1));
    push(steps, action("sync", Some(id), inventory_catalog()));
    push(steps, heartbeat(id, true, vec![]));
}
fn entry(id: &str, name: &str, recency: Value, archived: Value, observed: &str) -> Value {
    json!({"nativeId":id,"summary":{"name":name,"recency":recency,"archived":archived},"observedAt":observed})
}
fn snapshot(id: &str, revision: u64, entries: Vec<Value>) -> Value {
    operation(
        "sync",
        Some(id),
        json!({"namespace":"fixture","kind":"item","schemaVersion":"1.0.0","mode":"snapshot","snapshotRevision":revision,"entries":entries}),
    )
}
fn delta(id: &str, revision: u64, expected: u64, entries: Vec<Value>, removed: Vec<&str>) -> Value {
    operation(
        "sync",
        Some(id),
        json!({"namespace":"fixture","kind":"item","schemaVersion":"1.0.0","mode":"delta","snapshotRevision":revision,"expectedRevision":expected,"entries":entries,"removedNativeIds":removed}),
    )
}
fn resource(node: &str, native: &str) -> Value {
    json!({"serviceNodeId":node,"namespace":"fixture","kind":"item","nativeId":native})
}
fn get(node: &str, native: &str) -> Value {
    operation("get", None, json!({"resourceRef":resource(node,native)}))
}
fn list(params: Value) -> Value {
    operation("list", None, params)
}
fn follow(mut step: Value, from: usize) -> Value {
    step["params"]["cursor"] = Value::Null;
    step["identityBindings"] =
        json!([{"from":from,"select":"/value/nextCursor","at":"/params/cursor"}]);
    step
}
const OBSERVED: &str = "1970-01-01T00:01:40.000Z";

#[test]
fn complete_snapshot_delta_replay_and_guard_order_match_original() {
    let mut steps = setup();
    push(&mut steps, connect("node", "sample", "host", 1));
    let premature = push(&mut steps, snapshot("node", 1, vec![]));
    push(
        &mut steps,
        action("sync", Some("node"), inventory_catalog()),
    );
    push(&mut steps, heartbeat("node", true, vec![]));
    let entries = vec![
        entry("b", "Beta", json!(2), json!(false), OBSERVED),
        entry("a", "Alpha", json!(1), json!(false), "1970-01-01T00:01:40Z"),
    ];
    push(&mut steps, action("clearHints", None, json!({})));
    let initial = push(&mut steps, snapshot("node", 4, entries.clone()));
    let replay = push(
        &mut steps,
        at(
            snapshot("node", 4, entries.iter().rev().cloned().collect()),
            110000,
        ),
    );
    let mut changed = entries.clone();
    changed[0]["summary"]["name"] = json!("Changed");
    let conflict = push(&mut steps, snapshot("node", 4, changed));
    let old = push(&mut steps, snapshot("node", 3, vec![]));
    let gap = push(&mut steps, delta("node", 5, 3, vec![], vec![]));
    let absent = push(&mut steps, delta("absent", 1, 0, vec![], vec![]));
    let valid = push(
        &mut steps,
        delta(
            "node",
            5,
            4,
            vec![entry("c", "Gamma", json!(3), json!(false), OBSERVED)],
            vec!["a"],
        ),
    );
    let stale_delta = push(&mut steps, delta("node", 5, 4, vec![], vec![]));
    let removed = push(&mut steps, get("node", "a"));
    let kept = push(&mut steps, get("node", "b"));
    let empty = push(&mut steps, at(snapshot("node", 6, vec![]), 120000));
    let rows = compare(&steps);
    assert_eq!(code(&rows, premature), "service_not_ready");
    success(&rows, initial);
    success(&rows, replay);
    assert_eq!(value(&rows, initial), value(&rows, replay));
    assert_eq!(value(&rows, initial)["observedAt"], "1970-01-01T00:01:40Z");
    for index in [conflict, old, gap, stale_delta] {
        assert_eq!(code(&rows, index), "revision_conflict");
        assert_eq!(
            rows["rows"][index]["error"]["data"]["details"]["currentRevision"],
            if index == stale_delta { 5 } else { 4 }
        );
    }
    assert_eq!(code(&rows, absent), "not_found");
    success(&rows, valid);
    assert_eq!(code(&rows, removed), "not_found");
    success(&rows, kept);
    success(&rows, empty);
    assert_eq!(
        state(&rows, initial)["changes"],
        json!(["inventory/fixture/item/node"])
    );
    assert_eq!(
        node(&rows, initial, "node")["lastObservationAt"],
        "1970-01-01T00:01:40Z"
    );
    assert!(
        state(&rows, empty)["inventory"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        value(&rows, empty)["observedAt"],
        "1970-01-01T00:02:00.000Z"
    );
}

#[test]
fn schema_calendar_identity_conflicts_and_deltas_validate_before_revision_guards() {
    let mut steps = setup();
    ready(&mut steps, "node", "host");
    let first = entry("one", "One", json!(1), json!(false), OBSERVED);
    push(&mut steps, snapshot("node", 1, vec![first.clone()]));
    let duplicate = push(
        &mut steps,
        snapshot("node", 1, vec![first.clone(), first.clone()]),
    );
    let mut bad = first.clone();
    bad["observedAt"] = json!("2024-02-30T00:00:00Z");
    let date = push(&mut steps, snapshot("node", 1, vec![bad]));
    let mut bad = first.clone();
    bad["summary"]["name"] = json!(4);
    let schema = push(&mut steps, snapshot("node", 1, vec![bad]));
    let duplicate_removed = push(&mut steps, delta("node", 2, 1, vec![], vec!["one", "one"]));
    let overlap = push(&mut steps, delta("node", 2, 1, vec![first], vec!["one"]));
    let mut unknown = snapshot("node", 2, vec![]);
    unknown["params"]["schemaVersion"] = json!("9.0.0");
    let unknown = push(&mut steps, unknown);
    let mut catalog = inventory_catalog();
    let mut kind = catalog["namespaces"][0]["inventoryKinds"][0].clone();
    kind["version"] = json!("2.0.0");
    catalog["namespaces"][0]["inventoryKinds"]
        .as_array_mut()
        .unwrap()
        .push(kind);
    push(&mut steps, action("sync", Some("node"), catalog));
    let mut mismatch = delta("node", 2, 1, vec![], vec![]);
    mismatch["params"]["schemaVersion"] = json!("2.0.0");
    let mismatch = push(&mut steps, mismatch);
    let mut replacement = snapshot("node", 2, vec![]);
    replacement["params"]["schemaVersion"] = json!("2.0.0");
    let replacement = push(&mut steps, replacement);
    let rows = compare(&steps);
    for index in [duplicate, duplicate_removed, overlap, unknown] {
        assert_eq!(code(&rows, index), "registry_invalid");
    }
    assert_eq!(code(&rows, date), "invalid_arguments");
    assert!(!code(&rows, schema).is_null());
    assert_eq!(code(&rows, mismatch), "revision_conflict");
    success(&rows, replacement);
}

#[test]
fn inventory_keysets_filter_archive_search_recency_and_cross_owner_unicode_ties() {
    let mut steps = setup();
    ready(&mut steps, "a", "host-a");
    ready(&mut steps, "b", "host-b");
    for id in ["a", "b"] {
        push(
            &mut steps,
            snapshot(
                id,
                1,
                vec![
                    entry("z", "İSTANBUL", json!(20), json!(false), OBSERVED),
                    entry("é", "Éclair MATCH", json!(20), json!(false), OBSERVED),
                    entry(
                        "😀",
                        "Greek ΟΣ",
                        json!("1970-01-01T00:00:00.030Z"),
                        json!(false),
                        OBSERVED,
                    ),
                    entry("a", "Archived Match", json!(40), json!(true), OBSERVED),
                    entry("null", "No archive", Value::Null, Value::Null, OBSERVED),
                ],
            ),
        );
    }
    let mut pages = vec![];
    let mut previous = None;
    for _ in 0..6 {
        let mut step = list(
            json!({"namespace":"fixture","kind":"item","archived":false,"sort":"recency-desc","limit":1}),
        );
        if let Some(from) = previous {
            step = follow(step, from);
        }
        let index = push(&mut steps, step);
        pages.push(index);
        previous = Some(index);
    }
    let search = push(
        &mut steps,
        list(
            json!({"namespace":"fixture","archived":false,"searchTerm":"MATCH","sort":"recency-desc"}),
        ),
    );
    let turkish = push(&mut steps, list(json!({"searchTerm":"i̇stanbul"})));
    let archived = push(&mut steps, list(json!({"archived":true})));
    let binary = push(&mut steps, list(json!({"sort":"native-id"})));
    let rows = compare(&steps);
    let ids: Vec<_> = pages
        .iter()
        .flat_map(|i| value(&rows, *i)["items"].as_array().unwrap())
        .map(|v| {
            (
                v["resourceRef"]["nativeId"].as_str().unwrap(),
                v["resourceRef"]["serviceNodeId"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ids[..6],
        [
            ("😀", "a"),
            ("😀", "b"),
            ("z", "a"),
            ("z", "b"),
            ("é", "a"),
            ("é", "b")
        ]
    );
    assert_eq!(value(&rows, search)["items"].as_array().unwrap().len(), 2);
    assert_eq!(value(&rows, turkish)["items"].as_array().unwrap().len(), 2);
    assert_eq!(value(&rows, archived)["items"].as_array().unwrap().len(), 2);
    assert_eq!(value(&rows, binary)["items"].as_array().unwrap().len(), 10);
}

#[test]
fn inventory_semantic_guards_selectors_and_staleness_match_original() {
    let mut steps = setup();
    ready(&mut steps, "node", "host");
    push(
        &mut steps,
        snapshot(
            "node",
            1,
            vec![entry("one", "One", json!(1), json!(false), OBSERVED)],
        ),
    );
    let fresh = push(&mut steps, at(get("node", "one"), 110000));
    let stale_contact = push(&mut steps, at(get("node", "one"), 110001));
    push(&mut steps, at(heartbeat("node", true, vec![]), 160000));
    let fresh_observation = push(&mut steps, at(get("node", "one"), 160000));
    let stale_observation = push(&mut steps, at(list(json!({})), 160001));
    let contradiction = push(
        &mut steps,
        list(json!({"resourceRef":resource("node","one"),"namespace":"other"})),
    );
    let owner = push(
        &mut steps,
        list(json!({"serviceNodeId":"node","hostId":"other"})),
    );
    let missing = push(&mut steps, list(json!({"serviceNodeId":"absent"})));
    let mut plain = snapshot("node", 1, vec![]);
    plain["params"]["kind"] = json!("plain");
    let empty_plain = push(&mut steps, plain.clone());
    let allowed_empty = push(&mut steps, list(json!({"archived":false})));
    plain["params"]["snapshotRevision"] = json!(2);
    plain["params"]["entries"] = json!([{"nativeId":"plain","summary":{},"observedAt":OBSERVED}]);
    push(&mut steps, plain);
    let mut denied = vec![];
    for params in [
        json!({"archived":false}),
        json!({"searchTerm":"x"}),
        json!({"sort":"recency-desc"}),
    ] {
        denied.push(push(&mut steps, list(params)));
    }
    push(&mut steps, action("disconnect", Some("node"), json!({})));
    let offline = push(&mut steps, get("node", "one"));
    push(&mut steps, json!({"op":"reopen"}));
    let restarted = push(
        &mut steps,
        list(json!({"namespace":"fixture","kind":"item"})),
    );
    let rows = compare(&steps);
    assert_eq!(value(&rows, fresh)["stale"], false);
    assert_eq!(value(&rows, stale_contact)["stale"], true);
    assert_eq!(value(&rows, fresh_observation)["stale"], false);
    assert_eq!(value(&rows, stale_observation)["items"][0]["stale"], true);
    for index in [contradiction, owner] {
        assert_eq!(code(&rows, index), "target_conflict");
    }
    assert_eq!(code(&rows, missing), "not_found");
    success(&rows, empty_plain);
    success(&rows, allowed_empty);
    for index in denied {
        assert_eq!(code(&rows, index), "invalid_arguments");
    }
    assert_eq!(value(&rows, offline)["stale"], true);
    assert_eq!(value(&rows, restarted)["items"][0]["stale"], true);
}

#[test]
fn inventory_cursor_identity_revisions_shapes_and_page_limits_match_original() {
    let mut steps = setup();
    ready(&mut steps, "node", "host");
    push(
        &mut steps,
        sql(
            "UPDATE metadata SET value='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' WHERE key='cursor_secret';",
        ),
    );
    // Reopen loads the persisted cursor key, then restore live registry readiness.
    push(&mut steps, json!({"op":"reopen"}));
    ready(&mut steps, "node", "host");
    push(
        &mut steps,
        snapshot(
            "node",
            1,
            (0..4)
                .map(|i| entry(&format!("id{i}"), "One", json!(1), json!(false), OBSERVED))
                .collect(),
        ),
    );
    let first = push(&mut steps, list(json!({"namespace":"fixture","limit":1})));
    let next = push(
        &mut steps,
        follow(list(json!({"namespace":"fixture","limit":2})), first),
    );
    let scope = push(
        &mut steps,
        follow(
            list(json!({"namespace":"fixture","searchTerm":"one","limit":1})),
            first,
        ),
    );
    push(&mut steps, delta("node", 2, 1, vec![], vec![]));
    let revision = push(
        &mut steps,
        follow(list(json!({"namespace":"fixture","limit":1})), first),
    );
    let invalid = push(&mut steps, list(json!({"cursor":"broken"})));
    // The original accepts an empty selected set even with semantic flags.
    let empty = push(
        &mut steps,
        list(
            json!({"namespace":"missing","archived":false,"sort":"recency-desc","searchTerm":"x"}),
        ),
    );
    let rows = compare(&steps);
    success(&rows, first);
    success(&rows, next);
    assert_eq!(value(&rows, next)["items"].as_array().unwrap().len(), 2);
    for index in [scope, revision, invalid] {
        assert_eq!(code(&rows, index), "invalid_cursor");
    }
    assert_eq!(value(&rows, empty)["items"], json!([]));
}

#[test]
fn inventory_every_write_boundary_and_deferred_commit_roll_back_before_retry() {
    let mut steps = setup();
    ready(&mut steps, "node", "host");
    push(
        &mut steps,
        snapshot(
            "node",
            1,
            vec![entry("one", "One", json!(1), json!(false), OBSERVED)],
        ),
    );
    push(
        &mut steps,
        sql(
            "CREATE TABLE fixture_deferred(value TEXT REFERENCES metadata(key) DEFERRABLE INITIALLY DEFERRED) STRICT;",
        ),
    );
    let mut failures = vec![];
    for trigger in [
        "CREATE TRIGGER fixture_failure BEFORE INSERT ON inventory_sets BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        "CREATE TRIGGER fixture_failure BEFORE DELETE ON inventory BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        "CREATE TRIGGER fixture_failure BEFORE INSERT ON inventory BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        "CREATE TRIGGER fixture_failure BEFORE UPDATE ON service_nodes BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        "CREATE TRIGGER fixture_failure AFTER UPDATE ON service_nodes BEGIN INSERT INTO fixture_deferred VALUES('missing'); END;",
    ] {
        push(&mut steps, action("clearHints", None, json!({})));
        push(&mut steps, sql(trigger));
        failures.push(push(
            &mut steps,
            snapshot(
                "node",
                2,
                vec![entry("two", "Two", json!(2), json!(false), OBSERVED)],
            ),
        ));
        push(&mut steps, sql("DROP TRIGGER fixture_failure;"));
    }
    let retried = push(
        &mut steps,
        snapshot(
            "node",
            2,
            vec![entry("two", "Two", json!(2), json!(false), OBSERVED)],
        ),
    );
    let mut expired = list(json!({}));
    expired["overdue"] = json!(true);
    let overdue = push(&mut steps, expired);
    let rows = compare(&steps);
    for index in failures {
        assert_eq!(code(&rows, index), "internal_error");
        assert_eq!(state(&rows, index)["inventorySets"][0]["revision"], 1);
        assert_eq!(state(&rows, index)["inventory"][0]["native_id"], "one");
        assert_eq!(state(&rows, index)["changes"], json!([]));
    }
    success(&rows, retried);
    assert_eq!(code(&rows, overdue), "limit_exceeded");
}

#[test]
fn inventory_sql_runtime_uses_original_scalar_pointer_and_numeric_date_rules() {
    let mut steps = setup();
    let mut query = operation("sql", None, json!({}));
    query["sql"] = json!(
        "SELECT ivy_recency('{\"v\":1.5}','/v') AS fractional,ivy_recency('{\"v\":9007199254740992}','/v') AS unsafe,ivy_recency('{\"v\":-42}','/v') AS negative,ivy_recency('{\"v\":\"1/2/2000\"}','/v') AS date,ivy_recency('{\"v\":\"not-a-date\"}','/v') AS invalid,ivy_recency('{\"v\":true}','/v') AS flag,ivy_contains('{\"a\":[\"ÉCLAIR\"]}','/a/0','écl') AS searched,ivy_contains('{\"v\":null}','/v','') AS null_value,ivy_contains('{\"v\":\"x\"}','/v',NULL) AS null_needle,ivy_contains('{\"v\":\"İΣ\"}','/v','i̇ς') AS unicode;"
    );
    let observed = push(&mut steps, query);
    let mut length = operation("sql", None, json!({}));
    length["sql"] = json!("SELECT ivy_recency('{\"a\":[1,2]}','/a/length') AS length;");
    let length = push(&mut steps, length);
    let rows = compare(&steps);
    success(&rows, observed);
    assert_eq!(value(&rows, observed)[0]["fractional"], 0);
    assert_eq!(value(&rows, observed)[0]["unsafe"], 0);
    assert_eq!(value(&rows, observed)[0]["negative"], -42);
    assert_eq!(value(&rows, observed)[0]["searched"], 1);
    assert_eq!(value(&rows, observed)[0]["unicode"], 1);
    assert_eq!(value(&rows, length)[0]["length"], 0);
}

#[test]
fn inventory_page_byte_limits_trim_before_cursor_and_refuse_an_oversized_first_item() {
    let mut steps = setup();
    ready(&mut steps, "node", "host");
    let mut entries = vec![];
    for i in 0..4 {
        entries.push(entry(
            &format!("id{i}"),
            &"x".repeat(600000),
            json!(1),
            json!(false),
            OBSERVED,
        ));
    }
    let mut seeded = snapshot("node", 1, entries);
    seeded["compactInventory"] = json!(true);
    push(&mut steps, seeded);
    let mut first_step = list(json!({"limit":50}));
    first_step["compactInventory"] = json!(true);
    let first = push(&mut steps, first_step);
    let mut next_step = follow(list(json!({"limit":50})), first);
    next_step["compactInventory"] = json!(true);
    let next = push(&mut steps, next_step);
    let mut huge = snapshot(
        "node",
        2,
        vec![entry(
            "huge",
            &"y".repeat(2 * 1024 * 1024),
            json!(1),
            json!(false),
            OBSERVED,
        )],
    );
    huge["compactInventory"] = json!(true);
    let summary_too_large = push(&mut steps, huge);
    let mut retained = sql(
        "UPDATE inventory SET summary=json_object('name',replace(printf('%2097152s',''),' ','y'),'recency',1,'archived',json('false')) WHERE native_id='id0';",
    );
    retained["compactInventory"] = json!(true);
    push(&mut steps, retained);
    let mut oversized = list(json!({}));
    oversized["compactInventory"] = json!(true);
    let oversized = push(&mut steps, oversized);
    let rows = compare(&steps);
    assert_eq!(value(&rows, first)["items"].as_array().unwrap().len(), 3);
    assert_eq!(value(&rows, next)["items"].as_array().unwrap().len(), 1);
    assert_eq!(code(&rows, oversized), "result_too_large");
    assert_eq!(code(&rows, summary_too_large), "content_too_large");
}

#[test]
fn inventory_utf16_sql_aliases_and_retained_missing_schema_projection_match_original() {
    let mut steps = setup();
    ready(&mut steps, "node", "host");
    let mut aliased = snapshot(
        "node",
        1,
        vec![
            entry("raw", "First", json!(1), json!(false), OBSERVED),
            entry("replacement", "Last", json!(2), json!(false), OBSERVED),
        ],
    );
    aliased["parameterUnits"] =
        json!({"/entries/0/nativeId":[0xd800],"/entries/1/nativeId":[0xfffd]});
    let initial = push(&mut steps, aliased.clone());
    let replay = push(&mut steps, aliased);
    let listed = push(&mut steps, list(json!({})));
    let mut raw_get = get("node", "raw");
    raw_get["parameterUnits"] = json!({"/resourceRef/nativeId":[0xd800]});
    let raw_get = push(&mut steps, raw_get);
    push(
        &mut steps,
        sql("UPDATE inventory SET schema_version='9.0.0';"),
    );
    let absent_schema = push(&mut steps, list(json!({})));
    let get_old = push(&mut steps, get("node", "�"));
    let rows = compare(&steps);
    success(&rows, initial);
    assert_eq!(
        state(&rows, initial)["inventory"].as_array().unwrap().len(),
        1
    );
    assert_eq!(code(&rows, replay), "revision_conflict");
    assert_eq!(value(&rows, listed)["items"][0]["summary"]["name"], "Last");
    success(&rows, raw_get);
    assert_eq!(value(&rows, absent_schema)["items"], json!([]));
    success(&rows, get_old);
    assert_eq!(value(&rows, get_old)["schemaVersion"], "9.0.0");
}

