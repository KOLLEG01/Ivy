//! The unchanged AgentManager registry and installed native tool projection.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, agent_bundle::bundled_schema,
    agent_schema::validate_agent, browser_notice::browser_notice_topic, hash::hash_json,
    json::parse, native_contract::native_tool_definition,
};
use std::sync::OnceLock;

pub const NATIVE_INVENTORY_SCHEMA_VERSION: &str = "1.1.0";
pub const NATIVE_PROJECT_SCHEMA_VERSION: &str = "2.0.0";

pub fn agent_schema(name: &JsString) -> IvyResult<IvyJson> {
    let mut reference = JsString::from("#/$defs/");
    reference.0.extend_from_slice(&name.0);
    bundled_schema(
        &reference,
        Some(&"https://ivy.invalid/schemas/agent.schema.json".into()),
    )
}

struct Metadata {
    registry: IvyJson,
    summaries: IvyJson,
}

fn metadata() -> IvyResult<&'static Metadata> {
    static METADATA: OnceLock<IvyResult<Metadata>> = OnceLock::new();
    METADATA
        .get_or_init(|| {
            let text = include_str!("registry_metadata.json");
            let mut document = parse(text, text.len())?;
            let mut registry = document.remove("registry").ok_or_else(IvyError::internal)?;
            fill_schemas(&mut registry)?;
            Ok(Metadata {
                registry,
                summaries: document
                    .remove("nativeSummaries")
                    .ok_or_else(IvyError::internal)?,
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}

// Only the checked-in metadata is a template. Provider schemas are inserted
// afterward and may themselves contain arbitrary keys resembling these markers.
fn fill_schemas(value: &mut IvyJson) -> IvyResult<()> {
    if let Some(name) = value.get("$agentSchema").and_then(IvyJson::as_js_string) {
        *value = agent_schema(name)?;
    } else if let Some(namespace) = value
        .get("$browserNoticeTopic")
        .and_then(IvyJson::as_js_string)
    {
        *value = browser_notice_topic(namespace);
    } else {
        match value {
            IvyJson::Array(values) => {
                for value in values {
                    fill_schemas(value)?;
                }
            }
            IvyJson::Object(pairs) => {
                for (_, value) in pairs {
                    fill_schemas(value)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn matches(value: &[u16], expected: &str) -> bool {
    value.iter().copied().eq(expected.encode_utf16())
}

fn starts_ascii_insensitive(value: &[u16], prefix: &str) -> bool {
    value.len() >= prefix.len()
        && value.iter().zip(prefix.bytes()).all(|(actual, expected)| {
            let actual = if (65..=90).contains(actual) {
                actual + 32
            } else {
                *actual
            };
            actual == u16::from(expected)
        })
}

fn codex_group(name: &JsString) -> JsString {
    let mut parts = name.0.split(|unit| *unit == 47);
    let head = parts.next().unwrap_or_default();
    let action = parts.next().unwrap_or_default();
    if matches(head, "thread") {
        return if matches(action, "realtime") {
            "codex/tasks/voice"
        } else if [
            "read", "items", "turns", "history", "messages", "loaded", "output",
        ]
        .iter()
        .any(|prefix| starts_ascii_insensitive(action, prefix))
        {
            "codex/tasks/content"
        } else if ["name", "metadata", "goal", "settings", "config", "set"]
            .iter()
            .any(|prefix| starts_ascii_insensitive(action, prefix))
        {
            "codex/tasks/settings"
        } else {
            "codex/tasks/lifecycle"
        }
        .into();
    }
    for (heads, group) in [
        (&["project", "threadSection"][..], "codex/projects"),
        (&["turn"][..], "codex/turns"),
        (
            &["account", "model", "modelProvider", "getAuthStatus"][..],
            "codex/models-account",
        ),
        (
            &[
                "config",
                "configRequirements",
                "environment",
                "permissionProfile",
                "externalAgentConfig",
                "windowsSandbox",
                "experimentalFeature",
            ][..],
            "codex/configuration",
        ),
        (
            &[
                "ui",
                "plugin",
                "marketplace",
                "skills",
                "mcpServer",
                "mcpServerStatus",
            ][..],
            "codex/extensions",
        ),
        (
            &[
                "fs",
                "command",
                "process",
                "fuzzyFileSearch",
                "gitDiffToRemote",
            ][..],
            "codex/files-commands",
        ),
    ] {
        if heads.iter().any(|expected| matches(head, expected)) {
            return group.into();
        }
    }
    let mut group = JsString::from("codex/");
    group.0.extend_from_slice(head);
    group
}

fn slash_words(value: &JsString) -> JsString {
    let mut text = Vec::with_capacity(value.0.len());
    for unit in &value.0 {
        if *unit == 47 {
            text.extend([32, 47, 32]);
        } else {
            text.push(*unit);
        }
    }
    JsString(text)
}

fn codex_summary(name: &JsString, group: &JsString, metadata: &Metadata) -> IvyResult<JsString> {
    if let Some(summary) = metadata
        .summaries
        .get_key(name)
        .and_then(IvyJson::as_js_string)
    {
        return Ok(summary.clone());
    }
    let expanded = slash_words(name);
    let mut summary = JsString(Vec::with_capacity(expanded.0.len()));
    for (index, unit) in expanded.0.iter().enumerate() {
        if index > 0 && (97..=122).contains(&expanded.0[index - 1]) && (65..=90).contains(unit) {
            summary.0.push(32);
        }
        summary.0.push(*unit);
    }
    summary.0.extend(": ".encode_utf16());
    let groups = metadata
        .registry
        .required("namespaces")?
        .as_array()
        .unwrap()[0]
        .required("discoveryGroups")?
        .as_array()
        .unwrap();
    let description = groups
        .iter()
        .find(|entry| entry.get("group").and_then(IvyJson::as_js_string) == Some(group))
        .and_then(|entry| entry.get("description"))
        .and_then(IvyJson::as_js_string)
        .cloned()
        .unwrap_or_else(|| "Native Codex tools.".into());
    summary.0.extend(description.0);
    Ok(summary)
}

pub fn agent_registry(catalog: &IvyJson) -> IvyResult<IvyJson> {
    validate_agent("Catalog", catalog)?;
    let catalog_hash = JsString::from(hash_json(catalog)?);
    let metadata = metadata()?;
    let mut registry = metadata.registry.clone();
    let mut namespaces = registry.remove("namespaces").unwrap().into_array().unwrap();
    let mut groups = namespaces[0]
        .remove("discoveryGroups")
        .unwrap()
        .into_array()
        .unwrap();
    let mut tools = Vec::new();
    for method in catalog.required("clientRequests")?.as_array().unwrap() {
        let name = method.required("method")?.as_js_string().unwrap();
        let group = codex_group(name);
        let summary = codex_summary(name, &group, metadata)?;
        let mut definition = native_tool_definition(catalog, method, Some(&catalog_hash))?;
        definition.insert(
            "discovery",
            IvyJson::object([
                ("group", IvyJson::String(group.clone())),
                ("summary", IvyJson::String(summary)),
            ]),
        );
        tools.push(definition);
        if !groups
            .iter()
            .any(|entry| entry.get("group").and_then(IvyJson::as_js_string) == Some(&group))
        {
            let mut description = slash_words(&group);
            description.0.extend(" native Codex tools.".encode_utf16());
            groups.push(IvyJson::object([
                ("group", IvyJson::String(group)),
                ("description", IvyJson::String(description)),
            ]));
        }
    }
    namespaces[0].insert("tools", IvyJson::Array(tools));
    namespaces[0].insert("discoveryGroups", IvyJson::Array(groups));
    registry.insert("namespaces", IvyJson::Array(namespaces));
    Ok(registry)
}
