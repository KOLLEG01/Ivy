//! Pure managed MCP output planning after URL/schema validation and state read.
use crate::environment_text::*;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::digest};
use std::collections::HashSet;

pub struct McpOutputInput<'a> {
    /// Already decoded with the original fatal UTF-8/BOM behavior.
    pub source: &'a str,
    pub state: Option<&'a IvyJson>,
    pub authority: &'a str,
    pub sections: Option<&'a [String]>,
    pub server_names: &'a [JsString],
    pub app_id: Option<&'a str>,
    pub desired_version: &'a str,
}
pub struct McpOutput {
    pub bytes: Vec<u8>,
    pub state: IvyJson,
    pub applied: bool,
    pub managed_bytes: usize,
}
pub(crate) fn require(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("target_conflict", message))
    }
}
pub(crate) fn hash(block: Option<&str>) -> IvyJson {
    block
        .map(|v| IvyJson::from(digest(v)))
        .unwrap_or(IvyJson::Null)
}
pub(crate) fn state_present(value: Option<&IvyJson>) -> bool {
    value.is_some_and(|value| !value.is_null())
}
pub(crate) fn validate_authority(state: Option<&IvyJson>, authority: &str) -> IvyResult<()> {
    let version = state.and_then(|s| s.get("schemaVersion"));
    require(
        !state_present(state)
            || ((version == Some(&IvyJson::Number(2.0)))
                && state.and_then(|s| s.get("authority")) == Some(&IvyJson::from(authority))),
        "Another Hive or host owns this Codex MCP output.",
    )
}

/// Current version-2 ownership and the existing hash-based file checks only.
pub fn plan_mcp_output(input: McpOutputInput<'_>) -> IvyResult<McpOutput> {
    validate_authority(input.state, input.authority)?;
    let block = render_codex(input.sections);
    let block_hash = hash(block.as_deref());
    let applied_hash = input.state.and_then(|s| s.get("appliedBlockHash"));
    let accepted: HashSet<String> = [applied_hash, Some(&block_hash)]
        .into_iter()
        .flatten()
        .filter_map(IvyJson::as_js_string)
        .filter(|s| !s.0.is_empty())
        .filter_map(|s| s.to_utf8().ok())
        .collect();
    let current = codex_block(input.source, &accepted)?;
    require(
        ![BEGIN_MCP, END_MCP, BEGIN_PERMISSIONS, END_PERMISSIONS]
            .iter()
            .any(|marker| current.outside.contains(marker)),
        "Codex configuration requires explicit environment import.",
    )?;
    if input.state.and_then(|s| s.get("schemaVersion")) == Some(&IvyJson::Number(2.0)) {
        let current_hash = hash(current.block.as_deref());
        require(
            Some(&current_hash) == applied_hash || current_hash == block_hash,
            "The Ivy-managed Codex block has local changes.",
        )?;
    }
    finish_mcp_output(input, block, current.outside)
}

pub(crate) fn finish_mcp_output(
    input: McpOutputInput<'_>,
    block: Option<String>,
    mut unmanaged: String,
) -> IvyResult<McpOutput> {
    let block_hash = hash(block.as_deref());
    if block.is_some() {
        let conflicts = server_sections(&unmanaged);
        for name in input.server_names {
            require(
                !conflicts
                    .iter()
                    .any(|value| JsString::from(value.clone()) == *name),
                &format!(
                    "MCP server {} already has an unmanaged configuration.",
                    name.to_utf8()?
                ),
            )?;
        }
        if let Some(app) = input.app_id.filter(|app| !app.is_empty()) {
            require(
                !has_app_section(&unmanaged, app),
                "The hosted Ivy app already has an unmanaged configuration.",
            )?;
        }
        let keys = root_configuration_keys(&unmanaged);
        for key in ["sandbox_mode", "sandbox_workspace_write"] {
            require(
                !keys.iter().any(|value| value == key),
                &format!("Codex {key} already has an unmanaged configuration."),
            )?;
        }
        require(
            !has_legacy_sandbox_table(&unmanaged),
            "Codex sandbox_workspace_write conflicts with the managed permission profile.",
        )?;
        unmanaged = adopt_matching_permissions(&unmanaged)?;
    }
    Ok(McpOutput {
        bytes: with_block(&unmanaged, block.as_deref()).into_bytes(),
        state: IvyJson::object([
            ("schemaVersion", IvyJson::Number(2.0)),
            ("authority", input.authority.into()),
            ("appliedBlockHash", block_hash),
            ("appliedVersion", input.desired_version.into()),
        ]),
        applied: block.is_some(),
        managed_bytes: block.as_ref().map_or(0, String::len),
    })
}
