use super::*;
use crate::retention::graph_tests::{LIVE, PEAK};
use std::{path::Path, sync::atomic::Ordering as AtomicOrdering};
use crate::native_runtime::NativeRuntime;

fn namespace_store(source: &str) -> Store {
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(
        r#"INSERT INTO principals VALUES('fixture');
           INSERT INTO service_nodes VALUES('fixture','fixture','host','service',1,
             '{"serviceNodeId":"fixture","hostId":"host","serviceName":"service","connected":true,"synced":true,"ready":true,"desiredEnabled":true,"lastContactAt":"1970-01-01T00:00:00.000Z"}');"#,
    ).unwrap();
    store.connection().execute("INSERT INTO registries VALUES('fixture',?)", [source]).unwrap();
    store
}

fn full_registry_live_catalog(owner: &Registry, store: &Store, id: &JsString) -> IvyResult<Option<Catalog>> {
    let Some(registry) = owner.get_registry(store, id)? else { return Ok(None) };
    let mut bindings = HashMap::new();
    for ns in array(&registry, "namespaces")? {
        let name = string(ns, "namespace")?.clone();
        let values = array(ns, "tools")?.iter().map(|tool| Ok(ToolBinding::new(
            join([string(tool, "namespace")?.clone(), ".".into(), string(tool, "name")?.clone()]),
            ivy_contracts::hash::tool_definition_hash(tool)?.into(), tool.clone(),
        ))).collect::<IvyResult<Vec<_>>>()?;
        bindings.insert(name, values);
    }
    let mut result = Catalog { namespaces: Vec::new(), tools: Vec::new(), notifications: Vec::new() };
    for ns in array(&registry, "namespaces")? {
        let name = string(ns, "namespace")?.clone();
        for notification in optional_array(ns, "notifications")? {
            let text = |key| notification.required(key)?.as_js_string().cloned().ok_or_else(IvyError::internal);
            result.notifications.push(crate::registry_catalog::NotificationDefinition {
                namespace: name.clone(), name: text("name")?,
                version: text("version")?, description: text("description")?,
                payload_schema: Arc::new(notification.required("payloadSchema")?.clone()),
            });
        }
        let start = result.tools.len();
        result.tools.extend(bindings.remove(&name).unwrap_or_default());
        result.namespaces.push(crate::registry_catalog::Namespace {
            name, tools: start..result.tools.len(),
            guide_markdown: ns.get("guideMarkdown").and_then(IvyJson::as_js_string).cloned(),
        });
    }
    Ok(Some(result))
}

#[test]
fn cold_catalog_projection_preserves_definitions_ranges_and_error_order() {
    let store = namespace_store("null");
    let id = JsString::from("fixture");
    for source in ["null", r#"{"namespaces":[]}"#,
        r#"{"namespaces":[{"namespace":"\ud800","guideMarkdown":"\ud801","tools":[
          {"namespace":"\ud800","name":"first","before":17,"inputSchema":{"\ud802":"\ud803"},"outputSchema":false,"discovery":{"unknown":true}},
          {"namespace":"\ud800","name":"second","inputSchema":true,"outputSchema":null}],
          "notifications":[{"name":"\ud804","version":"1.0.0","description":"\ud805","payloadSchema":{"unknown":[true,9007199254740992]}}]},
          {"namespace":"empty","guideMarkdown":{"ignored":true},"tools":[],"notifications":null}]}"#,
        r#"{"namespaces":[{"namespace":"same","tools":[{"namespace":"same","name":"first"}]},
          {"namespace":"same","tools":[{"namespace":"same","name":"last","inputSchema":false}],"notifications":[]}]}"#,
        "[]", "[[]]", "{}", "\u{2000}null", r#"{"namespaces":null}"#,
        r#"{"namespaces":[null]}"#, r#"{"namespaces":[[]]}"#,
        r#"{"namespaces":[{"tools":[]}]}"#, r#"{"namespaces":[{"namespace":17,"tools":[]}]}"#,
        r#"{"namespaces":[{"namespace":"fixture","tools":null}]}"#,
        r#"{"namespaces":[{"namespace":"fixture","tools":[null]}]}"#,
        r#"{"namespaces":[{"namespace":"fixture","tools":[{"name":"x"}]}]}"#,
        r#"{"namespaces":[{"namespace":"fixture","tools":[],"notifications":{}}]}"#,
        r#"{"namespaces":[{"namespace":"fixture","tools":[],"notifications":[{}]}]}"#,
        r#"{"namespaces":[{"namespace":"fixture","tools":[],"notifications":[{"name":"x","version":"1.0.0","description":"x"}]}]}"#,
        r#"{"namespaces":[{"namespace":"early","tools":[],"notifications":[{}]},
          {"namespace":"later","tools":[{"namespace":"later","name":"bad","inputSchema":9007199254740992}]}]}"#,
        r#"{"namespaces":[]} trailing"#] {
        store.connection().execute("UPDATE registries SET registry_json=?", [source]).unwrap();
        let expected = full_registry_live_catalog(&Registry::default(), &store, &id).map_err(|e| e.to_wire());
        let actual = Registry::default().live_catalog(&store, &id).map_err(|e| e.to_wire());
        assert_eq!(actual.as_ref().map(|v| v.as_deref()), expected.as_ref().map(|v| v.as_ref()), "{source}");
    }
    store.connection().execute("DELETE FROM registries", []).unwrap();
    assert!(Registry::default().live_catalog(&store, &id).unwrap().is_none());
}

#[test]
fn cold_catalog_projection_reduces_peak_without_increasing_retained_schema_capacity() {
    const CHILD: &str = "IVY_COLD_CATALOG_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::catalog_memory_tests::cold_catalog_projection_reduces_peak_without_increasing_retained_schema_capacity", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let schema_bytes = 48 * 1024 + 1;
    let source = serde_json::json!({"namespaces":[{"namespace":"fixture","guideMarkdown":"Fixture guide.",
        "tools":(0..4).map(|i| serde_json::json!({"namespace":"fixture","name":format!("call{i}"),
            "inputSchema":{"description":"s".repeat(schema_bytes)},"outputSchema":{"description":"s".repeat(schema_bytes)}})).collect::<Vec<_>>(),
        "notifications":(0..4).map(|i| serde_json::json!({"name":format!("changed{i}"),"version":"1.0.0","description":"Fixture notification.",
            "payloadSchema":{"description":"s".repeat(schema_bytes*2)}})).collect::<Vec<_>>(),
        "topics":[{"payloadSchema":{"description":"s".repeat(1024*1024)}}]}],
        "contracts":[{"description":"s".repeat(1024*1024)}]}).to_string();
    let store = namespace_store(&source);
    drop(source);
    let id = JsString::from("fixture");
    let mut owner = Registry::default();
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let old = full_registry_live_catalog(&owner, &store, &id).unwrap().unwrap();
    let full_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    let full_retained = old.retained_bytes();
    drop(old);
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let catalog = owner.live_catalog(&store, &id).unwrap().unwrap();
    let projected_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    let projected_retained = catalog.retained_bytes();
    assert_eq!(catalog.tools.len(), 4);assert_eq!(catalog.notifications.len(), 4);
    assert!(Arc::ptr_eq(&catalog, &owner.live_catalog(&store, &id).unwrap().unwrap()));
    eprintln!("Hive cold catalog Rust allocation peak: full {full_peak} bytes, projected {projected_peak} bytes; retained full {full_retained} bytes, projected {projected_retained} bytes for 786448 bytes of retained schema text and 2 MiB unrelated text");
    // Rust allocation requests only; SQLite's C heap and process RSS are excluded.
    assert!(full_peak > 8 * 1024 * 1024);
    assert!(projected_peak + 4 * 1024 * 1024 < full_peak, "Cold catalog copied or decoded unrelated schemas");
    assert!(projected_retained <= full_retained + 1024, "Moved parser buffers retained excess capacity");
}

fn full_registry_mcp_names(owner: &Registry, store: &Store, id: &JsString) -> IvyResult<Vec<JsString>> {
    let mut names = Vec::new();
    if let Some(registry) = owner.get_registry(store, id)? {
        for ns in array(&registry, "namespaces")? {
            for tool in array(ns, "tools")? {
                require_object(tool)?;
                if let Some(name) = tool.get("discovery").and_then(|v| v.get("mcp"))
                    .and_then(|v| v.get("name")).and_then(IvyJson::as_js_string)
                    .filter(|v| !v.0.is_empty()) {
                    names.push(name.clone());
                }
            }
        }
    }
    Ok(names)
}

#[test]
fn mcp_name_projection_preserves_names_optional_metadata_and_catalog_errors() {
    let store = namespace_store("null");
    let owner = Registry::default();
    let id = JsString::from("fixture");
    for source in ["null", " \t\r\nnull ", r#"{"namespaces":[]}"#,
        r#"{"namespaces":[{"tools":[{},
          {"discovery":null},{"discovery":[]},{"discovery":"ignored"},{"discovery":17},
          {"discovery":{}},{"discovery":{"mcp":null}},{"discovery":{"mcp":[]}},
          {"discovery":{"mcp":17}},{"discovery":{"mcp":{}}},
          {"discovery":{"mcp":{"name":null}}},{"discovery":{"mcp":{"name":false}}},
          {"discovery":{"mcp":{"name":[]}}},{"discovery":{"mcp":{"name":""}}},
          {"discovery":{"mcp":{"name":"fixture_read"}},"inputSchema":{"unknown":"\ud802"}},
          {"discovery":{"mcp":{"name":"fixture_read"}},"description":"\ud803"},
          {"discovery":{"mcp":{"name":"\ud800"}}}
        ]},{"namespace":null,"tools":[{"discovery":{"mcp":{"name":"second"}}}]}]}"#,
        "[]", "[[]]", "1", "{}", "\u{2000}null", r#"{"namespaces":null}"#,
        r#"{"namespaces":{}}"#, r#"{"namespaces":[null]}"#,
        r#"{"namespaces":[[]]}"#, r#"{"namespaces":[{}]}"#,
        r#"{"namespaces":[{"tools":null}]}"#, r#"{"namespaces":[{"tools":{}}]}"#,
        r#"{"namespaces":[{"tools":[null]}]}"#, r#"{"namespaces":[{"tools":[[]]}]}"#,
        r#"{"namespaces":[{"tools":[1]}]}"#, r#"{"namespaces":[]} trailing"#] {
        store.connection().execute("UPDATE registries SET registry_json=?", [source]).unwrap();
        let expected = full_registry_mcp_names(&owner, &store, &id).map_err(|e| e.code);
        assert_eq!(owner.mcp_names(&store, &id).map_err(|e| e.code), expected, "{source}");
    }
    store.connection().execute("DELETE FROM registries", []).unwrap();
    assert!(owner.mcp_names(&store, &id).unwrap().is_empty());
}

#[test]
fn mcp_name_projection_does_not_materialize_unrelated_definitions() {
    const CHILD: &str = "IVY_MCP_NAMES_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::catalog_memory_tests::mcp_name_projection_does_not_materialize_unrelated_definitions", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let source = serde_json::json!({"namespaces":[{"tools":[{
        "discovery":{"mcp":{"name":"fixture_read"}},
        "inputSchema":{"description":"s".repeat(2*1024*1024)},
        "outputSchema":false
    }],"topics":[{"payloadSchema":{"description":"s".repeat(1024*1024)}}]}]}).to_string();
    let store = namespace_store(&source);
    drop(source);
    let owner = Registry::default();
    let id = JsString::from("fixture");
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let expected = full_registry_mcp_names(&owner, &store, &id).unwrap();
    let full_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    for _ in 0..8 { assert_eq!(owner.mcp_names(&store, &id).unwrap(), expected); }
    let projected_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    eprintln!("Hive MCP name projection Rust allocation peak: full {full_peak} bytes, projected {projected_peak} bytes across eight reads with 3 MiB unrelated schema text");
    // Rust allocator requests; excludes SQLite's C heap and process RSS.
    assert!(full_peak > 8 * 1024 * 1024);
    assert!(projected_peak < 32 * 1024, "MCP name selection decoded definitions: {projected_peak}");
}

#[test]
fn namespace_names_projection_only_allocates_names() {
    const CHILD: &str = "IVY_NAMESPACE_NAMES_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::catalog_memory_tests::namespace_names_projection_only_allocates_names", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let source = serde_json::json!({"namespaces":[
        {"namespace":"fixture", "tools":[{"inputSchema":{"description":"s".repeat(2 * 1024 * 1024)}}]},
        {"namespace":"other", "tools":[]}
    ]}).to_string();
    let store = namespace_store(&source);
    drop(source);
    let owner = Registry::default();
    let id = JsString::from("fixture");
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let full = owner.get_registry(&store, &id).unwrap().unwrap();
    let expected: Vec<_> = full.required("namespaces").unwrap().as_array().unwrap().iter()
        .map(|ns| ns.required("namespace").unwrap().clone()).collect();
    let full_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    drop(full);
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    for _ in 0..8 {
        assert_eq!(owner.namespace_names(&store, &id).unwrap(), expected);
    }
    let projected_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    eprintln!("Hive namespace names Rust allocation peak: full registry {full_peak} bytes, projected {projected_peak} bytes across eight reads with 2 MiB opaque schema text");
    // Rust allocator requests only; SQLite's native heap and process RSS are excluded.
    assert!(full_peak > 2 * 1024 * 1024);
    assert!(projected_peak < 128 * 1024, "Namespace list decoded definitions: {projected_peak}");
}

#[test]
fn namespace_names_preserve_values_and_malformed_catalog_errors() {
    let store = namespace_store("null");
    let owner = Registry::default();
    let id = JsString::from("fixture");
    for source in ["null", "{}", r#"{"namespaces":null}"#, r#"{"namespaces":[]}"#,
        r#"{"namespaces":[{"namespace":"fixture","tools":[{"description":"\ud801"}]},{"namespace":"\ud800"},{"namespace":null},{"namespace":17},{"namespace":{"field":"value"}}]}"#] {
        store.connection().execute("UPDATE registries SET registry_json=?", [source]).unwrap();
        let full = owner.get_registry(&store, &id).unwrap();
        let expected: Vec<_> = full.as_ref().and_then(|v| v.get("namespaces"))
            .filter(|v| !v.is_null()).map(|v| v.as_array().unwrap()).unwrap_or(&[])
            .iter().map(|ns| ns.required("namespace").unwrap().clone()).collect();
        assert_eq!(owner.namespace_names(&store, &id).unwrap(), expected);
    }
    for source in ["[]", "[[]]", "1", "\u{2000}null", r#"{"namespaces":{}}"#,
        r#"{"namespaces":[1]}"#, r#"{"namespaces":[{}]}"#, r#"{"namespaces":[]} trailing"#] {
        store.connection().execute("UPDATE registries SET registry_json=?", [source]).unwrap();
        assert_eq!(owner.namespace_names(&store, &id).unwrap_err().code, "internal_error", "{source}");
    }
    store.connection().execute("DELETE FROM registries", []).unwrap();
    assert!(owner.namespace_names(&store, &id).unwrap().is_empty());
}

#[test]
fn topic_projection_preserves_namespace_presence_filters_and_errors() {
    let store = namespace_store("null");
    let owner = Registry::default();
    let id = JsString::from("fixture");
    for source in ["null", "{}", r#"{"namespaces":null}"#, r#"{"namespaces":[]}"#,
        r#"{"namespaces":[null,[],7,"name",{},
          {"topics":[{"topic":"missing"}]},
          {"namespace":null,"topics":[{"topic":"null-name"}]},
          {"namespace":"fixture","topics":[{"topic":"value","payloadSchema":{"unknown":"\ud800"}}]},
          {"namespace":"\ud801","topics":[true,null]}]}"#] {
        store.connection().execute("UPDATE registries SET registry_json=?", [source]).unwrap();
        let full = owner.get_registry(&store, &id).unwrap();
        let expected: Vec<_> = full.as_ref().and_then(|v| v.get("namespaces"))
            .and_then(IvyJson::as_array).into_iter().flatten().flat_map(|ns| {
                ns.get("topics").and_then(IvyJson::as_array).into_iter().flatten()
                    .map(|topic| (ns.get("namespace").cloned(), topic.clone()))
            }).collect();
        let actual: Vec<_> = owner.topic_namespaces(&store, &id, None).unwrap().into_iter()
            .flat_map(|ns| ns.topics.into_iter().map(move |topic| (ns.namespace.clone(), topic))).collect();
        assert_eq!(actual, expected, "{source}");
    }
    let source = r#"{"namespaces":[{"namespace":"other","topics":17},
      {"namespace":"fixture","topics":[{"topic":"selected"}]}]}"#;
    store.connection().execute("UPDATE registries SET registry_json=?", [source]).unwrap();
    let selected = owner.topic_namespaces(&store, &id, Some(&"fixture".into())).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].topics[0].get("topic"), Some(&"selected".into()));
    assert_eq!(owner.topic_namespaces(&store, &id, None).err().unwrap().code, "internal_error");
    for source in ["[]", "1", "\u{2000}null", r#"{"namespaces":{}}"#,
        r#"{"namespaces":[{"topics":{}}]}"#, r#"{"namespaces":[]} trailing"#] {
        store.connection().execute("UPDATE registries SET registry_json=?", [source]).unwrap();
        assert_eq!(owner.topic_namespaces(&store, &id, None).err().unwrap().code, "internal_error", "{source}");
    }
    store.connection().execute("DELETE FROM registries", []).unwrap();
    assert!(owner.topic_namespaces(&store, &id, None).unwrap().is_empty());
}

#[test]
fn topic_projection_does_not_decode_tool_or_unselected_topic_schemas() {
    const CHILD: &str = "IVY_TOPIC_PROJECTION_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::catalog_memory_tests::topic_projection_does_not_decode_tool_or_unselected_topic_schemas", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let source = serde_json::json!({"namespaces":[
        {"namespace":"other","topics":[{"topic":"other","payloadSchema":{"description":"s".repeat(1024*1024)}}]},
        {"namespace":"fixture","tools":[{"inputSchema":{"description":"s".repeat(2*1024*1024)}}],
         "topics":[{"topic":"selected","payloadSchema":{"type":"object"}}]}
    ]}).to_string();
    let store = namespace_store(&source);
    drop(source);
    let owner = Registry::default();
    let id = JsString::from("fixture");
    let namespace = JsString::from("fixture");
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let full = owner.get_registry(&store, &id).unwrap().unwrap();
    let full_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    drop(full);
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    for _ in 0..4 {
        let selected = owner.topic_namespaces(&store, &id, Some(&namespace)).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].topics.len(), 1);
    }
    let projected_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    eprintln!("Hive topic projection Rust allocation peak: full {full_peak} bytes, projected {projected_peak} bytes with 3 MiB unneeded schema text");
    assert!(full_peak > 6 * 1024 * 1024);
    assert!(projected_peak < 128 * 1024, "Topic reads decoded unneeded schemas: {projected_peak}");
}

#[test]
fn namespace_selection_and_summary_do_not_materialize_definitions() {
    const CHILD: &str = "IVY_NAMESPACE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::catalog_memory_tests::namespace_selection_and_summary_do_not_materialize_definitions", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let source = {
        let schema = serde_json::json!({"type":"object", "description":"s".repeat(128 * 1024)});
        let namespaces: Vec<_> = ["other", "fixture", "tiny"].into_iter().map(|namespace| {
            let schema = if namespace == "tiny" { serde_json::json!(true) } else { schema.clone() };
            let tools: Vec<_> = (0..8).map(|index| serde_json::json!({
                "namespace":namespace, "name":format!("call{index}"), "description":"Registered fixture tool.",
                "inputSchema":schema, "outputSchema":schema,
            })).collect();
            serde_json::json!({"namespace":namespace, "description":"Fixture description.",
                "guideMarkdown":"Fixture guide.", "tools":tools, "topics":[]})
        }).collect();
        serde_json::json!({"namespaces":namespaces}).to_string()
    };
    let store = namespace_store(&source);
    drop(source);
    let owner = Registry::default();
    let id = JsString::from("fixture");
    let namespace = JsString::from("fixture");
    let tiny = JsString::from("tiny");
    let target = IvyJson::object([("serviceNodeId", id.clone().into())]);
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let started = std::time::Instant::now();
    for _ in 0..8 {
        let selected = owner.select(&store, &namespace, &target, &NativeRuntime, 0.0).unwrap();
        assert_eq!(selected.get("serviceNodeId"), Some(&id.clone().into()));
        let summary = owner.snapshot(&store, &namespace, &IvyJson::object([] as [(&str, IvyJson); 0]), &NativeRuntime, 0.0).unwrap();
        assert_eq!(summary.get("toolCount"), Some(&8.0.into()));
        assert_eq!(summary.get("topicCount"), Some(&0.0.into()));
        assert_eq!(summary.get("description"), Some(&"Fixture description.".into()));
        let selected = owner.namespace(&store, &id, &tiny).unwrap();
        assert_eq!(selected.required("tools").unwrap().as_array().unwrap().len(), 8);
    }
    let peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    eprintln!("Hive namespace selection/summary Rust allocation peak {peak} bytes across 24 reads with 4 MiB opaque schema text; elapsed {} microseconds", started.elapsed().as_micros());
    // Rust allocator requests only; SQLite's native heap and process RSS are excluded.
    assert!(peak < 128 * 1024, "Namespace metadata reads decoded unneeded definitions: {peak} bytes");
}

#[test]
fn namespace_projection_preserves_utf16_metadata_and_lookup_errors() {
    let source = r#"{"namespaces":[
        {"namespace":"fixture","description":"Description \ud800","guideMarkdown":"Guide \ud801","tools":[{},{}],"topics":[{}]},
        {"namespace":"\ud802","description":null,"guideMarkdown":17,"tools":[],"topics":[]}
    ]}"#;
    let store = namespace_store(source);
    let owner = Registry::default();
    let id = JsString::from("fixture");
    let target = IvyJson::object([("serviceNodeId", id.clone().into())]);
    let summary = owner.snapshot(&store, &"fixture".into(), &target, &NativeRuntime, 0.0).unwrap();
    assert_eq!(summary.required("description").unwrap().as_js_string().unwrap().0.last(), Some(&0xd800));
    assert_eq!(summary.required("guideMarkdown").unwrap().as_js_string().unwrap().0.last(), Some(&0xd801));
    assert_eq!(summary.get("toolCount"), Some(&2.0.into()));
    assert_eq!(summary.get("topicCount"), Some(&1.0.into()));
    let invalid_utf16 = JsString(vec![0xd802]);
    let summary = owner.snapshot(&store, &invalid_utf16, &target, &NativeRuntime, 0.0).unwrap();
    assert_eq!(summary.get("description"), Some(&IvyJson::Null));
    assert_eq!(summary.get("guideMarkdown"), Some(&17.0.into()));
    assert_eq!(owner.namespace(&store, &id, &invalid_utf16).unwrap().get("namespace"), Some(&invalid_utf16.into()));
    assert_eq!(owner.namespace(&store, &id, &"absent".into()).unwrap_err().code, "not_found");
    for source in ["null", r#"{"namespaces":[]}"#] {
        store.connection().execute("UPDATE registries SET registry_json=?", [source]).unwrap();
        assert_eq!(owner.namespace(&store, &id, &"fixture".into()).unwrap_err().code, "not_found");
    }
    for source in ["[]", "[[]]", "{}", "\u{2000}null", r#"{"namespaces":null}"#, r#"{"namespaces":[1]}"#, r#"{"namespaces":[{"namespace":"fixture","description":"x","guideMarkdown":"x","tools":null,"topics":[] }]}"#] {
        store.connection().execute("UPDATE registries SET registry_json=?", [source]).unwrap();
        assert_eq!(owner.snapshot(&store, &"fixture".into(), &target, &NativeRuntime, 0.0).unwrap_err().code, "internal_error");
    }
    store.connection().execute("DELETE FROM registries", []).unwrap();
    assert_eq!(owner.namespace(&store, &id, &"fixture".into()).unwrap_err().code, "not_found");
}

#[test]
fn repeated_routing_handoff_shares_the_registered_catalog() {
    const CHILD: &str = "IVY_REGISTRY_CATALOG_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::catalog_memory_tests::repeated_routing_handoff_shares_the_registered_catalog", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(
        "INSERT INTO principals VALUES('fixture');
         INSERT INTO service_nodes VALUES('fixture','fixture','host','service',1,'{}');",
    ).unwrap();
    let schema = IvyJson::object([
        ("type", "object".into()),
        ("description", "s".repeat(128 * 1024).into()),
    ]);
    let tools = (0..8).map(|index| IvyJson::object([
        ("namespace", "fixture".into()),
        ("name", format!("call{index}").into()),
        ("interfaceVersion", "1.0.0".into()),
        ("description", "Registered fixture tool.".into()),
        ("inputSchema", schema.clone()),
        ("outputSchema", schema.clone()),
    ])).collect();
    let registry = IvyJson::object([("namespaces", IvyJson::Array(vec![IvyJson::object([
        ("namespace", "fixture".into()),
        ("guideMarkdown", "Fixture tool guide.".into()),
        ("tools", IvyJson::Array(tools)),
    ])]))]);
    store.connection().execute("INSERT INTO registries VALUES('fixture',?)", [encoded(&registry).unwrap()]).unwrap();
    let mut owner = Registry::default();
    drop(owner.live_catalog(&store, &"fixture".into()).unwrap());
    let id = JsString::from("fixture");
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let handoffs: Vec<_> = (0..16).map(|_| owner.live_catalog(&store, &id).unwrap().unwrap()).collect();
    let peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    eprintln!("Hive registered-catalog Rust allocation peak above baseline: {peak} bytes for 16 handoffs of eight tools with 256 KiB schema text each");
    assert_eq!(handoffs.len(), 16);
    // Rust allocation requests only; SQLite's native heap and process RSS are excluded.
    assert!(peak < 64 * 1024, "Routing handoffs copied registered definitions: {peak} bytes");
}

#[test]
fn routing_status_projection_does_not_retain_unneeded_node_details() {
    const CHILD: &str = "IVY_ROUTING_STATUS_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::catalog_memory_tests::routing_status_projection_does_not_retain_unneeded_node_details", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let mut node = IvyJson::object([
        ("serviceNodeId", "fixture".into()),
        ("hostId", "host".into()),
        ("serviceName", "service".into()),
        ("principalId", "principal".into()),
        ("connected", true.into()),
        ("synced", true.into()),
        ("ready", true.into()),
        ("desiredEnabled", true.into()),
        ("diagnostic", IvyJson::object([("message", "d".repeat(1024 * 1024).into())])),
    ]);
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let full_nodes: Vec<_> = (0..32).map(|_| node.clone()).collect();
    std::hint::black_box(&full_nodes);
    let full_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    drop(full_nodes);

    let mut routing = crate::live_routing::LiveRouting::new(false, None);
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    for round in 0..4 {
        for index in 0..32 {
            node.insert("serviceNodeId", format!("fixture-{index}").into());
            routing.update(&node, (round == 0).then_some(1.0), None).unwrap();
            routing.caller(&IvyJson::object([
                ("serviceNodeId", format!("fixture-{index}").into()),
                ("generation", 1.0.into()),
            ])).unwrap();
        }
    }
    let projected_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    assert_eq!(node.required("diagnostic").unwrap().required("message").unwrap()
        .as_js_string().unwrap().utf16_len(), 1024 * 1024);
    eprintln!("Hive routing status Rust allocation peak: full nodes {full_peak} bytes, native projection {projected_peak} bytes for 32 routes and four updates each with 1 MiB diagnostic text");
    // Rust allocation requests only; the admitted status remains owned by its caller.
    assert!(full_peak > 64 * 1024 * 1024);
    assert!(projected_peak < 128 * 1024, "Routing retained unneeded status bodies: {projected_peak}");
}

#[test]
fn tool_definition_hash_borrows_schemas_without_a_deep_copy() {
    const CHILD: &str = "IVY_TOOL_HASH_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::catalog_memory_tests::tool_definition_hash_borrows_schemas_without_a_deep_copy", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let definition = IvyJson::object([
        ("name", "fixture".into()),
        ("inputSchema", IvyJson::object([("description", "i".repeat(512 * 1024).into())])),
        ("outputSchema", IvyJson::object([("description", "o".repeat(512 * 1024).into())])),
        ("discovery", IvyJson::object([("description", "d".repeat(2 * 1024 * 1024).into())])),
    ]);
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let copied = IvyJson::object(definition.as_object().unwrap().iter()
        .filter(|(key, _)| !key.eq_str("discovery")).cloned());
    let expected = ivy_contracts::hash::hash_json(&copied).unwrap();
    let copied_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    drop(copied);
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let actual = ivy_contracts::hash::tool_definition_hash(&definition).unwrap();
    let borrowed_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    assert_eq!(actual, expected);
    eprintln!("Hive tool-definition hash Rust allocation peak: copied {copied_peak} bytes, borrowed {borrowed_peak} bytes for 1 MiB schema text");
    // Allocator requests above the owned definition, excluding SQLite's heap and RSS.
    assert!(copied_peak > borrowed_peak + 1536 * 1024, "Tool hashing copied schemas");
}
