use ivy_contracts::{
    IvyJson, IvyResult, agent_bundle, core_bundle, core_schema, host_bundle,
    json::{parse, stringify},
};
use sha2::{Digest, Sha256};

#[test]
fn all_canonical_definitions_and_operation_inputs_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/canonical-bundle-oracle.mjs",
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
    assert!(rows.len() > 400);
    let catalog: &'static serde_json::Map<String, serde_json::Value> =
        core_schema::operation_catalog();
    assert!(std::ptr::eq(catalog, core_schema::operation_catalog()));
    // Heap traversal also keeps bundling within a small native-thread stack.
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            for row in rows {
                let reference = parse(row["reference"].as_str().unwrap(), 10000).unwrap();
                let document = row["document"]
                    .as_str()
                    .map(|text| parse(text, 10000).unwrap());
                let reference = reference.as_js_string().unwrap();
                let document = document.as_ref().and_then(IvyJson::as_js_string);
                if row["kind"] == "operation" {
                    let method = reference.to_utf8().unwrap();
                    let definition = core_schema::operation_definition(&method);
                    assert_eq!(definition.is_some(), row["ownOperation"].as_bool().unwrap());
                    assert_eq!(
                        definition,
                        (!row["descriptor"].is_null()).then_some(&row["descriptor"])
                    );
                    if let Some(names) = row["catalogNames"].as_array() {
                        assert_eq!(
                            catalog.keys().map(String::as_str).collect::<Vec<_>>(),
                            names
                                .iter()
                                .map(|name| name.as_str().unwrap())
                                .collect::<Vec<_>>()
                        );
                    }
                    for case in row["validation"].as_array().unwrap() {
                        let value = parse(case["input"].as_str().unwrap(), usize::MAX).unwrap();
                        let result = if case["direction"] == "input" {
                            core_schema::validate_input(&method, &value)
                        } else {
                            core_schema::validate_output(&method, &value)
                        };
                        let expected = &case["expected"];
                        assert_eq!(
                            result.is_ok(),
                            expected["ok"].as_bool().unwrap(),
                            "{method}: {case}: {result:?}"
                        );
                        if let Err(error) = result {
                            assert_eq!(
                                error.code,
                                expected["code"].as_str().unwrap(),
                                "{method}: {case}"
                            );
                            assert_eq!(
                                error.message,
                                expected["message"].as_str().unwrap(),
                                "{method}: {case}"
                            );
                            assert_eq!(
                                error.outcome.as_str(),
                                expected["outcome"].as_str().unwrap(),
                                "{method}: {case}"
                            );
                        }
                    }
                }
                let result: IvyResult<IvyJson> = match row["kind"].as_str().unwrap() {
                    "core" => core_bundle::bundled_schema(reference, document),
                    "host" => host_bundle::bundled_schema(reference, document),
                    "agent" => agent_bundle::bundled_schema(reference, document),
                    "operation" => {
                        core_bundle::operation_input_schema(&reference.to_utf8().unwrap())
                    }
                    _ => unreachable!(),
                };
                let expected = &row["expected"];
                assert_eq!(
                    result.is_ok(),
                    expected["ok"].as_bool().unwrap(),
                    "{row}: {result:?}"
                );
                match result {
                    Ok(schema) => {
                        let text = stringify(&schema, usize::MAX).unwrap();
                        assert_eq!(
                            text.len(),
                            expected["bytes"].as_u64().unwrap() as usize,
                            "{row}"
                        );
                        assert_eq!(
                            Sha256::digest(text.as_bytes())
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<String>(),
                            expected["sha256"].as_str().unwrap(),
                            "{row}"
                        );
                        let definitions = schema.get("$defs").unwrap().as_object().unwrap();
                        assert_eq!(
                            definitions
                                .iter()
                                .map(|(key, _)| key.to_utf8().unwrap())
                                .collect::<Vec<_>>(),
                            expected["definitions"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|v| v.as_str().unwrap())
                                .collect::<Vec<_>>(),
                            "{row}"
                        );
                        // Every rewritten reference must resolve in the returned local graph.
                        let mut pending = vec![&schema];
                        while let Some(value) = pending.pop() {
                            match value {
                                IvyJson::Array(items) => pending.extend(items),
                                IvyJson::Object(pairs) => {
                                    for (key, value) in pairs {
                                        assert!(key != &"$id".into() && key != &"$schema".into());
                                        if key == &"$ref".into() {
                                            let reference = value.as_str().unwrap();
                                            let name = reference.strip_prefix("#/$defs/").unwrap();
                                            assert!(
                                                schema.get("$defs").unwrap().get(name).is_some(),
                                                "{row}"
                                            );
                                        }
                                        pending.push(value);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(error) => {
                        assert_eq!(error.code, expected["code"].as_str().unwrap(), "{row}");
                        assert_eq!(
                            error.message,
                            expected["message"].as_str().unwrap(),
                            "{row}"
                        );
                        assert_eq!(
                            error.outcome.as_str(),
                            expected["outcome"].as_str().unwrap(),
                            "{row}"
                        );
                    }
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
