//! MCP TOML rendering and credential destination policy.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, json::stringify, whatwg_url::parse_url,
};
use std::collections::HashSet;

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn quoted(value: &JsString) -> IvyResult<String> {
    // Preserve JSON.stringify's escaping, including isolated UTF-16 surrogates.
    stringify(&IvyJson::String(value.clone()), usize::MAX)
}

/// Match the original single trailing-slash removal before resolving the two
/// Hive resources. Authorization compares complete normalized URL strings.
pub fn hive_mcp_urls(hive_url: &JsString) -> IvyResult<HashSet<String>> {
    let base = if hive_url.0.last() == Some(&47) {
        &hive_url.0[..hive_url.0.len() - 1]
    } else {
        &hive_url.0[..]
    };
    ["/mcp", "/mcp-dev"]
        .into_iter()
        .map(|suffix| {
            let mut input = base.to_vec();
            input.extend(suffix.encode_utf16());
            Ok(parse_url(&JsString(input))?.href)
        })
        .collect()
}

/// Inputs have passed the shared document and OpenAI snapshot schemas. Disabled
/// documents return before duplicate/URL policy checks, as in the original.
pub fn render_mcp_sections(
    document: &IvyJson,
    hive_urls: &HashSet<String>,
    credential: &JsString,
    app_id: Option<&JsString>,
    credentials: &crate::mcp_credentials::McpCredentials,
) -> IvyResult<Option<Vec<String>>> {
    if document.get("enabled") != Some(&IvyJson::Bool(true)) {
        return Ok(None);
    }
    let servers = field(document, "servers")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let mut names = HashSet::new();
    let mut blocks = Vec::new();
    for server in servers {
        let name = text(server, "name")?;
        if !names.insert(name.clone()) {
            return Err(IvyError::new(
                "invalid_arguments",
                "Managed MCP server names must be unique.",
            ));
        }
        let resolved =
            crate::mcp_credentials::resolve_mcp_server(server, hive_urls, credential, credentials)?;
        let mut lines = vec![
            format!("[mcp_servers.{}]", quoted(name)?),
            format!("url = {}", quoted(&resolved.url.into())?),
        ];
        if let Some(authorization) = resolved.authorization {
            lines.push(format!(
                "http_headers = {{ Authorization = {} }}",
                quoted(&authorization.into())?
            ));
        }
        for (key, property) in [
            ("enabled", "enabled"),
            ("startup_timeout_sec", "startupTimeoutSeconds"),
            ("tool_timeout_sec", "toolTimeoutSeconds"),
        ] {
            lines.push(format!(
                "{key} = {}",
                stringify(field(server, property)?, usize::MAX)?
            ));
        }
        blocks.push(lines.join("\n"));
    }
    let mut sections = Vec::new();
    if let Some(app_id) = app_id.filter(|value| !value.0.is_empty()) {
        sections.extend([
            format!("[apps.{}]", quoted(app_id)?),
            "enabled = false".into(),
            String::new(),
        ]);
    }
    sections.extend(blocks);
    Ok(Some(sections))
}
