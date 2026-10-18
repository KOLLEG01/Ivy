use ivy_contracts::{
    IvyJson,
    json::parse,
    ui_route::{RESERVED_UI_SLUGS, ui_path},
};

#[test]
fn routes_and_reserved_names_match_original_javascript() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/ui-route-oracle.mjs",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let names: Vec<&str> = data["reservedUiSlugs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(names, RESERVED_UI_SLUGS);
    let rows = data["rows"].as_array().unwrap();
    assert!(rows.len() > 600);
    for row in rows {
        let metadata = parse(row["metadata"].as_str().unwrap(), usize::MAX).unwrap();
        let result = ui_path(
            metadata.get("uiId").unwrap().as_js_string().unwrap(),
            metadata.get("slug").and_then(IvyJson::as_js_string),
        );
        let actual = match result {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", value.into())]),
            Err(e) => IvyJson::object([("ok", false.into()), ("error", e.to_wire())]),
        };
        assert_eq!(
            actual,
            parse(row["result"].as_str().unwrap(), usize::MAX).unwrap(),
            "{row}"
        );
    }
}
