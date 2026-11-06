use ivy_contracts::{
    IvyJson as Json,
    json::{canonical, parse},
};

// Keep the original registry corpus intact while checking the backend extension
// independently before comparing every remaining field with that corpus.
pub fn registry_without_checked_backends(registry: &Json) -> Json {
    let backend = parse(
        r#"{"type":"object","properties":{"backendId":{"type":"string","pattern":"^[a-z][a-z0-9-]{0,63}$","not":{"enum":["agent"]}},"provider":{"enum":["codex","claude"]},"accountId":{"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"}},"required":["backendId","provider","accountId"],"additionalProperties":false}"#,
        usize::MAX,
    )
    .unwrap();
    let reference = Json::object([("$ref", "#/$defs/AgentBackendIdentity".into())]);
    let mut projected = registry.clone();
    let Json::Array(namespaces) = projected.get_mut("namespaces").unwrap() else {
        panic!("registry namespaces must be an array");
    };
    let mut checked = 0;
    for namespace in namespaces {
        let Json::Array(tools) = namespace.get_mut("tools").unwrap() else {
            panic!("registry tools must be an array");
        };
        for tool in tools {
            let Some(defs) = tool.get_mut("outputSchema").and_then(|s| s.get_mut("$defs"))
            else {
                continue;
            };
            if defs.get("AgentPlan").is_none() {
                continue;
            }
            assert_eq!(
                canonical(defs.required("AgentBackendIdentity").unwrap(), usize::MAX).unwrap(),
                canonical(&backend, usize::MAX).unwrap(),
                "complete backend identity schema"
            );
            let properties = defs
                .get_mut("AgentPlan")
                .unwrap()
                .get_mut("properties")
                .unwrap();
            assert_eq!(
                canonical(properties.required("backend").unwrap(), usize::MAX).unwrap(),
                canonical(&reference, usize::MAX).unwrap(),
                "optional plan backend reference"
            );
            properties.remove("backend").unwrap();
            defs.remove("AgentBackendIdentity").unwrap();
            checked += 1;
        }
    }
    assert_eq!(checked, 8, "all backend-bearing output schemas were checked");
    projected
}
