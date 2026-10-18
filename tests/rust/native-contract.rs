use ivy_contracts::{
    IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
    json::{parse, stringify},
    native_contract::{NativeContract, is_read_only_native_method, native_tool_definition},
};
fn text(value: &IvyJson) -> String {
    stringify(value, usize::MAX).unwrap()
}
fn summary(value: &IvyJson) -> IvyResult<IvyJson> {
    let encoded = text(value);
    Ok(IvyJson::object([
        ("hash", hash_json(value)?.into()),
        ("bytes", IvyJson::Number(encoded.len() as f64)),
        ("orderedHash", digest(encoded).into()),
    ]))
}
fn capture(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(e) => IvyJson::object([("ok", false.into()), ("error", e.to_wire())]),
    }
}
fn expected(value: &serde_json::Value) -> IvyJson {
    parse(&serde_json::to_string(value).unwrap(), usize::MAX).unwrap()
}
#[test]
fn retained_catalogs_and_projections_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/native-contract-oracle.mjs",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 40);
    let mut count = 0;
    for row in rows {
        let source = if let Some(path) = row["path"].as_str() {
            std::fs::read_to_string(root.join(path)).unwrap()
        } else {
            row["source"].as_str().unwrap().into()
        };
        let mut original = parse(&source, usize::MAX).unwrap();
        let selected = NativeContract::new(&original);
        let label = row["path"].as_str().or(row["label"].as_str()).unwrap();
        assert_eq!(
            capture(
                selected
                    .as_ref()
                    .map_err(Clone::clone)
                    .and_then(|c| summary(c.catalog()))
            ),
            expected(&row["constructed"]),
            "{label}"
        );
        let Ok(contract) = selected else {
            continue;
        };
        original.insert("clientRequests", IvyJson::Array(vec![]));
        assert_eq!(
            contract.catalog_hash(),
            hash_json(contract.catalog()).unwrap()
        );
        let mut copy = contract.definitions();
        copy.clear();
        assert_eq!(
            contract.definitions().len(),
            row["methods"].as_array().unwrap().len()
        );
        let catalog_hash = JsString::from(contract.catalog_hash());
        for (index, method) in row["methods"].as_array().unwrap().iter().enumerate() {
            let name = parse(method["name"].as_str().unwrap(), usize::MAX)
                .unwrap()
                .into_js_string()
                .unwrap();
            count += 1;
            assert_eq!(
                is_read_only_native_method(&name),
                method["readOnly"].as_bool().unwrap()
            );
            let definition = contract.definition(&name).unwrap();
            assert_eq!(
                summary(definition).unwrap(),
                expected(&method["definition"]),
                "{label}: {name:?}"
            );
            assert_eq!(
                contract.definition_hash(&name).unwrap(),
                method["definitionHash"].as_str().unwrap()
            );
            let source_method = contract
                .catalog()
                .get("clientRequests")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m.get("method").unwrap().as_js_string() == Some(&name))
                .unwrap();
            assert_eq!(
                summary(
                    &native_tool_definition(
                        contract.catalog(),
                        source_method,
                        if index == 0 {
                            None
                        } else {
                            Some(&catalog_hash)
                        }
                    )
                    .unwrap()
                )
                .unwrap(),
                expected(&method["projection"])
            );
            for schema in method["schemas"].as_array().unwrap() {
                let names = parse(schema["reserved"].as_str().unwrap(), usize::MAX)
                    .unwrap()
                    .into_array()
                    .unwrap()
                    .into_iter()
                    .map(|v| v.into_js_string().unwrap())
                    .collect::<Vec<_>>();
                let projected = contract.input_schema(&name, &names);
                assert_eq!(
                    capture(
                        projected
                            .as_ref()
                            .map_err(Clone::clone)
                            .and_then(|v| summary(v))
                    ),
                    expected(&schema["result"]),
                    "{label} {name:?} {names:?}"
                );
                if let Ok(value) = projected {
                    let mut other = names.clone();
                    other.reverse();
                    other.extend(names.clone());
                    assert!(std::sync::Arc::ptr_eq(
                        &value,
                        &contract.input_schema(&name, &other).unwrap()
                    ));
                }
            }
            for check in method["verify"].as_array().unwrap() {
                let mut value = definition.clone();
                let mut hash = JsString::from(contract.definition_hash(&name).unwrap());
                match check["kind"].as_str().unwrap() {
                    "original" => {}
                    "decorated" => value.insert(
                        "discovery",
                        IvyJson::object([
                            ("group", "codex/turns".into()),
                            ("summary", "Read retained data.".into()),
                        ]),
                    ),
                    "changed" => {
                        value.insert("description", "changed".into());
                        hash = hash_json(&value).unwrap().into();
                    }
                    "wrongHash" => hash = format!("sha256:{}", "0".repeat(64)).into(),
                    "invalid" => value = IvyJson::Object(vec![]),
                    _ => unreachable!(),
                };
                assert_eq!(
                    capture(
                        contract
                            .verify_definition(&name, &value, &hash)
                            .map(|()| IvyJson::Null)
                    ),
                    expected(&check["result"]),
                    "{label} {name:?}"
                );
            }
        }
        assert_eq!(
            capture(contract.definition(&"unrecognized/method".into()).cloned()),
            expected(&row["unknown"])
        );
        assert_eq!(
            capture(
                contract
                    .definition_hash(&"unrecognized/method".into())
                    .map(IvyJson::from)
            ),
            expected(&row["unknownHash"])
        );
    }
    assert!(count > 1000);
}
