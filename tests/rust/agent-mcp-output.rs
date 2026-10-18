use ivy_agent_manager::mcp_migration::plan_mcp_import;
use ivy_agent_manager::mcp_output::{McpOutputInput, plan_mcp_output};
use ivy_agent_manager::mcp_render::{hive_mcp_urls, render_mcp_sections};
use ivy_contracts::{
    IvyJson,
    json::{canonical, parse},
};
use std::{path::PathBuf, process::Command, sync::OnceLock};
fn string(v: &IvyJson) -> String {
    v.as_js_string().unwrap().to_utf8().unwrap()
}
pub(super) fn requires_import(row: &IvyJson) -> bool {
    row.get("state").and_then(|s| s.get("schemaVersion")) == Some(&IvyJson::Number(1.0))
        || [
            ivy_agent_manager::environment_text::BEGIN_MCP,
            ivy_agent_manager::environment_text::END_MCP,
            ivy_agent_manager::environment_text::BEGIN_PERMISSIONS,
            ivy_agent_manager::environment_text::END_PERMISSIONS,
        ]
        .iter()
        .any(|marker| string(row.get("source").unwrap()).contains(marker))
}
#[test]
fn explicit_conversion_preserves_original_rules_and_runtime_requires_current_format() {
    let data = original();
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
        let input = || McpOutputInput {
            source: &source,
            state: row.get("state"),
            authority: &authority,
            sections: sections.as_deref(),
            server_names: &names,
            app_id: app.as_deref(),
            desired_version: &version,
        };
        let runtime = plan_mcp_output(input());
        if requires_import(row) {
            assert!(
                matches!(runtime,Err(ref error)if error.code=="target_conflict"),
                "runtime must require explicit import in {index}"
            );
        }
        let project = |result: ivy_contracts::IvyResult<
            ivy_agent_manager::mcp_output::McpOutput,
        >| match result {
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
        let actual = project(plan_mcp_import(input()));
        assert_eq!(
            &actual,
            row.get("expected").unwrap(),
            "Case {index}: {row:?}"
        );
        if !requires_import(row) {
            assert_eq!(
                &project(runtime),
                row.get("expected").unwrap(),
                "unchanged runtime case {index}"
            );
        }
    }
}

pub(super) fn original() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
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
        parse(
            std::str::from_utf8(&output.stdout).unwrap(),
            2 * 1024 * 1024,
        )
        .unwrap()
    })
}
