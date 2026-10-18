use ivy_agent_manager::mcp_render::{hive_mcp_urls, render_mcp_sections};
use ivy_contracts::{IvyJson, IvyResult, JsString, json::parse};
use std::{collections::HashSet, path::PathBuf, process::Command};
fn js(v: &IvyJson) -> &JsString {
    v.as_js_string().unwrap()
}
fn result(value: IvyResult<IvyJson>) -> IvyJson {
    match value {
        Ok(v) => IvyJson::object([("result", v)]),
        Err(e) => IvyJson::object([("error", e.to_wire())]),
    }
}
#[test]
fn rendering_and_credential_destinations_match_original() {
    let output = Command::new("node")
        .arg("tests/fixtures/agent-mcp-render-oracle.mjs")
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
    for row in data.get("resources").unwrap().as_array().unwrap() {
        let actual = hive_mcp_urls(js(row.get("hive").unwrap()));
        if let Some(expected) = row.get("expected").unwrap().get("result") {
            let expected: HashSet<_> = expected
                .as_array()
                .unwrap()
                .iter()
                .map(|s| js(s).to_utf8().unwrap())
                .collect();
            assert_eq!(actual.unwrap(), expected, "{row:?}");
        } else {
            assert_eq!(
                result(actual.map(|_| IvyJson::Null)),
                *row.get("expected").unwrap(),
                "{row:?}"
            );
        }
    }
    let rows = data.get("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 143);
    for (index, row) in rows.iter().enumerate() {
        let urls = row
            .get("hiveUrls")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|s| js(s).to_utf8().unwrap())
            .collect();
        let actual = render_mcp_sections(
            row.get("document").unwrap(),
            &urls,
            js(row.get("credential").unwrap()),
            row.get("appId").unwrap().as_js_string(),
        );
        assert_eq!(
            result(actual.map(|v| {
                v.map(|v| IvyJson::Array(v.into_iter().map(IvyJson::from).collect()))
                    .unwrap_or(IvyJson::Null)
            })),
            *row.get("expected").unwrap(),
            "Case {index}: {row:?}"
        );
    }
}
