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
fn dynamic_validator_cache_keeps_valid_entries_after_failed_compiles() {
    use ivy_contracts::schema::SchemaValidators;
    use std::sync::Arc;
    let validators = SchemaValidators::new();
    let schema = parse(r#"{"type":"string"}"#, 1000).unwrap();
    let first = validators.compile(&schema).unwrap();
    assert!(Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    for i in 0..300 {
        let rejected = IvyJson::object([
            ("title", i.to_string().into()),
            ("enum", IvyJson::Array(vec![])),
        ]);
        assert!(validators.compile(&rejected).is_err());
    }
    assert!(Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    // Neither compiler nor admission failures retain entries or evict successes.
    assert!(
        validators
            .compile(&IvyJson::object([("unknown", true.into())]))
            .is_err()
    );
    assert!(Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    validators
        .compile(&parse(r#"{"type":"integer"}"#, 1000).unwrap())
        .unwrap();
    assert!(Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
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

#[test]
fn native_schema_resources_require_real_fields_and_schema_values() {
    use ivy_contracts::schema::{SchemaValidators, admit_schema, validate_compiler_keyword_values};
    for id in [
        IvyJson::Null,
        false.into(),
        IvyJson::Number(0.0),
        IvyJson::Array(vec![]),
        IvyJson::Object(Vec::new()),
    ] {
        let schema = IvyJson::object([("$id", id)]);
        let admitted = admit_schema(&schema).unwrap();
        assert_eq!(
            admitted.resource_index().err().unwrap().code,
            "registry_invalid"
        );
        assert_eq!(
            admitted.registered_resources().err().unwrap().code,
            "registry_invalid"
        );
    }
    for name in ["constructor", "__proto__", "toString"] {
        let schema = IvyJson::object([
            ("$id", name.into()),
            (
                "$defs",
                IvyJson::object([("node", IvyJson::object([("type", "string".into())]))]),
            ),
            ("$ref", "#/$defs/node".into()),
        ]);
        let admitted = admit_schema(&schema).unwrap();
        assert_eq!(
            admitted.registered_resources().unwrap().base_id,
            name.into()
        );
        let compiled = SchemaValidators::new().compile(&schema).unwrap();
        assert!(compiled.matches(&"text".into()).unwrap());
        assert!(!compiled.matches(&IvyJson::Number(1.0)).unwrap());
        let annotation = IvyJson::object([(
            "description",
            IvyJson::object([(name, IvyJson::object([("$id", "child".into())]))]),
        )]);
        let admitted_annotation = admit_schema(&annotation).unwrap();
        let graph = admitted_annotation.resource_graph();
        assert!(
            graph
                .nodes
                .iter()
                .any(|node| node.pointer == format!("/description/{name}").into())
        );
    }
    for keyword in ["properties", "$defs"] {
        let schema = IvyJson::object([(keyword, IvyJson::object([("__proto__", true.into())]))]);
        assert_eq!(
            admit_schema(&schema).err().unwrap().code,
            "registry_invalid"
        );
    }
    for source in [
        r#"{"required":[null]}"#,
        r#"{"required":[1]}"#,
        r#"{"properties":[]}"#,
        r#"{"items":[]}"#,
    ] {
        let schema = parse(source, 1000).unwrap();
        assert_eq!(
            validate_compiler_keyword_values(&schema).unwrap_err().code,
            "registry_invalid"
        );
        assert!(SchemaValidators::new().compile(&schema).is_err());
    }
    let schema = parse(r#"{"description":{"value":"text","list":[{}]}}"#, 1000).unwrap();
    let admitted = admit_schema(&schema).unwrap();
    let resources = admitted.compiler_resources().unwrap();
    assert!(
        resources
            .resolve(&"".into(), &"#/description/list/0".into())
            .unwrap()
            .is_some()
    );
    for pointer in [
        "#/description/value",
        "#/description/%76alue",
        "#/description/list/length",
        "#/description/constructor",
        "#/description/__proto__",
    ] {
        assert!(
            !matches!(resources.resolve(&"".into(), &pointer.into()), Ok(Some(_))),
            "{pointer}"
        );
    }
    for suffix in ["constructor", "__proto__", "title", "allOf/length"] {
        let schema = IvyJson::object([(
            "$schema",
            format!("https://json-schema.org/draft/2020-12/schema#/{suffix}").into(),
        )]);
        assert_eq!(
            admit_schema(&schema)
                .unwrap()
                .validate_meta_schema()
                .unwrap_err()
                .code,
            "registry_invalid"
        );
    }
}

#[test]
fn compiled_literals_compare_structural_json_without_conversion_hooks() {
    use ivy_contracts::schema::SchemaValidators;
    let literal = parse(
        r#"{"constructor":{},"valueOf":[1,2],"toString":"literal","nested":{"\ud800":true,"b":2}}"#,
        1000,
    )
    .unwrap();
    let reordered = parse(
        r#"{"nested":{"b":2,"\ud800":true},"toString":"literal","valueOf":[1,2],"constructor":{}}"#,
        1000,
    )
    .unwrap();
    for keyword in ["const", "enum"] {
        let schema = IvyJson::object([(
            keyword,
            if keyword == "enum" {
                IvyJson::Array(vec![literal.clone()])
            } else {
                literal.clone()
            },
        )]);
        let compiled = SchemaValidators::new().compile(&schema).unwrap();
        assert!(compiled.matches(&literal).unwrap());
        assert!(compiled.matches(&reordered).unwrap());
        for changed in [
            r#"{"constructor":0,"valueOf":[1,2],"toString":"literal","nested":{"\ud800":true,"b":2}}"#,
            r#"{"constructor":{},"valueOf":[2,1],"toString":"literal","nested":{"\ud800":true,"b":2}}"#,
        ] {
            assert!(!compiled.matches(&parse(changed, 1000).unwrap()).unwrap());
        }
    }
    let unique = SchemaValidators::new()
        .compile(&parse(r#"{"type":"array","uniqueItems":true}"#, 1000).unwrap())
        .unwrap();
    assert!(
        !unique
            .matches(&IvyJson::Array(vec![literal.clone(), reordered]))
            .unwrap()
    );
    assert!(
        unique
            .matches(&IvyJson::Array(vec![
                literal,
                IvyJson::object([("constructor", true.into())])
            ]))
            .unwrap()
    );
}

#[test]
fn native_keywords_reject_non_schemas_and_keep_utf16_property_names() {
    use ivy_contracts::{
        JsString,
        schema::{SchemaValidators, validate_compiler_keyword_values},
    };
    for valid in [true.into(), false.into(), IvyJson::Object(Vec::new())] {
        validate_compiler_keyword_values(&valid).unwrap();
    }
    for invalid in [
        IvyJson::Null,
        IvyJson::Number(0.0),
        "".into(),
        "string".into(),
        IvyJson::Array(Vec::new()),
    ] {
        assert_eq!(
            validate_compiler_keyword_values(&invalid).unwrap_err().code,
            "registry_invalid"
        );
    }
    for source in [
        r#"{"$async":null}"#,
        r#"{"$async":0}"#,
        r#"{"$async":""}"#,
        r#"{"$async":true}"#,
        r#"{"type":""}"#,
        r#"{"type":[]}"#,
        r#"{"allOf":[0]}"#,
        r#"{"anyOf":[[]]}"#,
        r#"{"oneOf":[]}"#,
        r#"{"prefixItems":["x"]}"#,
        r#"{"then":"x"}"#,
        r#"{"else":0}"#,
    ] {
        assert_eq!(
            validate_compiler_keyword_values(&parse(source, 1000).unwrap())
                .unwrap_err()
                .code,
            "registry_invalid",
            "{source}"
        );
    }
    validate_compiler_keyword_values(
        &parse(
            r#"{"$async":false,"type":["string","null"],"anyOf":[true,{"type":"string"}]}"#,
            1000,
        )
        .unwrap(),
    )
    .unwrap();
    for key in [JsString(vec![0xd800]), JsString(vec![0xdc00]), "雪".into()] {
        let schema = IvyJson::object([
            ("type", "object".into()),
            (
                "properties",
                IvyJson::Object(vec![(
                    key.clone(),
                    IvyJson::object([("type", "integer".into())]),
                )]),
            ),
            (
                "required",
                IvyJson::Array(vec![IvyJson::String(key.clone())]),
            ),
            ("additionalProperties", false.into()),
        ]);
        let compiled = SchemaValidators::new().compile(&schema).unwrap();
        assert!(
            compiled
                .matches(&IvyJson::Object(vec![(key.clone(), IvyJson::Number(1.0))]))
                .unwrap()
        );
        assert!(
            !compiled
                .matches(&IvyJson::Object(vec![(key.clone(), "bad".into())]))
                .unwrap()
        );
        assert!(!compiled.matches(&IvyJson::Object(Vec::new())).unwrap());
    }
}

#[test]
fn native_uri_schemes_namespaces_and_ip_literals_use_literal_syntax() {
    use ivy_contracts::schema::uri::{full_path, resolve};
    for id in [
        "%68ttp://example.invalid/a",
        "%u0068ttp://example.invalid/a",
        "h%74tp://example.invalid/a",
        "1http://example.invalid/a",
        "bad_name:resource",
        "http://[::1/a",
        "http://[invalid]/a",
        "http://[]/a",
        "custom://x[foo]/a",
    ] {
        assert_eq!(
            full_path(&id.into()).unwrap_err().code,
            "registry_invalid",
            "{id}"
        );
        assert_eq!(
            resolve(&"https://base.invalid/a".into(), &id.into())
                .unwrap_err()
                .code,
            "registry_invalid",
            "{id}"
        );
    }
    for id in ["urn:ſ:abc", "urn:K:abc", "urn:x:K"] {
        assert_eq!(
            full_path(&id.into()).unwrap_err().code,
            "registry_invalid",
            "{id}"
        );
    }
    for (id, expected) in [
        (
            "HTTPS://EXAMPLE.invalid:443/a",
            "https://example.invalid/a#",
        ),
        ("EXAMPLE+V1.0-TEST:resource", "example+v1.0-test:resource#"),
        ("urn:EXAMPLE:Item", "urn:example:Item#"),
        ("urn:UUID:ABCD", "urn:uuid:abcd#"),
        ("urn:example:%E9%9B%AA", "urn:example:%E9%9B%AA#"),
        ("http://[0:0:0:0:0:0:0:1]/a", "http://[::1]/a#"),
        (
            "custom://[vF.address:future]/a",
            "custom://[vf.address:future]/a#",
        ),
    ] {
        assert_eq!(full_path(&id.into()).unwrap(), expected.into(), "{id}");
    }
}

#[test]
fn uri_fragments_keep_complete_names_and_select_the_exact_schema() {
    use ivy_contracts::{
        JsString,
        schema::{SchemaValidators, uri::resolve},
    };
    for (separator, encoded) in [('\u{2028}', "%E2%80%A8"), ('\u{2029}', "%E2%80%A9")] {
        assert_eq!(
            resolve(&"custom:a/b#old".into(), &format!("#a{separator}b").into()).unwrap(),
            format!("custom:a/b#a{encoded}b").into()
        );
        assert_eq!(
            resolve(&"custom:a/b".into(), &format!("#a{separator}%").into())
                .unwrap_err()
                .code,
            "registry_invalid"
        );
        let name = format!("value{separator}suffix");
        let mut schema = IvyJson::object([
            ("$id", "https://example.invalid/root".into()),
            (
                "$defs",
                IvyJson::Object(vec![
                    ("value".into(), IvyJson::object([("type", "number".into())])),
                    (
                        JsString::from(name.as_str()),
                        IvyJson::object([("type", "string".into())]),
                    ),
                ]),
            ),
            ("$ref", format!("#/$defs/{name}").into()),
        ]);
        let compiled = SchemaValidators::new().compile(&schema).unwrap();
        assert!(compiled.matches(&"text".into()).unwrap());
        assert!(!compiled.matches(&IvyJson::Number(1.0)).unwrap());

        schema.insert("$ref", format!("#/$defs/value{separator}absent").into());
        assert!(SchemaValidators::new().compile(&schema).is_err());

        let aliases = IvyJson::object([
            ("$id", "#x".into()),
            (
                "$defs",
                IvyJson::object([(
                    "child",
                    IvyJson::object([
                        ("$id", format!("a#/{separator}").into()),
                        ("type", "string".into()),
                    ]),
                )]),
            ),
        ]);
        let admitted = ivy_contracts::schema::admit_schema(&aliases).unwrap();
        let index = admitted.resource_index().unwrap();
        assert!(index.references.iter().any(|reference| {
            reference.alias
                && reference.id == format!("a#/{encoded}").into()
                && reference.full_path == "#/$defs/child".into()
        }));
        assert!(
            !index
                .references
                .iter()
                .any(|reference| { reference.alias && reference.id == "a".into() })
        );
    }
}
