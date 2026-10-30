//! Project historical JSON traces onto the native internal catalog in tests only.
#![allow(dead_code)]
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_hive::{registry_catalog::{Catalog, Namespace, NotificationDefinition, ToolBinding}, storage::RoutingUpdate};
use std::sync::Arc;

pub fn catalog_from_wire(value: &IvyJson) -> IvyResult<Catalog> {
    let namespaces = value.get("namespaces").and_then(IvyJson::as_array)
        .into_iter().flatten().map(|name| Ok(Namespace {
            name: name.as_js_string().ok_or_else(IvyError::internal)?.clone(),
            guide_markdown: None,
            tools: 0..0,
        })).collect::<IvyResult<_>>()?;
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
        node: value.required("node")?.clone(),
        generation: value.get("generation").and_then(IvyJson::as_f64),
        catalog: value.get("catalog").filter(|v| !v.is_null())
            .map(catalog_from_wire).transpose()?.map(Arc::new),
    })
}
pub fn routing_to_wire(value: &RoutingUpdate) -> IvyJson {
    let mut wire = IvyJson::object([("node", value.node.clone())]);
    if let Some(generation) = value.generation { wire.insert("generation", generation.into()); }
    if let Some(catalog) = &value.catalog { wire.insert("catalog", catalog_to_wire(catalog)); }
    wire
}
