use crate::{IvyError, IvyResult, JsString, json::encode_uri_component};

/// Top-level routes owned by Hive, unavailable as UI slugs.
pub const RESERVED_UI_SLUGS: &[&str] = &[
    "api",
    "mcp",
    "mcp-dev",
    "mcp-icon.svg",
    "mcp-dev-icon.svg",
    "ui",
    "console",
    "login",
    "logout",
    "oauth",
    "health",
    ".well-known",
    "app",
    "app-worker.js",
];

pub fn ui_path(ui_id: &JsString, slug: Option<&JsString>) -> IvyResult<String> {
    let slug = slug.filter(|value| !value.0.is_empty());
    let encoded = encode_uri_component(slug.unwrap_or(ui_id)).map_err(|_| IvyError::internal())?;
    Ok(format!(
        "{}{encoded}/",
        if slug.is_some() { "" } else { "ui/" }
    ))
}
