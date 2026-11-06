//! Project historical JSON traces onto the native internal catalog in tests only.
#![allow(dead_code)]
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_hive::{registry_catalog::{Catalog, Namespace, NotificationDefinition, ToolBinding}, storage::RoutingUpdate, live_routing::RoutingNode};
use std::sync::Arc;

pub fn catalog_from_wire(value: &IvyJson) -> IvyResult<Catalog> {
    let namespaces = value.get("namespaces").and_then(IvyJson::as_array)
        .into_iter().flatten().map(|name| Ok(Namespace::new(
            name.as_js_string().ok_or_else(IvyError::internal)?.clone(), None, 0..0,
        ))).collect::<IvyResult<_>>()?;
    let tools = value.get("tools").and_then(IvyJson::as_array)
        .into_iter().flatten().map(|binding| Ok(ToolBinding::new(
            binding.required("qualifiedName")?.as_js_string()
                .ok_or_else(IvyError::internal)?.clone(),
            binding.required("definitionHash")?.as_js_string()
                .ok_or_else(IvyError::internal)?.clone(),
            binding.required("definition")?.clone(),
        ))).collect::<IvyResult<_>>()?;
    Ok(Catalog {
        namespaces,
        tools,
        notifications: value.get("notifications").and_then(IvyJson::as_array)
            .into_iter().flatten().map(|notification| {
                let text = |key| notification.required(key)?.as_js_string()
                    .cloned().ok_or_else(IvyError::internal);
                Ok(NotificationDefinition {
                    namespace: text("namespace")?, name: text("name")?, version: text("version")?,
                    // Historical routing-only fixtures omit the registry description.
                    description: notification.get("description").and_then(IvyJson::as_js_string)
                        .cloned().unwrap_or_else(|| "".into()),
                    payload_schema: Arc::new(notification.required("payloadSchema")?.clone()),
                })
            }).collect::<IvyResult<_>>()?,
    })
}
pub fn catalog_to_wire(value: &Catalog) -> IvyJson {
    IvyJson::object([
        ("namespaces", IvyJson::Array(value.namespaces.iter().map(|ns| ns.name.clone().into()).collect())),
        ("tools", IvyJson::Array(value.tools.iter().map(ToolBinding::to_wire).collect())),
        ("notifications", IvyJson::Array(value.notifications.iter().map(|notification| IvyJson::object([
            ("namespace", notification.namespace.clone().into()),
            ("name", notification.name.clone().into()),
            ("version", notification.version.clone().into()),
            ("description", notification.description.clone().into()),
            ("payloadSchema", notification.payload_schema.as_ref().clone()),
        ])).collect())),
    ])
}
pub fn routing_from_wire(value: &IvyJson) -> IvyResult<RoutingUpdate> {
    Ok(RoutingUpdate {
        node: RoutingNode::from_status(value.required("node")?)?,
        generation: value.get("generation").and_then(IvyJson::as_f64),
        catalog: value.get("catalog").filter(|v| !v.is_null())
            .map(catalog_from_wire).transpose()?.map(Arc::new),
    })
}
pub fn routing_to_wire(value: &RoutingUpdate) -> IvyJson {
    let mut wire = IvyJson::object([("node", node_to_wire(&value.node))]);
    if let Some(generation) = value.generation { wire.insert("generation", generation.into()); }
    if let Some(catalog) = &value.catalog { wire.insert("catalog", catalog_to_wire(catalog)); }
    wire
}
fn node_to_wire(value: &RoutingNode) -> IvyJson {
    IvyJson::object([
        ("serviceNodeId", value.id.clone().into()),
        ("hostId", value.host.clone().map_or(IvyJson::Null, Into::into)),
        ("serviceName", value.service.clone().map_or(IvyJson::Null, Into::into)),
        ("principalId", value.principal.clone().map_or(IvyJson::Null, Into::into)),
        ("connected", value.connected.into()), ("synced", value.synced.into()),
        ("ready", value.ready.into()), ("desiredEnabled", value.enabled.into()),
    ])
}
pub fn project_routing_notification(value: &mut serde_json::Value) {
    // Historical internal notifications carried full status. Compare precisely
    // the identity/flags consumed by routing, leaving all public results intact.
    if let Some(node) = value.pointer_mut("/routing/node") {
        let routing = RoutingNode::from_status(&IvyJson::from_serde(node).unwrap()).unwrap();
        *node = serde_json::from_str(&ivy_contracts::json::stringify(&node_to_wire(&routing), usize::MAX).unwrap()).unwrap();
    }
}

/// Supply the explicit batch field only for the retained, pre-contract traces.
/// Keep their input hashes, original result assertions and compressed oracles intact.
pub fn legacy_inventory_source_times(steps: &mut [serde_json::Value], profile: &mut serde_json::Value) {
    use serde_json::{Value, json};
    for step in steps {
        let now = step["now"].as_f64().unwrap_or(100_000.0) as i64;
        let parameters = if step["op"] == "inventory" && step["method"] == "sync" {
            &mut step["params"]
        } else if step["request"]["method"] == "inventory.sync" {
            &mut step["request"]["params"]
        } else { continue };
        assert!(parameters.get("observedAt").is_none(), "This adapter is only for original traces");
        let entries = parameters["entries"].as_array().unwrap();
        let observed = match entries.iter().filter_map(|entry| entry["observedAt"].as_str()).max() {
            Some(value) => value.to_owned(),
            None => {
                let iso = chrono::DateTime::from_timestamp_millis(now).unwrap()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                let units = json!(iso.encode_utf16().collect::<Vec<_>>());
                // The old empty-batch path formatted the clock without parsing it.
                // The new UTC boundary needs that same finite date observation.
                let dates = profile["dates"].as_array_mut().unwrap();
                if let Some(date) = dates.iter().find(|date| date[0] == units) {
                    assert_eq!(date[1], json!(now));
                } else { dates.push(json!([units, now])); }
                let times = profile["iso"].as_array_mut().unwrap();
                if let Some(time) = times.iter().find(|time| time[0] == now) {
                    assert_eq!(time[1], json!(iso));
                } else { times.push(json!([now, iso])); }
                iso
            }
        };
        parameters["observedAt"] = Value::String(observed);
    }
}
