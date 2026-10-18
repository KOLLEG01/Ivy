use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
    schema::{bundle_local_schema, pointer_parts, scalar_types},
};

#[test]
fn schema_graph_helpers_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 250);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), 2_000_000).unwrap();
        let before = stringify(&input, 2_000_000).unwrap();
        let result = match row["operation"].as_str().unwrap() {
            "pointer" => pointer_parts(input.as_js_string().unwrap())
                .map(|parts| IvyJson::Array(parts.into_iter().map(IvyJson::String).collect())),
            "scalar" => scalar_types(
                input.get("schema").unwrap(),
                input.get("pointer").unwrap().as_js_string().unwrap(),
            )
            .map(|types| IvyJson::Array(types.into_iter().map(IvyJson::from).collect())),
            "bundle" => bundle_local_schema(
                input.get("document").unwrap(),
                input.get("name").unwrap().as_js_string().unwrap(),
            ),
            operation => panic!("Unknown oracle operation {operation}"),
        };
        let actual = match result {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
            Err(error) => IvyJson::object([
                ("ok", false.into()),
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
            ]),
        };
        assert_eq!(
            stringify(&actual, 2_000_000).unwrap(),
            row["result"].as_str().unwrap(),
            "{}",
            row["name"]
        );
        assert_eq!(
            stringify(&input, 2_000_000).unwrap(),
            before,
            "Input mutated: {}",
            row["name"]
        );
    }
}
