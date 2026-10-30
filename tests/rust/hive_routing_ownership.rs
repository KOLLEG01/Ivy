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
    let call = routing.prepare(&context(), params()).unwrap();
    assert_eq!(routing.complete(&call.context, "reply".into()).unwrap(), "reply".into());
}

fn notification_catalog() -> Catalog {
    let notifications: Vec<_> = (0..4).map(|index| json!({
        "namespace":"provider", "name":format!("changed{index}"), "version":"1.0.0",
        "description":"Fixture notification.",
        "payloadSchema":{"type":"integer", "description":"n".repeat(128 * 1024)}
    })).collect();
    catalog_fixture::catalog_from_wire(&value(json!({
        "namespaces":["provider"], "tools":[], "notifications":notifications
    }))).unwrap()
}
fn notification_context(generation: f64) -> IvyJson {
    value(json!({"serviceNodeId":"provider", "generation":generation}))
}
fn notice(payload: IvyJson) -> IvyJson {
    IvyJson::object([
        ("namespace", "provider".into()), ("name", "changed0".into()),
        ("version", "1.0.0".into()), ("payload", payload),
    ])
}

#[test]
fn notification_handoff_does_not_copy_opaque_schema_trees() {
    use crate::retention::graph_tests::{LIVE, PEAK};
    use std::sync::atomic::Ordering;
    const CHILD: &str = "IVY_ROUTING_NOTIFICATION_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "live_routing::ownership_tests::notification_handoff_does_not_copy_opaque_schema_trees", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let owner = notification_catalog();
    let status = node();
    let mut peaks = vec![];
    for validate in [false, true] {
        let mut routing = LiveRouting::new(validate, None);
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        routing.update(&status, Some(7.0), Some(&owner)).unwrap();
        let peak = PEAK.load(Ordering::Relaxed) - before;
        eprintln!("Hive notification catalog handoff Rust allocation peak: {peak} bytes; validateMessages={validate}; four 128 KiB opaque schema descriptions");
        // Rust allocator requests only; validator compilation and process RSS are excluded.
        peaks.push(peak);
        routing.notification(&notification_context(7.0), &notice(1.0.into())).unwrap();
        assert!(routing.notification(&notification_context(7.0), &notice("invalid".into())).is_err());
    }
    assert!(peaks.iter().all(|peak| *peak < 64 * 1024), "Routing copied notification schema trees: {peaks:?}");
}

#[test]
fn notification_schema_owners_follow_catalog_changes_and_connection_guards() {
    for validate in [false, true] {
        let mut routing = LiveRouting::new(validate, None);
        let owner = notification_catalog();
        let schema = Arc::downgrade(&owner.notifications[0].payload_schema);
        routing.update(&node(), Some(7.0), Some(&owner)).unwrap();
        let name = key(["provider".into(), "changed0".into(), "1.0.0".into()]);
        assert!(Arc::ptr_eq(&owner.notifications[0].payload_schema,
            &routing.route(&"provider".into()).unwrap().notifications[&name]));
        drop(owner);
        assert_eq!(schema.strong_count(), 1);
        routing.notification(&notification_context(7.0), &notice(1.0.into())).unwrap();

        let mut replacement = notification_catalog();
        replacement.notifications[0].payload_schema = Arc::new(value(json!({"type":"string"})));
        routing.update(&node(), Some(7.0), Some(&replacement)).unwrap();
        assert!(schema.upgrade().is_none());
        assert!(routing.notification(&notification_context(7.0), &notice(1.0.into())).is_err());
        routing.notification(&notification_context(7.0), &notice("changed".into())).unwrap();
        routing.update(&node(), Some(7.0), None).unwrap();
        routing.notification(&notification_context(7.0), &notice("same connection".into())).unwrap();

        routing.update(&node(), Some(8.0), None).unwrap();
        assert_eq!(routing.notification(&notification_context(7.0), &notice("old".into()))
            .unwrap_err().code, "stale_generation");
        assert_eq!(routing.notification(&notification_context(8.0), &notice("unregistered".into()))
            .unwrap_err().code, "not_found");
        routing.clear();
        assert_eq!(routing.notification(&notification_context(8.0), &notice("cleared".into()))
            .unwrap_err().code, "stale_generation");
    }
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
        .map(|_| routing.prepare(&context(), params()).unwrap().context)
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
    let current = routing.prepare(&context(), params()).unwrap().context;
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
    let old = routing.prepare(&context(), params()).unwrap().context;
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

    let current = routing.prepare(&context(), params()).unwrap().context;
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
    let call = routing.prepare(&context(), params.clone()).unwrap();
    assert!(call.context.output_schema.is_none());
    assert_eq!(call.arguments, dynamic);
    assert_eq!(call.operation_id, Some(operation));
    assert_eq!(
        routing.complete(&call.context, false.into()).unwrap(),
        false.into()
    );

    params.insert("expectedDefinitionHash", "changed".into());
    assert_eq!(
        routing.prepare(&context(), params.clone()).unwrap_err().code,
        "tool_definition_changed"
    );
    params.insert("expectedDefinitionHash", "original-hash".into());
    routing
        .update(&node(), Some(7.0), Some(&catalog("string", false, false)))
        .unwrap();
    assert_eq!(
        routing.prepare(&context(), params.clone()).unwrap_err().code,
        "not_found"
    );
    assert_eq!(
        routing
            .prepare_dashboard_image(&context(), &"provider".into(), 1.0.into())
            .unwrap_err()
            .code,
        "provider_contract_error"
    );
}

#[test]
fn tool_and_dashboard_calls_move_owned_argument_buffers() {
    use crate::retention::graph_tests::{LIVE, PEAK};
    use std::sync::atomic::Ordering;
    const CHILD: &str = "IVY_ROUTING_ARGUMENT_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "live_routing::ownership_tests::tool_and_dashboard_calls_move_owned_argument_buffers", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let mut status = node();
    status.insert("serviceName", "dashboards".into());
    let mut routing = LiveRouting::new(false, None);
    routing.update(&status, Some(7.0), Some(&catalog("string", true, true))).unwrap();
    let context = context();
    let node = JsString::from("provider");
    for dashboard in [false, true] {
        let payload = IvyJson::String(JsString(vec![b'x' as u16; 1024 * 1024]));
        let payload_pointer = payload.as_js_string().unwrap().0.as_ptr();
        let expected = ivy_contracts::json::stringify(&payload, usize::MAX).unwrap();
        let operation = IvyJson::String(JsString(vec![0xd800, 0, 0xdc00]));
        let operation_pointer = operation.as_js_string().unwrap().0.as_ptr();
        let mut request = params();
        request.insert("arguments", payload);
        request.insert("operationId", operation);
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let call = if dashboard {
            routing.prepare_dashboard_image(&context, &node, request.remove("arguments").unwrap())
        } else {
            routing.prepare(&context, request)
        }.unwrap();
        let peak = PEAK.load(Ordering::Relaxed) - before;
        eprintln!("Hive routed-arguments Rust allocation peak: {peak} bytes; dashboard={dashboard}; 1 MiB payload text");
        // Routing only: message validation, parsing, encoding and process RSS are excluded.
        assert!(peak < 64 * 1024, "Routing copied the argument payload: {peak}");
        assert_eq!(call.arguments.as_js_string().unwrap().0.as_ptr(), payload_pointer);
        assert_eq!(ivy_contracts::json::stringify(&call.arguments, usize::MAX).unwrap(), expected);
        if dashboard {
            assert_eq!(call.context.qualified_name, JsString::from("dashboards.image"));
            assert!(call.operation_id.is_none());
        } else {
            assert_eq!(call.context.qualified_name, JsString::from("provider.call"));
            let operation = call.operation_id.as_ref().unwrap().as_js_string().unwrap();
            assert_eq!(operation.0.as_ptr(), operation_pointer);
            assert_eq!(operation.0, [0xd800, 0, 0xdc00]);
        }
    }
}
