use super::*;
use crate::{native_runtime::NativeRuntime, retention::graph_tests::{LIVE, PEAK}};
use std::{path::Path, sync::atomic::Ordering as AtomicOrdering};

fn discovery() -> Discovery {
    Discovery {
        indexes: vec![], index_bytes: 0, core_index: vec![], core_bindings: CoreCalls::default(),
    }
}

#[test]
fn core_definitions_are_shared_without_internal_schema_copies() {
    const CHILD: &str = "IVY_CORE_BINDING_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "discovery::memory_tests::core_definitions_are_shared_without_internal_schema_copies", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let barrier = std::sync::Barrier::new(8);
    let concurrent = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8).map(|_| scope.spawn(|| {
            barrier.wait();
            CoreCalls.borrowed_binding(&"hive.objects.read".into()).unwrap()
        })).collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect::<Vec<_>>()
    });
    assert!(concurrent.iter().all(|binding| std::ptr::eq(*binding, concurrent[0])));
    // Use the largest real bundled definition, including its opaque schemas.
    let largest = core_methods().into_iter().map(|method| {
        &CoreCalls.borrowed_binding(&format!("hive.{method}").as_str().into()).unwrap().binding
    }).max_by_key(|binding| binding.retained_bytes()).unwrap();
    let params = IvyJson::object([
        ("qualifiedName", largest.qualified_name.clone().into()),
        ("expectedDefinitionHash", largest.definition_hash.clone().into()),
        ("arguments", empty()),
    ]);
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let wire = CoreCalls.binding(&largest.qualified_name).unwrap();
    let copied_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    assert_eq!(wire.get("definition"), Some(&largest.definition()));
    drop(wire);
    let first = CoreCalls;
    let second = CoreCalls;
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    for _ in 0..16 {
        for owner in [&first, &second] {
            assert!(std::ptr::eq(largest, &owner.borrowed_binding(&largest.qualified_name).unwrap().binding));
            let resolved = owner.resolve(&params).unwrap();
            assert!(largest.metadata.required("name").unwrap().as_js_string().unwrap().eq_str(resolved.method));
            assert_eq!(resolved.params, params.required("arguments").unwrap());
        }
    }
    let shared_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    eprintln!("Core binding Rust allocation peaks: wire copy {copied_peak} bytes; 32 shared internal resolutions {shared_peak} bytes");
    assert!(copied_peak > 16 * 1024, "The real definition must exercise a substantial copy");
    assert!(shared_peak < 16 * 1024, "Internal calls copied the core schema: {shared_peak} bytes");
}

#[test]
fn search_index_does_not_retain_or_copy_the_registry_source() {
    const CHILD: &str = "IVY_DISCOVERY_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "discovery::memory_tests::search_index_does_not_retain_or_copy_the_registry_source", "--nocapture"])
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
    let source = {
        let schema = serde_json::json!({"type":"object", "description":"s".repeat(128 * 1024)});
        let tools: Vec<_> = (0..8).map(|index| serde_json::json!({
            "namespace":"fixture", "name":format!("call{index}"),
            "description":"Registered search tool.", "inputSchema":schema, "outputSchema":schema,
        })).collect();
        serde_json::json!({"namespaces":[{
            "namespace":"fixture", "description":"Search fixture.", "tools":tools,
        }]}).to_string()
    };
    let identity = digest(source.as_bytes());
    store.connection().execute("INSERT INTO registries VALUES('fixture',?)", [&source]).unwrap();
    drop(source);
    let node: JsString = "fixture".into();
    let mut discovery = discovery();
    NativeRuntime.word_character(b'a' as u32);
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let started = std::time::Instant::now();
    let first = discovery.index(&store, &node, &NativeRuntime).unwrap();
    let miss_elapsed = started.elapsed();
    let miss_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    let retained = LIVE.load(AtomicOrdering::Relaxed) - before;
    assert_eq!(first.tools.len(), 8);
    assert_eq!(first.identity, identity);
    // A cache hit must not even read the body, independently of Rust allocations.
    store.connection().authorizer(Some(|context: rusqlite::hooks::AuthContext<'_>| {
        use rusqlite::hooks::{AuthAction, Authorization};
        match context.action {
            AuthAction::Read { table_name: "registries", column_name: "registry_json", .. } => Authorization::Deny,
            _ => Authorization::Allow,
        }
    })).unwrap();
    let before_hits = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before_hits, AtomicOrdering::Relaxed);
    let started = std::time::Instant::now();
    for _ in 0..16 {
        let next = discovery.index(&store, &node, &NativeRuntime).unwrap();
        assert!(Arc::ptr_eq(&first, &next));
    }
    let elapsed = started.elapsed();
    let peak = PEAK.load(AtomicOrdering::Relaxed) - before_hits;
    store.connection().authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>).unwrap();
    eprintln!("Hive Discovery Rust allocations for eight tools with 256 KiB schema text each: retained index {retained} bytes; cache-hit peak {peak} bytes across 16 reads");
    eprintln!("Discovery fixture cache-hit time across 16 reads: {} microseconds", elapsed.as_micros());
    eprintln!("Discovery fixture first index: peak {miss_peak} bytes, {} microseconds", miss_elapsed.as_micros());
    // A schema-only edit must still change the identity used by paging cursors.
    store.connection().execute("UPDATE registries SET registry_json=replace(registry_json,'ssss','tttt')", []).unwrap();
    let changed = discovery.index(&store, &node, &NativeRuntime).unwrap();
    assert_ne!(changed.identity, first.identity);
    assert_eq!(changed.tools[0].qualified_name, first.tools[0].qualified_name);
    assert!(!Arc::ptr_eq(&first, &changed));
    // Rust allocation requests only; SQLite's native heap and process RSS are excluded.
    assert!(retained < 128 * 1024, "Search retained the registry body: {retained} bytes");
    assert!(miss_peak < 128 * 1024, "Index construction decoded opaque schemas: {miss_peak} bytes");
    assert!(peak < 64 * 1024, "Cache hits copied the registry body: {peak} bytes");
}

#[test]
fn search_projection_preserves_utf16_metadata_and_skips_deep_opaque_values() {
    let schema = format!("{}0{}", "[".repeat(512), "]".repeat(512));
    let source = format!(r#"{{
        "discoveryHint":" \ud805 ",
        "namespaces":[{{"namespace":"fixture","description":"Namespace \ud800",
            "guideMarkdown":"unused \ud806",
            "discoveryGroups":[{{"group":"fixture/group","description":"Old"}},
                {{"group":"fixture/group","description":"Group \ud804"}}],
            "tools":[{{"name":"call","description":"Tool \ud801",
                "inputSchema":{schema},"outputSchema":{{"\ud807":"\ud808"}},
                "annotations":{{"readOnlyHint":true}},
                "discovery":{{"group":"fixture/group","summary":"\ud802",
                    "keywords":["find","\ud803"],"mcp":{{"name":"fixture_call","surface":"ivy"}}}}
            }}]
        }}]
    }}"#);
    let index = catalog::read(&source, digest(source.as_bytes()), &NativeRuntime).unwrap();
    assert_eq!(index.description, JsString(vec![0xd805]));
    assert_eq!(index.groups.len(), 1);
    assert_eq!(index.groups[0].1.0.last(), Some(&0xd804));
    assert_eq!(index.tools.len(), 1);
    let tool = &index.tools[0];
    assert_eq!(tool.qualified_name, JsString::from("fixture.call"));
    assert_eq!(tool.description, JsString(vec![0xd802]));
    assert_eq!(tool.group_description, index.groups[0].1);
    assert_eq!(tool.effect, "read");
    assert!(tool.words.contains(&JsString::from("find")));
    assert_eq!(tool.mcp.as_ref().unwrap().get("name"), Some(&"fixture_call".into()));
}

#[test]
fn registry_identity_follows_savepoint_rollback_and_delete() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(
        "INSERT INTO principals VALUES('fixture');
         INSERT INTO service_nodes VALUES('fixture','fixture','host','service',1,'{}');",
    ).unwrap();
    let source = r#"{"namespaces":[],"discoveryHint":"original"}"#;
    store.connection().execute("INSERT INTO registries VALUES('fixture',?)", [source]).unwrap();
    let node: JsString = "fixture".into();
    let mut discovery = discovery();
    let first = discovery.index(&store, &node, &NativeRuntime).unwrap();
    assert_eq!(first.identity, digest(source.as_bytes()));
    store.connection().execute_batch(
        "SAVEPOINT edit;
         UPDATE registries SET registry_json=replace(registry_json,'original','changed');",
    ).unwrap();
    let changed = discovery.index(&store, &node, &NativeRuntime).unwrap();
    assert_ne!(changed.identity, first.identity);
    assert_eq!(changed.description, JsString::from("changed"));
    store.connection().execute_batch("ROLLBACK TO edit; RELEASE edit;").unwrap();
    let reverted = discovery.index(&store, &node, &NativeRuntime).unwrap();
    assert_eq!(reverted.identity, first.identity);
    assert_eq!(reverted.description, first.description);
    store.connection().execute("DELETE FROM registries", []).unwrap();
    assert_eq!(discovery.index(&store, &node, &NativeRuntime).unwrap().identity, digest(b""));
    store.connection().execute("INSERT INTO registries VALUES('fixture',?)", [source]).unwrap();
    assert_eq!(discovery.index(&store, &node, &NativeRuntime).unwrap().identity, first.identity);
}

#[test]
fn registry_identity_is_rebuilt_from_online_backup() {
    let path = std::env::temp_dir().join(format!("ivy-discovery-{}.sqlite", uuid::Uuid::new_v4()));
    let backup = path.with_extension("backup.sqlite");
    let source = "{ \"namespaces\": [], \"discoveryHint\": \"saved\" }";
    {
        let store = Store::open(&path).unwrap();
        store.connection().execute_batch(
            "INSERT INTO principals VALUES('fixture');
             INSERT INTO service_nodes VALUES('fixture','fixture','host','service',1,'{}');",
        ).unwrap();
        store.connection().execute("INSERT INTO registries VALUES('fixture',?)", [source]).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(store.backup_to(backup.to_str().unwrap().into(), || Ok(()))).unwrap();
    }
    {
        let connection = Connection::open(&backup).unwrap();
        assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_schema WHERE name='registry_identities'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    }
    {
        let store = Store::open(&backup).unwrap();
        let node: JsString = "fixture".into();
        let mut discovery = discovery();
        let index = discovery.index(&store, &node, &NativeRuntime).unwrap();
        assert_eq!(index.identity, digest(source.as_bytes()));
        assert_eq!(index.description, JsString::from("saved"));
    }
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(backup).unwrap();
}

fn reference_schema_result(tool: &IvyJson) -> IvyResult<IvyJson> {
    let output = tool.get("outputSchema").cloned().unwrap_or(IvyJson::Null);
    let input = field(tool, "inputSchema")?.clone();
    let hash = hash_json(&IvyJson::object([
        ("inputSchema", input.clone()), ("outputSchema", output.clone()),
    ]))?;
    Ok(IvyJson::object([
        ("name", field(tool, "name")?.clone()),
        ("description", tool.get("description").cloned().unwrap_or_else(|| "".into())),
        ("inputSchema", input), ("outputSchema", output),
        ("schemaHash", hash.into()), ("complete", true.into()),
    ]))
}
fn core_params(arguments: IvyJson) -> IvyJson {
    let binding = CoreCalls.borrowed_binding(&"hive.objects.read".into()).unwrap();
    IvyJson::object([
        ("qualifiedName", binding.binding.qualified_name.clone().into()),
        ("expectedDefinitionHash", binding.binding.definition_hash.clone().into()),
        ("serviceName", "hive".into()), ("arguments", arguments),
    ])
}

#[test]
fn resolved_calls_borrow_core_and_service_arguments() {
    let params = core_params(IvyJson::object([
        (JsString(vec![0xd800]), IvyJson::String(JsString(vec![0xd801]))),
    ]));
    let core = CoreCalls.resolve(&params).unwrap();
    assert_eq!(core.method, "objects.read");
    assert!(std::ptr::eq(core.params, params.required("arguments").unwrap()));
    assert!(std::ptr::eq(core.method.as_ptr(), operation_catalog().keys()
        .find(|name| name.as_str() == "objects.read").unwrap().as_ptr()));
    let mut service = params.clone();
    service.insert("serviceName", "fixture".into());
    service.insert("serviceNodeId", "node".into());
    let owner = discovery();
    let call = owner.resolve_call(&service).unwrap();
    assert_eq!(call.method, "tools.call");
    assert!(std::ptr::eq(call.params, &service));
    assert!(owner.resolve_call(&params).unwrap().params == core.params);
}

#[test]
fn tool_schema_projection_matches_original_and_moves_schema_buffers() {
    let opaque = IvyJson::object([(JsString(vec![0xd800]), IvyJson::String(JsString(vec![0xd801]))) ]);
    let mut duplicate = vec![("inputSchema".into(), opaque.clone()), ("inputSchema".into(), true.into()),
        ("name".into(), "opaque".into()), ("description".into(), IvyJson::Null)];
    duplicate.push(("outputSchema".into(), false.into()));
    let fixtures = vec![IvyJson::Null, empty(), IvyJson::object([("name", "missing input".into())]),
        IvyJson::object([("inputSchema", true.into())]),
        IvyJson::object([("name", "empty output".into()), ("inputSchema", opaque.clone())]),
        IvyJson::object([("name", "both".into()), ("inputSchema", opaque.clone()), ("outputSchema", opaque)]),
        IvyJson::Object(duplicate)];
    let encoded = |result: IvyResult<IvyJson>| stringify(&match result {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(error) => IvyJson::object([("error", error.to_wire())]),
    }, usize::MAX).unwrap();
    for tool in fixtures {
        assert_eq!(encoded(tool_schema_result(tool.clone())), encoded(reference_schema_result(&tool)));
    }
    let tool = IvyJson::object([("name", "owned".into()), ("inputSchema", "first".into()), ("outputSchema", "second".into())]);
    let pointers: Vec<_> = ["inputSchema", "outputSchema"].iter().map(|name|
        tool.required(name).unwrap().as_js_string().unwrap().0.as_ptr()).collect();
    let result = tool_schema_result(tool).unwrap();
    for (name, pointer) in ["inputSchema", "outputSchema"].into_iter().zip(pointers) {
        assert_eq!(result.required(name).unwrap().as_js_string().unwrap().0.as_ptr(), pointer);
    }
}

#[test]
fn call_resolution_and_schema_projection_avoid_large_tree_copies() {
    const CHILD: &str = "IVY_DISCOVERY_CALL_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "discovery::memory_tests::call_resolution_and_schema_projection_avoid_large_tree_copies", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let owner = discovery();
    // Initialize the finite core binding before measuring per-call allocations.
    drop(core_params(empty()));
    for kind in ["core", "service", "schema"] {
        let mut original_peak = 0;
        for original in [true, false] {
            let payload = || IvyJson::from("X".repeat(1024 * 1024));
            let input = if kind == "schema" {
                IvyJson::object([("name", "large".into()), ("inputSchema", payload()), ("outputSchema", payload())])
            } else {
                let mut params = core_params(IvyJson::object([("opaque", payload())]));
                if kind == "service" { params.insert("serviceName", "fixture".into()); params.insert("serviceNodeId", "node".into()); }
                params
            };
            let before = LIVE.load(AtomicOrdering::Relaxed); PEAK.store(before, AtomicOrdering::Relaxed);
            let output = if kind == "schema" {
                std::borrow::Cow::Owned(if original { reference_schema_result(&input) } else { tool_schema_result(input) }.unwrap())
            } else if original {
                let call = owner.resolve_call(&input).unwrap();
                std::borrow::Cow::Owned(IvyJson::object([("method", call.method.into()), ("params", call.params.clone())]))
            } else {
                std::borrow::Cow::Borrowed(owner.resolve_call(&input).unwrap().params)
            };
            let peak = PEAK.load(AtomicOrdering::Relaxed) - before;
            eprintln!("Hive discovery Rust allocation peak: {peak} bytes; kind={kind}; original={original}; 1 MiB text per payload/schema");
            if original { original_peak = peak; }
            else if kind == "schema" { assert!(peak + 1024 * 1024 < original_peak, "Schema projection copied trees: {peak} / {original_peak}"); }
            else { assert!(peak < 8 * 1024, "Call resolution copied arguments: {peak}"); }
            drop(output);
        }
    }
}

// Previous full-node catalog path, retained only as a comparison oracle.
fn reference_mcp_catalog(
    owner: &mut Discovery,
    store: &Store,
    registry: &mut Registry,
    name: Option<&JsString>,
    runtime: &dyn Runtime,
    now: f64,
) -> IvyResult<Vec<IvyJson>> {
    fn policy(definition: &IvyJson) -> IvyResult<JsString> {
        match mcp_name(definition).filter(|v| v.as_object().is_some()) {
            Some(mcp) => Ok(concat(&[
                string(mcp, "name")?,
                &"\0".into(),
                mcp.get("surface")
                    .filter(|v| !v.is_null())
                    .and_then(IvyJson::as_js_string)
                    .unwrap_or(&"ivy_dev".into()),
            ])),
            None => Ok("".into()),
        }
    }
    let include = |definition: &IvyJson| -> bool {
        mcp_name(definition).is_some_and(|mcp| {
            mcp.as_object().is_some()
                && name.is_none_or(|n| {
                    n.0.is_empty() || mcp.get("name").and_then(IvyJson::as_js_string) == Some(n)
                })
        })
    };
    let mut active: HashMap<(JsString, JsString), HashSet<JsString>> = HashMap::new();
    for node in registry
        .nodes(store, runtime, now)?
        .iter()
        .filter(|v| Registry::eligible(v))
    {
        if let Some(catalog) = registry
            .live_catalog(store, string(node, "serviceNodeId")?)?
        {
            for ns in &catalog.namespaces {
                for binding in catalog.bindings(ns) {
                    active
                        .entry((
                            string(node, "serviceName")?.clone(),
                            binding.qualified_name.clone(),
                        ))
                        .or_default()
                        .insert(policy(&binding.metadata)?);
                }
            }
        }
    }
    let mut result = vec![];
    for method in core_methods() {
        let mut binding = owner.core_binding(&format!("hive.{method}").as_str().into())?;
        if include(binding.get("definition").unwrap_or(&IvyJson::Null)) {
            binding.insert("serviceName", "hive".into());
            binding.insert("provider", IvyJson::Null);
            binding.insert("guideMarkdown", "".into());
            result.push(binding);
        }
    }
    for node in registry.nodes(store, runtime, now)? {
        let Some(catalog) = registry
            .live_catalog(store, string(&node, "serviceNodeId")?)?
        else {
            continue;
        };
        for ns in &catalog.namespaces {
            let guide = guide(ns.guide_markdown.as_ref().ok_or_else(IvyError::internal)?, &catalog)?;
            for binding in catalog.bindings(ns) {
                let available = Registry::eligible(&node);
                let current = active.get(&(
                    string(&node, "serviceName")?.clone(),
                    binding.qualified_name.clone(),
                ));
                let binding_policy = policy(&binding.metadata)?;
                if !available
                    && current.is_some_and(|v| v.len() == 1 && !v.contains(&binding_policy))
                {
                    continue;
                }
                if include(&binding.metadata) {
                    let mut binding = binding.to_wire();
                    binding.insert("serviceName", field(&node, "serviceName")?.clone());
                    binding.insert("provider", provider(&node)?);
                    binding.insert("guideMarkdown", IvyJson::String(guide.clone()));
                    result.push(binding);
                }
            }
        }
    }
    result.sort_by(|a, b| {
        let a_name = mcp_name(a.get("definition").unwrap())
            .unwrap()
            .get("name")
            .unwrap()
            .as_js_string()
            .unwrap();
        let b_name = mcp_name(b.get("definition").unwrap())
            .unwrap()
            .get("name")
            .unwrap()
            .as_js_string()
            .unwrap();
        runtime.compare_english(a_name, b_name).then_with(|| {
            runtime.compare_english(
                a.get("provider")
                    .and_then(|v| v.get("serviceNodeId"))
                    .and_then(IvyJson::as_js_string)
                    .unwrap_or(&"".into()),
                b.get("provider")
                    .and_then(|v| v.get("serviceNodeId"))
                    .and_then(IvyJson::as_js_string)
                    .unwrap_or(&"".into()),
            )
        })
    });
    Ok(result)
}

fn insert_catalog_provider(store: &Store, id: &str, flags: [IvyJson; 4], mcp: Option<(&str, &str)>, padding: usize) {
    let mut node = IvyJson::object([
        ("serviceNodeId", id.into()), ("serviceName", IvyJson::String(JsString(vec![b's' as u16,0,0xd800]))),
        ("hostId", IvyJson::String(JsString(vec![0xd801,0,b'h' as u16]))),
        ("lastContactAt", "1970-01-01T00:00:00.000Z".into()), ("padding", "p".repeat(padding).into()),
    ]);
    for (key, value) in ["connected","synced","ready","desiredEnabled"].into_iter().zip(flags) { node.insert(key,value); }
    store.connection().execute("INSERT INTO service_nodes VALUES(?1,'fixture','host','service',1,?2)",
        rusqlite::params![id,canonical(&node,usize::MAX).unwrap()]).unwrap();
    let mut tool = IvyJson::object([
        ("namespace", "fixture".into()), ("name", "read".into()), ("description", "Read a fixture.".into()),
        ("inputSchema", empty()), ("outputSchema", IvyJson::Null),
    ]);
    if let Some((name,surface))=mcp { tool.insert("discovery",IvyJson::object([("mcp",IvyJson::object([
        ("name",name.into()),("surface",surface.into()),
    ]))])); }
    let catalog=IvyJson::object([("namespaces",IvyJson::Array(vec![IvyJson::object([
        ("namespace","fixture".into()),("description","Fixture tools.".into()),("guideMarkdown","Fixture guide.".into()),("tools",IvyJson::Array(vec![tool])),
    ])]))]);
    store.connection().execute("INSERT INTO registries VALUES(?1,?2)",
        rusqlite::params![id,canonical(&catalog,usize::MAX).unwrap()]).unwrap();
}

#[test]
fn mcp_provider_projection_keeps_utf16_flags_policies_and_order() {
    let store=Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute("INSERT INTO principals VALUES('fixture')",[]).unwrap();
    for (id,flags,mcp) in [
        ("a", [true.into(),true.into(),true.into(),true.into()],Some(("fixture_read","ivy"))),
        ("b", [false.into(),true.into(),true.into(),true.into()],Some(("old_read","ivy_dev"))),
        ("c", [true.into(),true.into(),true.into(),true.into()],Some(("other_read","ivy_dev"))),
        ("d", [true.into(),IvyJson::Null,true.into(),true.into()],Some(("old_read","ivy_dev"))),
        ("e", [true.into(),true.into(),true.into(),1.0.into()],None),
    ] { insert_catalog_provider(&store,id,flags,mcp,0); }
    store.connection().execute("UPDATE service_nodes SET status_json=json_remove(status_json,'$.ready') WHERE id='e'",[]).unwrap();
    let mut owner=Registry::default();
    for phase in 0..3 {
        if phase==1 { store.connection().execute("UPDATE service_nodes SET status_json=json_set(status_json,'$.connected',json('false')) WHERE id='c'",[]).unwrap(); }
        if phase==2 { store.connection().execute("UPDATE service_nodes SET status_json=json_set(status_json,'$.desiredEnabled',json('false')) WHERE id='a'",[]).unwrap(); }
        let full=owner.nodes(&store,&NativeRuntime,1_000_000.0).unwrap();
        let compact=owner.catalog_providers(&store).unwrap();
        assert_eq!(full.len(),compact.len());
        for (old,new) in full.iter().zip(&compact) {
            assert_eq!(old.get("serviceNodeId").unwrap().as_js_string(),Some(&new.service_node_id));
            assert_eq!(old.get("serviceName").unwrap().as_js_string(),Some(&new.service_name));
            assert_eq!(old.get("hostId").unwrap().as_js_string(),Some(&new.host_id));
            assert_eq!(Registry::eligible(old),new.available);
        }
        for name in [None,Some(""),Some("fixture_read"),Some("old_read"),Some("absent")] {
            let name=name.map(JsString::from);
            let old=reference_mcp_catalog(&mut discovery(),&store,&mut owner,name.as_ref(),&NativeRuntime,1_000_000.0).unwrap();
            let new=discovery().mcp_catalog(&store,&mut owner,name.as_ref(),&NativeRuntime,1_000_000.0).unwrap();
            assert_eq!(stringify(&IvyJson::Array(new.clone()),usize::MAX).unwrap(),stringify(&IvyJson::Array(old.clone()),usize::MAX).unwrap(),"phase {phase}, name {name:?}");
            for surface in ["ivy","ivy_dev"] {
                assert_eq!(crate::mcp::surface_bindings(new.clone(),&surface.into()).map(IvyJson::Array).map_err(|e|e.to_wire()),
                    crate::mcp::surface_bindings(old.clone(),&surface.into()).map(IvyJson::Array).map_err(|e|e.to_wire()));
            }
            if name.as_ref().is_some_and(|n|n.eq_str("old_read")) {
                assert_eq!(new.is_empty(),phase==1,"One live policy suppresses the different offline policy");
            }
        }
    }
}

#[test]
fn mcp_catalog_avoids_full_status_and_diagnostic_working_sets() {
    const CHILD:&str="IVY_MCP_PROVIDER_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","discovery::memory_tests::mcp_catalog_avoids_full_status_and_diagnostic_working_sets","--nocapture"])
            .env(CHILD,"1").output().unwrap();
        eprint!("{}",String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));return;
    }
    let store=Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute("INSERT INTO principals VALUES('fixture')",[]).unwrap();
    for i in 0..64 { insert_catalog_provider(&store,&format!("provider{i:02}"),[true.into(),true.into(),true.into(),true.into()],Some(("fixture_read","ivy")),32*1024); }
    let diagnostic=serde_json::json!({"source":"unrelated","status":"current","message":"d".repeat(2*1024*1024)}).to_string();
    store.connection().execute("INSERT INTO diagnostics VALUES('large',?)",[diagnostic]).unwrap();
    let mut owner=Registry::default();let name=JsString::from("fixture_read");let mut catalog=discovery();
    drop(catalog.mcp_catalog(&store,&mut owner,Some(&name),&NativeRuntime,0.0).unwrap());
    let before=LIVE.load(AtomicOrdering::Relaxed);PEAK.store(before,AtomicOrdering::Relaxed);
    let started=std::time::Instant::now();
    let old=reference_mcp_catalog(&mut catalog,&store,&mut owner,Some(&name),&NativeRuntime,0.0).unwrap();
    let old_elapsed=started.elapsed();let old_peak=PEAK.load(AtomicOrdering::Relaxed)-before;
    let expected=hash_json(&IvyJson::Array(old)).unwrap();
    store.connection().authorizer(Some(|context:rusqlite::hooks::AuthContext<'_>| {
        match context.action {
            rusqlite::hooks::AuthAction::Read{table_name:"diagnostics",..}=>rusqlite::hooks::Authorization::Deny,
            _=>rusqlite::hooks::Authorization::Allow,
        }
    })).unwrap();
    let before=LIVE.load(AtomicOrdering::Relaxed);PEAK.store(before,AtomicOrdering::Relaxed);
    let started=std::time::Instant::now();
    let new=catalog.mcp_catalog(&store,&mut owner,Some(&name),&NativeRuntime,0.0).unwrap();
    let elapsed=started.elapsed();let peak=PEAK.load(AtomicOrdering::Relaxed)-before;
    store.connection().authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>)->rusqlite::hooks::Authorization>).unwrap();
    assert_eq!(new.len(),64);assert_eq!(hash_json(&IvyJson::Array(new)).unwrap(),expected);
    eprintln!("Hive MCP catalog Rust allocation peak: full nodes {old_peak} bytes, compact providers {peak} bytes for 64 providers, 2 MiB status and 2 MiB diagnostics; elapsed {old_elapsed:?} / {elapsed:?}");
    assert!(old_peak>peak+2*1024*1024,"Provider projection retained status bodies: {old_peak} / {peak}");
    assert!(peak<1024*1024,"Compact catalog unexpectedly large: {peak}");
}

#[test]
fn discovery_hint_projection_preserves_optional_types_and_utf16() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(
        "INSERT INTO principals VALUES('fixture');
         INSERT INTO service_nodes VALUES('fixture','fixture','host','service',1,'{}');",
    ).unwrap();
    let owner = Registry::default();
    let id = JsString::from("fixture");
    assert_eq!(owner.discovery_hint(&store, &id).unwrap(), None);
    let mut values = vec![IvyJson::Null, empty(), IvyJson::Array(vec![]), false.into(), 17.0.into(), "root".into()];
    for hint in [
        IvyJson::Null, false.into(), 17.0.into(), IvyJson::Array(vec!["ignored".into()]),
        IvyJson::object([("ignored", "x".repeat(64 * 1024).into())]), "".into(),
        "\u{feff} \u{2028}hint \u{a0}".into(),
        IvyJson::String(JsString(vec![0xfeff, b'a' as u16, 0, 0xd800, b'b' as u16, 0xdc00, 0x2028])),
    ] {
        values.push(IvyJson::object([("discoveryHint", hint)]));
    }
    for value in values {
        store.connection().execute(
            "INSERT INTO registries VALUES('fixture',?) ON CONFLICT(node_id) DO UPDATE SET registry_json=excluded.registry_json",
            [canonical(&value, usize::MAX).unwrap()],
        ).unwrap();
        let expected = owner.get_registry(&store, &id).map(|value| value.and_then(|value|
            value.get("discoveryHint").and_then(IvyJson::as_js_string).cloned()
        )).map_err(|error| error.to_wire());
        assert_eq!(owner.discovery_hint(&store, &id).map_err(|error| error.to_wire()), expected);
    }
    store.connection().execute("DELETE FROM registries", []).unwrap();
    assert_eq!(owner.discovery_hint(&store, &id).unwrap(), None);
}

#[test]
fn instruction_projection_preserves_hints_policies_and_revisions() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute("INSERT INTO principals VALUES('fixture')", []).unwrap();
    for (id, active, mcp, hint) in [
        ("a", true, Some(("fixture_read", "ivy")), "\u{feff} Duplicate capability.\u{2028}"),
        ("b", false, Some(("old_read", "ivy_dev")), "Offline capability."),
        ("c", true, Some(("fixture_read", "ivy")), "Duplicate capability."),
        ("d", true, None, "Hidden capability."),
        ("e", true, Some(("fixture_read", "ivy")), "\u{feff} \u{2028}"),
        ("f", true, Some(("other_read", "ivy_dev")), "Sorted capability."),
    ] {
        insert_catalog_provider(&store, id, [active.into(), true.into(), true.into(), true.into()], mcp, 0);
        store.connection().execute(
            "UPDATE registries SET registry_json=json_set(registry_json,'$.discoveryHint',json(?)) WHERE node_id=?",
            rusqlite::params![canonical(&IvyJson::from(hint), usize::MAX).unwrap(), id],
        ).unwrap();
    }
    store.connection().execute("UPDATE service_nodes SET status_json=json_set(status_json,'$.serviceName','another') WHERE id='f'", []).unwrap();
    let mut registry = Registry::default();
    let mut owner = discovery();
    for phase in 0..3 {
        if phase == 1 {
            let hint = IvyJson::String(JsString(vec![b'n' as u16, 0, 0xd800, 0xdc00, 0xd801]));
            store.connection().execute(
                "UPDATE registries SET registry_json=json_set(registry_json,'$.discoveryHint',json(?)) WHERE node_id='a'",
                [canonical(&hint, usize::MAX).unwrap()],
            ).unwrap();
        }
        if phase == 2 {
            store.connection().execute("DELETE FROM registries WHERE node_id='b'", []).unwrap();
        }
        let expected = original_instructions(&mut owner, &store, &mut registry, &NativeRuntime, 1_000_000.0).unwrap();
        store.connection().authorizer(Some(|context: rusqlite::hooks::AuthContext<'_>| {
            match context.action {
                rusqlite::hooks::AuthAction::Read { table_name: "diagnostics", .. } => rusqlite::hooks::Authorization::Deny,
                _ => rusqlite::hooks::Authorization::Allow,
            }
        })).unwrap();
        let actual = owner.instructions(&store, &mut registry, &NativeRuntime, 1_000_000.0).unwrap();
        store.connection().authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>).unwrap();
        assert_eq!(actual, expected, "phase {phase}");
        let instructions = text(string(&actual, "instructions").unwrap());
        assert!(!instructions.contains("Hidden capability."));
        assert_eq!(instructions.matches("Duplicate capability.").count(), 1);
        assert_eq!(instructions.contains("Offline capability."), phase < 2);
    }
}

#[test]
fn instructions_avoid_full_status_and_registry_working_sets() {
    const CHILD: &str = "IVY_INSTRUCTION_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "discovery::memory_tests::instructions_avoid_full_status_and_registry_working_sets", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute("INSERT INTO principals VALUES('fixture')", []).unwrap();
    for i in 0..64 {
        let id = format!("provider{i:02}");
        insert_catalog_provider(&store, &id, [true.into(), true.into(), true.into(), true.into()], Some(("fixture_read", "ivy")), 32 * 1024);
        store.connection().execute(
            "UPDATE registries SET registry_json=json_set(registry_json,'$.discoveryHint','Fixture capability.','$.opaque',?) WHERE node_id=?",
            rusqlite::params!["r".repeat(32 * 1024), id],
        ).unwrap();
    }
    let diagnostic = serde_json::json!({"source":"unrelated","status":"current","message":"d".repeat(2 * 1024 * 1024)}).to_string();
    store.connection().execute("INSERT INTO diagnostics VALUES('large',?)", [diagnostic]).unwrap();
    let mut registry = Registry::default();
    let mut owner = discovery();
    drop(original_instructions(&mut owner, &store, &mut registry, &NativeRuntime, 0.0).unwrap());
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let started = std::time::Instant::now();
    let old = original_instructions(&mut owner, &store, &mut registry, &NativeRuntime, 0.0).unwrap();
    let old_elapsed = started.elapsed();
    let old_peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    let expected = hash_json(&old).unwrap();
    drop(old);
    store.connection().authorizer(Some(|context: rusqlite::hooks::AuthContext<'_>| {
        match context.action {
            rusqlite::hooks::AuthAction::Read { table_name: "diagnostics", .. } => rusqlite::hooks::Authorization::Deny,
            _ => rusqlite::hooks::Authorization::Allow,
        }
    })).unwrap();
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let started = std::time::Instant::now();
    let new = owner.instructions(&store, &mut registry, &NativeRuntime, 0.0).unwrap();
    let elapsed = started.elapsed();
    let peak = PEAK.load(AtomicOrdering::Relaxed) - before;
    store.connection().authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>).unwrap();
    assert_eq!(hash_json(&new).unwrap(), expected);
    eprintln!("Hive instructions Rust allocation peak: full nodes/registries {old_peak} bytes, projected hints {peak} bytes for 64 providers, 2 MiB status, 2 MiB registries and 2 MiB diagnostics; elapsed {old_elapsed:?} / {elapsed:?}");
    assert!(old_peak > peak + 2 * 1024 * 1024, "Instructions retained status bodies: {old_peak} / {peak}");
    assert!(peak < 2 * 1024 * 1024, "Projected instructions unexpectedly large: {peak}");
}

fn original_instructions(
        owner: &mut Discovery,
        store: &Store,
        registry: &mut Registry,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        let catalog = owner.mcp_catalog(store, registry, None, runtime, now)?;
        let mut guides = vec![];
        for binding in &catalog {
            if (binding
                .get("provider")
                .is_some_and(|v| v.as_object().is_some()))
                && binding
                    .get("guideMarkdown")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| !v.0.is_empty())
            {
                guides.push(concat(&[
                    string(binding, "serviceName")?,
                    &"/".into(),
                    string(field(binding, "definition")?, "namespace")?,
                    &": ".into(),
                    string(binding, "guideMarkdown")?,
                ]));
            }
        }
        let mut hints: Vec<(JsString, Vec<JsString>)> = vec![];
        for node in registry.nodes(store, runtime, now)? {
            if !owner
                .index(store, string(&node, "serviceNodeId")?, runtime)?
                .tools
                .iter()
                .any(|t| t.mcp.is_some())
            {
                continue;
            }
            let Some(hint) = registry
                .get_registry(store, string(&node, "serviceNodeId")?)?
                .as_ref()
                .and_then(|v| v.get("discoveryHint"))
                .and_then(IvyJson::as_js_string)
                .map(JsString::trim_ecmascript)
                .filter(|v| !v.0.is_empty())
            else {
                continue;
            };
            let name = string(&node, "serviceName")?.clone();
            if let Some((_, values)) = hints.iter_mut().find(|(n, _)| n == &name) {
                if !values.contains(&hint) {
                    values.push(hint);
                }
            } else {
                hints.push((name, vec![hint]));
            }
        }
        hints.sort_by(|a, b| runtime.compare_english(&a.0, &b.0));
        let mut value = JsString::from(include_str!(
            "../../instructions/hive-mcp-instructions.md"
        ))
        .trim_ecmascript();
        if !hints.is_empty() {
            value = concat(&[
                &value,
                &"\n\nServices registered to this Hive provide the following capabilities:\n"
                    .into(),
            ]);
            let mut lines = vec![];
            for (name, mut values) in hints {
                values.sort_by(|a, b| runtime.compare_english(a, b));
                for hint in values {
                    lines.push(concat(&[&"- ".into(), &name, &": ".into(), &hint]));
                }
            }
            value = concat(&[&value, &joined(&lines, "\n")]);
        }
        let mut seen = HashSet::new();
        guides.retain(|v| seen.insert(v.clone()));
        guides.sort_by(|a, b| runtime.compare_english(a, b));
        if !guides.is_empty() {
            value = concat(&[
                &value,
                &"\n\nTool workflows:\n".into(),
                &joined(
                    &guides
                        .iter()
                        .map(|v| concat(&[&"- ".into(), v]))
                        .collect::<Vec<_>>(),
                    "\n",
                ),
            ]);
        }
        require(
            text(&value).len() <= 32768,
            "limit_exceeded",
            "Composed Hive instructions exceed 32 KiB.",
        )?;
        Ok(IvyJson::object([("instructions", IvyJson::String(value))]))
    }
