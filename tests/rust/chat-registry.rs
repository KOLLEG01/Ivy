use ivy_chat_bridge::registry::chat_registry;
use ivy_contracts::{hash::digest, json::stringify, wire_schema::validate_shared};
use std::process::Command;

#[test]
fn complete_registry_matches_original_typescript_bytes_and_wire_contract() {
    let output = Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-registry-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let registry = chat_registry().unwrap();
    validate_shared("RegistrySync", &registry).unwrap();
    let actual = stringify(&registry, usize::MAX).unwrap();
    assert_eq!(actual.len(), output.stdout.len(), "registry byte length");
    assert_eq!(
        digest(actual.as_bytes()),
        digest(&output.stdout),
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
