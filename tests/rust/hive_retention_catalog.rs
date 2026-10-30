use super::*;
use super::graph_tests::{LIVE, PEAK};
use crate::native_runtime::NativeRuntime;
use std::{path::Path, sync::atomic::Ordering as AtomicOrdering};

struct Collated;
impl Runtime for Collated {
    fn parse_date(&self, _: &JsString) -> f64 {
        unreachable!()
    }
    fn iso_date(&self, _: f64) -> IvyResult<JsString> {
        unreachable!()
    }
    fn compare_contract_keys(&self, _: &JsString, _: &JsString) -> Ordering {
        Ordering::Equal
    }
}

fn policy(marker: usize) -> IvyJson {
    IvyJson::object([
        ("awaitOwnerReadiness", true.into()),
        ("objects", IvyJson::object([("mode", "retain".into())])),
        ("revisions", IvyJson::object([
            ("maximumCount", IvyJson::from(marker as f64)),
            ("mode", "bounded".into()),
        ])),
    ])
}
fn definition(key: &JsString, version: &str, marker: usize) -> IvyJson {
    IvyJson::object([
        ("key", key.clone().into()),
        ("version", version.into()),
        ("retention", policy(marker)),
    ])
}
fn insert(store: &Store, key: &JsString, sql_version: &str, definition: &IvyJson) {
    store.connection().execute(
        "INSERT OR IGNORE INTO contract_families VALUES(?,'fixture','application/json','retain')",
        [text(key)],
    ).unwrap();
    store.connection().execute(
        "INSERT INTO contracts VALUES(?,?,?)",
        params![text(key), sql_version, canonical(definition, MANAGEMENT_FRAME_BYTES).unwrap()],
    ).unwrap();
}

#[test]
fn original_keys_numeric_versions_and_stable_ties_keep_owner_acknowledgements() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    let mut first = JsString::from("fixture/");
    first.0.push(0xd800);
    let middle = JsString::from("fixture/middle");
    let mut last = JsString::from("fixture/");
    last.0.push(0xd801);
    assert_eq!(text(&first), text(&last));
    insert(&store, &first, "row-1", &definition(&first, "2.0.0", 2));
    insert(&store, &middle, "row-2", &definition(&middle, "1.0.0", 1));
    insert(&store, &last, "row-3", &definition(&last, "3.0.0", 3));
    insert(&store, &first, "row-4", &definition(&first, "10.0.0", 10));
    // Equal versions retain the first definition, independently of SQL identity.
    insert(&store, &first, "row-5", &definition(&first, "10.0.0", 999));
    let selected = contracts(&store, &Collated).unwrap();
    assert_eq!(selected.iter().map(|c| &c.key).collect::<Vec<_>>(), [&first, &middle, &last]);
    for (contract, version, marker) in [
        (&selected[0], "10.0.0", 10),
        (&selected[1], "1.0.0", 1),
        (&selected[2], "3.0.0", 3),
    ] {
        assert_eq!(contract.policy, policy(marker));
        assert!(waiting_for_owner(&store, contract).unwrap());
        let acknowledgement = hash_json(&IvyJson::object([
            ("version", version.into()),
            ("policy", policy(marker)),
        ])).unwrap();
        store.set_metadata(
            &format!("retention_owner_ready:{}:{version}", text(&contract.key)),
            &acknowledgement,
        ).unwrap();
        assert!(!waiting_for_owner(&store, contract).unwrap());
    }
}

#[test]
fn obsolete_policy_is_skipped_but_selected_policy_and_all_metadata_are_required() {
    let key = JsString::from("fixture/catalog");
    let store = Store::open(Path::new(":memory:")).unwrap();
    let mut old = definition(&key, "1.0.0", 1);
    old.remove("retention");
    insert(&store, &key, "old", &old);
    insert(&store, &key, "new", &definition(&key, "2.0.0", 2));
    assert_eq!(contracts(&store, &NativeRuntime).unwrap()[0].policy, policy(2));
    let mut latest = definition(&key, "3.0.0", 3);
    latest.remove("retention");
    insert(&store, &key, "latest", &latest);
    assert!(contracts(&store, &NativeRuntime).is_err());

    for field in ["key", "version"] {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let mut invalid = definition(&key, "1.0.0", 1);
        invalid.insert(field, IvyJson::from(12.0));
        insert(&store, &key, "invalid", &invalid);
        insert(&store, &key, "valid", &definition(&key, "2.0.0", 2));
        assert!(contracts(&store, &NativeRuntime).is_err());
    }
}

#[test]
fn contract_loading_has_a_bounded_allocation_peak_with_large_unused_definitions() {
    const CHILD: &str = "IVY_RETENTION_CATALOG_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "retention::catalog_tests::contract_loading_has_a_bounded_allocation_peak_with_large_unused_definitions", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    const FAMILIES: usize = 64;
    const VERSIONS: usize = 4;
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    let docs = "d".repeat(192 * 1024);
    let schema = IvyJson::object([("$comment", IvyJson::from("s".repeat(128 * 1024)))]);
    store.transaction(|store| {
        for family in 0..FAMILIES {
            let key = JsString::from(format!("fixture/catalog-{family:03}"));
            for version in 1..=VERSIONS {
                let version = format!("{version}.0.0");
                let mut value = definition(&key, &version, 7);
                value.insert("specMarkdown", IvyJson::from(docs.as_str()));
                value.insert("jsonSchema", schema.clone());
                insert(store, &key, &version, &value);
            }
        }
        Ok(())
    }).unwrap();
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let selected = contracts(&store, &NativeRuntime).unwrap();
    let peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    let retained = LIVE.load(AtomicOrdering::Relaxed) - before;
    eprintln!("Hive retention contract Rust allocation peak above baseline: {peak} bytes, retained {retained} bytes for {FAMILIES} families and {} versions", FAMILIES * VERSIONS);
    assert_eq!(selected.len(), FAMILIES);
    assert!(selected.iter().all(|c| c.policy == policy(7)));
    // Actual Rust allocation requests; SQLite's native heap and process RSS are excluded.
    assert!(peak < 1024 * 1024, "Retention loaded unused contract bodies: {peak} bytes");
}
