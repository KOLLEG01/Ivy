use ivy_contracts::{
    IvyJson,
    canonical_bundle::Bundle,
    core_bundle, core_schema,
    json::{parse, stringify},
};

fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}

#[test]
fn packaged_definitions_are_self_contained_and_preserve_existing_bundles() {
    let documents = [
        (
            "Wire",
            include_str!("../../specs/schemas/hive-wire.schema.json"),
        ),
        (
            "Operation",
            include_str!("../../specs/schemas/hive-operations.schema.json"),
        ),
        ("Host", include_str!("../../specs/schemas/host.schema.json")),
        (
            "Agent",
            include_str!("../../specs/schemas/agent.schema.json"),
        ),
        ("Chat", include_str!("../../specs/schemas/chat.schema.json")),
        (
            "TaskBoard",
            include_str!("../../specs/schemas/task-board.schema.json"),
        ),
    ]
    .map(|(prefix, text)| (prefix.to_owned(), json(text)));
    let bundle = Bundle::from_documents(documents.to_vec()).unwrap();
    let mut count = 0;
    for (prefix, document) in &documents {
        let id = document.get("$id").unwrap().as_str().unwrap();
        for (name, _) in document.get("$defs").unwrap().as_object().unwrap() {
            let name = name.to_utf8().unwrap();
            let schema = bundle.definition(&id, &name).unwrap();
            let mut pending = vec![&schema];
            while let Some(value) = pending.pop() {
                match value {
                    IvyJson::Object(fields) => {
                        for (key, child) in fields {
                            assert!(key != &"$id".into() && key != &"$schema".into());
                            if key == &"$ref".into() {
                                let reference = child.as_str().unwrap();
                                let definition = reference.strip_prefix("#/$defs/").unwrap();
                                assert!(schema.get("$defs").unwrap().get(definition).is_some());
                            }
                            pending.push(child);
                        }
                    }
                    IvyJson::Array(items) => pending.extend(items),
                    _ => {}
                }
            }
            if ["Wire", "Operation", "Host"].contains(&prefix.as_str()) {
                let existing = core_bundle::bundled_schema(
                    &format!("#/$defs/{name}").into(),
                    Some(&id.clone().into()),
                )
                .unwrap();
                assert_eq!(
                    stringify(&schema, usize::MAX).unwrap(),
                    stringify(&existing, usize::MAX).unwrap()
                );
            }
            count += 1;
        }
    }
    assert!(count > 200);
    for method in core_schema::operation_catalog().keys() {
        assert_eq!(
            core_bundle::operation_input_schema(method)
                .unwrap()
                .get("type"),
            Some(&"object".into())
        );
    }
}

#[test]
fn owned_catalog_resolves_recursive_cross_document_refs_without_prototypes() {
    let bundle = Bundle::from_documents(vec![
        ("First".to_owned(), json(r##"{"$id":"https://example.invalid/first.json","$defs":{"Root":{"type":"object","properties":{"next":{"$ref":"second.json#/$defs/Node"}}}}}"##)),
        ("Second".to_owned(), json(r##"{"$id":"https://example.invalid/second.json","$defs":{"Node":{"type":"object","properties":{"next":{"$ref":"#/$defs/Node"},"value":{"type":"string"}}}}}"##)),
    ]).unwrap();
    let bundled = bundle
        .definition("https://example.invalid/first.json", "Root")
        .unwrap();
    let expected = json(
        r##"{"type":"object","properties":{"next":{"$ref":"#/$defs/SecondNode"}},"$defs":{"SecondNode":{"type":"object","properties":{"next":{"$ref":"#/$defs/SecondNode"},"value":{"type":"string"}}}}}"##,
    );
    assert_eq!(bundled, expected);
    assert!(
        bundle
            .definition("https://example.invalid/first.json", "constructor")
            .is_err()
    );
    assert!(
        bundle
            .definition("https://example.invalid/missing.json", "Root")
            .is_err()
    );
    assert!(core_bundle::operation_input_schema("constructor").is_err());
}
