#[test]
fn native_mcp_payload_and_sanitized_failures_match_original_typescript() {
    use ivy_contracts::{IvyJson, builtin_schema::json_equal, json::parse};
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/data-collector-mcp-result-oracle.mjs",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    for (index, row) in rows.as_array().unwrap().iter().enumerate() {
        let (value, error) =
            match ivy_data_collector::mcp_result::tool_result(row.get("input").unwrap()) {
                Ok(value) => (value, IvyJson::Null),
                Err(e) => (
                    IvyJson::Null,
                    IvyJson::object([
                        ("code", e.code.into()),
                        ("message", e.message.into()),
                        ("outcome", e.outcome.as_str().into()),
                    ]),
                ),
            };
        assert!(
            json_equal(&value, row.get("value").unwrap()),
            "case {index} value {value:?}; expected {row:?}"
        );
        assert!(
            json_equal(&error, row.get("error").unwrap()),
            "case {index} error {error:?}; expected {row:?}"
        );
    }
}
