use ivy_contracts::{IvyJson, json::parse};

#[test]
fn dynamic_validators_check_json_fields_types_and_structural_uniqueness() {
    let validators = ivy_contracts::schema::SchemaValidators::new();
    for (schema, value, valid) in [
        (
            r#"{"type":"object","required":["constructor"]}"#,
            r#"{}"#,
            false,
        ),
        (
            r#"{"type":"object","required":["constructor"]}"#,
            r#"{"constructor":"owned"}"#,
            true,
        ),
        (r#"{"type":"object","required":[""]}"#, r#"{}"#, false),
        (r#"{"type":"object","required":[""]}"#, r#"{"":true}"#, true),
        (
            r#"{"type":"array","items":{"type":"string"},"uniqueItems":true}"#,
            r#"["__proto__","__proto__"]"#,
            false,
        ),
        (r#"{"type":"array","uniqueItems":true}"#, r#"[1,"1"]"#, true),
        (
            r#"{"type":"array","uniqueItems":true}"#,
            r#"[{"a":1,"b":2},{"b":2,"a":1}]"#,
            false,
        ),
        (
            r#"{"type":"string","minLength":2,"pattern":"^[a-z]+$"}"#,
            r#""abc""#,
            true,
        ),
        (
            r#"{"type":"string","minLength":2,"pattern":"^[a-z]+$"}"#,
            r#""A""#,
            false,
        ),
    ] {
        let schema = parse(schema, 10000).unwrap();
        let value = parse(value, 10000).unwrap();
        assert_eq!(validators.validate(&schema, &value, 10000).is_ok(), valid);
    }
    assert!(
        validators
            .compile(&parse(r#"{"required":[1]}"#, 1000).unwrap())
            .is_err()
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
