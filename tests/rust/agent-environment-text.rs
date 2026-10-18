use ivy_agent_manager::environment_text::{
    ManagedBlock, codex_block, managed_block, render_codex, with_block,
};
use ivy_contracts::{IvyJson, IvyResult, json::parse};
use std::{collections::HashSet, path::PathBuf, process::Command};
fn string(value: &IvyJson) -> String {
    value.as_js_string().unwrap().to_utf8().unwrap()
}
fn block(value: ManagedBlock) -> IvyJson {
    IvyJson::object([
        (
            "block",
            value.block.map(IvyJson::from).unwrap_or(IvyJson::Null),
        ),
        ("outside", value.outside.into()),
    ])
}
#[test]
fn managed_toml_markers_recovery_and_placement_match_original() {
    let output = Command::new("node")
        .arg("tests/fixtures/agent-environment-text-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        2 * 1024 * 1024,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 195);
    for (index, row) in rows.iter().enumerate() {
        let result: IvyResult<IvyJson> = match string(row.get("op").unwrap()).as_str() {
            "managed" => managed_block(
                &string(row.get("source").unwrap()),
                &string(row.get("begin").unwrap()),
                &string(row.get("end").unwrap()),
            )
            .map(block),
            "codex" => {
                let hashes: HashSet<_> = row
                    .get("hashes")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(string)
                    .collect();
                codex_block(&string(row.get("source").unwrap()), &hashes).map(block)
            }
            "render" => {
                let sections = row
                    .get("sections")
                    .unwrap()
                    .as_array()
                    .map(|v| v.iter().map(string).collect::<Vec<_>>());
                Ok(render_codex(sections.as_deref())
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null))
            }
            "with" => {
                let b = row
                    .get("block")
                    .unwrap()
                    .as_js_string()
                    .map(|v| v.to_utf8().unwrap());
                Ok(with_block(&string(row.get("source").unwrap()), b.as_deref()).into())
            }
            _ => panic!("unexpected operation"),
        };
        let actual = match result {
            Ok(value) => IvyJson::object([("result", value)]),
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        assert_eq!(
            &actual,
            row.get("expected").unwrap(),
            "Case {index}: {:?}",
            row.get("op")
        );
    }
}
