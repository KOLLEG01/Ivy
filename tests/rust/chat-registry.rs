#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_chat_bridge::registry::chat_registry;
use ivy_contracts::{hash::digest, json::stringify, wire_schema::validate_shared};
use std::process::Command;

#[test]
fn complete_registry_matches_original_typescript_bytes_and_wire_contract() {
    let output = reference_data::read(include_bytes!("fixtures/chat-registry-cases.json.gz"));
    let registry = chat_registry().unwrap();
    validate_shared("RegistrySync", &registry).unwrap();
    let actual = stringify(&registry, usize::MAX).unwrap();
    assert_eq!(actual.len(), output.len(), "registry byte length");
    assert_eq!(
        digest(actual.as_bytes()),
        digest(&output),
        "full registry: schemas, descriptions, MCP discovery, retention, versions and ordering"
    );
    assert_eq!(
        registry.get("namespaces").unwrap().as_array().unwrap()[0]
            .get("tools")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        14
    );
    assert_eq!(
        registry.get("contracts").unwrap().as_array().unwrap().len(),
        8
    );
}
