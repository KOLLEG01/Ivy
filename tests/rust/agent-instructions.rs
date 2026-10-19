use ivy_agent_manager::instructions::compose_instructions;
#[path = "agent-claude-mcp.rs"]
mod claude_mcp_cases;
#[path = "agent-instructions-files.rs"]
mod instruction_file_cases;
#[path = "agent-instructions-source.rs"]
mod instruction_source_cases;
#[path = "agent-environment.rs"]
mod environment_cases;
#[path = "agent-environment-manager.rs"]
mod environment_manager_cases;
#[path = "agent-environment-defaults.rs"]
mod environment_defaults_cases;
// These Agent-owner checks use synthetic RPC streams and stop before any CLI
// probe. They do not replay the separate native-launch qualification target.
#[path = "agent-native-owner.rs"]
mod native_owner_cases;
#[path = "agent-environment-conflicts.rs"]
mod environment_conflict_cases;
#[path = "agent-environment-text.rs"]
mod environment_text_cases;
#[path = "agent-mcp-files.rs"]
mod mcp_file_cases;
#[path = "agent-mcp-import.rs"]
mod mcp_import_cases;
#[path = "agent-mcp-manager.rs"]
mod mcp_manager_cases;
#[path = "agent-mcp-output.rs"]
mod mcp_output_cases;
#[path = "agent-mcp-prepare.rs"]
mod mcp_prepare_cases;
#[path = "agent-mcp-render.rs"]
mod mcp_render_cases;
#[path = "agent-skills.rs"]
mod skills_cases;
#[path = "agent-skills-output.rs"]
mod skills_output_cases;
#[path = "agent-skills-files.rs"]
mod skills_file_cases;
use ivy_contracts::{
    IvyJson, JsString,
    hash::hash_json,
    json::{parse, stringify},
};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn document(host: bool, enabled: IvyJson, include_local: bool) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", if host { "host".into() } else { IvyJson::Null }),
        ("enabled", enabled),
        ("includeLocal", include_local.into()),
        (
            "text",
            "# Actions attributed to the user\r\nKeep original 😀 text.".into(),
        ),
    ])
}
fn source(mode: usize, host: bool, include_local: bool) -> IvyJson {
    if mode == 0 {
        return IvyJson::Null;
    }
    let doc = document(
        host,
        match mode {
            1 => IvyJson::Null,
            2 => false.into(),
            _ => true.into(),
        },
        include_local,
    );
    IvyJson::object([
        ("contentHash", hash_json(&doc).unwrap().into()),
        ("document", doc),
        ("objectId", "original".into()),
        ("revision", IvyJson::Number(7.0)),
    ])
}
fn option(value: &IvyJson) -> Option<&IvyJson> {
    if value == &IvyJson::Null {
        None
    } else {
        Some(value)
    }
}

#[test]
fn inheritance_local_bytes_and_version_identities_match_unchanged_typescript() {
    let mut cases = Vec::new();
    let mut results = Vec::new();
    for global_mode in 0..4 {
        for host_mode in 0..4 {
            for has_defaults in [false, true] {
                for include_local in [false, true] {
                    for local in [
                        None,
                        Some(&b""[..]),
                        Some(&b"\xef\xbb\xbfOriginal local\r\n"[..]),
                        Some(&b"\xff"[..]),
                    ] {
                        let global = source(global_mode, false, !include_local);
                        let host = source(host_mode, true, include_local);
                        let defaults = if has_defaults {
                            document(false, true.into(), include_local)
                        } else {
                            IvyJson::Null
                        };
                        let discovered = JsString(vec![b'A' as u16, 0xd800, b'Z' as u16]);
                        let actual = compose_instructions(
                            option(&global),
                            option(&host),
                            local,
                            option(&defaults),
                            &discovered,
                        );
                        results.push(match actual {
                            Ok(v) => IvyJson::object([
                                (
                                    "body",
                                    v.body
                                        .map(|b| String::from_utf8(b).unwrap().into())
                                        .unwrap_or(IvyJson::Null),
                                ),
                                ("version", v.version.into()),
                            ]),
                            Err(e) => IvyJson::object([
                                ("code", e.code.into()),
                                ("outcome", e.outcome.as_str().into()),
                            ]),
                        });
                        cases.push(IvyJson::object([
                            ("global", global),
                            ("host", host),
                            ("defaults", defaults),
                            (
                                "local",
                                local
                                    .map(|bytes| {
                                        IvyJson::Array(
                                            bytes
                                                .iter()
                                                .map(|b| IvyJson::Number(*b as f64))
                                                .collect(),
                                        )
                                    })
                                    .unwrap_or(IvyJson::Null),
                            ),
                            ("discovered", IvyJson::String(discovered)),
                        ]));
                    }
                }
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child = Command::new("node")
        .arg("tests/fixtures/agent-instructions-oracle.mjs")
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            stringify(&IvyJson::Array(cases), 8 * 1024 * 1024)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        8 * 1024 * 1024,
    )
    .unwrap();
    assert_eq!(IvyJson::Array(results), oracle);
}

#[test]
fn instructions_do_not_truncate_at_the_utf8_byte_boundary() {
    let defaults = document(false, true.into(), false);
    let small = compose_instructions(None, None, None, Some(&defaults), &"".into()).unwrap();
    let available = 32768 - small.body.unwrap().len();
    let exact = compose_instructions(
        None,
        None,
        None,
        Some(&defaults),
        &"a".repeat(available).into(),
    )
    .unwrap();
    assert_eq!(exact.body.unwrap().len(), 32768);
    let error = compose_instructions(
        None,
        None,
        None,
        Some(&defaults),
        &format!("{}é", "a".repeat(available - 1)).into(),
    )
    .unwrap_err();
    assert_eq!(error.code, "limit_exceeded");
}

#[test]
fn only_the_initial_exact_global_heading_is_demoted() {
    for (text, expected) in [
        (
            "# Actions attributed to the user",
            "## Actions attributed to the user",
        ),
        (
            "# Actions attributed to the user\r",
            "# Actions attributed to the user\r",
        ),
        (
            "# Actions attributed to the users\n",
            "# Actions attributed to the users\n",
        ),
    ] {
        let mut defaults = document(false, true.into(), false);
        defaults.insert("text", text.into());
        let result =
            compose_instructions(None, None, None, Some(&defaults), &"remote".into()).unwrap();
        assert!(
            String::from_utf8(result.body.unwrap())
                .unwrap()
                .starts_with(&format!("{expected}\n\n## Ivy MCP"))
        );
    }
}
