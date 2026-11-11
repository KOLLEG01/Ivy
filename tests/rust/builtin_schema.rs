use ivy_contracts::{IvyJson, builtin_schema::SchemaBundle, json::parse};

#[test]
fn shared_trusted_roots_keep_local_references_and_owned_lifetimes() {
    use ivy_contracts::canonical_bundle::Bundle;
    use std::sync::Arc;
    let roots: Vec<_> = [
        r##"{"$id":"https://example.invalid/shared.json","$defs":{"Word":{"type":"string","pattern":"^a"}}}"##,
        r##"{"$ref":"#/$defs/value","$defs":{"value":{"allOf":[{"$ref":"https://example.invalid/shared.json#/$defs/Word"},{"const":"alpha"}]}}}"##,
        r##"{"$ref":"#/$defs/value","$defs":{"value":{"allOf":[{"$ref":"https://example.invalid/shared.json#/$defs/Word"},{"const":"another"}]}}}"##,
    ].into_iter().map(|text| Arc::new(parse(text, 10000).unwrap())).collect();
    let validator = SchemaBundle::from_shared_documents(roots.clone()).unwrap();
    let catalog = Bundle::from_shared_documents(vec![("Shared".into(), roots[0].clone())]).unwrap();
    drop(roots);
    for (document, value, expected) in [
        (1, "alpha", true),
        (1, "another", false),
        (2, "another", true),
        (2, "alpha", false),
        (2, "beta", false),
    ] {
        assert_eq!(
            validator
                .matches_document(document, "#", &value.into())
                .unwrap(),
            expected
        );
    }
    assert!(validator.matches_document(3, "#", &IvyJson::Null).is_err());
    let schema = catalog
        .definition("https://example.invalid/shared.json", "Word")
        .unwrap();
    let bundled = SchemaBundle::from_documents(vec![schema]).unwrap();
    assert!(bundled.matches("#", &"alpha".into()).unwrap());
    assert!(!bundled.matches("#", &"beta".into()).unwrap());
}

#[test]
fn named_schema_rejects_a_legacy_id_keyword_while_transport_keeps_its_original_data_id() {
    use ivy_contracts::core_schema::validate_transport;
    let error = validate_transport("ProviderCall/properties", &IvyJson::Null).unwrap_err();
    assert_eq!(
        error.to_wire(),
        ivy_contracts::IvyError::internal().to_wire()
    );
    let value = IvyJson::object([
        ("jsonrpc", "2.0".into()),
        ("id", "original-request".into()),
        ("method", "provider.invoke".into()),
        (
            "params",
            IvyJson::object([
                ("qualifiedName", "agent.read".into()),
                ("arguments", IvyJson::object([] as [(&str, IvyJson); 0])),
                (
                    "definitionHash",
                    format!("sha256:{}", "0".repeat(64)).into(),
                ),
                ("generation", 1.0.into()),
                ("callerPrincipalId", "caller".into()),
            ]),
        ),
    ]);
    validate_transport("ProviderCall", &value).unwrap();
    assert_eq!(value.get("id"), Some(&"original-request".into()));
}

#[test]
fn trusted_document_pointers_select_declared_schemas_only() {
    let bundle = SchemaBundle::from_documents(vec![
        parse(
            r##"{
        "$id":"https://example.invalid/schema.json",
        "$defs":{"plain":{"type":"string"},"a/b":{"type":"integer"},"deny":false,
            "constructor":{"const":"owned"},"scalar":"invalid schema"}}
    "##,
            10000,
        )
        .unwrap(),
    ])
    .unwrap();
    for (reference, value, valid) in [
        ("#/$defs/plain", r#""text""#, true),
        ("#/$defs/plain", "4", false),
        ("#/$defs/a~1b", "4", true),
        ("#/$defs/a~1b", "4.5", false),
        ("#/$defs/deny", "null", false),
        ("#/$defs/constructor", r#""owned""#, true),
        ("#/$defs/constructor", r#""other""#, false),
    ] {
        assert_eq!(
            bundle
                .matches(reference, &parse(value, 1000).unwrap())
                .unwrap(),
            valid
        );
    }
    for reference in ["#/$defs/toString", "#/$defs/__proto__", "#/$defs/scalar"] {
        assert!(
            bundle
                .matches(reference, &parse("null", 1000).unwrap())
                .is_err()
        );
    }
}

#[test]
fn packaged_taskboard_contract_checks_cross_document_fields_and_limits() {
    let bundle = SchemaBundle::new(&[
        include_str!("../../specs/schemas/task-board.schema.json"),
        include_str!("../../specs/schemas/hive-wire.schema.json"),
        include_str!("../../specs/schemas/hive-operations.schema.json"),
        include_str!("../../specs/schemas/host.schema.json"),
        include_str!("../../specs/schemas/agent.schema.json"),
    ])
    .unwrap();
    for (input, valid) in [
        (r#"{"objectId":"sample-object","revision":1}"#, true),
        (r#"{"objectId":"","revision":1}"#, false),
        (r#"{"objectId":"sample-object","revision":0}"#, false),
        (r#"{"objectId":"sample-object","revision":1.5}"#, false),
        (
            r#"{"objectId":"sample-object","revision":1,"extra":true}"#,
            false,
        ),
    ] {
        assert_eq!(
            bundle
                .matches(
                    "https://ivy.invalid/schemas/task-board.schema.json#/$defs/ObjectPin",
                    &parse(input, 1000).unwrap()
                )
                .unwrap(),
            valid
        );
    }
}
