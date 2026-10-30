//! Read namespace names and summaries without decoding opaque definitions.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use serde::{Deserialize, de::IgnoredAny};
use serde_json::value::RawValue;

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
#[derive(Deserialize)]
struct Named<'a> {
    #[serde(borrow)]
    namespace: &'a RawValue,
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
