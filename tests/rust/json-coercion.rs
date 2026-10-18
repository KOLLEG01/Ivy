use ivy_contracts::{
    IvyJson,
    json::{parse, try_js_string},
};
#[test]
fn string_coercion_matches_original_node_and_ivy_error_boundary() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/json-coercion-oracle.mjs",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert!(rows.len() > 250);
    for row in rows {
        let value = parse(row["value"].as_str().unwrap(), usize::MAX).unwrap();
        let actual = match try_js_string(&value) {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", IvyJson::String(value))]),
            Err(e) => IvyJson::object([("ok", false.into()), ("error", e.to_wire())]),
        };
        assert_eq!(
            actual,
            parse(row["expected"].as_str().unwrap(), usize::MAX).unwrap(),
            "{row}"
        );
    }
}
