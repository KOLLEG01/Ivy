use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};

#[test]
fn dynamic_validators_match_original_admission_compilation_and_evaluation() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-runtime-oracle.mjs",
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
    assert!(rows.len() > 10_000);
    let mut shared =
        std::collections::HashMap::<String, ivy_contracts::schema::SchemaValidators>::new();
    let mut mismatches = std::collections::BTreeMap::<String, (usize, String)>::new();
    for (index, row) in rows.iter().enumerate() {
        let schema = parse(row["schema"].as_str().unwrap(), 1_000_000).unwrap();
        let value = parse(row["value"].as_str().unwrap(), 1_000_000).unwrap();
        let fresh = ivy_contracts::schema::SchemaValidators::new();
        let validators = match row["pool"].as_str() {
            Some(key) => shared.entry(key.into()).or_default(),
            None => &fresh,
        };
        let actual = match validators.validate(&schema, &value, 1024 * 1024) {
            Ok(()) => IvyJson::object([("ok", true.into())]),
            Err(error) => IvyJson::object([("ok", false.into()), ("wire", error.to_wire())]),
        };
        let expected = parse(row["result"].as_str().unwrap(), 1_000_000).unwrap();
        if !ivy_contracts::builtin_schema::json_equal(&actual, &expected) {
            let key = row["schema"].as_str().unwrap().to_owned();
            let entry = mismatches.entry(key).or_insert_with(|| {
                (
                    0,
                    format!(
                        "row{index} value {} expected {} actual {}",
                        row["value"],
                        row["result"],
                        stringify(&actual, 1_000_000).unwrap()
                    ),
                )
            });
            entry.0 += 1;
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} differing schema groups: {mismatches:#?}",
        mismatches.len()
    );
}

#[test]
fn dynamic_validator_cache_bounds_failed_compiles_and_retains_owned_handles() {
    use ivy_contracts::schema::SchemaValidators;
    use std::sync::Arc;
    let validators = SchemaValidators::new();
    let schema = parse(r#"{"type":"string"}"#, 1000).unwrap();
    let first = validators.compile(&schema).unwrap();
    assert!(Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    for i in 0..255 {
        let rejected = IvyJson::object([
            ("title", i.to_string().into()),
            ("enum", IvyJson::Array(vec![])),
        ]);
        assert!(validators.compile(&rejected).is_err());
    }
    assert!(Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    // Admission failures occur before the batch count/cache reset.
    assert!(
        validators
            .compile(&IvyJson::object([("unknown", true.into())]))
            .is_err()
    );
    assert!(Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    validators
        .compile(&parse(r#"{"type":"integer"}"#, 1000).unwrap())
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    assert!(first.matches(&"still usable".into()).unwrap());
    assert!(!first.matches(&IvyJson::Number(1.0)).unwrap());
    // Value canonicalization/limits precede schema compilation/admission.
    let error = validators
        .validate(
            &IvyJson::object([("unknown", true.into())]),
            &"too long".into(),
            2,
        )
        .unwrap_err();
    assert_eq!(error.code, "content_too_large");
}

#[test]
fn dynamic_reference_evaluation_keeps_native_stack_bounded() {
    std::thread::Builder::new().stack_size(256 * 1024).spawn(|| {
        let schema = parse(r##"{"$defs":{"node":{"type":"object","properties":{"next":{"$ref":"#/$defs/node"},"value":{"type":"integer"}},"additionalProperties":false}},"$ref":"#/$defs/node"}"##, 1000).unwrap();
        let validators = ivy_contracts::schema::SchemaValidators::new();
        for valid in [true, false] {
            let mut value = IvyJson::object([("value", if valid { IvyJson::Number(1.0) } else { "bad".into() })]);
            for _ in 0..127 { value = IvyJson::object([("next", value)]); }
            let result = validators.validate(&schema, &value, 1024 * 1024);
            if valid { result.unwrap(); } else { assert_eq!(result.unwrap_err().code, "invalid_arguments"); }
        }
    }).unwrap().join().unwrap();
}
