use super::*;
use ivy_contracts::core_schema::{validate_input, validate_output};
use ivy_hive::query::count_objects;
use std::collections::BTreeMap;

fn params(value: Value) -> IvyJson {
    IvyJson::from_serde(&value).unwrap()
}
fn count(store: &Store, input: &IvyJson) -> IvyJson {
    validate_input("objects.count", input).unwrap();
    let page = store
        .with_budget(Duration::from_secs(10), |s| count_objects(s, input))
        .unwrap();
    validate_output("objects.count", &page).unwrap();
    page
}
fn groups(page: &IvyJson, counted: bool) -> BTreeMap<String, u64> {
    let mut values = BTreeMap::new();
    for item in page.get("items").unwrap().as_array().unwrap() {
        let key = canonical(item.get("values").unwrap(), usize::MAX).unwrap();
        *values.entry(key).or_default() += if counted {
            item.get("count").unwrap().as_f64().unwrap() as u64
        } else {
            1
        };
        if counted {
            assert_eq!(item.as_object().unwrap().len(), 2);
        }
    }
    values
}

#[test]
fn sql_counts_match_current_rows_across_versions_filters_archives_and_scalar_types() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(&seed(false)).unwrap();
    for versions in [
        json!(["1.0.0"]),
        json!(["2.0.0"]),
        json!(["1.0.0", "2.0.0"]),
    ] {
        for archived in [false, true] {
            for filter in [
                json!({"op":"isNull","field":"object.parentId"}),
                json!({"op":"eq","field":"object.parentId","value":"id01"}),
                json!({"op":"or","args":[{"op":"contains","field":"data:/title","value":"Äbc"},{"op":"eq","field":"data:/flag","value":true}]}),
            ] {
                let input = json!({"contractKey":"fixture/value","contractVersions":versions,"groupBy":["data:/flag","data:/score"],"includeArchived":archived,"where":filter,"limit":200});
                let mut rows = input.clone();
                rows.as_object_mut().unwrap().remove("groupBy");
                rows["select"] = input["groupBy"].clone();
                let rows = store
                    .with_budget(Duration::from_secs(10), |s| query_objects(s, &params(rows)))
                    .unwrap();
                assert!(rows.get("nextCursor").unwrap().is_null());
                let counted = count(&store, &params(input));
                assert_eq!(groups(&counted, true), groups(&rows, false));
                assert!(counted.get("nextCursor").unwrap().is_null());
            }
        }
    }
    // An older retained revision must never contribute to a current count.
    store.connection().execute_batch("INSERT INTO revisions SELECT object_id,revision+1,contract_key,contract_version,encoding,content,content_hash,byte_length,created_at,references_json FROM revisions WHERE object_id='id01'; UPDATE objects SET current_revision=current_revision+1 WHERE id='id01';").unwrap();
    let result = count(
        &store,
        &params(
            json!({"contractKey":"fixture/value","groupBy":["object.id"],"where":{"op":"eq","field":"object.id","value":"id01"}}),
        ),
    );
    assert_eq!(
        result.get("items").unwrap().as_array().unwrap()[0].get("count"),
        Some(&1.0.into())
    );
}

#[test]
fn counted_group_keysets_preserve_types_null_ties_and_original_cursors_after_reopen() {
    let root = Root::new();
    let path = root.0.join("counts.sqlite");
    let store = Store::open(&path).unwrap();
    store.connection().execute_batch(&seed(false)).unwrap();
    let full = json!({"contractKey":"fixture/value","groupBy":["data:/flag","data:/score"],"includeArchived":true,"limit":200});
    let expected = count(&store, &params(full.clone()))
        .get("items")
        .unwrap()
        .clone();
    let mut input = full;
    input["limit"] = 1.into();
    let mut items = vec![];
    let mut cursors = std::collections::BTreeSet::new();
    drop(store);
    loop {
        let store = Store::open(&path).unwrap();
        let page = count(&store, &params(input.clone()));
        let next = page.get("nextCursor").unwrap();
        let batch = page.get("items").unwrap().as_array().unwrap();
        assert_eq!(batch.len(), 1);
        items.extend(batch.iter().cloned());
        if next.is_null() {
            break;
        }
        let cursor = next.as_js_string().unwrap().to_utf8().unwrap();
        assert!(cursors.insert(cursor.clone()) && cursor.len() <= 8192);
        input["cursor"] = cursor.into();
    }
    assert_eq!(IvyJson::Array(items), expected);
    let empty = count(
        &Store::open(&path).unwrap(),
        &params(
            json!({"contractKey":"fixture/value","groupBy":["object.name"],"where":{"op":"eq","field":"object.id","value":"absent"}}),
        ),
    );
    assert!(empty.get("items").unwrap().as_array().unwrap().is_empty());
    assert!(empty.get("nextCursor").unwrap().is_null());
}

#[test]
fn counted_cursors_bind_method_fields_filter_versions_archive_and_limit() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(&seed(false)).unwrap();
    let input = json!({"contractKey":"fixture/value","contractVersions":["1.0.0"],"groupBy":["data:/flag","data:/score"],"limit":1});
    let page = count(&store, &params(input.clone()));
    let cursor = page.get("nextCursor").unwrap().clone();
    assert!(!cursor.is_null());
    for (key, changed) in [
        ("groupBy", json!(["data:/score", "data:/flag"])),
        (
            "where",
            json!({"op":"eq","field":"data:/flag","value":true}),
        ),
        ("contractVersions", json!(["2.0.0"])),
        ("includeArchived", json!(true)),
        ("limit", json!(2)),
        ("contractKey", json!("fixture/large")),
    ] {
        let mut other = params(input.clone());
        other.insert(key, params(changed));
        other.insert("cursor", cursor.clone());
        assert_eq!(
            store
                .with_budget(Duration::from_secs(10), |s| count_objects(s, &other))
                .unwrap_err()
                .code,
            "invalid_cursor",
            "{key}"
        );
    }
    let mut other = params(
        json!({"contractKey":"fixture/value","contractVersions":["1.0.0"],"select":["data:/flag","data:/score"],"limit":1}),
    );
    other.insert("cursor", cursor);
    assert_eq!(
        query_objects(&store, &other).unwrap_err().code,
        "invalid_cursor"
    );
    let mut identity = params(input.clone());
    identity.insert("method", "objects.count".into());
    let invalid = store
        .cursor(&identity, &params(json!({"sort":[]})))
        .unwrap();
    let mut other = params(input);
    other.insert("cursor", invalid.into());
    assert_eq!(
        count_objects(&store, &other).unwrap_err().code,
        "invalid_cursor"
    );
}

#[test]
fn count_admission_reuses_scalar_contracts_complexity_and_database_work_budget() {
    let valid = json!({"contractKey":"fixture/value","contractVersions":["1.0.0"],"groupBy":["data:/flag"]});
    validate_input("objects.count", &params(valid.clone())).unwrap();
    for (key, value) in [
        ("limit", json!(0)),
        ("limit", json!(201)),
        ("groupBy", json!([])),
        (
            "groupBy",
            json!(["object.id", "object.name", "object.path"]),
        ),
        ("groupBy", json!(["object.id", "object.id"])),
        ("select", json!(["object.id"])),
        ("includeContent", json!(true)),
        ("groupBy", json!(["SQL:count(*)"])),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        assert_eq!(
            validate_input("objects.count", &params(invalid))
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(&seed(false)).unwrap();
    let mut input = valid;
    input["groupBy"] = json!(["data:/obj"]);
    assert_eq!(
        count_objects(&store, &params(input)).unwrap_err().code,
        "invalid_arguments"
    );
    let mut input = params(json!({"contractKey":"fixture/value","groupBy":["object.name"]}));
    input.insert("where", params(json!({"op":"and","args":(0..64).map(|_|json!({"op":"eq","field":"object.id","value":"id01"})).collect::<Vec<_>>()})));
    assert_eq!(
        count_objects(&store, &input).unwrap_err().code,
        "limit_exceeded"
    );
    let input = params(json!({"contractKey":"fixture/value","groupBy":["object.name"]}));
    assert_eq!(
        store
            .with_budget(Duration::ZERO, |s| count_objects(s, &input))
            .unwrap_err()
            .code,
        "limit_exceeded"
    );
    assert!(
        !count(&store, &input)
            .get("items")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty(),
        "A failed budget must not poison the next read."
    );
}

#[test]
fn large_group_keys_are_readable_without_emitting_an_unusable_continuation() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    let mut sql = seed(false);
    sql.push_str("BEGIN;");
    for i in 0..2 {
        insert(
            &mut sql,
            &format!("wide{i}"),
            "fixture/large",
            "1.0.0",
            "NULL",
            false,
            "json",
            &json!({"text":format!("{i}{}", "x".repeat(12000))}).to_string(),
            1,
        );
    }
    sql.push_str("COMMIT;");
    store.connection().execute_batch(&sql).unwrap();
    let mut input = params(
        json!({"contractKey":"fixture/large","contractVersions":["1.0.0"],"groupBy":["data:/text"],"limit":1}),
    );
    assert_eq!(
        count_objects(&store, &input).unwrap_err().code,
        "result_too_large"
    );
    input.insert("limit", 200.0.into());
    let page = count(&store, &input);
    assert_eq!(page.get("items").unwrap().as_array().unwrap().len(), 2);
    assert!(page.get("nextCursor").unwrap().is_null());
}

#[test]
fn ten_thousand_matching_objects_return_four_compact_counts_without_object_ids() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    let mut sql = seed(false);
    sql.push_str("BEGIN;");
    for i in 0..10_000 {
        insert(
            &mut sql,
            &format!("bulk{i:05}"),
            "fixture/value",
            "1.0.0",
            "NULL",
            false,
            "json",
            &json!({"flag":i%2==0,"score":i%4,"title":"bulk"}).to_string(),
            1,
        );
    }
    sql.push_str("COMMIT;");
    store.connection().execute_batch(&sql).unwrap();
    let group = json!(["data:/flag", "data:/score"]);
    let base = json!({"contractKey":"fixture/value","contractVersions":["1.0.0"],"where":{"op":"contains","field":"object.name","value":"bulk"},"limit":200});
    let mut old = base.clone();
    old["select"] = group.clone();
    let before = std::time::Instant::now();
    let mut old_rows = 0;
    let mut old_bytes = 0;
    let mut old_calls = 0;
    loop {
        let page = store
            .with_budget(Duration::from_secs(10), |s| {
                query_objects(s, &params(old.clone()))
            })
            .unwrap();
        old_calls += 1;
        old_rows += page.get("items").unwrap().as_array().unwrap().len();
        old_bytes += canonical(&page, usize::MAX).unwrap().len();
        if page.get("nextCursor").unwrap().is_null() {
            break;
        }
        old["cursor"] = page
            .get("nextCursor")
            .unwrap()
            .as_js_string()
            .unwrap()
            .to_utf8()
            .unwrap()
            .into();
    }
    let old_time = before.elapsed();
    let mut input = base;
    input["groupBy"] = group;
    let before = std::time::Instant::now();
    let counted = count(&store, &params(input));
    let new_time = before.elapsed();
    let new_bytes = canonical(&counted, usize::MAX).unwrap().len();
    assert_eq!(old_rows, 10_000);
    assert_eq!(old_calls, 50);
    assert_eq!(counted.get("items").unwrap().as_array().unwrap().len(), 4);
    assert_eq!(groups(&counted, true).values().sum::<u64>(), 10_000);
    assert!(
        counted.get("nextCursor").unwrap().is_null() && new_bytes < 1024 && old_bytes > 1_000_000
    );
    eprintln!(
        "Hive controlled counts: {old_rows} individual rows/{old_calls} calls/{old_bytes} response bytes/{old_time:?} -> four groups/one call/{new_bytes} bytes/{new_time:?}; in-process SQL comparison, not productive latency acceptance."
    );
}
