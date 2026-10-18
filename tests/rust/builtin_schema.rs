use ivy_contracts::{builtin_schema::SchemaBundle, json::parse};

#[test]
fn compiler_reference_fragments_match_pinned_ajv() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/schema-reference-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 1500);
    let bundle =
        SchemaBundle::new(&[include_str!("../fixtures/schema-reference.schema.json")]).unwrap();
    for row in rows {
        let value = parse(row["input"].as_str().unwrap(), 10000).unwrap();
        let result = bundle.matches(row["reference"].as_str().unwrap(), &value);
        assert_eq!(
            result.is_ok(),
            row["result"]["ok"].as_bool().unwrap(),
            "{row}: {result:?}"
        );
        if let Ok(valid) = result {
            assert_eq!(valid, row["result"]["valid"].as_bool().unwrap(), "{row}");
        }
    }
}
#[test]
fn repository_schemas_match_the_existing_ajv_boundary() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("--experimental-transform-types")
        .arg("--disable-warning=ExperimentalWarning")
        .arg("tests/fixtures/builtin-schema-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 1000);
    let bundle = SchemaBundle::new(&[
        include_str!("../../specs/schemas/hive-wire.schema.json"),
        include_str!("../../specs/schemas/hive-operations.schema.json"),
        include_str!("../../specs/schemas/hive-transport.schema.json"),
        include_str!("../../specs/schemas/host.schema.json"),
    ])
    .unwrap();
    for row in rows {
        let value = parse(row["input"].as_str().unwrap(), 1_000_000).unwrap();
        assert_eq!(
            bundle
                .matches(row["reference"].as_str().unwrap(), &value)
                .unwrap_or_else(|error| panic!("{row}: {error:?}")),
            row["valid"].as_bool().unwrap(),
            "{row}"
        );
        if let Some(name) = row["hostName"].as_str() {
            let error = ivy_contracts::core_schema::validate_host(name, &value).err();
            assert_eq!(
                error.as_ref().map(|e| e.code.as_str()),
                row["hostError"]["code"].as_str(),
                "{row}"
            );
            assert_eq!(
                error.as_ref().map(|e| e.message.as_str()),
                row["hostError"]["message"].as_str(),
                "{row}"
            );
        }
    }
}
