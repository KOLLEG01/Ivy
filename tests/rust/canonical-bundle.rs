use ivy_contracts::{
    IvyJson, IvyResult, agent_bundle, chat_bundle, core_bundle, host_bundle,
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
                let result: IvyResult<IvyJson> = match row["kind"].as_str().unwrap() {
                    "core" => core_bundle::bundled_schema(reference, document),
                    "host" => host_bundle::bundled_schema(reference, document),
                    "agent" => agent_bundle::bundled_schema(reference, document),
                    "chat" => chat_bundle::bundled_schema(reference, document),
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
                            error.outcome.to_json(),
                            IvyJson::from(expected["outcome"].as_str().unwrap()),
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
