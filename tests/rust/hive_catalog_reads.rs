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

fn full_contract_page(store: &Store, params: &IvyJson, node: &IvyJson) -> IvyResult<IvyJson> {
    let catalog = Registry::default().required_contracts(store, string(params, "serviceNodeId")?)?;
    let has_catalog = catalog.is_some();
    let values = catalog.filter(|value| !value.is_null())
        .map(|value| value.into_array().ok_or_else(IvyError::internal))
        .transpose()?.unwrap_or_default();
    let records = values.into_iter().enumerate().filter_map(|(index, value)| {
        selected(params, "key", value.get("key")).then(|| IvyJson::object([
            ("value", value), ("index", IvyJson::Number(index as f64)),
        ]))
    }).collect();
    let mut snapshot = IvyJson::object([("records", IvyJson::Array(records))]);
    if let Some(captured) = node.get("lastSuccessfulSyncAt") {
        snapshot.insert("capturedAt", captured.clone());
    }
    let hash = hash_json(&snapshot)?;
    let records = snapshot.remove("records").and_then(IvyJson::into_array).unwrap();
    let mut identity = objects::page_identity("serviceNodes.contracts", params)?;
    identity.insert("snapshot", hash.into());
    let mut page = crate::paging::catalog_page(store, records, &identity,
        |record| format!("{:08}", record.get("index").unwrap().as_f64().unwrap() as usize).into(),
        params.get("limit").and_then(IvyJson::as_f64).map(|value| value as usize),
        optional_string(params, "cursor"))?;
    let items = page.remove("items").and_then(IvyJson::into_array).unwrap();
    page.insert("items", IvyJson::Array(items.into_iter()
        .map(|mut record| record.remove("value").unwrap()).collect()));
    page.insert("provider", provider(node));
    page.insert("hasCatalog", has_catalog.into());
    if let Some(captured) = node.get("lastSuccessfulSyncAt") {
        page.insert("capturedAt", captured.clone());
    }
    Ok(page)
}

fn contract_store() -> Store {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    store.connection().execute_batch(r#"INSERT INTO principals VALUES('fixture');
        INSERT INTO service_nodes VALUES('fixture','fixture','host','service',1,'{}');"#).unwrap();
    store
}
fn contract_source(store: &Store, source: Option<&str>) {
    store.connection().execute("DELETE FROM registries", []).unwrap();
    if let Some(source) = source {
        store.connection().execute("INSERT INTO registries VALUES('fixture',?)", [source]).unwrap();
    }
}
fn compare_contract_page(store: &Store, params: &IvyJson, node: &IvyJson) -> IvyResult<IvyJson> {
    let expected = full_contract_page(store, params, node).map_err(|error| error.to_wire());
    let actual = service_contracts::list(store, params, node);
    assert_eq!(actual.as_ref().map_err(IvyError::to_wire), expected.as_ref().map_err(Clone::clone));
    actual
}

#[test]
fn contract_page_identity_keeps_filters_original_indices_hashes_and_errors() {
    let store = contract_store();
    let values = r#"[null,false,17,"scalar",{},{"key":17},{"key":true},
        {"key":"other"},{"key":"\ud800","readScope":{"\ud801":"\ud802"}},
        {"key":"\ud800","writeVersions":[]},{"key":"�"},{"key":"nul\u0000tail"},
        {"key":"\ud83d\ude00"},{"key":"escaped"},{"key":"\u0065scaped"}]"#;
    for source in [None, Some("null"), Some("{}"), Some(r#"{"requiredContracts":null}"#),
        Some(r#"{"requiredContracts":false}"#), Some("[]"), Some("1"),
        Some(r#"{"requiredContracts":[]} trailing"#),
        Some(&format!(r#"{{"requiredContracts":{values}}}"#)),
        Some(r#"{"requiredContracts":[{"key":"unsafe","n":9007199254740992}]}"#)]
    {
        contract_source(&store, source);
        for captured in [None, Some(IvyJson::Null), Some("2026-01-01T00:00:00Z".into()),
            Some(IvyJson::String(JsString(vec![0xd800]))), Some(IvyJson::Number(f64::NAN))]
        {
            let mut node = IvyJson::object([("serviceNodeId", "fixture".into())]);
            if let Some(captured) = captured { node.insert("lastSuccessfulSyncAt", captured); }
            for filter in [None, Some("".into()), Some("other".into()), Some("absent".into()),
                Some(IvyJson::String(JsString(vec![0xd800]))), Some("�".into()),
                Some("nul\0tail".into()), Some("😀".into()), Some("escaped".into()), Some("true".into())]
            {
                let mut params = IvyJson::object([("serviceNodeId", "fixture".into()), ("limit", 1.0.into())]);
                if let Some(filter) = filter { params.insert("key", filter); }
                if let Ok(first) = compare_contract_page(&store, &params, &node) {
                    let mut page = first;
                    while !page.get("nextCursor").unwrap().is_null() {
                        params.insert("cursor", page.get("nextCursor").unwrap().clone());
                        page = compare_contract_page(&store, &params, &node).unwrap();
                    }
                }
                params.insert("cursor", "invalid".into());
                let _ = compare_contract_page(&store, &params, &node);
            }
        }
    }
    // A filtered-out edit leaves the snapshot intact; a selected edit invalidates
    // its cursor even when it lies beyond this response's first entry.
    let node = IvyJson::object([("serviceNodeId", "fixture".into()), ("lastSuccessfulSyncAt", IvyJson::Null)]);
    let mut params = IvyJson::object([("serviceNodeId", "fixture".into()), ("key", "same".into()), ("limit", 1.0.into())]);
    contract_source(&store, Some(r#"{"requiredContracts":[{"key":"same"},{"key":"other"},{"key":"same"}]}"#));
    let page = compare_contract_page(&store, &params, &node).unwrap();
    params.insert("cursor", page.get("nextCursor").unwrap().clone());
    contract_source(&store, Some(r#"{"requiredContracts":[{"key":"same"},{"key":"changed"},{"key":"same"}]}"#));
    compare_contract_page(&store, &params, &node).unwrap();
    contract_source(&store, Some(r#"{"requiredContracts":[{"key":"same"},{"key":"changed"},{"key":"same","extra":1}]}"#));
    assert_eq!(compare_contract_page(&store, &params, &node).unwrap_err().code, "invalid_cursor");
    contract_source(&store, Some(r#"{"requiredContracts":[{"key":"same"},{"key":"changed"},{"key":"same","extra":9007199254740992}]}"#));
    assert_eq!(compare_contract_page(&store, &params, &node).unwrap_err().code, "invalid_arguments");
}

#[test]
fn contract_pages_keep_wrapper_byte_limits_and_cursor_boundaries() {
    let store = contract_store();
    let node = IvyJson::object([("serviceNodeId", "fixture".into())]);
    let mut params = IvyJson::object([("serviceNodeId", "fixture".into()), ("limit", 50.0.into())]);
    let source = serde_json::json!({"requiredContracts":(0..3).map(|index|
        serde_json::json!({"key":format!("entry{index}"),"scope":"s".repeat(1100*1024)})
    ).collect::<Vec<_>>()}).to_string();
    contract_source(&store, Some(&source));
    let page = compare_contract_page(&store, &params, &node).unwrap();
    assert_eq!(page.get("items").unwrap().as_array().unwrap().len(), 1);
    params.insert("cursor", page.get("nextCursor").unwrap().clone());
    let page = compare_contract_page(&store, &params, &node).unwrap();
    assert_eq!(page.get("items").unwrap().as_array().unwrap()[0].get("key"), Some(&"entry1".into()));
    contract_source(&store, Some(&serde_json::json!({"requiredContracts":[{"scope":"s".repeat(crate::paging::PAGE_BYTES)}]}).to_string()));
    params.remove("cursor");
    assert_eq!(compare_contract_page(&store, &params, &node).unwrap_err().code, "result_too_large");
}

#[test]
fn contract_pages_stream_snapshot_and_load_only_selected_requirements() {
    const CHILD: &str = "IVY_CONTRACT_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "kernel::catalogs::read_tests::contract_pages_stream_snapshot_and_load_only_selected_requirements", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let store = contract_store();
    let node = IvyJson::object([("serviceNodeId", "fixture".into()), ("lastSuccessfulSyncAt", IvyJson::Null)]);
    let mut params = IvyJson::object([("serviceNodeId", "fixture".into()), ("limit", 1.0.into())]);
    let source = serde_json::json!({"requiredContracts":(0..32).map(|index|
        serde_json::json!({"key":format!("entry{index}"),"scope":"s".repeat(32*1024)})
    ).collect::<Vec<_>>()}).to_string();
    contract_source(&store, Some(&source));
    drop(source);
    for filter in [None, Some("entry31")] {
        if let Some(filter) = filter { params.insert("key", filter.into()); }
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let expected = full_contract_page(&store, &params, &node).unwrap();
        let full_peak = PEAK.load(Ordering::Relaxed) - before;
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let page = service_contracts::list(&store, &params, &node).unwrap();
        let projected_peak = PEAK.load(Ordering::Relaxed) - before;
        assert_eq!(page, expected);
        eprintln!("Hive contract page Rust allocation peak: full {full_peak} bytes, streamed {projected_peak} bytes; filter {filter:?}, 1 MiB stored scope text");
        // Rust requests only; SQLite's C heap and process RSS are excluded.
        assert!(full_peak > 3 * 1024 * 1024);
        assert!(projected_peak < 1024 * 1024, "Complete requirement array was retained");
    }
}

fn node_page_store() -> Store {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    store.connection().execute("INSERT INTO principals VALUES('fixture')",[]).unwrap();
    store
}

fn insert_inventory_fixture(store: &Store, id: &str, payload_bytes: usize) {
    insert_page_node(store,id,&IvyJson::object([
        ("serviceNodeId",id.into()),("hostId","host".into()),("serviceName","service".into()),
        ("connected",true.into()),("synced",true.into()),("ready",true.into()),("desiredEnabled",true.into()),
        ("lastContactAt","1970-01-01T00:00:00.000Z".into()),("opaque","s".repeat(payload_bytes).into()),
    ]));
    store.connection().execute("INSERT OR IGNORE INTO inventory_schemas VALUES('fixture','item','1','{}')",[]).unwrap();
    store.connection().execute("INSERT INTO inventory_sets VALUES(?1,'fixture','item',8,'1','1970-01-01T00:00:00.000Z')",[id]).unwrap();
    for native in ["a","b"] {
        store.connection().execute("INSERT INTO inventory VALUES(?1,'fixture','item',?2,'1','{}','1970-01-01T00:00:00.000Z')",params![id,native]).unwrap();
    }
    let diagnostic=IvyJson::object([("source",format!("service:{id}").into()),("status","current".into()),
        ("lastObservedAt","1970-01-01T00:00:00.000Z".into()),("message","m".repeat(payload_bytes).into())]);
    store.connection().execute("INSERT INTO diagnostics VALUES(?1,?2)",params![id,canonical(&diagnostic,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
}

#[test]
fn inventory_provider_projection_preserves_fields_flags_and_original_utf16() {
    let store=node_page_store();let registry=Registry::default();let runtime=crate::native_runtime::NativeRuntime;
    insert_inventory_fixture(&store,"node",0);
    for flag in ["connected","synced","ready","desiredEnabled"] {
        for value in [IvyJson::Bool(true),IvyJson::Bool(false),IvyJson::Null,1.0.into(),"true".into()] {
            let mut node=registry.node(&store,&"node".into(),&runtime,0.0).unwrap();
            node.insert(flag,value);node.insert("hostId",IvyJson::String(JsString(vec![0xd800,0,0xd801])));
            store.connection().execute("UPDATE service_nodes SET status_json=?1 WHERE id='node'",[canonical(&node,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
            for now in [0.0,10_000.0,10_001.0] {
                let full=registry.node(&store,&"node".into(),&runtime,now).unwrap();
                let compact=registry.inventory_provider(&store,&"node".into(),&runtime,now).unwrap();
                for field in ["serviceNodeId","hostId","serviceName","connected","synced","ready","desiredEnabled","lastContactAt","stale"] {
                    assert_eq!(compact.get(field),full.get(field),"{flag} / {field}");
                }
                assert_eq!(Registry::eligible(&compact),Registry::eligible(&full));
                assert!(compact.get("opaque").is_none());assert!(compact.get("diagnostic").unwrap().is_null());
            }
        }
    }
}

#[test]
fn inventory_pages_skip_unselected_status_and_diagnostics_keep_guards_and_utf16_aliases() {
    let store=node_page_store();let registry=Registry::default();let runtime=crate::native_runtime::NativeRuntime;
    insert_inventory_fixture(&store,"a",0);insert_inventory_fixture(&store,"z",0);
    store.connection().execute("UPDATE service_nodes SET status_json='invalid' WHERE id='z'",[]).unwrap();
    store.connection().execute("UPDATE diagnostics SET diagnostic_json=json_set(diagnostic_json,'$.lastObservedAt',42)",[]).unwrap();
    let params=IvyJson::object([("limit",1.0.into())]);
    let page=crate::inventory::list(&store,&registry,&params,&runtime,0.0).unwrap();
    let item=&page.get("items").unwrap().as_array().unwrap()[0];
    assert_eq!(item.get("stale").and_then(IvyJson::as_bool),Some(false));
    assert!(page.get("nextCursor").unwrap().as_js_string().is_some());
    let resource=item.get("resourceRef").unwrap();
    assert_eq!(crate::inventory::get(&store,&registry,resource,&runtime,0.0).unwrap(),*item);
    let conflict=IvyJson::object([("serviceNodeId","a".into()),("hostId","other".into()),("cursor","invalid".into())]);
    assert_eq!(crate::inventory::list(&store,&registry,&conflict,&runtime,0.0).unwrap_err().code,"target_conflict");
    let missing=IvyJson::object([("serviceNodeId","missing".into()),("cursor","invalid".into())]);
    assert_eq!(crate::inventory::list(&store,&registry,&missing,&runtime,0.0).unwrap_err().code,"not_found");
    let empty=IvyJson::object([("namespace","empty".into())]);
    assert!(crate::inventory::list(&store,&registry,&empty,&runtime,0.0).unwrap().get("items").unwrap().as_array().unwrap().is_empty());
    // Original JSON node identity remains distinct from its lossy SQL alias.
    store.connection().execute("UPDATE service_nodes SET status_json=json_set(status_json,'$.serviceNodeId',json('\"\\ud800\"')) WHERE id='a'",[]).unwrap();
    assert_eq!(crate::inventory::list(&store,&registry,&params,&runtime,0.0).unwrap_err().code,"storage_invalid");
    assert!(crate::inventory::get(&store,&registry,resource,&runtime,0.0).is_ok());
}

#[test]
fn inventory_pages_bound_provider_state_without_status_or_diagnostic_bodies() {
    const CHILD:&str="IVY_INVENTORY_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","kernel::catalogs::read_tests::inventory_pages_bound_provider_state_without_status_or_diagnostic_bodies","--nocapture"])
            .env(CHILD,"1").output().unwrap();
        eprint!("{}",String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));return;
    }
    let store=node_page_store();let registry=Registry::default();let runtime=crate::native_runtime::NativeRuntime;
    for index in 0..64 {insert_inventory_fixture(&store,&format!("node{index:03}"),32*1024);}
    let before=LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
    let full=registry.nodes(&store,&runtime,0.0).unwrap().into_iter().map(|node|
        (string(&node,"serviceNodeId").unwrap().clone(),node)).collect::<std::collections::HashMap<_,_>>();
    let full_peak=PEAK.load(Ordering::Relaxed)-before;
    assert_eq!(full.len(),64);drop(full);
    let before=LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
    let page=crate::inventory::list(&store,&registry,&IvyJson::object([("limit",1.0.into())]),&runtime,0.0).unwrap();
    let page_peak=PEAK.load(Ordering::Relaxed)-before;
    let items=page.get("items").unwrap().as_array().unwrap();assert_eq!(items.len(),1);
    assert_eq!(items[0].get("resourceRef").unwrap().get("serviceNodeId"),Some(&"node000".into()));
    assert_eq!(items[0].get("stale"),Some(&false.into()));
    eprintln!("Hive inventory Rust allocation peak: full provider map {full_peak} bytes, selected page {page_peak} bytes for 64 providers, 2 MiB status text and 2 MiB diagnostics");
    // The old provider map alone costs this much. The new measurement includes
    // the complete SQL page, set-revision cursor identity and response sizing.
    // SQLite's C heap and process RSS are outside this Rust allocation probe.
    assert!(full_peak>8*1024*1024);assert!(page_peak<512*1024);
}
fn insert_page_node(store: &Store, id: &str, node: &IvyJson) {
    store.connection().execute("INSERT INTO service_nodes VALUES(?1,'fixture','host','service',1,?2)",
        params![id,canonical(node,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
}
fn full_node_catalog(store: &Store, registry: &Registry, method: &str, params: &IvyJson, now: f64) -> IvyResult<IvyJson> {
    let nodes = registry.nodes(store,&crate::native_runtime::NativeRuntime,now)?;
    let records = if method == "serviceNodes.list" {
        nodes.into_iter().filter(|node| selected(params,"hostId",node.get("hostId"))
            && selected(params,"serviceName",node.get("serviceName"))).collect()
    } else {
        let mut seen = std::collections::HashSet::new();
        let mut hosts = Vec::new();
        for node in &nodes {
            let host = string(node,"hostId")?;
            if seen.insert(host.clone()) {
                let owned = nodes.iter().filter(|node| node.get("hostId")==Some(&IvyJson::String(host.clone()))).collect::<Vec<_>>();
                hosts.push(IvyJson::object([("hostId",host.clone().into()),
                    ("serviceNodeIds",IvyJson::Array(owned.iter().map(|node| node.get("serviceNodeId").cloned().unwrap_or(IvyJson::Null)).collect())),
                    ("connected",owned.iter().any(|node| node.get("connected").and_then(IvyJson::as_bool)==Some(true)).into())]));
            }
        }
        hosts
    };
    let identity = objects::page_identity(method,params)?;
    crate::paging::catalog_page(store,records,&identity,
        |record| key(record,if method=="hosts.list" {"hostId"} else {"serviceNodeId"}),
        params.get("limit").and_then(IvyJson::as_f64).map(|value| value as usize),optional_string(params,"cursor"))
}
fn compare_node_catalog(store: &Store, registry: &Registry, method: &str, params: &IvyJson, now: f64) -> IvyResult<IvyJson> {
    let expected = full_node_catalog(store,registry,method,params,now).map_err(|e|e.to_wire());
    let actual = if method=="hosts.list" {registry.host_page(store,params)}
        else {registry.node_page(store,params,&crate::native_runtime::NativeRuntime,now)};
    assert!(actual.as_ref().map_err(IvyError::to_wire)==expected.as_ref().map_err(Clone::clone),
        "{method} page differs; actual error {:?}",actual.as_ref().err().map(|e| &e.code));
    actual
}

#[test]
fn node_and_host_pages_keep_filters_live_fields_exact_cursors_and_lossy_sort_ties() {
    let store = node_page_store();
    let registry = Registry::default();
    let text = |units: &[u16]| IvyJson::String(JsString(units.to_vec()));
    let node_ids = ["alpha".into(),"nul\0tail".into(),text(&[0xd800]),text(&[0xd801]),
        "�".into(),"😀".into(),"é".into(),"z".into(),"beta".into(),"last".into()];
    let hosts = ["host".into(),"nul\0tail".into(),text(&[0xd800]),text(&[0xd801]),"�".into(),
        "😀".into(),"host".into(),"host".into(),"other".into(),"other".into()];
    for (index,(id,host)) in node_ids.iter().zip(&hosts).enumerate() {
        let node = IvyJson::object([("serviceNodeId",id.clone()),("hostId",host.clone()),
            ("serviceName",if index%2==0 {"even"} else {"odd"}.into()),
            ("connected",(index%3==0).into()),("synced",true.into()),("ready",(index%2==0).into()),
            ("desiredEnabled",true.into()),("lastContactAt","1970-01-01T00:00:00.000Z".into())]);
        insert_page_node(&store,&format!("row{index:03}"),&node);
        let source = {
            let mut units: Vec<_> = "service:".encode_utf16().collect();
            units.extend(id.as_js_string().unwrap().0.iter().copied());
            IvyJson::String(JsString(units))
        };
        for (which,date) in [("first","1970-01-01T00:00:00.000Z"),("latest","1970-01-01T00:00:01.000Z"),("tie","1970-01-01T00:00:01.000Z")] {
            let diagnostic = IvyJson::object([("source",source.clone()),("status","current".into()),
                ("lastObservedAt",date.into()),("code",which.into()),("severity","info".into()),
                ("message",text(&[0xd802,0,0xd803]))]);
            store.connection().execute("INSERT INTO diagnostics VALUES(?1,?2)",
                params![format!("{index}-{which}"),canonical(&diagnostic,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
        }
    }
    store.connection().execute("UPDATE service_nodes SET status_json=replace(status_json,'\"other\"','\"\\u006fther\"') WHERE id='row009'",[]).unwrap();
    for now in [0.0,10_000.0,10_001.0] {
        for method in ["serviceNodes.list","hosts.list"] {
            for limit in [1,2,50] {
                let filters = if method=="hosts.list" {vec![(None,None)]} else {
                    vec![(None,None),(Some("".into()),Some("".into())),(Some("host".into()),None),
                        (Some(hosts[2].clone()),None),(Some(hosts[1].clone()),None),
                        (None,Some("odd".into())),(Some("absent".into()),None)]
                };
                for (host,service) in filters {
                    let mut params = IvyJson::object([("limit",(limit as f64).into())]);
                    if let Some(host)=host {params.insert("hostId",host);}
                    if let Some(service)=service {params.insert("serviceName",service);}
                    let mut page = compare_node_catalog(&store,&registry,method,&params,now).unwrap();
                    let mut pages = 1;
                    while !page.get("nextCursor").unwrap().is_null() {
                        params.insert("cursor",page.get("nextCursor").unwrap().clone());
                        page = compare_node_catalog(&store,&registry,method,&params,now).unwrap();
                        pages += 1;assert!(pages<=node_ids.len()+1);
                    }
                    params.insert("cursor","invalid".into());
                    assert_eq!(compare_node_catalog(&store,&registry,method,&params,now).unwrap_err().code,"invalid_cursor");
                }
            }
        }
    }
}

#[test]
fn node_pages_keep_diagnostic_response_byte_limits() {
    let store = node_page_store();
    let registry = Registry::default();
    for index in 0..3 {
        let id = format!("node{index}");
        insert_page_node(&store,&id,&IvyJson::object([("serviceNodeId",id.clone().into()),("hostId","host".into()),
            ("serviceName","service".into()),("connected",true.into())]));
        let diagnostic = IvyJson::object([("source",format!("service:{id}").into()),("status","current".into()),
            ("lastObservedAt","1970-01-01T00:00:00.000Z".into()),("message","m".repeat(1100*1024).into())]);
        store.connection().execute("INSERT INTO diagnostics VALUES(?1,?2)",params![id,canonical(&diagnostic,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
    }
    let mut params = IvyJson::object([("limit",50.0.into())]);
    let first = compare_node_catalog(&store,&registry,"serviceNodes.list",&params,0.0).unwrap();
    assert_eq!(first.get("items").unwrap().as_array().unwrap().len(),1);
    params.insert("cursor",first.get("nextCursor").unwrap().clone());
    let second = compare_node_catalog(&store,&registry,"serviceNodes.list",&params,0.0).unwrap();
    assert_eq!(second.get("items").unwrap().as_array().unwrap()[0].get("serviceNodeId"),Some(&"node1".into()));
    params.remove("cursor");
    let hosts = compare_node_catalog(&store,&registry,"hosts.list",&params,0.0).unwrap();
    assert_eq!(hosts.get("items").unwrap().as_array().unwrap()[0].get("serviceNodeIds").unwrap().as_array().unwrap().len(),3);
}

#[test]
fn node_and_host_pages_load_only_selected_status_and_skip_unreturned_diagnostics() {
    const CHILD: &str = "IVY_NODE_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","kernel::catalogs::read_tests::node_and_host_pages_load_only_selected_status_and_skip_unreturned_diagnostics","--nocapture"])
            .env(CHILD,"1").output().unwrap();
        eprint!("{}",String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let store = node_page_store();
    let registry = Registry::default();
    for index in 0..64 {
        let id = format!("node{index:03}");
        insert_page_node(&store,&id,&IvyJson::object([("serviceNodeId",id.clone().into()),("hostId",format!("host{}",index%4).into()),
            ("serviceName","service".into()),("connected",true.into())]));
        let diagnostic = IvyJson::object([("source",format!("service:{id}").into()),("status","current".into()),
            ("lastObservedAt","1970-01-01T00:00:00.000Z".into()),("message","m".repeat(32*1024).into())]);
        store.connection().execute("INSERT INTO diagnostics VALUES(?1,?2)",params![id,canonical(&diagnostic,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
    }
    let params = IvyJson::object([("limit",1.0.into())]);
    for method in ["serviceNodes.list","hosts.list"] {
        let before = LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
        let expected = full_node_catalog(&store,&registry,method,&params,0.0).unwrap();
        let full_peak = PEAK.load(Ordering::Relaxed)-before;
        let before = LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
        let actual = if method=="hosts.list" {registry.host_page(&store,&params)}
            else {registry.node_page(&store,&params,&crate::native_runtime::NativeRuntime,0.0)}.unwrap();
        let projected_peak = PEAK.load(Ordering::Relaxed)-before;
        assert_eq!(actual,expected);
        eprintln!("Hive {method} Rust allocation peak: full {full_peak} bytes, paged {projected_peak} bytes for 64 nodes and 2 MiB diagnostic messages");
        assert!(full_peak>8*1024*1024);
        // Rust requests only; SQLite's C heap and process RSS are excluded.
        assert!(projected_peak<512*1024,"Unreturned diagnostics or full node catalogs were retained");
    }
}


fn full_observation_catalog(store: &Store, registry: &Registry, params: &IvyJson, now: f64) -> IvyResult<IvyJson> {
    let host=optional_string(params,"hostId").map(text);
    let mut statement=sql(store,store.connection().prepare_cached(
        "SELECT node_id,reported_at,snapshot_json FROM host_reports WHERE (?1 IS NULL OR host_id=?1) ORDER BY host_id LIMIT 1001"))?;
    let records=sql(store,statement.query_map([host],|row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?))))?
        .map(|row| {
            let (id,at,snapshot)=sql(store,row)?;
            let snapshot=decode(&snapshot)?;
            let reporter=registry.node(store,&id.into(),&crate::native_runtime::NativeRuntime,now)?;
            Ok(IvyJson::object([("snapshot",snapshot),("serviceNodeId",reporter.get("serviceNodeId").unwrap().clone()),
                ("reportedAt",at.into()),("stale",(reporter.get("connected").and_then(IvyJson::as_bool)!=Some(true)
                    ||reporter.get("ready").and_then(IvyJson::as_bool)!=Some(true)
                    ||reporter.get("stale").and_then(IvyJson::as_bool)==Some(true)).into())]))
        }).collect::<IvyResult<Vec<_>>>()?;
    require(records.len()<=1000,"limit_exceeded","Host observation catalog exceeds its operating bound.")?;
    crate::paging::catalog_page(store,records,&objects::page_identity("hosts.observations",params)?,
        |record|key(record.get("snapshot").and_then(|v|v.get("status")).unwrap_or(&IvyJson::Null),"hostId"),
        params.get("limit").and_then(IvyJson::as_f64).map(|v|v as usize),optional_string(params,"cursor"))
}
fn compare_observation_catalog(store: &Store, registry: &Registry, params: &IvyJson, now: f64) -> IvyResult<IvyJson> {
    let expected=full_observation_catalog(store,registry,params,now).map_err(|error|error.to_wire());
    let actual=registry.observation_page(store,params,&crate::native_runtime::NativeRuntime,now);
    assert!(actual.as_ref().map_err(IvyError::to_wire)==expected.as_ref().map_err(Clone::clone),
        "Host observation page differs: {:?}",actual.as_ref().err().map(|e|&e.code));
    actual
}
fn insert_host_report(store: &Store, host: &str, id: &str, key: IvyJson, body_bytes: usize) {
    let snapshot=IvyJson::object([("status",IvyJson::object([("hostId",key),
        ("opaque","s".repeat(body_bytes).into())]))]);
    store.connection().execute("INSERT INTO host_reports VALUES(?1,?2,1,'1970-01-01T00:00:00.000Z',?3)",
        params![host,id,canonical(&snapshot,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
}

#[test]
fn observation_pages_keep_live_fields_host_filters_exact_cursors_and_sort_ties() {
    let store=node_page_store();let registry=Registry::default();
    let keys=vec!["z".into(),"nul\0tail".into(),IvyJson::String(JsString(vec![0xd800])),
        IvyJson::String(JsString(vec![0xd801])),"�".into(),"😀".into(),"é".into(),
        "z".into(),"".into(),IvyJson::Null,false.into(),"empty filter".into()];
    for (index,key) in keys.into_iter().enumerate() {
        let id=format!("node{index:03}");
        let mut node=IvyJson::object([("serviceNodeId",id.clone().into()),("hostId","host".into()),
            ("connected",(index%3!=0).into()),("ready",(index%2==0).into()),
            ("stale",true.into()),("lastContactAt","1970-01-01T00:00:00.000Z".into())]);
        if index==1 {node.remove("lastContactAt");}
        if index==4 {node.insert("lastContactAt","invalid date".into());}
        insert_page_node(&store,&id,&node);
        insert_host_report(&store,if index==11 {""} else {&id},&id,key,0);
    }
    for now in [0.0,10_000.0,10_001.0] {
        for host in [None,Some("".into()),Some("node003".into()),Some("absent".into()),
            Some(JsString(vec![0xd800]))] {
            for limit in [1,2,50] {
                let mut params=IvyJson::object([("limit",(limit as f64).into())]);
                if let Some(host)=host.clone() {params.insert("hostId",host.into());}
                let mut page=compare_observation_catalog(&store,&registry,&params,now).unwrap();
                let mut pages=1;
                while !page.get("nextCursor").unwrap().is_null() {
                    params.insert("cursor",page.get("nextCursor").unwrap().clone());
                    page=compare_observation_catalog(&store,&registry,&params,now).unwrap();
                    pages+=1;assert!(pages<=13);
                }
                params.insert("cursor","invalid".into());
                assert_eq!(compare_observation_catalog(&store,&registry,&params,now).unwrap_err().code,"invalid_cursor");
            }
        }
    }
}

#[test]
fn observation_pages_keep_operating_and_response_bounds() {
    let store=node_page_store();let registry=Registry::default();
    let empty=IvyJson::object(std::iter::empty::<(&str,IvyJson)>());
    insert_page_node(&store,"reporter",&IvyJson::object([("serviceNodeId","reporter".into()),("connected",true.into())]));
    for index in 0..1001 {
        let host=format!("host{index:04}");insert_host_report(&store,&host,"reporter",host.clone().into(),0);
    }
    for cursor in [None,Some("invalid")] {
        let mut params=IvyJson::object([("limit",1.0.into())]);
        if let Some(cursor)=cursor {params.insert("cursor",cursor.into());}
        assert_eq!(compare_observation_catalog(&store,&registry,&params,0.0).unwrap_err().code,"limit_exceeded");
    }
    let filtered=IvyJson::object([("hostId","host1000".into())]);
    assert_eq!(compare_observation_catalog(&store,&registry,&filtered,0.0).unwrap().get("items").unwrap().as_array().unwrap().len(),1);
    store.connection().execute("DELETE FROM host_reports WHERE host_id='host1000'",[]).unwrap();
    compare_observation_catalog(&store,&registry,&empty,0.0).unwrap();
    store.connection().execute("DELETE FROM host_reports",[]).unwrap();
    for index in 0..3 {let host=format!("host{index}");insert_host_report(&store,&host,"reporter",host.clone().into(),1100*1024);}
    let mut params=empty.clone();
    for _ in 0..3 {
        let page=compare_observation_catalog(&store,&registry,&params,0.0).unwrap();
        assert_eq!(page.get("items").unwrap().as_array().unwrap().len(),1);
        params=IvyJson::object([("cursor",page.get("nextCursor").unwrap().clone())]);
    }
    store.connection().execute("DELETE FROM host_reports",[]).unwrap();
    insert_host_report(&store,"huge","reporter","huge".into(),2*1024*1024);
    assert_eq!(compare_observation_catalog(&store,&registry,&empty,0.0).unwrap_err().code,"result_too_large");
    assert_eq!(compare_observation_catalog(&store,&registry,&IvyJson::object([("cursor","invalid".into())]),0.0).unwrap_err().code,"invalid_cursor");
}

#[test]
fn observation_pages_load_selected_snapshots_without_reporter_diagnostics() {
    const CHILD: &str="IVY_OBSERVATION_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","kernel::catalogs::read_tests::observation_pages_load_selected_snapshots_without_reporter_diagnostics","--nocapture"])
            .env(CHILD,"1").output().unwrap();
        eprint!("{}",String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));return;
    }
    let store=node_page_store();let registry=Registry::default();
    for index in 0..64 {
        let id=format!("node{index:03}");insert_namespace_provider(&store,&id,true,vec!["fixture".into()],32*1024,0);
        insert_host_report(&store,&id,&id,id.clone().into(),32*1024);
    }
    let params=IvyJson::object([("limit",1.0.into())]);
    let before=LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
    let expected=full_observation_catalog(&store,&registry,&params,0.0).unwrap();
    let full_peak=PEAK.load(Ordering::Relaxed)-before;
    let before=LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
    let actual=registry.observation_page(&store,&params,&crate::native_runtime::NativeRuntime,0.0).unwrap();
    let projected_peak=PEAK.load(Ordering::Relaxed)-before;
    assert_eq!(actual,expected);
    eprintln!("Hive host observation Rust allocation peak: full {full_peak} bytes, paged {projected_peak} bytes for 64 reports, 2 MiB snapshots and 2 MiB diagnostics");
    // Rust requests only; SQLite's C heap and process RSS are excluded.
    assert!(full_peak>4*1024*1024);assert!(projected_peak<512*1024,"Unreturned snapshots or reporter diagnostics retained");
}

fn full_protected_builds(store: &Store, current: &JsString) -> IvyResult<std::collections::HashSet<JsString>> {
    let registry=Registry::default();let runtime=&crate::native_runtime::NativeRuntime;
    let mut builds=std::collections::HashSet::from([current.clone()]);
    for node in registry.nodes(store,runtime,0.0)? {
        if let Some(id)=node.get("buildId").and_then(IvyJson::as_js_string) {builds.insert(id.clone());}
    }
    let mut statement=sql(store,store.connection().prepare_cached(
        "SELECT node_id,snapshot_json FROM host_reports ORDER BY host_id LIMIT 1001"))?;
    let snapshots=sql(store,statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))))?
        .map(|row| {
            let (id,value)=sql(store,row)?;let snapshot=decode(&value)?;
            if snapshot.as_object().is_none(){return Err(IvyError::internal());}
            registry.node(store,&id.into(),runtime,0.0)?;Ok(snapshot)
        }).collect::<IvyResult<Vec<_>>>()?;
    let mut optional=|value:Option<&IvyJson>| {
        if let Some(id)=value.and_then(IvyJson::as_js_string).filter(|id|!id.0.is_empty()){builds.insert(id.clone());}
    };
    for snapshot in snapshots {
        let status=snapshot.get("status").ok_or_else(IvyError::internal)?;
        optional(status.get("executor").and_then(|v|v.get("buildId")));
        for (name,fields) in [("instances",&["installedBuild","observedBuild"][..]),
            ("unfinished",&["previousBuild","targetBuild","observedBuild"][..])] {
            let values=match status.get(name).ok_or_else(IvyError::internal)? {IvyJson::Array(a)=>a.as_slice(),IvyJson::String(_)=>&[],_=>return Err(IvyError::internal())};
            for value in values {
                if value.is_null(){return Err(IvyError::internal());}
                for field in fields {optional(value.get(*field));}
            }
        }
    }
    Ok(builds)
}
fn compare_protected_builds(store: &Store) -> IvyResult<std::collections::HashSet<JsString>> {
    let expected=full_protected_builds(store,&"kernel".into()).map_err(|e|e.to_wire());
    let actual=crate::package_kernel::protected_builds(store,&"kernel".into());
    assert_eq!(actual.as_ref().map_err(IvyError::to_wire),expected.as_ref().map_err(Clone::clone));actual
}
fn set_host_snapshot(store: &Store, host: &str, node: &str, value: &IvyJson) {
    store.connection().execute("INSERT OR REPLACE INTO host_reports VALUES(?1,?2,1,'1970-01-01T00:00:00.000Z',?3)",
        params![host,node,canonical(value,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
}

#[test]
fn package_protection_keeps_exact_build_ids_optional_shapes_and_corrupt_record_errors() {
    let store=node_page_store();
    let ids=["".into(),"same".into(),"nul\0tail".into(),IvyJson::String(JsString(vec![0xd800])),
        IvyJson::String(JsString(vec![0xd801])),"�".into(),"😀".into()];
    for (index,id) in ids.iter().enumerate() {
        let node=format!("node{index}");insert_page_node(&store,&node,&IvyJson::object([
            ("serviceNodeId",node.clone().into()),("buildId",id.clone()),("connected",false.into())]));
        let status=IvyJson::object([("executor",IvyJson::object([("buildId",id.clone())])),
            ("instances",IvyJson::Array(vec![IvyJson::object([("installedBuild",id.clone()),("observedBuild","shared".into())])])),
            ("unfinished",IvyJson::Array(vec![IvyJson::object([("previousBuild",id.clone()),("targetBuild","target".into()),("observedBuild","observed".into())])]))]);
        set_host_snapshot(&store,&node,&node,&IvyJson::object([("status",status)]));
    }
    let builds=compare_protected_builds(&store).unwrap();
    for id in &ids {assert!(builds.contains(id.as_js_string().unwrap()));}
    for id in ["kernel","shared","target","observed"] {assert!(builds.contains(&id.into()));}
    for value in [IvyJson::Null,false.into(),17.0.into(),"".into(),IvyJson::Array(vec![]),IvyJson::object([("opaque",true.into())])] {
        let status=IvyJson::object([("executor",IvyJson::object([("buildId",value.clone())])),
            ("instances",IvyJson::Array(vec![false.into(),17.0.into(),"scalar".into(),IvyJson::Array(vec![]),
                IvyJson::object([("installedBuild",value.clone()),("observedBuild",value.clone())])])),
            ("unfinished",IvyJson::Array(vec![IvyJson::object([("previousBuild",value.clone()),("targetBuild",value.clone()),("observedBuild",value)])]))]);
        set_host_snapshot(&store,"node0","node0",&IvyJson::object([("status",status)]));compare_protected_builds(&store).unwrap();
    }
    for source in ["null","false","[]","{}",r#"{"status":null}"#,
        r#"{"status":{"instances":null,"unfinished":[]}}"#,
        r#"{"status":{"instances":[null],"unfinished":[]}}"#,
        r#"{"status":{"instances":[],"unfinished":[null]}}"#] {
        set_host_snapshot(&store,"node0","node0",&parse(source,usize::MAX).unwrap());
        assert_eq!(compare_protected_builds(&store).unwrap_err().code,"internal_error");
    }
    set_host_snapshot(&store,"node0","node0",&parse(r#"{"status":{"instances":"characters","unfinished":"later"}}"#,usize::MAX).unwrap());
    compare_protected_builds(&store).unwrap();
    store.connection().execute("UPDATE service_nodes SET status_json='null' WHERE id='node0'",[]).unwrap();
    assert_eq!(compare_protected_builds(&store).unwrap_err().code,"internal_error");
}

#[test]
fn package_protection_keeps_missing_reporter_priority_before_reference_validation() {
    let store=node_page_store();insert_page_node(&store,"reporter",&IvyJson::object([("serviceNodeId","reporter".into())]));
    set_host_snapshot(&store,"a","reporter",&IvyJson::object([("status",IvyJson::Null)]));
    store.connection().execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    set_host_snapshot(&store,"z","gone",&IvyJson::object([("status",IvyJson::object([
        ("instances",IvyJson::Array(vec![])),("unfinished",IvyJson::Array(vec![]))]))]));
    store.connection().execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    assert_eq!(compare_protected_builds(&store).unwrap_err().code,"not_found");
    store.connection().execute("DELETE FROM host_reports WHERE host_id='z'",[]).unwrap();
    assert_eq!(compare_protected_builds(&store).unwrap_err().code,"internal_error");
}

#[test]
fn package_protection_includes_hosts_beyond_the_old_observation_listing_bound() {
    let store=node_page_store();insert_page_node(&store,"reporter",&IvyJson::object([("serviceNodeId","reporter".into())]));
    for index in 0..1020 {
        let host=format!("host{index:04}");
        let status=IvyJson::object([("executor",IvyJson::object([("buildId",host.clone().into())])),
            ("instances",IvyJson::Array(vec![])),("unfinished",IvyJson::Array(vec![]))]);
        set_host_snapshot(&store,&host,"reporter",&IvyJson::object([("status",status)]));
    }
    let old=full_protected_builds(&store,&"kernel".into()).unwrap();
    let actual=crate::package_kernel::protected_builds(&store,&"kernel".into()).unwrap();
    assert_eq!(old.len(),1002);assert_eq!(actual.len(),1021);assert!(old.is_subset(&actual));
    for index in 0..1020 {assert!(actual.contains(&format!("host{index:04}").into()));}
}

#[test]
fn package_protection_streams_ids_without_status_snapshot_or_diagnostic_bodies() {
    const CHILD:&str="IVY_PACKAGE_PROTECTION_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","kernel::catalogs::read_tests::package_protection_streams_ids_without_status_snapshot_or_diagnostic_bodies","--nocapture"])
            .env(CHILD,"1").output().unwrap();
        eprint!("{}",String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));return;
    }
    let store=node_page_store();
    for index in 0..64 {
        let id=format!("node{index:03}");insert_namespace_provider(&store,&id,false,vec!["fixture".into()],32*1024,0);
        let status=IvyJson::object([("executor",IvyJson::object([("buildId",id.clone().into())])),
            ("instances",IvyJson::Array(vec![])),("unfinished",IvyJson::Array(vec![])),("opaque","s".repeat(32*1024).into())]);
        set_host_snapshot(&store,&id,&id,&IvyJson::object([("status",status)]));
    }
    let before=LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
    let expected=full_protected_builds(&store,&"kernel".into()).unwrap();let full_peak=PEAK.load(Ordering::Relaxed)-before;
    let before=LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
    let actual=crate::package_kernel::protected_builds(&store,&"kernel".into()).unwrap();let projected_peak=PEAK.load(Ordering::Relaxed)-before;
    assert_eq!(actual,expected);assert_eq!(actual.len(),65);
    eprintln!("Hive protected build IDs Rust allocation peak: full {full_peak} bytes, projected {projected_peak} bytes for 64 hosts, 2 MiB snapshots and 2 MiB diagnostics");
    // Rust requests only; SQLite's C heap and process RSS are excluded.
    assert!(full_peak>4*1024*1024);assert!(projected_peak<512*1024,"Unneeded report or diagnostic bodies retained");
}

fn full_namespace_catalog(store: &Store, registry: &Registry, params: &IvyJson, now: f64) -> IvyResult<IvyJson> {
    let mut records: Vec<IvyJson> = Vec::new();
    for node in registry.nodes(store,&crate::native_runtime::NativeRuntime,now)? {
        for name in registry.namespace_names(store,string(&node,"serviceNodeId")?)? {
            if let Some(record)=records.iter_mut().find(|record| record.get("namespace")==Some(&name)) {
                let mut providers=record.remove("providers").and_then(IvyJson::into_array).unwrap();
                providers.push(provider(&node));record.insert("providers",IvyJson::Array(providers));
            } else {
                records.push(IvyJson::object([("namespace",name),("providers",IvyJson::Array(vec![provider(&node)]))]));
            }
        }
    }
    let identity=objects::page_identity("namespaces.list",params)?;
    crate::paging::catalog_page(store,records,&identity,|record|key(record,"namespace"),
        params.get("limit").and_then(IvyJson::as_f64).map(|value|value as usize),optional_string(params,"cursor"))
}
fn compare_namespace_catalog(store: &Store, registry: &Registry, params: &IvyJson, now: f64) -> IvyResult<IvyJson> {
    let expected=full_namespace_catalog(store,registry,params,now).map_err(|error|error.to_wire());
    let actual=registry.namespace_page(store,params,&crate::native_runtime::NativeRuntime,now);
    assert!(actual.as_ref().map_err(IvyError::to_wire)==expected.as_ref().map_err(Clone::clone),
        "Namespace page differs; actual error {:?}",actual.as_ref().err().map(|e|&e.code));
    actual
}
fn insert_namespace_provider(store: &Store, id: &str, online: bool, names: Vec<IvyJson>, message_bytes: usize, schema_bytes: usize) {
    insert_page_node(store,id,&IvyJson::object([("serviceNodeId",id.into()),("hostId","host".into()),
        ("serviceName","service".into()),("connected",online.into()),("ready",online.into()),
        ("synced",true.into()),("desiredEnabled",true.into()),("lastContactAt","1970-01-01T00:00:00.000Z".into())]));
    let catalog=IvyJson::object([("namespaces",IvyJson::Array(names.into_iter().map(|namespace|
        IvyJson::object([("namespace",namespace),("tools",IvyJson::Array(vec![IvyJson::object([
            ("inputSchema",IvyJson::object([("description","s".repeat(schema_bytes).into())]))
        ])]))])
    ).collect()))]);
    store.connection().execute("INSERT INTO registries VALUES(?1,?2)",params![id,canonical(&catalog,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
    let diagnostic=IvyJson::object([("source",format!("service:{id}").into()),("status","current".into()),
        ("lastObservedAt","1970-01-01T00:00:00.000Z".into()),("code","fixture".into()),
        ("message","m".repeat(message_bytes).into())]);
    store.connection().execute("INSERT INTO diagnostics VALUES(?1,?2)",params![id,canonical(&diagnostic,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
}

#[test]
fn namespace_pages_keep_names_provider_duplicates_live_status_and_exact_cursors() {
    let store=node_page_store();let registry=Registry::default();
    let a=IvyJson::String(JsString(vec![0xd800]));
    let b=IvyJson::String(JsString(vec![0xd801]));
    insert_namespace_provider(&store,"a\"",true,vec![a.clone(),b.clone(),"same".into(),"same".into(),
        "nul\0tail".into(),"�".into(),"alpha".into(),IvyJson::Null,17.0.into(),
        IvyJson::object([("ignored","value".into())]),"".into(),"😀".into()],10,0);
    insert_namespace_provider(&store,"a\\",false,vec![b.clone(),"alpha".into(),"same".into(),a.clone()],10,0);
    insert_namespace_provider(&store,"nul\0node",true,vec!["same".into(),"second".into()],10,0);
    insert_namespace_provider(&store,"z",true,vec!["same".into(),"last".into()],10,0);
    insert_page_node(&store,"absent",&IvyJson::object([("serviceNodeId","absent".into()),("hostId","host".into())]));
    for (id,source) in [("null","null"),("empty","{}"),("null-names",r#"{"namespaces":null}"#)] {
        insert_page_node(&store,id,&IvyJson::object([("serviceNodeId",id.into()),("hostId","host".into())]));
        store.connection().execute("INSERT INTO registries VALUES(?1,?2)",params![id,source]).unwrap();
    }
    store.connection().execute("UPDATE registries SET registry_json=replace(registry_json,'\"alpha\"','\"\\u0061lpha\"') WHERE node_id=?1",["a\\"]).unwrap();
    for now in [0.0,10_001.0] {
        for limit in [1,2,50] {
            let mut params=IvyJson::object([("limit",(limit as f64).into())]);
            let mut page=compare_namespace_catalog(&store,&registry,&params,now).unwrap();
            let mut count=1;
            while !page.get("nextCursor").unwrap().is_null() {
                params.insert("cursor",page.get("nextCursor").unwrap().clone());
                page=compare_namespace_catalog(&store,&registry,&params,now).unwrap();
                count+=1;assert!(count<20);
            }
            params.insert("cursor","invalid".into());
            assert_eq!(compare_namespace_catalog(&store,&registry,&params,now).unwrap_err().code,"invalid_cursor");
        }
    }
    let all=compare_namespace_catalog(&store,&registry,&IvyJson::object([("limit",50.0.into())]),0.0).unwrap();
    let same=all.get("items").unwrap().as_array().unwrap().iter().find(|record|record.get("namespace")==Some(&"same".into())).unwrap();
    let providers=same.get("providers").unwrap().as_array().unwrap();
    assert_eq!(providers.len(),5);
    assert_eq!(providers[0].get("node").unwrap().get("serviceNodeId"),Some(&"a\"".into()));
    assert_eq!(providers[1],providers[0]);
    assert_eq!(providers[2].get("eligible"),Some(&false.into()));
}

#[test]
fn namespace_pages_keep_provider_response_byte_limits() {
    let store=node_page_store();let registry=Registry::default();
    for index in 0..3 {
        insert_namespace_provider(&store,&format!("node{index}"),true,vec![format!("namespace{index}").into()],1100*1024,0);
    }
    let mut params=IvyJson::object([("limit",50.0.into())]);
    let first=compare_namespace_catalog(&store,&registry,&params,0.0).unwrap();
    assert_eq!(first.get("items").unwrap().as_array().unwrap().len(),1);
    params.insert("cursor",first.get("nextCursor").unwrap().clone());
    let second=compare_namespace_catalog(&store,&registry,&params,0.0).unwrap();
    assert_eq!(second.get("items").unwrap().as_array().unwrap()[0].get("namespace"),Some(&"namespace1".into()));
    store.connection().execute("UPDATE registries SET registry_json=replace(registry_json,'namespace1','namespace0') WHERE node_id='node1'",[]).unwrap();
    params.remove("cursor");
    assert_eq!(compare_namespace_catalog(&store,&registry,&params,0.0).unwrap_err().code,"result_too_large");
}

#[test]
fn namespace_pages_materialize_only_selected_providers_and_skip_schema_payloads() {
    const CHILD: &str="IVY_NAMESPACE_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","kernel::catalogs::read_tests::namespace_pages_materialize_only_selected_providers_and_skip_schema_payloads","--nocapture"])
            .env(CHILD,"1").output().unwrap();
        eprint!("{}",String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));return;
    }
    let store=node_page_store();let registry=Registry::default();
    for index in 0..64 {
        insert_namespace_provider(&store,&format!("node{index:03}"),true,
            (0..4).map(|which|format!("ns{index:03}-{which}").into()).collect(),32*1024,16*1024);
    }
    let params=IvyJson::object([("limit",1.0.into())]);
    let before=LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
    let expected=full_namespace_catalog(&store,&registry,&params,0.0).unwrap();
    let full_peak=PEAK.load(Ordering::Relaxed)-before;
    let before=LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
    let page=registry.namespace_page(&store,&params,&crate::native_runtime::NativeRuntime,0.0).unwrap();
    let paged_peak=PEAK.load(Ordering::Relaxed)-before;
    assert!(page==expected,"Namespace memory-case response differs");
    eprintln!("Hive namespace page Rust allocation peak: full {full_peak} bytes, paged {paged_peak} bytes for 64 providers, 256 names, 2 MiB diagnostic messages and 4 MiB schema text");
    // Rust requests only; SQLite's C heap and process RSS are excluded.
    assert!(full_peak>16*1024*1024);
    assert!(paged_peak<512*1024,"Unselected status/providers/schema payloads were retained");
}

struct TopicRecord {
    provider: IvyJson,
    namespace: IvyJson,
    definition: IvyJson,
}
fn topic_page_records(records: Vec<TopicRecord>) -> IvyResult<(String, Vec<IvyJson>)> {
    let mut providers = Vec::with_capacity(records.len());
    let identity = IvyJson::Array(records.into_iter().map(|record| {
        let id = record.provider.get("node").and_then(|node| node.get("serviceNodeId"))
            .cloned().unwrap_or_else(|| "hive".into());
        providers.push(record.provider);
        IvyJson::object([
            ("serviceNodeId", id), ("namespace", record.namespace), ("definition", record.definition),
        ])
    }).collect());
    let hash = hash_json(&identity)?;
    let records = identity.into_array().ok_or_else(IvyError::internal)?
        .into_iter().zip(providers).map(|(mut record, provider)| IvyJson::object([
            ("provider", provider),
            ("namespace", record.remove("namespace").unwrap_or(IvyJson::Null)),
            ("definition", record.remove("definition").unwrap_or(IvyJson::Null)),
        ])).collect();
    Ok((hash, records))
}
fn full_topics(store: &Store, registry: &Registry, params: &IvyJson, now: f64) -> IvyResult<IvyJson> {
    let resource = params.get("resourceRef");
    let id = params
        .get("serviceNodeId")
        .filter(|v| !v.is_null())
        .or_else(|| resource.and_then(|v| v.get("serviceNodeId")));
    let namespace = params
        .get("namespace")
        .filter(|v| !v.is_null())
        .or_else(|| resource.and_then(|v| v.get("namespace")));
    require(
        !params
            .get("serviceNodeId")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
            || !resource.is_some_and(|v| v.as_object().is_some())
            || params.get("serviceNodeId") == resource.and_then(|v| v.get("serviceNodeId")),
        "target_conflict",
        "Resource and node selectors disagree.",
    )?;
    require(
        !params
            .get("namespace")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
            || !resource.is_some_and(|v| v.as_object().is_some())
            || params.get("namespace") == resource.and_then(|v| v.get("namespace")),
        "target_conflict",
        "Resource and namespace selectors disagree.",
    )?;
    let nodes = if id
        .and_then(IvyJson::as_js_string)
        .is_some_and(|v| !v.0.is_empty())
    {
        vec![
            registry.node(
                store,
                id.and_then(IvyJson::as_js_string)
                    .ok_or_else(IvyError::internal)?,
                &crate::native_runtime::NativeRuntime,
                now,
            )?,
        ]
    } else {
        registry
            .nodes(store, &crate::native_runtime::NativeRuntime, now)?
    };
    if id
        .and_then(IvyJson::as_js_string)
        .is_some_and(|v| !v.0.is_empty())
    {
        let node = &nodes[0];
        require(
            selected(params, "hostId", node.get("hostId"))
                && selected(params, "serviceName", node.get("serviceName")),
            "target_conflict",
            "The selected resource has a different owner.",
        )?;
        if namespace
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        {
            registry.namespace(
                store,
                string(node, "serviceNodeId")?,
                namespace
                    .and_then(IvyJson::as_js_string)
                    .ok_or_else(IvyError::internal)?,
            )?;
        }
    }
    let mut records = vec![];
    for node in nodes.into_iter().filter(|n| {
        selected(params, "hostId", n.get("hostId"))
            && selected(params, "serviceName", n.get("serviceName"))
    }) {
        let selected = namespace.and_then(IvyJson::as_js_string).filter(|name| !name.0.is_empty());
        for ns in registry.topic_namespaces(store, string(&node, "serviceNodeId")?, selected)? {
            for topic in ns.topics {
                if prefixed(&topic, "topic", params.get("topicPrefix"))? {
                    records.push(TopicRecord {
                        provider: provider(&node),
                        namespace: ns.namespace.as_ref().ok_or_else(IvyError::internal)?.clone(),
                        definition: topic,
                    });
                }
            }
        }
    }
    if !id
        .and_then(IvyJson::as_js_string)
        .is_some_and(|v| !v.0.is_empty())
        && !params
            .get("hostId")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        && (!namespace
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
            || is_text(namespace, "hive"))
        && (!params
            .get("serviceName")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
            || is_text(params.get("serviceName"), "hive"))
    {
        let mut statement = sql(
            store,
            store
                .connection()
                .prepare_cached("SELECT definition FROM topics WHERE owner='hive'"),
        )?;
        for row in sql(
            store,
            statement.query_map([], |r| r.get::<_, String>(0)),
        )? {
            let mut topic = decode(&sql(store, row)?)?;
            if !prefixed(&topic, "topic", params.get("topicPrefix"))? {
                continue;
            }
            topic.insert("title", "Object changes".into());
            topic.insert("description","A Hive object is written, moved, archived or deleted according to the selected topic version.".into());
            topic.insert("eventKinds", IvyJson::Array(vec![]));
            records.push(TopicRecord {
                provider: IvyJson::Null,
                namespace: "hive".into(),
                definition: topic,
            });
        }
    }
    let (snapshot, records) = topic_page_records(records)?;
    let mut identity=objects::page_identity("topics.list",params)?;identity.insert("snapshot",snapshot.into());
    crate::paging::catalog_page(
        store,
        records,
        &identity,
        |v| {
            let mut out = v
                .get("provider")
                .and_then(|v| v.get("node"))
                .map(|v| key(v, "serviceNodeId"))
                .unwrap_or_else(|| "hive".into());
            for part in [
                key(v, "namespace"),
                key(v.get("definition").unwrap_or(&IvyJson::Null), "topic"),
                key(v.get("definition").unwrap_or(&IvyJson::Null), "version"),
            ] {
                out.0.push(31);
                out.0.extend(part.0);
            }
            out
        },
        params.get("limit").and_then(IvyJson::as_f64).map(|v|v as usize),
        optional_string(params,"cursor"),
    )
}

fn topic_definition(name: IvyJson, version: IvyJson, schema_bytes: usize) -> IvyJson {
    IvyJson::object([("topic",name),("version",version),("title","Changed".into()),
        ("description","Fixture changes".into()),("eventKinds",IvyJson::Array(vec![])),
        ("payloadSchema",IvyJson::object([("description","s".repeat(schema_bytes).into())]))])
}
fn insert_topic_provider(store: &Store, id: &str, namespaces: Vec<IvyJson>, message_bytes: usize) {
    insert_page_node(store,id,&IvyJson::object([("serviceNodeId",id.into()),("hostId","host".into()),
        ("serviceName","service".into()),("connected",true.into()),("ready",true.into()),
        ("synced",true.into()),("desiredEnabled",true.into()),("lastContactAt","1970-01-01T00:00:00.000Z".into())]));
    let catalog=IvyJson::object([("namespaces",IvyJson::Array(namespaces))]);
    store.connection().execute("INSERT INTO registries VALUES(?1,?2)",params![id,canonical(&catalog,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
    if message_bytes>0 {
        let diagnostic=IvyJson::object([("source",format!("service:{id}").into()),("status","current".into()),
            ("lastObservedAt","1970-01-01T00:00:00.000Z".into()),("message","m".repeat(message_bytes).into())]);
        store.connection().execute("INSERT INTO diagnostics VALUES(?1,?2)",params![id,canonical(&diagnostic,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
    }
}
fn topic_namespace(name: IvyJson, topics: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([("namespace",name),("topics",IvyJson::Array(topics))])
}
fn actual_topics(store: &Store, registry: &Registry, params: &IvyJson, now: f64) -> IvyResult<IvyJson> {
    topic_catalog::list(store,registry,params,&crate::native_runtime::NativeRuntime,now)
}
fn compare_topics(store: &Store, registry: &Registry, params: &IvyJson, now: f64) -> IvyResult<IvyJson> {
    let expected=full_topics(store,registry,params,now).map_err(|e|e.to_wire());
    let actual=actual_topics(store,registry,params,now);
    assert!(actual.as_ref().map_err(IvyError::to_wire)==expected.as_ref().map_err(Clone::clone),
        "Topic page differs; actual error {:?}, expected error {:?}",actual.as_ref().err().map(|e|&e.code),
        expected.as_ref().err().and_then(|e|e.get("code")));
    actual
}
#[test]
fn topic_pages_keep_filters_snapshot_hashes_exact_cursors_and_stable_utf16_keys() {
    let store=node_page_store();let registry=Registry::default();
    let lone=IvyJson::String(JsString(vec![0xd800]));
    let names=vec!["alpha".into(),lone.clone(),IvyJson::String(JsString(vec![0xd801])),"�".into(),
        "nul\0tail".into(),"😀".into(),IvyJson::String(JsString(vec![0xd83d]))];
    for id in ["alpha","beta","hive"] {
        insert_topic_provider(&store,id,vec![
            topic_namespace("fixture".into(),names.iter().map(|name|topic_definition(name.clone(),"1.0.0".into(),0)).collect()),
            topic_namespace(lone.clone(),vec![topic_definition("same".into(),"1.0.0".into(),0)]),
            topic_namespace("fixture".into(),vec![topic_definition("alpha".into(),"1.0.0".into(),0)]),
            topic_namespace("nul\0tail".into(),vec![topic_definition("last".into(),"1.0.0".into(),0)]),
        ],1024);
    }
    let builtin=topic_definition("changed".into(),"1.0.0".into(),0);
    store.connection().execute("INSERT INTO topics VALUES('changed','1.0.0','hive',?)",
        [canonical(&builtin,MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
    store.connection().execute("UPDATE service_nodes SET status_json=json_set(status_json,'$.hostId','other','$.serviceName','other') WHERE id='beta'",[]).unwrap();
    store.connection().execute("UPDATE registries SET registry_json=replace(registry_json,'fixture','\\u0066ixture') WHERE node_id='hive'",[]).unwrap();
    let filters=vec![
        IvyJson::object([] as [(&str,IvyJson);0]),IvyJson::object([("namespace","fixture".into())]),
        IvyJson::object([("namespace",lone)]),IvyJson::object([("namespace","nul\0tail".into())]),
        IvyJson::object([("namespace","absent".into())]),IvyJson::object([("namespace","hive".into())]),
        IvyJson::object([("hostId","host".into())]),IvyJson::object([("serviceName","other".into())]),
        IvyJson::object([("serviceName","hive".into())]),IvyJson::object([("topicPrefix","al".into())]),
        IvyJson::object([("topicPrefix",IvyJson::String(JsString(vec![0xd83d])))]),
        IvyJson::object([("serviceNodeId","alpha".into()),("namespace","fixture".into())]),
        IvyJson::object([("resourceRef",IvyJson::object([("serviceNodeId","beta".into()),("namespace","fixture".into())]))]),
        IvyJson::object([("serviceNodeId","".into()),("namespace","".into()),("hostId","".into()),("topicPrefix","".into())]),
    ];
    for now in [0.0,10_001.0] {for filter in &filters {for limit in [1,3,50] {
        let mut params=filter.clone();params.insert("limit",(limit as f64).into());
        let mut pages=0;
        loop {
            let page=compare_topics(&store,&registry,&params,now).unwrap();
            pages+=1;assert!(pages<50);
            let cursor=page.get("nextCursor").unwrap();if cursor.is_null(){break;}
            params.insert("cursor",cursor.clone());
        }
        params.insert("cursor","invalid".into());
        assert_eq!(compare_topics(&store,&registry,&params,now).unwrap_err().code,"invalid_cursor");
    }}}
    for params in [
        IvyJson::object([("serviceNodeId","absent".into())]),
        IvyJson::object([("serviceNodeId","alpha".into()),("hostId","other".into())]),
        IvyJson::object([("serviceNodeId","alpha".into()),("namespace","absent".into())]),
        IvyJson::object([("namespace","other".into()),("resourceRef",IvyJson::object([("namespace","fixture".into())]))]),
        IvyJson::object([("serviceNodeId","alpha".into()),("resourceRef",IvyJson::object([("serviceNodeId","beta".into())]))]),
    ] {compare_topics(&store,&registry,&params,0.0).unwrap_err();}
    let mut params=IvyJson::object([("limit",1.0.into())]);
    let first=compare_topics(&store,&registry,&params,0.0).unwrap();
    params.insert("cursor",first.get("nextCursor").unwrap().clone());
    store.connection().execute("UPDATE registries SET registry_json=replace(registry_json,'Fixture changes','Changed description') WHERE node_id='beta'",[]).unwrap();
    assert_eq!(compare_topics(&store,&registry,&params,0.0).unwrap_err().code,"invalid_cursor");
}
#[test]
fn topic_pages_keep_response_bounds_and_catalog_error_priority() {
    for (topic_bytes,message_bytes) in [(600*1024,600*1024),(1100*1024,1100*1024)] {
        let store=node_page_store();let registry=Registry::default();
        insert_topic_provider(&store,"node",vec![topic_namespace("fixture".into(),(0..3)
            .map(|i|topic_definition(format!("changed{i}").into(),"1.0.0".into(),topic_bytes)).collect())],message_bytes);
        let result=compare_topics(&store,&registry,&IvyJson::object([("limit",3.0.into()),("serviceNodeId","node".into())]),0.0);
        if topic_bytes==600*1024 {assert_eq!(result.unwrap().get("items").unwrap().as_array().unwrap().len(),1);}
        else {assert_eq!(result.unwrap_err().code,"result_too_large");}
    }
    for source in ["null","{}","{\"namespaces\":null}","{\"namespaces\":[]}","[","{\"namespaces\":true}",
        "{\"namespaces\":[null,false,1,\"skip\",{\"namespace\":\"fixture\",\"topics\":null}]}",
        "{\"namespaces\":[{\"namespace\":\"fixture\",\"topics\":{}}]}",
        "{\"namespaces\":[{\"topics\":[{\"topic\":\"changed\",\"version\":\"1.0.0\"}]}]}",
        "{\"namespaces\":[{\"namespace\":null,\"topics\":[true]}]}"] {
        let store=node_page_store();let registry=Registry::default();
        insert_topic_provider(&store,"node",vec![],0);
        store.connection().execute("UPDATE registries SET registry_json=? WHERE node_id='node'",[source]).unwrap();
        for prefix in [None,Some("changed")] {
            let mut params=IvyJson::object([("cursor","invalid".into())]);
            if let Some(prefix)=prefix {params.insert("topicPrefix",prefix.into());}
            compare_topics(&store,&registry,&params,0.0).unwrap_err();
        }
    }
}
#[test]
fn topic_pages_materialize_selected_providers_and_stream_snapshot_definitions() {
    const CHILD:&str="IVY_TOPIC_CATALOG_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none(){
        let result=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","kernel::catalogs::read_tests::topic_pages_materialize_selected_providers_and_stream_snapshot_definitions","--nocapture"])
            .env(CHILD,"1").output().unwrap();
        eprint!("{}",String::from_utf8_lossy(&result.stderr));assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stdout));
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed;"));return;
    }
    let store=node_page_store();let registry=Registry::default();
    for i in 0..64 {insert_topic_provider(&store,&format!("node{i:03}"),vec![topic_namespace("fixture".into(),
        (0..4).map(|j|topic_definition(format!("changed{j}").into(),"1.0.0".into(),8192)).collect())],32*1024);}
    let params=IvyJson::object([("limit",1.0.into()),("namespace","fixture".into())]);
    let before=LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
    let full=full_topics(&store,&registry,&params,0.0).unwrap();let full_peak=PEAK.load(Ordering::Relaxed)-before;
    drop(full);
    let before=LIVE.load(Ordering::Relaxed);PEAK.store(before,Ordering::Relaxed);
    let paged=actual_topics(&store,&registry,&params,0.0).unwrap();let peak=PEAK.load(Ordering::Relaxed)-before;
    assert_eq!(paged.get("items").unwrap().as_array().unwrap().len(),1);
    eprintln!("Hive topic catalog Rust allocation peak: full {full_peak} bytes, paged {peak} bytes for 64 providers, 256 topics, 2 MiB diagnostics and 2 MiB schema text");
    assert!(full_peak>16*1024*1024);assert!(peak<1024*1024,"All topic/provider bodies retained");
}
