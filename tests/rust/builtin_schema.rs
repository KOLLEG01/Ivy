use ivy_contracts::{builtin_schema::SchemaBundle, json::parse};

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
