//! Read registry metadata without decoding unrelated definitions.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use serde::{Deserialize, de::IgnoredAny};
use serde_json::value::RawValue;
use crate::registry_catalog::{Catalog as LiveCatalog, Namespace, NotificationDefinition, ToolBinding};
use std::collections::HashMap;

#[derive(Deserialize)]
struct Catalog<'a> {
    #[serde(borrow)]
    namespaces: Vec<&'a RawValue>,
}
#[derive(Deserialize)]
struct Name<'a> {
    #[serde(borrow)]
    namespace: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct Names<'a> {
    #[serde(borrow)]
    namespaces: Option<Vec<&'a RawValue>>,
}
#[cfg(test)]
#[derive(Deserialize)]
struct Named<'a> {
    #[serde(borrow)]
    namespace: &'a RawValue,
}
#[derive(Deserialize)]
struct Tools<'a> {
    #[serde(borrow)]
    tools: Vec<&'a RawValue>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LiveNamespace<'a> {
    #[serde(borrow)]
    namespace: &'a RawValue,
    #[serde(borrow)]
    tools: Vec<&'a RawValue>,
    #[serde(borrow)]
    guide_markdown: Option<&'a RawValue>,
    #[serde(borrow)]
    notifications: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct ToolDiscovery<'a> {
    #[serde(borrow)]
    discovery: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct DiscoveryMcp<'a> {
    #[serde(borrow)]
    mcp: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct McpName<'a> {
    #[serde(borrow)]
    name: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct TopicFields<'a> {
    #[serde(default, borrow, deserialize_with = "present_raw")]
    namespace: Option<&'a RawValue>,
    #[serde(borrow)]
    topics: Option<&'a RawValue>,
}
fn present_raw<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(de).map(Some)
}

pub(crate) struct TopicNamespace {
    pub namespace: Option<IvyJson>,
    pub topics: Vec<IvyJson>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Summary<'a> {
    #[serde(borrow)]
    description: &'a RawValue,
    guide_markdown: &'a RawValue,
    tools: Vec<IgnoredAny>,
    topics: Vec<IgnoredAny>,
}

pub(super) struct NamespaceSummary {
    pub description: IvyJson,
    pub guide_markdown: IvyJson,
    pub tool_count: usize,
    pub topic_count: usize,
}

pub(super) fn value(raw: &RawValue) -> IvyResult<IvyJson> {
    parse(raw.get(), usize::MAX).map_err(|_| IvyError::internal())
}

fn live_value(raw: &RawValue) -> IvyResult<IvyJson> {
    let mut value = value(raw)?;
    // Moving parsed schemas must not retain more spare capacity than the old
    // cloned catalogs. Compact at this long-lived ownership boundary.
    {
        let mut pending = vec![&mut value];
        while let Some(value) = pending.pop() {
            match value {
                IvyJson::String(text) => text.0.shrink_to_fit(),
                IvyJson::Array(values) => {
                    values.shrink_to_fit();
                    pending.extend(values.iter_mut());
                }
                IvyJson::Object(fields) => {
                    fields.shrink_to_fit();
                    for (key, value) in fields { key.0.shrink_to_fit(); pending.push(value); }
                }
                _ => {}
            }
        }
    }
    Ok(value)
}

pub(super) fn live_catalog(source: &str) -> IvyResult<Option<LiveCatalog>> {
    let source = source.trim_matches([' ', '\t', '\r', '\n']);
    if source == "null" {
        return Ok(None);
    }
    if !source.starts_with('{') {
        return Err(IvyError::internal());
    }
    let catalog: Catalog<'_> = serde_json::from_str(source).map_err(|_| IvyError::internal())?;
    let mut namespaces = Vec::with_capacity(catalog.namespaces.len());
    let mut bindings = HashMap::new();
    // Hash all tools before reading notifications, as in the original builder.
    for raw in catalog.namespaces {
        if !raw.get().starts_with('{') {
            return Err(IvyError::internal());
        }
        let ns: LiveNamespace<'_> = serde_json::from_str(raw.get()).map_err(|_| IvyError::internal())?;
        let name = live_value(ns.namespace)?.into_js_string().ok_or_else(IvyError::internal)?;
        let tools = ns.tools.into_iter().map(|raw| ToolBinding::from_definition(live_value(raw)?))
            .collect::<IvyResult<Vec<_>>>()?;
        bindings.insert(name.clone(), tools);
        namespaces.push((name, ns.guide_markdown, ns.notifications));
    }
    let mut result = LiveCatalog { namespaces: Vec::with_capacity(namespaces.len()), tools: Vec::new(), notifications: Vec::new() };
    for (name, guide, notifications) in namespaces {
        if let Some(raw) = notifications {
            let values: Vec<&RawValue> = serde_json::from_str(raw.get()).map_err(|_| IvyError::internal())?;
            for raw in values {
                result.notifications.push(NotificationDefinition::new(&name, live_value(raw)?)?);
            }
        }
        let start = result.tools.len();
        result.tools.extend(bindings.remove(&name).unwrap_or_default());
        result.namespaces.push(Namespace {
            name,
            tools: start..result.tools.len(),
            guide_markdown: guide.filter(|raw| raw.get().starts_with('"'))
                .map(|raw| live_value(raw)?.into_js_string().ok_or_else(IvyError::internal)).transpose()?,
        });
    }
    Ok(Some(result))
}

#[cfg(test)]
pub(super) fn names(source: &str) -> IvyResult<Vec<IvyJson>> {
    let source = source.trim_matches([' ', '\t', '\r', '\n']);
    if source == "null" {
        return Ok(Vec::new());
    }
    if !source.starts_with('{') {
        return Err(IvyError::internal());
    }
    let catalog: Names<'_> = serde_json::from_str(source).map_err(|_| IvyError::internal())?;
    catalog.namespaces.unwrap_or_default().into_iter().map(|raw| {
        if !raw.get().starts_with('{') {
            return Err(IvyError::internal());
        }
        let named: Named<'_> = serde_json::from_str(raw.get()).map_err(|_| IvyError::internal())?;
        value(named.namespace)
    }).collect()
}

pub(super) fn mcp_names(source: &str) -> IvyResult<Vec<JsString>> {
    let source = source.trim_matches([' ', '\t', '\r', '\n']);
    if source == "null" {
        return Ok(Vec::new());
    }
    if !source.starts_with('{') {
        return Err(IvyError::internal());
    }
    let catalog: Catalog<'_> = serde_json::from_str(source).map_err(|_| IvyError::internal())?;
    let mut names = Vec::new();
    for raw in catalog.namespaces {
        if !raw.get().starts_with('{') {
            return Err(IvyError::internal());
        }
        let namespace: Tools<'_> = serde_json::from_str(raw.get()).map_err(|_| IvyError::internal())?;
        for raw in namespace.tools {
            if !raw.get().starts_with('{') {
                return Err(IvyError::internal());
            }
            let tool: ToolDiscovery<'_> = serde_json::from_str(raw.get()).map_err(|_| IvyError::internal())?;
            let Some(raw) = tool.discovery.filter(|raw| raw.get().starts_with('{')) else { continue };
            let discovery: DiscoveryMcp<'_> = serde_json::from_str(raw.get()).map_err(|_| IvyError::internal())?;
            let Some(raw) = discovery.mcp.filter(|raw| raw.get().starts_with('{')) else { continue };
            let mcp: McpName<'_> = serde_json::from_str(raw.get()).map_err(|_| IvyError::internal())?;
            if let Some(raw) = mcp.name.filter(|raw| raw.get().starts_with('"')) {
                let name = value(raw)?.into_js_string().ok_or_else(IvyError::internal)?;
                if !name.0.is_empty() { names.push(name); }
            }
        }
    }
    Ok(names)
}

pub(super) fn find<'a>(source: &'a str, namespace: &JsString) -> IvyResult<Option<&'a RawValue>> {
    // Stored registries are validated and canonicalized on sync. Borrow the
    // SQLite row; neither its source nor an unselected definition is copied.
    let source = source.trim_matches([' ', '\t', '\r', '\n']);
    if source == "null" {
        return Ok(None);
    }
    if !source.starts_with('{') {
        return Err(IvyError::internal());
    }
    let catalog = serde_json::from_str::<Catalog<'_>>(source)
        .map_err(|_| IvyError::internal())?;
    for raw in catalog.namespaces {
        if !raw.get().starts_with('{') {
            return Err(IvyError::internal());
        }
        let name: Name<'_> = serde_json::from_str(raw.get()).map_err(|_| IvyError::internal())?;
        let raw_name = name.namespace.filter(|raw| raw.get().starts_with('"'));
        if raw_name.map(value).transpose()?.as_ref().and_then(IvyJson::as_js_string) == Some(namespace) {
            return Ok(Some(raw));
        }
    }
    Ok(None)
}

pub(super) fn topics(source: &str, namespace: Option<&JsString>) -> IvyResult<Vec<TopicNamespace>> {
    let source = source.trim_matches([' ', '\t', '\r', '\n']);
    if source == "null" {
        return Ok(Vec::new());
    }
    if !source.starts_with('{') {
        return Err(IvyError::internal());
    }
    let catalog: Names<'_> = serde_json::from_str(source).map_err(|_| IvyError::internal())?;
    let mut selected = Vec::new();
    for raw in catalog.namespaces.unwrap_or_default() {
        if !raw.get().starts_with('{') {
            continue;
        }
        let named: TopicFields<'_> = serde_json::from_str(raw.get()).map_err(|_| IvyError::internal())?;
        let name = named.namespace.map(value).transpose()?;
        if namespace.is_some_and(|namespace| name.as_ref().and_then(IvyJson::as_js_string) != Some(namespace)) {
            continue;
        }
        let topics = named.topics.map(|raw| serde_json::from_str::<Vec<&RawValue>>(raw.get())
            .map_err(|_| IvyError::internal())).transpose()?.unwrap_or_default();
        selected.push(TopicNamespace {
            namespace: name,
            topics: topics.into_iter().map(value).collect::<IvyResult<_>>()?,
        });
    }
    Ok(selected)
}

pub(super) fn summary(raw: &RawValue) -> IvyResult<NamespaceSummary> {
    let summary: Summary<'_> = serde_json::from_str(raw.get()).map_err(|_| IvyError::internal())?;
    Ok(NamespaceSummary {
        description: value(summary.description)?,
        guide_markdown: value(summary.guide_markdown)?,
        tool_count: summary.tools.len(),
        topic_count: summary.topics.len(),
    })
}

pub(super) fn not_found() -> IvyError {
    IvyError::new("not_found", "Namespace is not in the selected provider catalog.")
}
