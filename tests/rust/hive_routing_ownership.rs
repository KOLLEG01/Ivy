use super::*;
use ivy_contracts::json::parse;
use serde_json::json;
#[path = "hive_catalog_fixture.rs"]
mod catalog_fixture;

fn value(value: serde_json::Value) -> IvyJson {
    IvyJson::from_serde(&value).unwrap()
}
fn node() -> IvyJson {
    value(json!({
        "serviceNodeId":"provider", "hostId":"host", "serviceName":"provider",
        "principalId":"provider-principal", "connected":true, "synced":true,
        "ready":true, "desiredEnabled":true
    }))
}
fn catalog(output: &str, published: bool, read_only: bool) -> Catalog {
    let definition = json!({
        "namespace":"provider", "name":"call", "description":"d".repeat(128 * 1024),
        "inputSchema":{"type":"integer", "description":"i".repeat(128 * 1024)},
        "outputSchema":{"type":output, "description":"o".repeat(128 * 1024)},
        "discovery":{"mcp":if published { json!({}) } else { json!(false) }},
        "annotations":{"readOnlyHint":read_only}
    });
    let mut dashboard = definition.clone();
    dashboard["namespace"] = json!("dashboards");
    dashboard["name"] = json!("image");
    catalog_fixture::catalog_from_wire(&value(json!({
        "namespaces":["provider", "dashboards"], "notifications":[],
        "tools":[
            {"qualifiedName":"provider.call", "definitionHash":"original-hash", "definition":definition},
            {"qualifiedName":"dashboards.image", "definitionHash":"image-hash", "definition":dashboard}
        ]
    }))).unwrap()
}
fn context() -> IvyJson {
    value(json!({"principalId":"caller", "transport":"mcp"}))
}
fn params() -> IvyJson {
    value(
        json!({"qualifiedName":"provider.call", "expectedDefinitionHash":"original-hash", "arguments":1}),
    )
}

#[test]
fn catalog_handoff_does_not_copy_opaque_schema_trees() {
    use crate::retention::graph_tests::{LIVE, PEAK};
    use std::sync::atomic::Ordering;
    const CHILD: &str = "IVY_ROUTING_CATALOG_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "live_routing::ownership_tests::catalog_handoff_does_not_copy_opaque_schema_trees", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let owner = catalog("string", true, true);
    let status = node();
    let mut routing = LiveRouting::new(true, None);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    routing.update(&status, Some(7.0), Some(&owner)).unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - before;
    eprintln!("Hive routing catalog handoff Rust allocation peak: {peak} bytes for four 128 KiB opaque schema descriptions");
    // Rust allocator requests only; validator compilation and process RSS are excluded.
    assert!(peak < 64 * 1024, "Routing copied schema trees: {peak} bytes");
    let call = routing.prepare(&context(), &params()).unwrap();
    assert_eq!(routing.complete(&call.context, "reply".into()).unwrap(), "reply".into());
}

#[test]
fn pending_calls_share_the_dispatched_schema_until_the_last_reply_releases_it() {
    let mut routing = LiveRouting::new(true, None);
    routing
        .update(&node(), Some(7.0), Some(&catalog("string", true, true)))
        .unwrap();
    let old_schema = Arc::downgrade(
        &routing.route(&"provider".into()).unwrap().tools[&JsString::from("provider.call")]
            .schemas
            .as_ref()
            .unwrap()
            .output,
    );
    let mut calls: Vec<_> = (0..64)
        .map(|_| routing.prepare(&context(), &params()).unwrap().context)
        .collect();
    assert_eq!(old_schema.strong_count(), 65);
    for call in &calls[1..] {
        assert!(Arc::ptr_eq(
            calls[0].output_schema.as_ref().unwrap(),
            call.output_schema.as_ref().unwrap()
        ));
    }

    // A same-connection catalog change must not change an already sent contract.
    routing
        .update(&node(), Some(7.0), Some(&catalog("integer", true, true)))
        .unwrap();
    assert_eq!(old_schema.strong_count(), 64);
    assert_eq!(
        routing.complete(&calls[0], "reply".into()).unwrap(),
        "reply".into()
    );
    let error = routing.complete(&calls[0], 1.0.into()).unwrap_err();
    assert_eq!(error.code, "provider_contract_error");
    assert_eq!(error.outcome, Outcome::Unknown);
    let current = routing.prepare(&context(), &params()).unwrap().context;
    assert!(!Arc::ptr_eq(
        calls[0].output_schema.as_ref().unwrap(),
        current.output_schema.as_ref().unwrap()
    ));
    assert_eq!(routing.complete(&current, 1.0.into()).unwrap(), 1.0.into());

    let last = calls.pop().unwrap();
    drop(calls);
    assert_eq!(old_schema.strong_count(), 1);
    drop(last);
    assert!(old_schema.upgrade().is_none());
}

#[test]
fn retained_schemas_do_not_admit_replies_from_replaced_or_cleared_connections() {
    let mut routing = LiveRouting::new(true, None);
    let catalog = catalog("string", true, true);
    routing.update(&node(), Some(7.0), Some(&catalog)).unwrap();
    let old = routing.prepare(&context(), &params()).unwrap().context;
    let schema = Arc::downgrade(old.output_schema.as_ref().unwrap());
    routing.update(&node(), Some(8.0), Some(&catalog)).unwrap();
    let error = routing.complete(&old, "reply".into()).unwrap_err();
    assert_eq!(error.code, "stale_generation");
    assert_eq!(error.outcome, Outcome::Unknown);
    drop(catalog);
    drop(old);
    // The replacement connection shares this catalog's schema; it cannot admit
    // an old reply merely because the contract owner is still alive.
    assert_eq!(schema.strong_count(), 1);

    let current = routing.prepare(&context(), &params()).unwrap().context;
    routing.clear();
    let error = routing.complete(&current, "reply".into()).unwrap_err();
    assert_eq!(error.code, "stale_generation");
    assert_eq!(error.outcome, Outcome::Unknown);
    drop(current);
    assert!(schema.upgrade().is_none());
}

#[test]
fn disabled_message_validation_retains_no_tool_schemas_but_keeps_dispatch_guards() {
    let mut routing = LiveRouting::new(false, None);
    routing
        .update(&node(), Some(7.0), Some(&catalog("string", true, true)))
        .unwrap();
    assert!(
        routing
            .route(&"provider".into())
            .unwrap()
            .tools
            .values()
            .all(|tool| tool.schemas.is_none())
    );
    let mut params = params();
    let dynamic = parse(
        r#"{"\ud800":["\udfff",9007199254740992],"unknown":true}"#,
        usize::MAX,
    )
    .unwrap();
    let operation = parse(r#""original-\ud800""#, usize::MAX).unwrap();
    params.insert("arguments", dynamic.clone());
    params.insert("operationId", operation.clone());
    let call = routing.prepare(&context(), &params).unwrap();
    assert!(call.context.output_schema.is_none());
    assert_eq!(call.arguments, dynamic);
    assert_eq!(call.operation_id, Some(operation));
    assert_eq!(
        routing.complete(&call.context, false.into()).unwrap(),
        false.into()
    );

    params.insert("expectedDefinitionHash", "changed".into());
    assert_eq!(
        routing.prepare(&context(), &params).unwrap_err().code,
        "tool_definition_changed"
    );
    params.insert("expectedDefinitionHash", "original-hash".into());
    routing
        .update(&node(), Some(7.0), Some(&catalog("string", false, false)))
        .unwrap();
    assert_eq!(
        routing.prepare(&context(), &params).unwrap_err().code,
        "not_found"
    );
    assert_eq!(
        routing
            .prepare_dashboard_image(&context(), &"provider".into(), &1.0.into())
            .unwrap_err()
            .code,
        "provider_contract_error"
    );
}
