use super::*;
use std::path::Path;

fn fixture() -> Store {
    Store::open(Path::new(":memory:")).unwrap()
}

fn insert(store: &Store, key_json: &str, version: &str, spec_json: &str, schema: Option<&str>) {
    let key = parse(key_json, usize::MAX).unwrap();
    let key = String::from_utf16_lossy(&key.as_js_string().unwrap().0);
    let media = if schema.is_some() {
        "application/json"
    } else {
        "text/plain"
    };
    store
        .connection()
        .execute(
            "INSERT OR IGNORE INTO contract_families VALUES (?, '{\"kind\":\"agent\"}', ?, 'retain')",
            params![key, media],
        )
        .unwrap();
    let schema = schema
        .map(|schema| format!(",\"jsonSchema\":{schema}"))
        .unwrap_or_default();
    let definition = format!(
        "{{\"key\":{key_json},\"version\":\"{version}\",\"owner\":{{\"kind\":\"agent\"}},\"mediaType\":\"{media}\",\"specMarkdown\":{spec_json},\"retention\":{{\"objects\":{{\"mode\":\"retain\"}},\"revisions\":{{\"mode\":\"all\"}}}}{schema}}}"
    );
    let definition = canonical(
        &parse(&definition, MANAGEMENT_FRAME_BYTES).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO contracts VALUES (?,?,?)",
            params![key, version, definition],
        )
        .unwrap();
}

fn params(value: serde_json::Value) -> IvyJson {
    IvyJson::from_serde(&value).unwrap()
}
fn items(page: &IvyJson) -> &[IvyJson] {
    page.get("items").unwrap().as_array().unwrap()
}
fn follow(params: &IvyJson, page: &IvyJson) -> IvyJson {
    let mut params = params.clone();
    let cursor = page.get("nextCursor").unwrap();
    assert!(!cursor.is_null());
    params.insert("cursor", cursor.clone());
    params
}

#[test]
fn numeric_latest_and_text_keysets_keep_cursor_identity_and_lookahead() {
    let store = fixture();
    for version in [
        "2.0.0",
        "10.0.0",
        "10.0.999999999999999999999999999999",
        "10.1.0",
        "999999999999999999999999999999.0.0",
    ] {
        insert(&store, "\"a\"", version, "\"version\"", None);
    }
    for key in ["\"a0\"", "\"a@\"", "\"é\"", "\"😀\""] {
        insert(&store, key, "1.0.0", "\"other\"", None);
    }
    assert_eq!(
        latest(&store, &"a".into()).unwrap().get("version"),
        Some(&"999999999999999999999999999999.0.0".into())
    );
    let query = params(serde_json::json!({"limit":1}));
    let mut query_page = query.clone();
    let mut keys = Vec::new();
    loop {
        let page = list(&store, "contracts.list", &query_page).unwrap();
        keys.extend(
            items(&page)
                .iter()
                .map(|record| record.get("key").unwrap().as_str().unwrap()),
        );
        if page.get("nextCursor").unwrap().is_null() {
            break;
        }
        query_page = follow(&query, &page);
    }
    assert_eq!(keys, ["a0", "a", "a@", "é", "😀"]);
    let versions = list(
        &store,
        "contracts.list",
        &params(serde_json::json!({"key":"a","allVersions":true})),
    )
    .unwrap();
    assert_eq!(
        items(&versions)
            .iter()
            .map(|record| record.get("version").unwrap().as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "10.0.0",
            "10.0.999999999999999999999999999999",
            "10.1.0",
            "2.0.0",
            "999999999999999999999999999999.0.0"
        ]
    );
    let page = list(&store, "contracts.list", &query).unwrap();
    let mut changed = follow(&query, &page);
    changed.insert("allVersions", false.into());
    assert_eq!(
        list(&store, "contracts.list", &changed).unwrap_err().code,
        "invalid_cursor"
    );
    assert_eq!(
        list(&store, "contracts.summaries", &follow(&query, &page))
            .unwrap_err()
            .code,
        "invalid_cursor"
    );
    assert!(
        items(
            &list(
                &store,
                "contracts.list",
                &params(serde_json::json!({"key":"missing"}))
            )
            .unwrap()
        )
        .is_empty()
    );
}

#[test]
fn summary_projection_keeps_utf16_whitespace_schema_presence_and_lossy_key_aliases() {
    let store = fixture();
    insert(&store, "\"\\ud800\"", "1.0.0", "\"old\"", Some("true"));
    let spec = format!(
        "\"\\ufeff  {}\\ud83d\\ude00 tail\\u0085\\udfff\"",
        "x".repeat(511)
    );
    insert(&store, "\"�\"", "2.0.0", &spec, Some("false"));
    let query = params(serde_json::json!({}));
    let page = list(&store, "contracts.summaries", &query).unwrap();
    assert_eq!(items(&page).len(), 2);
    assert_eq!(items(&page)[0], items(&page)[1]);
    let description = items(&page)[0]
        .get("description")
        .unwrap()
        .as_js_string()
        .unwrap();
    assert_eq!(description.0.len(), 512);
    assert_eq!(description.0[511], 0xd83d);
    assert_eq!(items(&page)[0].get("hasJsonSchema"), Some(&true.into()));
    assert!(items(&page)[0].get("jsonSchema").is_none());
    let query = params(serde_json::json!({"limit":1}));
    let page = list(&store, "contracts.summaries", &query).unwrap();
    assert_eq!(items(&page).len(), 1);
    assert!(
        items(&list(&store, "contracts.summaries", &follow(&query, &page)).unwrap()).is_empty()
    );
    let all = list(
        &store,
        "contracts.summaries",
        &params(serde_json::json!({"allVersions":true})),
    )
    .unwrap();
    assert_eq!(
        items(&all)[0].get("key").unwrap().as_js_string().unwrap().0,
        [0xd800]
    );
    insert(
        &store,
        "\"blank\"",
        "1.0.0",
        "\"\\ufeff\\t\\r\\n\\u2009\"",
        None,
    );
    insert(
        &store,
        "\"spaces\"",
        "1.0.0",
        "\" first\\u00a0\\t second\\u0085third \\r\\n\"",
        None,
    );
    insert(&store, "\"lone\"", "1.0.0", "\"\\udfff \\ud800\"", None);
    let lone = list(
        &store,
        "contracts.summaries",
        &params(serde_json::json!({"key":"lone"})),
    )
    .unwrap();
    assert_eq!(
        items(&lone)[0]
            .get("description")
            .unwrap()
            .as_js_string()
            .unwrap()
            .0,
        [0xdfff, 32, 0xd800]
    );
    for (key, description) in [("blank", "blank"), ("spaces", "first second\u{85}third")] {
        let page = list(
            &store,
            "contracts.summaries",
            &params(serde_json::json!({"key":key})),
        )
        .unwrap();
        assert_eq!(
            items(&page)[0].get("description"),
            Some(&description.into())
        );
        assert_eq!(items(&page)[0].get("hasJsonSchema"), Some(&false.into()));
    }
}

#[test]
fn only_selected_bodies_are_read_and_latest_does_not_parse_older_bodies() {
    let store = fixture();
    insert(&store, "\"a\"", "1.0.0", "\"old\"", None);
    insert(&store, "\"a\"", "2.0.0", "\"current\"", None);
    insert(&store, "\"b\"", "1.0.0", "\"later\"", None);
    store.connection().execute_batch("DROP TRIGGER contracts_immutable_update; UPDATE contracts SET definition='[' WHERE version='1.0.0'").unwrap();
    let query = params(serde_json::json!({"limit":1}));
    for method in ["contracts.list", "contracts.summaries"] {
        let page = list(&store, method, &query).unwrap();
        assert_eq!(items(&page)[0].get("version"), Some(&"2.0.0".into()));
        assert!(list(&store, method, &follow(&query, &page)).is_err());
        let mut invalid = query.clone();
        invalid.insert("cursor", "invalid".into());
        assert_eq!(
            list(&store, method, &invalid).unwrap_err().code,
            "invalid_cursor"
        );
    }
    assert_eq!(
        latest(&store, &"a".into()).unwrap().get("version"),
        Some(&"2.0.0".into())
    );
    assert_eq!(
        latest(&store, &"missing".into()).unwrap_err().code,
        "contract_not_found"
    );
}

#[test]
fn schema_bodies_stay_out_of_summaries_and_byte_limits_continue_at_the_last_item() {
    let store = fixture();
    let schema = format!("{{\"const\":\"{}\"}}", "x".repeat(1_100_000));
    for key in ["\"a\"", "\"b\"", "\"c\""] {
        insert(&store, key, "1.0.0", "\"small\"", Some(&schema));
    }
    let query = params(serde_json::json!({"limit":1000,"allVersions":true}));
    let mut query_page = query.clone();
    for expected in ["a", "b", "c"] {
        let page = list(&store, "contracts.list", &query_page).unwrap();
        assert_eq!(items(&page).len(), 1);
        assert_eq!(items(&page)[0].get("key"), Some(&expected.into()));
        if expected == "c" {
            assert!(page.get("nextCursor").unwrap().is_null());
        } else {
            query_page = follow(&query, &page);
        }
    }
    let summaries = list(&store, "contracts.summaries", &query).unwrap();
    assert_eq!(items(&summaries).len(), 3);
    assert!(canonical(&summaries, MANAGEMENT_FRAME_BYTES).unwrap().len() < 1024);
    let schema = format!("{{\"const\":\"{}\"}}", "x".repeat(paging::PAGE_BYTES));
    insert(&store, "\"huge\"", "1.0.0", "\"small\"", Some(&schema));
    let query = params(serde_json::json!({"key":"huge"}));
    assert_eq!(
        list(&store, "contracts.list", &query).unwrap_err().code,
        "result_too_large"
    );
    assert_eq!(
        items(&list(&store, "contracts.summaries", &query).unwrap()).len(),
        1
    );
}
