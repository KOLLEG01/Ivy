//! Explicit MCP layout conversion. Normal refresh never invokes this planner.
use crate::{
    environment_text::*,
    mcp_output::{McpOutput, McpOutputInput, finish_mcp_output, hash, require, state_present},
};
use ivy_contracts::{IvyJson, IvyResult, hash::digest};
use std::collections::HashSet;

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

/// The original v1/v2 conversion and recovery rules, only for explicit import.
pub fn plan_mcp_import(input: McpOutputInput<'_>) -> IvyResult<McpOutput> {
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
    finish_mcp_output(input, block, old_permissions.outside)
}
