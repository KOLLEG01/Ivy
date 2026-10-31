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

fn native_bindings(tools: Vec<IvyJson>) -> Vec<crate::registry_catalog::ToolBinding> {
    tools.into_iter().map(|mut tool| crate::registry_catalog::ToolBinding::new(
        tool.remove("qualifiedName").unwrap().into_js_string().unwrap(),
        tool.remove("definitionHash").unwrap().into_js_string().unwrap(),
        tool.remove("definition").unwrap(),
    )).collect()
}

fn owned_tool_identity(id: &JsString, guide: &IvyJson, tools: &[crate::registry_catalog::ToolBinding]) -> IvyJson {
    IvyJson::object([
        ("nodeId", id.clone().into()),
        ("tools", IvyJson::Array(tools.iter().map(crate::registry_catalog::ToolBinding::to_wire).collect())),
        ("guide", guide.clone()),
    ])
}

#[test]
fn tool_page_identity_keeps_hashes_filters_and_error_order() {
    let id = JsString::from("fixture");
    let guide = IvyJson::String(JsString(vec![0xd800]));
    let tools = fixture_tools(0);
    let expected = hash_json(&IvyJson::object([
        ("nodeId", id.clone().into()), ("tools", IvyJson::Array(tools.clone())), ("guide", guide.clone()),
    ])).unwrap();
    let bindings = native_bindings(tools.clone());
    let params = IvyJson::object([("namePrefix", "call3".into())]);
    let (hash, filtered) = tool_page_records(&id, &guide, &bindings, &params).unwrap();
    assert_eq!(hash, expected);
    assert_eq!(hash_json(&IvyJson::Array(filtered.iter().map(|b| b.to_wire()).collect())).unwrap(),
        hash_json(&IvyJson::Array(vec![tools[3].clone()])).unwrap());
    let (hash, all) = tool_page_records(&id, &guide, &bindings, &IvyJson::Null).unwrap();
    assert_eq!(hash, expected);
    assert_eq!(hash_json(&IvyJson::Array(all.iter().map(|b| b.to_wire()).collect())).unwrap(),
        hash_json(&IvyJson::Array(tools)).unwrap());
    let malformed = vec![crate::registry_catalog::ToolBinding::new("bad".into(), "hash".into(), IvyJson::Null)];
    let guide = IvyJson::Number(f64::NAN);
    assert_eq!(tool_page_records(&id, &guide, &malformed, &params).unwrap_err().code, "internal_error");
    assert_eq!(tool_page_records(&id, &guide, &malformed, &IvyJson::Null).unwrap_err().code, "invalid_arguments");
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
    let tools = native_bindings(fixture_tools(schema_bytes));
    let copied_schema_bytes = 8 * 2 * schema_bytes * std::mem::size_of::<u16>();
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let identity = owned_tool_identity(&id, &guide, &tools);
    let expected = ivy_contracts::hash::digest(canonical(&identity, MANAGEMENT_FRAME_BYTES).unwrap());
    let cloned_peak = PEAK.load(Ordering::Relaxed) - before;
    drop(identity);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let (hash, returned) = tool_page_records(&id, &guide, &tools, &IvyJson::Null).unwrap();
    let moved_peak = PEAK.load(Ordering::Relaxed) - before;
    assert_eq!(hash, expected);
    assert_eq!(returned.len(), 8);
    eprintln!("Hive tool-page identity Rust allocation peak: full materialization {cloned_peak} bytes, streamed identity {moved_peak} bytes for 1 MiB opaque schema text");
    // Compare allocator requests above the already-owned input, excluding RSS.
    assert!(cloned_peak > moved_peak + isize::try_from(copied_schema_bytes * 3 / 4).unwrap());
    // One tool's UTF-16 payload fits; retaining the encoded catalog does not.
    assert!(moved_peak < 512 * 1024, "Catalog hashing retained encoded text: {moved_peak}");
}

#[test]
fn tool_pages_keep_cursors_and_materialize_only_selected_definitions() {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let tools = native_bindings(fixture_tools(0));
    let id = JsString::from("fixture");
    let guide = IvyJson::from("Fixture guide.");
    let hash = hash_json(&owned_tool_identity(&id, &guide, &tools)).unwrap();
    let identity = IvyJson::object([("snapshot", hash.clone().into())]);
    let mut cursor = None;
    let mut count = 0;
    loop {
        let (actual_hash, filtered) = tool_page_records(&id, &guide, &tools, &IvyJson::Null).unwrap();
        assert_eq!(actual_hash, hash);
        let legacy = crate::paging::catalog_page(&store,
            tools.iter().map(crate::registry_catalog::ToolBinding::to_wire).collect(), &identity,
            |v| v.get("qualifiedName").unwrap().as_js_string().unwrap().clone(), Some(3), cursor.as_ref()).unwrap();
        let materialized = std::cell::Cell::new(0);
        let actual = crate::paging::catalog_page_with(&store, filtered, &identity,
            |binding| Ok(binding.qualified_name.clone()), |binding| {
                materialized.set(materialized.get() + 1); Ok(binding.to_wire())
            }, Some(3), cursor.as_ref()).unwrap();
        assert_eq!(actual, legacy);
        let page_count = actual.get("items").unwrap().as_array().unwrap().len();
        assert_eq!(materialized.get(), page_count);
        count += page_count;
        cursor = legacy.get("nextCursor").unwrap().as_js_string().cloned();
        if cursor.is_none() { break; }
    }
    assert_eq!(count, 8);
    let materialized = std::cell::Cell::new(0);
    let error = crate::paging::catalog_page_with(&store, tools.iter().collect(), &identity,
        |binding| Ok(binding.qualified_name.clone()), |binding| {
            materialized.set(materialized.get() + 1); Ok(binding.to_wire())
        }, Some(1), Some(&"invalid cursor".into())).unwrap_err();
    assert_eq!(error.code, "invalid_cursor");
    assert_eq!(materialized.get(), 0);
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

fn copied_contract_records(
    catalog: Option<&IvyJson>, captured: Option<&IvyJson>, params: &IvyJson,
) -> IvyResult<(String, Vec<IvyJson>)> {
    let mut records = vec![];
    for (index, value) in catalog.filter(|value| !value.is_null())
        .map(|value| value.as_array().ok_or_else(IvyError::internal))
        .transpose()?.unwrap_or(&[]).iter().enumerate()
    {
        if selected(params, "key", value.get("key")) {
            records.push(IvyJson::object([
                ("value", value.clone()), ("index", IvyJson::Number(index as f64)),
            ]));
        }
    }
    let mut identity = IvyJson::object([("records", IvyJson::Array(records.clone()))]);
    if let Some(captured) = captured { identity.insert("capturedAt", captured.clone()); }
    Ok((hash_json(&identity)?, records))
}

#[test]
fn contract_page_identity_keeps_filters_original_indices_hashes_and_errors() {
    let key = IvyJson::String(JsString(vec![0xd800]));
    let catalog = IvyJson::Array(vec![
        IvyJson::Null,
        IvyJson::object([("key", "other".into())]),
        IvyJson::object([("key", key.clone()), ("readScope", IvyJson::object([
            (JsString(vec![0xd801]), IvyJson::String(JsString(vec![0xd802]))),
            ("opaque".into(), IvyJson::Array(vec![false.into(), 9007199254740992.0.into()])),
        ]))]),
        IvyJson::object([("key", key.clone()), ("writeVersions", IvyJson::Array(vec![]))]),
    ]);
    for catalog in [None, Some(IvyJson::Null), Some(IvyJson::Bool(false)),
        Some(IvyJson::object([] as [(&str, IvyJson); 0])), Some(catalog),
        Some(IvyJson::Array(vec![IvyJson::Number(f64::NAN)]))]
    {
        for captured in [None, Some(IvyJson::Null), Some(key.clone()), Some(IvyJson::Number(f64::NAN))] {
            for params in [IvyJson::Null, IvyJson::object([("key", "".into())]),
                IvyJson::object([("key", key.clone())]), IvyJson::object([("key", "absent".into())])]
            {
                let expected = copied_contract_records(catalog.as_ref(), captured.as_ref(), &params)
                    .map_err(|e| (e.code, e.message));
                let actual = contract_page_records(catalog.clone(), captured.as_ref(), &params)
                    .map_err(|e| (e.code, e.message));
                assert_eq!(actual, expected);
                if params.get("key") == Some(&key) && let Ok((_, records)) = actual
                    && records.len() == 2
                {
                    assert_eq!(records[0].get("index"), Some(&2.0.into()));
                    assert_eq!(records[1].get("index"), Some(&3.0.into()));
                }
            }
        }
    }
}

#[test]
fn contract_page_identity_moves_requirements_without_deep_copies() {
    const CHILD: &str = "IVY_CONTRACT_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "kernel::catalogs::read_tests::contract_page_identity_moves_requirements_without_deep_copies", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let catalog = IvyJson::Array((0..16).map(|index| IvyJson::object([
        ("key", format!("fixture/contract{index}").into()),
        ("readScope", IvyJson::object([("roots", IvyJson::Array(vec!["r".repeat(64 * 1024).into()]))])),
    ])).collect());
    let payload = |value: &IvyJson| value.get("readScope").unwrap()
        .get("roots").unwrap().as_array().unwrap()[0].as_js_string().unwrap().0.as_ptr() as usize;
    let pointers: Vec<_> = catalog.as_array().unwrap().iter().map(payload).collect();
    let captured = IvyJson::from("2026-01-01T00:00:00Z");
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let (expected, copied) = copied_contract_records(Some(&catalog), Some(&captured), &IvyJson::Null).unwrap();
    let copied_peak = PEAK.load(Ordering::Relaxed) - before;
    drop(copied);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let (hash, records) = contract_page_records(Some(catalog), Some(&captured), &IvyJson::Null).unwrap();
    let moved_peak = PEAK.load(Ordering::Relaxed) - before;
    assert_eq!(hash, expected);
    let identity = IvyJson::object([("snapshot", hash.into())]);
    let page = crate::paging::catalog_page(&store, records, &identity,
        |record| format!("{:08}", record.get("index").unwrap().as_f64().unwrap() as usize).into(), Some(3), None).unwrap();
    let items = page.get("items").unwrap().as_array().unwrap();
    assert_eq!(items.iter().map(|record| payload(record.get("value").unwrap())).collect::<Vec<_>>(), pointers[..3]);
    let cursor = page.get("nextCursor").unwrap().as_js_string().unwrap();
    assert_eq!(crate::paging::catalog_boundary(&store, &identity, cursor).unwrap(), JsString::from("00000002"));
    assert!(crate::paging::catalog_boundary(&store, &IvyJson::Null, cursor).is_err());
    eprintln!("Hive contract-page identity Rust allocation peak: copied {copied_peak} bytes, moved {moved_peak} bytes for 1 MiB read-scope text");
    assert!(copied_peak > moved_peak + 3 * 1024 * 1024, "Contract identity copied requirements");
}
