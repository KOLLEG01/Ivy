use ivy_agent_manager::environment_text::{
    adopt_matching_permissions, has_app_section, has_legacy_sandbox_table, root_configuration_keys,
    server_sections,
};

#[test]
fn unmanaged_sections_and_root_keys_use_toml_lines_and_preserve_names() {
    let source = "# user\r\napproval_policy = 'never'\r\n\"default_permissions\" = \":danger-full-access\"\r\nsandbox_mode = 'workspace-write'\r\n\t[mcp_servers.\"Mixed-Case\"]\r\nurl = 'https://example.invalid'\r\n[mcp_servers.other.env]\r\nVALUE = 'x'\r\n[apps.'application']\r\nenabled = false\r\n[sandbox_workspace_write]\r\n";
    assert_eq!(server_sections(source), ["Mixed-Case", "other"]);
    assert_eq!(
        root_configuration_keys(source),
        ["approval_policy", "default_permissions", "sandbox_mode"]
    );
    assert!(has_legacy_sandbox_table(source));
    assert!(has_app_section(source, "application"));
    assert!(!has_app_section(source, "app"));
    assert!(!has_app_section(source, "Application"));
    assert!(root_configuration_keys("[other]\napproval_policy = 'ask'").is_empty());
    assert!(!has_legacy_sandbox_table(
        "[sandbox_workspace_write_other]\n"
    ));
    // Unicode separators are content, rather than extra TOML/regex line starts.
    assert!(server_sections("value = 'x\u{2028}[mcp_servers.hidden]'").is_empty());
    assert!(!has_app_section(
        "# comment\u{2029}[apps.application]",
        "application"
    ));
}

#[test]
fn permission_adoption_preserves_unmanaged_text_and_refuses_changed_or_duplicate_keys() {
    for newline in ["\n", "\r\n"] {
        let tail = format!("# keep{newline}[other]{newline}approval_policy = 'ask'{newline}");
        let source = format!(
            "approval_policy = 'never'{newline}\"default_permissions\" = \":danger-full-access\"{newline}{tail}"
        );
        assert_eq!(adopt_matching_permissions(&source).unwrap(), tail);
    }
    for source in [
        "approval_policy = 'ask'\n",
        "approval_policy = 'never' # locally annotated\n",
        "approval_policy = 'never'\napproval_policy = 'never'\n",
        "default_permissions = 'other'\n",
        "default_permissions = ':danger-full-access'\n'default_permissions' = ':danger-full-access'\n",
    ] {
        assert_eq!(
            adopt_matching_permissions(source).unwrap_err().code,
            "target_conflict"
        );
    }
    let source = "# untouched\ncustom = 'value'\n[other]\n";
    assert_eq!(adopt_matching_permissions(source).unwrap(), source);
}
