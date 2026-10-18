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
fn require(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("target_conflict", message))
    }
}
fn hash(block: Option<&str>) -> IvyJson {
    block
        .map(|v| IvyJson::from(digest(v)))
        .unwrap_or(IvyJson::Null)
}
fn state_present(value: Option<&IvyJson>) -> bool {
    match value {
        None | Some(IvyJson::Null) | Some(IvyJson::Bool(false)) => false,
        Some(IvyJson::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(IvyJson::String(s)) => !s.0.is_empty(),
        _ => true,
    }
}
fn legacy_block(begin: &str, sections: Option<&[String]>, end: &str) -> Option<String> {
    sections.map(|sections| {
        let mut parts = vec![begin];
        parts.extend(sections.iter().map(String::as_str));
        parts.push(end);
        parts.join("\n")
    })
}

pub(crate) fn validate_authority(state: Option<&IvyJson>, authority: &str) -> IvyResult<()> {
    let version = state.and_then(|s| s.get("schemaVersion"));
    require(
        !state_present(state)
            || ((version == Some(&IvyJson::Number(1.0)) || version == Some(&IvyJson::Number(2.0)))
                && state.and_then(|s| s.get("authority")) == Some(&IvyJson::from(authority))),
        "Another Hive or host owns this Codex MCP output.",
    )
}

/// Retain the version-1/version-2 state meanings and existing recovery-by-hash
/// checks. The caller owns filesystem locking, compare/replace and state commit.
pub fn plan_mcp_output(input: McpOutputInput<'_>) -> IvyResult<McpOutput> {
    let block = render_codex(input.sections);
    let block_hash = hash(block.as_deref());
    let old_mcp_expected = legacy_block(BEGIN_MCP, input.sections, END_MCP);
    let old_permissions_expected = input.sections.map(|_| {
        format!("{BEGIN_PERMISSIONS}\napproval_policy = \"never\"\ndefault_permissions = \":danger-full-access\"\n{END_PERMISSIONS}")
    });
    let state_version = input.state.and_then(|s| s.get("schemaVersion"));
    let v1 = state_version == Some(&IvyJson::Number(1.0));
    let v2 = state_version == Some(&IvyJson::Number(2.0));
    validate_authority(input.state, input.authority)?;
    let applied_hash = input.state.and_then(|s| s.get("appliedBlockHash"));
    let accepted: HashSet<String> = [applied_hash, Some(&block_hash)]
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_js_string())
        .filter(|s| !s.0.is_empty())
        .filter_map(|s| s.to_utf8().ok())
        .collect();
    let current = codex_block(input.source, &accepted)?;
    let old_mcp = managed_block(&current.outside, BEGIN_MCP, END_MCP)?;
    let old_permissions = managed_block(&old_mcp.outside, BEGIN_PERMISSIONS, END_PERMISSIONS)?;
    require(
        current.block.is_none() || (old_mcp.block.is_none() && old_permissions.block.is_none()),
        "Codex configuration contains both current and legacy Ivy blocks.",
    )?;
    require(
        !v2 || (old_mcp.block.is_none() && old_permissions.block.is_none()),
        "Codex configuration has unexpected legacy Ivy blocks.",
    )?;
    if v1 && current.block.is_some() {
        require(
            hash(current.block.as_deref()) == block_hash,
            "The Ivy-managed Codex block has local changes.",
        )?;
    } else if v1 {
        let mcp_hash = hash(old_mcp.block.as_deref());
        require(
            Some(&mcp_hash) == applied_hash || mcp_hash == hash(old_mcp_expected.as_deref()),
            "The Ivy-managed MCP block has local changes.",
        )?;
        let permissions_hash = hash(old_permissions.block.as_deref());
        let absent_permissions = IvyJson::Null;
        let applied_permissions = input
            .state
            .and_then(|s| s.get("appliedPermissionsBlockHash"))
            .filter(|v| !v.is_null())
            .unwrap_or(&absent_permissions);
        require(
            &permissions_hash == applied_permissions
                || permissions_hash == hash(old_permissions_expected.as_deref()),
            "The Ivy-managed permission block has local changes.",
        )?;
    } else if v2 {
        let current_hash = hash(current.block.as_deref());
        require(
            Some(&current_hash) == applied_hash || current_hash == block_hash,
            "The Ivy-managed Codex block has local changes.",
        )?;
    }
    let mut unmanaged = old_permissions.outside;
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
