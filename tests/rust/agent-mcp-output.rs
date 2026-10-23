use ivy_agent_manager::{
    environment_text::render_codex,
    mcp_output::{McpOutput, McpOutputInput, plan_mcp_output},
};
use ivy_contracts::{IvyJson as J, JsString, hash::digest};

fn plan(
    source: &str,
    state: Option<&J>,
    sections: Option<&[String]>,
) -> ivy_contracts::IvyResult<McpOutput> {
    let names = [JsString::from("fixture")];
    plan_mcp_output(McpOutputInput {
        source,
        state,
        authority: "owner",
        sections,
        server_names: &names,
        app_id: Some("application"),
        desired_version: "current",
    })
}

#[test]
fn current_mcp_ownership_updates_managed_content_and_preserves_unmanaged_configuration() {
    let sections = vec!["[mcp_servers.fixture]\nurl = \"https://example.invalid/mcp\"".into()];
    let fresh = plan(
        "# user\ncustom = true\n[other]\nvalue = 'keep'\n",
        None,
        Some(&sections),
    )
    .unwrap();
    assert!(fresh.applied);
    assert_eq!(
        fresh.managed_bytes,
        render_codex(Some(&sections)).unwrap().len()
    );
    assert_eq!(fresh.state.get("schemaVersion"), Some(&J::Number(2.0)));
    assert_eq!(fresh.state.get("authority"), Some(&J::from("owner")));
    assert_eq!(fresh.state.get("appliedVersion"), Some(&J::from("current")));
    assert_eq!(
        fresh.state.get("appliedBlockHash"),
        Some(&J::from(digest(render_codex(Some(&sections)).unwrap())))
    );
    let source = String::from_utf8(fresh.bytes).unwrap();
    let changed = vec!["[mcp_servers.fixture]\nurl = \"https://next.invalid/mcp\"".into()];
    let updated = plan(&source, Some(&fresh.state), Some(&changed)).unwrap();
    let updated_source = String::from_utf8(updated.bytes).unwrap();
    assert!(updated_source.contains("https://next.invalid/mcp"));
    assert!(updated_source.contains("custom = true"));
    assert!(updated_source.contains("[other]\nvalue = 'keep'"));
    let removed = plan(&updated_source, Some(&updated.state), None).unwrap();
    assert!(!removed.applied);
    assert_eq!(removed.managed_bytes, 0);
    let outside = String::from_utf8(removed.bytes).unwrap();
    assert!(!outside.contains("Ivy AgentManager Codex configuration"));
    assert!(outside.contains("custom = true"));
    assert!(outside.contains("[other]\nvalue = 'keep'"));
    assert_eq!(
        plan(
            &source.replace("https://example.invalid/mcp", "https://local.invalid"),
            Some(&fresh.state),
            Some(&changed)
        )
        .err()
        .unwrap()
        .code,
        "target_conflict"
    );
}

#[test]
fn mcp_refuses_foreign_legacy_and_unmanaged_owners_and_adopts_only_matching_permissions() {
    let sections = vec!["[mcp_servers.fixture]\nurl = \"https://example.invalid/mcp\"".into()];
    let fresh = plan("", None, Some(&sections)).unwrap();
    for (key, value) in [
        ("authority", J::from("other")),
        ("schemaVersion", J::Number(1.0)),
    ] {
        let mut state = fresh.state.clone();
        state.insert(key, value);
        assert_eq!(
            plan("", Some(&state), Some(&sections)).err().unwrap().code,
            "target_conflict"
        );
    }
    for source in [
        "[mcp_servers.fixture]\nurl = 'https://user.invalid'\n",
        "[apps.'application']\nenabled = false\n",
        "sandbox_mode = 'workspace-write'\n",
        "[sandbox_workspace_write]\nwritable_roots = []\n",
        "approval_policy = 'ask'\n",
        "# >>> Ivy AgentManager MCP configuration (managed; edit in AgentUI)\n",
    ] {
        assert_eq!(
            plan(source, None, Some(&sections)).err().unwrap().code,
            "target_conflict",
            "{source}"
        );
    }
    let adopted = plan(
        "approval_policy = 'never'\ndefault_permissions = ':danger-full-access'\n# retain\n",
        None,
        Some(&sections),
    )
    .unwrap();
    let source = String::from_utf8(adopted.bytes).unwrap();
    assert!(source.contains("# retain"));
    assert_eq!(source.matches("approval_policy").count(), 1);
    assert_eq!(source.matches("default_permissions").count(), 1);
}
