use ivy_agent_manager::environment_text::{
    BEGIN_CODEX, BEGIN_MCP, BEGIN_PERMISSIONS, END_CODEX, END_MCP, END_PERMISSIONS, codex_block,
    managed_block, render_codex, with_block,
};
use ivy_contracts::hash::digest;
use std::collections::HashSet;

#[test]
fn managed_toml_markers_preserve_unmanaged_text_and_reject_ambiguous_ownership() {
    for (begin, end) in [
        (BEGIN_CODEX, END_CODEX),
        (BEGIN_MCP, END_MCP),
        (BEGIN_PERMISSIONS, END_PERMISSIONS),
    ] {
        let untouched = "# user 😀\r\nroot = true\r\n";
        let found = managed_block(untouched, begin, end).unwrap();
        assert!(found.block.is_none());
        assert_eq!(found.outside, untouched);
        let block = format!("{begin}\r\nmanaged = true\r\n{end}");
        let found =
            managed_block(&format!("{untouched}{block}\n[other]\nx = 1\n"), begin, end).unwrap();
        assert_eq!(found.block.as_deref(), Some(block.as_str()));
        assert_eq!(found.outside, format!("{untouched}\n[other]\nx = 1\n"));
        for malformed in [
            begin.to_owned(),
            end.to_owned(),
            format!("{end}{begin}"),
            format!("{begin}{block}"),
            format!("{block}{end}"),
            format!("{block}\n{block}"),
        ] {
            assert_eq!(
                managed_block(&malformed, begin, end).err().unwrap().code,
                "target_conflict"
            );
        }
    }
}

#[test]
fn codex_recovery_requires_known_content_and_keeps_local_root_and_tables() {
    let sections =
        vec!["[mcp_servers.\"fixture\"]\nurl = \"https://example.invalid/mcp\"".to_owned()];
    assert!(render_codex(None).is_none());
    let block = render_codex(Some(&sections)).unwrap();
    let accepted = HashSet::from([digest(&block)]);
    for source in [block.clone(), block.trim_end_matches(END_CODEX).to_owned()] {
        let found = codex_block(&source, &accepted).unwrap();
        assert_eq!(found.block.as_deref(), Some(block.as_str()));
        assert!(found.outside.trim().is_empty());
    }
    let crlf = block.replace('\n', "\r\n");
    let owned_crlf = codex_block(&crlf, &HashSet::from([digest(&crlf)])).unwrap();
    assert_eq!(owned_crlf.block.as_deref(), Some(crlf.as_str()));
    assert_eq!(
        codex_block(&crlf, &accepted).err().unwrap().code,
        "target_conflict"
    );
    let source = block
        .replace(BEGIN_CODEX, &format!("{BEGIN_CODEX}\nuser_key = \"keep\""))
        .replace(
            END_CODEX,
            &format!("[other]\nvalue = \"keep 😀\"\n{END_CODEX}"),
        );
    let found = codex_block(&source, &accepted).unwrap();
    assert_eq!(found.block.as_deref(), Some(block.as_str()));
    assert!(found.outside.contains("user_key = \"keep\""));
    assert!(found.outside.contains("[other]\nvalue = \"keep 😀\""));
    for changed in [
        block.replace("approval_policy = \"never\"", "approval_policy = \"ask\""),
        block.replace(
            "approval_policy = \"never\"",
            "approval_policy = \"never\"\napproval_policy = \"never\"",
        ),
        block.replace(
            "default_permissions = \":danger-full-access\"",
            "default_permissions = \":danger-full-access\" # edit",
        ),
        block.replace("https://example.invalid/mcp", "https://other.invalid/mcp"),
        format!("{block}{END_CODEX}"),
    ] {
        assert_eq!(
            codex_block(&changed, &accepted).err().unwrap().code,
            "target_conflict"
        );
    }
    assert_eq!(
        codex_block(&block, &HashSet::new()).err().unwrap().code,
        "target_conflict"
    );
}

#[test]
fn managed_output_uses_file_lines_and_retains_bom_root_and_table_placement() {
    for newline in ["\n", "\r\n"] {
        let source = format!(
            "\u{feff}# user{newline}root = 1{newline}{newline}{newline}[other]{newline}x = 2{newline}"
        );
        let rendered = with_block(&source, Some("MANAGED"));
        assert!(rendered.starts_with('\u{feff}'));
        assert!(rendered.find("root = 1").unwrap() < rendered.find("MANAGED").unwrap());
        assert!(rendered.find("MANAGED").unwrap() < rendered.find("[other]").unwrap());
        assert!(rendered.contains("x = 2"));
        assert_eq!(with_block(&rendered, None), rendered);
    }
    assert_eq!(with_block("", None), "");
    assert_eq!(with_block("\u{feff}", None), "\u{feff}");
    assert_eq!(
        with_block("# user\n\n\nroot = 1", Some("MANAGED")),
        "# user\n\nroot = 1\n\nMANAGED\n"
    );
}
