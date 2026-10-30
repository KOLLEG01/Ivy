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
        CoreCalls.borrowed_binding(&format!("hive.{method}").as_str().into()).unwrap()
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
            assert!(std::ptr::eq(largest, owner.borrowed_binding(&largest.qualified_name).unwrap()));
            let resolved = owner.resolve(&params).unwrap();
            assert_eq!(resolved.get("method"), largest.metadata.get("name"));
            assert_eq!(resolved.get("params"), params.get("arguments"));
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
    let node = IvyJson::object([("serviceNodeId", "fixture".into())]);
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
    let node = IvyJson::object([("serviceNodeId", "fixture".into())]);
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
        let node = IvyJson::object([("serviceNodeId", "fixture".into())]);
        let mut discovery = discovery();
        let index = discovery.index(&store, &node, &NativeRuntime).unwrap();
        assert_eq!(index.identity, digest(source.as_bytes()));
        assert_eq!(index.description, JsString::from("saved"));
    }
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(backup).unwrap();
}
