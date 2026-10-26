use super::{IvyError, IvyJson, Root, Store, native_with_runtime, object_write, query, reference};
use ivy_contracts::IvyResult;
use ivy_hive::objects;
use serde_json::{Value, json};
use std::{collections::HashMap, time::Duration};

pub(super) fn characters(profile: &Value) -> HashMap<u32, bool> {
    profile["wordCharacters"]
        .as_array()
        .expect("Observed Unicode classification")
        .iter()
        .map(|r| (r[0].as_u64().unwrap() as u32, r[1].as_bool().unwrap()))
        .collect()
}
pub(super) fn classified(words: &HashMap<u32, bool>, point: u32) -> bool {
    *words
        .get(&point)
        .unwrap_or_else(|| panic!("Unobserved Unicode code point {point:x}"))
}
pub(super) fn execute(store: &mut Store, step: &Value, profile: &Value) -> IvyResult<IvyJson> {
    let mut params = IvyJson::from_serde(&step["params"])?;
    object_write::apply_units(&mut params, step);
    if step["method"] == "cursor" {
        return store
            .cursor(
                params.get("identity").ok_or_else(IvyError::internal)?,
                params.get("boundary").ok_or_else(IvyError::internal)?,
            )
            .map(IvyJson::from);
    }
    let words = characters(profile);
    store.with_budget(
        if step["overdue"] == true {
            Duration::ZERO
        } else {
            Duration::from_secs(10)
        },
        |store| objects::search(store, &params, &|point| classified(&words, point)),
    )
}
fn sql(value: impl Into<String>) -> Value {
    json!({"op":"sql","sql":value.into()})
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn setup() -> Vec<Value> {
    let mut seed = query::seed(false);
    seed.push_str("BEGIN IMMEDIATE;");
    for (id, parent, archived) in [
        ("child", "id01", false),
        ("grand", "child", false),
        ("hidden", "id01", true),
        ("hidden-child", "hidden", true),
        ("prefix-neighbor", "id02", false),
    ] {
        query::insert(
            &mut seed,
            id,
            "fixture/value",
            "1.0.0",
            &quote(parent),
            archived,
            "json",
            "{}",
            1,
        );
    }
    seed.push_str("UPDATE objects SET path='/id01/child',depth=2 WHERE id='child';UPDATE objects SET path='/id01/child/grand',depth=3 WHERE id='grand';UPDATE objects SET path='/id01/hidden',depth=2 WHERE id='hidden';UPDATE objects SET path='/id01/hidden/hidden-child',depth=3,archived_at=NULL WHERE id='hidden-child';UPDATE objects SET path='/id010/prefix-neighbor' WHERE id='prefix-neighbor';");
    let body = quote("needle shared alpha beta café cafe Č 東京 𐐀 𝟠 ١ Ⅷ ² foo_bar");
    seed.push_str(&format!("INSERT INTO object_fts(rowid,object_id,name,text) SELECT search_id,id,name,{body} FROM objects;UPDATE metadata SET value='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' WHERE key='cursor_secret';COMMIT;"));
    vec![sql(seed), json!({"op":"reopen"})]
}
fn operation(params: Value) -> Value {
    json!({"op":"search","method":"search","params":params})
}
fn push(steps: &mut Vec<Value>, step: Value) -> usize {
    let i = steps.len();
    steps.push(step);
    i
}
fn follow(params: Value, from: usize, select: &str) -> Value {
    let mut step = operation(params);
    step["params"]["cursor"] = Value::Null;
    step["identityBindings"] = json!([{"from":from,"select":select,"at":"/params/cursor"}]);
    step
}
fn token(steps: &mut Vec<Value>, params: &Value, boundary: Value) -> usize {
    let mut identity = params.clone();
    identity["method"] = "objects.search".into();
    push(
        steps,
        json!({"op":"search","method":"cursor","params":{"identity":identity,"boundary":boundary}}),
    )
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let expected = reference(&root.0.join("original.sqlite"), steps);
    let actual = native_with_runtime(
        &root.0.join("native.sqlite"),
        steps,
        Some(&expected["searchRuntime"]),
    );
    assert_eq!(actual["rows"].as_array().unwrap().len(), steps.len());
    assert_eq!(expected["rows"].as_array().unwrap().len(), steps.len());
    for (index, (a, b)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            a, b,
            "Search trace {index} ({}/{})",
            steps[index]["op"], steps[index]["method"]
        );
        if steps[index]["op"] == "sql" || steps[index]["requireSuccess"] == true {
            assert!(b["error"].is_null(), "Search setup {index}: {}", b["error"]);
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    expected
}
fn value(result: &Value, index: usize) -> &Value {
    &result["rows"][index]["value"]
}
fn code(result: &Value, index: usize) -> &Value {
    &result["rows"][index]["error"]["data"]["code"]
}
fn success(result: &Value, index: usize) {
    assert!(
        result["rows"][index]["error"].is_null(),
        "Search step {index}: {}",
        result["rows"][index]["error"]
    );
}
fn ids(result: &Value, index: usize) -> Vec<&str> {
    value(result, index)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect()
}

#[test]
fn search_fts_contract_exclusion_subtree_archive_and_restart_keysets_match_original() {
    let mut s = setup();
    let params = json!({"text":"needle","limit":7});
    let first = push(&mut s, operation(params.clone()));
    let second = push(&mut s, follow(params.clone(), first, "/value/nextCursor"));
    push(&mut s, json!({"op":"reopen"}));
    let restarted = push(&mut s, follow(params.clone(), second, "/value/nextCursor"));
    let all = push(
        &mut s,
        operation(json!({"text":"needle","includeArchived":true,"limit":200})),
    );
    let filtered = push(
        &mut s,
        operation(json!({"text":"needle","contractKey":"fixture/value","limit":200})),
    );
    let excluded = push(
        &mut s,
        operation(json!({"text":"needle","excludeContractKey":"fixture/value","limit":200})),
    );
    let none = push(
        &mut s,
        operation(
            json!({"text":"needle","contractKey":"fixture/value","excludeContractKey":"fixture/value"}),
        ),
    );
    let branch = push(
        &mut s,
        operation(json!({"text":"needle","rootId":"id01","limit":200})),
    );
    let archived = push(
        &mut s,
        operation(json!({"text":"needle","rootId":"id01","includeArchived":true,"limit":200})),
    );
    let missing = push(
        &mut s,
        operation(json!({"text":"needle","rootId":"missing"})),
    );
    let r = compare(&s);
    for i in [
        first, second, restarted, all, filtered, excluded, none, branch, archived,
    ] {
        success(&r, i);
    }
    assert_eq!(ids(&r, first).len(), 7);
    assert!(ids(&r, first).last() < ids(&r, second).first());
    assert!(ids(&r, second).last() < ids(&r, restarted).first());
    assert!(ids(&r, all).len() > ids(&r, filtered).len());
    assert_eq!(ids(&r, excluded), vec!["binary", "text"]);
    assert!(ids(&r, none).is_empty());
    assert_eq!(ids(&r, branch), vec!["child", "grand", "id01", "id20"]);
    assert_eq!(
        ids(&r, archived),
        vec!["child", "grand", "hidden", "hidden-child", "id01", "id20"]
    );
    assert_eq!(code(&r, missing), "not_found");
}

#[test]
fn search_words_use_observed_node_unicode_and_quote_all_fts_syntax_match_original() {
    let mut s = setup();
    let mut checked = vec![];
    for text in [
        "needle",
        "foo_bar",
        "café",
        "cafe\u{301}",
        "Č",
        "東京",
        "𐐀",
        "𝟠",
        "١",
        "Ⅷ",
        "²",
        "alpha beta",
        "needle OR absent",
        "needle*",
        "\"needle\"",
        "needle - absent",
        "needle\0beta",
        "😀—\u{301}",
        "_",
        "a\u{200d}b",
    ] {
        checked.push(push(&mut s, operation(json!({"text":text,"limit":200}))));
    }
    let mut raw = operation(json!({"text":"placeholder","limit":3}));
    raw["parameterUnits"] = json!({"/text":[55296,110,101,101,100,108,101,56320]});
    let raw = push(&mut s, raw);
    let follow_raw = push(
        &mut s,
        follow(
            json!({"text":"placeholder","limit":3}),
            raw,
            "/value/nextCursor",
        ),
    );
    s[follow_raw]["parameterUnits"] = s[raw]["parameterUnits"].clone();
    let r = compare(&s);
    for i in checked.iter().copied().chain([raw, follow_raw]) {
        success(&r, i);
    }
    assert!(!ids(&r, checked[0]).is_empty());
    assert!(ids(&r, checked[12]).is_empty());
    assert!(!ids(&r, checked[13]).is_empty());
    assert!(ids(&r, checked[17]).is_empty());
    assert_eq!(ids(&r, raw).len(), 3);
    assert!(ids(&r, raw).last() < ids(&r, follow_raw).first());
}

#[test]
fn search_cursor_json_coercion_scope_and_guard_order_match_original() {
    let mut s = setup();
    let params = json!({"text":"needle","limit":5});
    let first = push(&mut s, operation(params.clone()));
    let mut accepted = vec![];
    let mut coercion_errors = vec![];
    for boundary in [
        json!(null),
        json!(false),
        json!(true),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(""),
        json!("id10"),
        json!("zzzz"),
        json!([]),
        json!([null]),
        json!(["id10"]),
        json!(["id10", "id11"]),
        json!({}),
        json!({"valueOf":2}),
        json!({"toString":1}),
        json!([{"toString":1}]),
    ] {
        let errors = boundary.get("toString").is_some()
            || boundary.is_array() && boundary[0].get("toString").is_some();
        let t = token(&mut s, &params, boundary);
        let i = push(&mut s, follow(params.clone(), t, "/value"));
        if errors {
            coercion_errors.push(i);
        } else {
            accepted.push(i);
        }
    }
    let changed = push(
        &mut s,
        follow(
            json!({"text":"needle","limit":5,"includeArchived":false}),
            first,
            "/value/nextCursor",
        ),
    );
    let changed_limit = push(
        &mut s,
        follow(
            json!({"text":"needle","limit":6}),
            first,
            "/value/nextCursor",
        ),
    );
    let forged = push(
        &mut s,
        operation(json!({"text":"😀","cursor":"forged","rootId":"missing"})),
    );
    let missing = push(&mut s, operation(json!({"text":"😀","rootId":"missing"})));
    let empty = push(&mut s, operation(json!({"text":"😀"})));
    let r = compare(&s);
    for i in accepted {
        success(&r, i);
    }
    for i in coercion_errors {
        assert_eq!(code(&r, i), "internal_error");
    }
    for i in [changed, changed_limit, forged] {
        assert_eq!(code(&r, i), "invalid_cursor");
    }
    assert_eq!(code(&r, missing), "not_found");
    assert_eq!(ids(&r, empty), Vec::<&str>::new());
}

#[test]
fn search_word_budget_sql_deadline_and_selected_metadata_failure_order_match_original() {
    let mut s = setup();
    let thirty_two = push(
        &mut s,
        operation(json!({"text":vec!["needle";32].join(" ")})),
    );
    let thirty_three = push(
        &mut s,
        operation(json!({"text":vec!["needle";33].join(" ")})),
    );
    let mut timed = operation(json!({"text":"needle"}));
    timed["overdue"] = true.into();
    let timed = push(&mut s, timed);
    let recovered = push(&mut s, operation(json!({"text":"needle"})));
    push(
        &mut s,
        // Read within the original deferred-FK transaction window, then roll it back.
        sql("BEGIN IMMEDIATE;UPDATE objects SET current_revision=99 WHERE id='id39';"),
    );
    let early = push(&mut s, operation(json!({"text":"needle","limit":1})));
    let damaged = push(&mut s, operation(json!({"text":"needle","limit":200})));
    let unmatched = push(
        &mut s,
        operation(json!({"text":"missing-token","limit":200})),
    );
    push(&mut s, sql("ROLLBACK;"));
    let final_read = push(&mut s, operation(json!({"text":"needle","limit":200})));
    let r = compare(&s);
    for i in [thirty_two, recovered, early, unmatched, final_read] {
        success(&r, i);
    }
    for i in [thirty_three, timed] {
        assert_eq!(code(&r, i), "limit_exceeded");
    }
    assert_eq!(
        r["rows"][thirty_three]["error"]["message"],
        "Search supports at most 32 words."
    );
    assert_eq!(code(&r, damaged), "internal_error");
    assert!(ids(&r, unmatched).is_empty());
}

#[test]
fn search_metadata_pages_keep_original_row_limit_without_query_byte_cap_match_original() {
    let mut s = setup();
    let long = quote(&format!("/{}", "α".repeat(7000)));
    let mut wide = String::from("BEGIN IMMEDIATE;");
    for index in 0..148 {
        query::insert(
            &mut wide,
            &format!("wide{index:03}"),
            "fixture/value",
            "1.0.0",
            "NULL",
            false,
            "json",
            "{}",
            1,
        );
    }
    wide.push_str("INSERT INTO object_fts(rowid,object_id,name,text) SELECT search_id,id,name,'needle' FROM objects WHERE id GLOB 'wide*';");
    wide.push_str(&format!("UPDATE objects SET path={long}||'/'||id;COMMIT;"));
    push(&mut s, sql(wide));
    let whole = push(
        &mut s,
        operation(json!({"text":"needle","includeArchived":true,"limit":200})),
    );
    let small = push(
        &mut s,
        operation(json!({"text":"needle","includeArchived":true,"limit":7})),
    );
    let r = compare(&s);
    for i in [whole, small] {
        success(&r, i);
    }
    assert_eq!(ids(&r, whole).len(), 196);
    assert!(serde_json::to_vec(value(&r, whole)).unwrap().len() > ivy_hive::paging::PAGE_BYTES);
    assert!(value(&r, whole)["nextCursor"].is_null());
    assert_eq!(ids(&r, small).len(), 7);
    assert!(value(&r, small)["nextCursor"].is_string());
}

#[test]
fn search_keyset_survives_new_indexed_heads_fts_changes_and_opaque_cursor_bytes_match_original() {
    let mut s = setup();
    let params = json!({"text":"needle","limit":5});
    let first = push(&mut s, operation(params.clone()));
    push(
        &mut s,
        sql(
            "DELETE FROM object_fts WHERE object_id='id04';UPDATE objects SET updated_at='2026-02-03' WHERE id='id06';INSERT INTO object_fts(rowid,object_id,name,text) SELECT search_id,id,name,'needle new' FROM objects WHERE id='id04';",
        ),
    );
    let second = push(&mut s, follow(params.clone(), first, "/value/nextCursor"));
    let default = push(
        &mut s,
        operation(json!({"text":"needle","includeArchived":true})),
    );
    let mut nul = operation(json!({"text":"needle","cursor":"placeholder","limit":5}));
    nul["identityBindings"] =
        json!([{"from":first,"select":"/value/nextCursor","at":"/params/cursor"}]);
    let repeated = push(&mut s, nul);
    let t = token(&mut s, &params, json!("id10\0suffix"));
    let nul_boundary = push(&mut s, follow(params, t, "/value"));
    let r = compare(&s);
    for i in [first, second, default, repeated, nul_boundary] {
        success(&r, i);
    }
    assert_eq!(ids(&r, second), ids(&r, repeated));
    assert_eq!(ids(&r, default).len(), 48);
    assert!(value(&r, default)["nextCursor"].is_null());
    assert_eq!(ids(&r, nul_boundary).first(), Some(&"id11"));
}
