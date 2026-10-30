//! Borrow search metadata while schemas and other opaque catalog values are skipped.
use super::*;
use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Catalog<'a> {
    #[serde(borrow)]
    namespaces: Vec<Namespace<'a>>,
    discovery_hint: Option<&'a RawValue>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Namespace<'a> {
    #[serde(borrow)]
    namespace: &'a RawValue,
    description: &'a RawValue,
    discovery_groups: Option<Vec<Group<'a>>>,
    tools: Vec<Tool<'a>>,
}
#[derive(Deserialize)]
struct Group<'a> {
    #[serde(borrow)]
    group: &'a RawValue,
    description: &'a RawValue,
}
#[derive(Deserialize)]
struct Tool<'a> {
    #[serde(borrow)]
    name: &'a RawValue,
    description: &'a RawValue,
    annotations: Option<&'a RawValue>,
    discovery: Option<&'a RawValue>,
}
fn value(raw: &RawValue) -> IvyResult<IvyJson> {
    parse(raw.get(), usize::MAX).map_err(|_| IvyError::internal())
}
fn string_value(raw: &RawValue) -> IvyResult<JsString> {
    value(raw)?.into_js_string().ok_or_else(IvyError::internal)
}

pub(super) fn read(source: &str, identity: String, runtime: &dyn Runtime) -> IvyResult<Index> {
    // Registries are validated and canonicalized on sync. Raw strings preserve
    // their external UTF-16 semantics; serde never decodes skipped schema text.
    let catalog = if source.is_empty() || source == "null" {
        Catalog { namespaces: vec![], discovery_hint: None }
    } else {
        serde_json::from_str::<Catalog<'_>>(source).map_err(|_| IvyError::internal())?
    };
    let mut declared = vec![];
    let mut tools = vec![];
    let mut descriptions = vec![];
    for ns in catalog.namespaces {
        let namespace = string_value(ns.namespace)?;
        let mut groups = vec![];
        for group in ns.discovery_groups.unwrap_or_default() {
            let key = string_value(group.group)?;
            let description = string_value(group.description)?;
            if let Some((_, prior)) = groups.iter_mut().find(|(k, _)| k == &key) {
                *prior = description;
            } else {
                groups.push((key, description));
            }
        }
        for (key, description) in &groups {
            if let Some((_, prior)) = declared.iter_mut().find(|(k, _)| k == key) {
                *prior = description.clone();
            } else {
                declared.push((key.clone(), description.clone()));
            }
        }
        descriptions.push(string_value(ns.description)?);
        for tool in ns.tools {
            let discovery = tool.discovery.map(value).transpose()?;
            let annotations = tool.annotations.map(value).transpose()?;
            let mut entry = indexed(
                &namespace,
                &string_value(tool.name)?,
                &string_value(tool.description)?,
                annotations.as_ref().and_then(|v| v.get("readOnlyHint")),
                discovery.as_ref(),
                &groups,
                runtime,
            )?;
            entry.mcp = discovery.as_ref().and_then(|v| v.get("mcp"))
                .filter(|v| v.as_object().is_some()).cloned();
            tools.push(entry);
        }
    }
    let hint = catalog.discovery_hint.map(value).transpose()?
        .and_then(IvyJson::into_js_string)
        .map(|v| v.trim_ecmascript())
        .filter(|v| !v.0.is_empty());
    let descriptions = joined(&descriptions, "; ");
    let description = compact(&hint.unwrap_or_else(|| {
        if descriptions.0.is_empty() {
            "No synchronized tool catalog.".into()
        } else {
            descriptions
        }
    }));
    Ok(Index { identity, tools, groups: declared, description, bytes: 0 })
}
