use super::*;
use crate::retention::graph_tests::{LIVE, PEAK};
use std::sync::atomic::Ordering;

#[test]
fn catalog_page_moves_selected_records_without_copying_schema_payloads() {
    const CHILD: &str = "IVY_CATALOG_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "kernel::catalogs::read_tests::catalog_page_moves_selected_records_without_copying_schema_payloads", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"), "Memory child selected no test");
        return;
    }
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let schema_bytes = 64 * 1024;
    let tools = fixture_tools(schema_bytes);
    let payload = |tool: &IvyJson| tool.get("definition").unwrap()
        .get("inputSchema").unwrap().get("description").unwrap()
        .as_js_string().unwrap().0.as_ptr() as usize;
    let pointers: Vec<_> = tools.iter().map(payload).collect();
    let expected = hash_json(&IvyJson::Array(tools.clone())).unwrap();
    let identity = IvyJson::object([("method", "fixture.tools.list".into())]);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let page = crate::paging::catalog_page(&store, tools, &identity,
        |tool| tool.get("qualifiedName").unwrap().as_js_string().unwrap().clone(), None, None).unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - before;
    let items = page.get("items").unwrap().as_array().unwrap();
    assert_eq!(items.len(), 8);
    assert!(page.get("nextCursor").unwrap().is_null());
    assert_eq!(items.iter().map(payload).collect::<Vec<_>>(), pointers);
    assert_eq!(hash_json(page.get("items").unwrap()).unwrap(), expected);
    eprintln!("Hive catalog page Rust allocation peak: {peak} bytes above owned input for 1 MiB opaque schema text");
    // Response sizing may allocate one encoded record; the 2 MiB UTF-16 payload
    // must move into the page rather than being cloned alongside the input.
    assert!(peak < 1024 * 1024, "Catalog page copied schema payloads: {peak}");
}

fn fixture_tools(schema_bytes: usize) -> Vec<IvyJson> {
    (0..8).map(|index| IvyJson::object([
        ("qualifiedName", format!("fixture.call{index}").into()),
        ("definitionHash", "sha256:fixture".into()),
        ("definition", IvyJson::object([
            ("name", format!("call{index}").into()),
            ("inputSchema", IvyJson::object([("description", "s".repeat(schema_bytes).into())])),
            ("outputSchema", IvyJson::object([("description", "s".repeat(schema_bytes).into())])),
        ])),
    ])).collect()
}

#[test]
fn tool_page_identity_keeps_hashes_filters_and_error_order() {
    let id = JsString::from("fixture");
    let guide = IvyJson::String(JsString(vec![0xd800]));
    let tools = fixture_tools(0);
    let expected = hash_json(&IvyJson::object([
        ("nodeId", id.clone().into()), ("tools", IvyJson::Array(tools.clone())), ("guide", guide.clone()),
    ])).unwrap();
    let params = IvyJson::object([("namePrefix", "call3".into())]);
    let (hash, filtered) = tool_page_records(&id, &guide, tools.clone(), &params).unwrap();
    assert_eq!(hash, expected);
    assert_eq!(filtered, vec![tools[3].clone()]);
    let (hash, all) = tool_page_records(&id, &guide, tools.clone(), &IvyJson::Null).unwrap();
    assert_eq!(hash, expected);
    assert_eq!(all, tools);
    let malformed = vec![IvyJson::object([("definition", IvyJson::Null)])];
    let guide = IvyJson::Number(f64::NAN);
    assert_eq!(tool_page_records(&id, &guide, malformed.clone(), &params).unwrap_err().code, "internal_error");
    assert_eq!(tool_page_records(&id, &guide, malformed, &IvyJson::Null).unwrap_err().code, "invalid_arguments");
}

#[test]
fn tool_page_identity_does_not_clone_the_full_tool_vector() {
    const CHILD: &str = "IVY_TOOL_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "kernel::catalogs::read_tests::tool_page_identity_does_not_clone_the_full_tool_vector", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"), "Memory child selected no test");
        return;
    }
    let id = JsString::from("fixture");
    let guide = IvyJson::from("Fixture guide.");
    let schema_bytes = 64 * 1024;
    let tools = fixture_tools(schema_bytes);
    // Vec::clone copies initialized UTF-16 units, not the input's spare capacity.
    let copied_schema_bytes = 8 * 2 * schema_bytes * std::mem::size_of::<u16>();
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let identity = IvyJson::object([
        ("nodeId", id.clone().into()), ("tools", IvyJson::Array(tools.clone())), ("guide", guide.clone()),
    ]);
    let expected = hash_json(&identity).unwrap();
    let cloned_peak = PEAK.load(Ordering::Relaxed) - before;
    drop(identity);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let (hash, returned) = tool_page_records(&id, &guide, tools, &IvyJson::Null).unwrap();
    let moved_peak = PEAK.load(Ordering::Relaxed) - before;
    assert_eq!(hash, expected);
    assert_eq!(returned.len(), 8);
    eprintln!("Hive tool-page identity Rust allocation peak: cloned identity {cloned_peak} bytes, moved identity {moved_peak} bytes for 1 MiB opaque schema text");
    // Compare allocator requests above the already-owned input, excluding RSS.
    assert!(cloned_peak > moved_peak + isize::try_from(copied_schema_bytes * 3 / 4).unwrap());
}

fn fixture_topics(schema_bytes: usize) -> Vec<TopicRecord> {
    (0..8).map(|index| TopicRecord {
        provider: if index == 7 { IvyJson::Null } else {
            IvyJson::object([("node", IvyJson::object([("serviceNodeId", format!("fixture-{index}").into())]))])
        },
        namespace: "fixture".into(),
        definition: IvyJson::object([
            ("topic", format!("changed{index}").into()),
            ("version", "1.0.0".into()),
            ("payloadSchema", IvyJson::object([("description", "s".repeat(schema_bytes).into())])),
        ]),
    }).collect()
}

fn copied_topic_identity(records: &[TopicRecord]) -> IvyJson {
    IvyJson::Array(records.iter().map(|record| IvyJson::object([
        ("serviceNodeId", record.provider.get("node").and_then(|node| node.get("serviceNodeId"))
            .cloned().unwrap_or_else(|| "hive".into())),
        ("namespace", record.namespace.clone()),
        ("definition", record.definition.clone()),
    ])).collect())
}

#[test]
fn topic_page_identity_preserves_payloads_hashes_and_native_ownership() {
    for schema in [IvyJson::Bool(true), IvyJson::Bool(false), IvyJson::object([
        (JsString(vec![0xd800]), IvyJson::String(JsString(vec![0xd801]))),
        ("unknown".into(), IvyJson::Array(vec![IvyJson::Null, 17.0.into()])),
    ]), 9007199254740992.0.into()] {
        let mut records = fixture_topics(0);
        records[0].definition.insert("payloadSchema", schema);
        let expected_hash = hash_json(&copied_topic_identity(&records)).map_err(|e| e.code);
        let expected: Vec<_> = records.iter().map(|record| IvyJson::object([
            ("provider", record.provider.clone()), ("namespace", record.namespace.clone()),
            ("definition", record.definition.clone()),
        ])).collect();
        let actual = topic_page_records(records).map_err(|e| e.code);
        assert_eq!(actual.as_ref().map(|(hash, _)| hash).map_err(|code| code.clone()), expected_hash.as_ref().map_err(|code| code.clone()));
        if let Ok((_, returned)) = actual { assert_eq!(returned, expected); }
    }
}

#[test]
fn topic_page_identity_moves_schema_payloads_without_copying_them() {
    const CHILD: &str = "IVY_TOPIC_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "kernel::catalogs::read_tests::topic_page_identity_moves_schema_payloads_without_copying_them", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let records = fixture_topics(128 * 1024);
    let payload = |value: &IvyJson| value.get("payloadSchema").unwrap()
        .get("description").unwrap().as_js_string().unwrap().0.as_ptr() as usize;
    let pointers: Vec<_> = records.iter().map(|record| payload(&record.definition)).collect();
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let identity = copied_topic_identity(&records);
    let expected = hash_json(&identity).unwrap();
    let copied_peak = PEAK.load(Ordering::Relaxed) - before;
    drop(identity);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let (hash, returned) = topic_page_records(records).unwrap();
    let moved_peak = PEAK.load(Ordering::Relaxed) - before;
    assert_eq!(hash, expected);
    assert_eq!(returned.iter().map(|record| payload(record.get("definition").unwrap())).collect::<Vec<_>>(), pointers);
    eprintln!("Hive topic-page identity Rust allocation peak: copied {copied_peak} bytes, moved {moved_peak} bytes for 1 MiB schema text");
    assert!(copied_peak > moved_peak + 1536 * 1024, "Topic identity copied schemas");
}
