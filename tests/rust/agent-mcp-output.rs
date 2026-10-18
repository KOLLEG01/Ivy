use ivy_agent_manager::mcp_output::{McpOutputInput, plan_mcp_output};
use ivy_agent_manager::mcp_render::{hive_mcp_urls, render_mcp_sections};
use ivy_contracts::{
    IvyJson,
    json::{canonical, parse},
};
use std::{path::PathBuf, process::Command};
fn string(v: &IvyJson) -> String {
    v.as_js_string().unwrap().to_utf8().unwrap()
}
#[test]
fn ownership_and_output_match_original_manager_filesystem_effects() {
    let output = Command::new("node")
        .arg("tests/fixtures/agent-mcp-output-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        2 * 1024 * 1024,
    )
    .unwrap();
    let rows = data.as_array().unwrap();
    assert_eq!(rows.len(), 192);
    for (index, row) in rows.iter().enumerate() {
        let source = string(row.get("source").unwrap());
        let authority = string(row.get("authority").unwrap());
        let expected_sections = row
            .get("sections")
            .unwrap()
            .as_array()
            .map(|a| a.iter().map(string).collect::<Vec<_>>());
        let urls = hive_mcp_urls(row.get("hive").unwrap().as_js_string().unwrap()).unwrap();
        let sections = render_mcp_sections(
            row.get("document").unwrap(),
            &urls,
            row.get("credential").unwrap().as_js_string().unwrap(),
            row.get("appId").unwrap().as_js_string(),
        )
        .unwrap();
        assert_eq!(sections, expected_sections, "Rendering case {index}");
        let names = row
            .get("serverNames")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_js_string().unwrap().clone())
            .collect::<Vec<_>>();
        let app = row
            .get("appId")
            .unwrap()
            .as_js_string()
            .map(|s| s.to_utf8().unwrap());
        let version = string(row.get("desiredVersion").unwrap());
        let actual = match plan_mcp_output(McpOutputInput {
            source: &source,
            state: row.get("state"),
            authority: &authority,
            sections: sections.as_deref(),
            server_names: &names,
            app_id: app.as_deref(),
            desired_version: &version,
        }) {
            Ok(result) => IvyJson::object([(
                "result",
                IvyJson::object([
                    ("text", String::from_utf8(result.bytes).unwrap().into()),
                    // Compare the persisted state, after atomic_json's canonical encoding.
                    (
                        "state",
                        parse(&canonical(&result.state, usize::MAX).unwrap(), usize::MAX).unwrap(),
                    ),
                    ("applied", result.applied.into()),
                    ("managedBytes", IvyJson::Number(result.managed_bytes as f64)),
                ]),
            )]),
            Err(error) => IvyJson::object([("error", error.code.into())]),
        };
        assert_eq!(
            &actual,
            row.get("expected").unwrap(),
            "Case {index}: {row:?}"
        );
    }
}
