use ivy_agent_manager::{
    native_discovery::discover_native_tools,
    registry::{
        NATIVE_INVENTORY_SCHEMA_VERSION, NATIVE_PROJECT_SCHEMA_VERSION, agent_registry,
        agent_schema,
    },
};
use ivy_contracts::{
    IvyJson, IvyResult,
    hash::{hash_json, tool_definition_hash},
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::{collections::HashMap, path::PathBuf, process::Command, sync::OnceLock};

fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let result = Command::new("node")
            .arg("tests/fixtures/agent-registry-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        parse(
            std::str::from_utf8(&result.stdout).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
fn array<'a>(value: &'a IvyJson, key: &str) -> &'a [IvyJson] {
    value.get(key).unwrap().as_array().unwrap()
}
fn catalog(row: &IvyJson) -> IvyJson {
    if let Some(path) = row.get("path") {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join(path.as_str().unwrap());
        parse(
            &std::fs::read_to_string(path).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    } else {
        row.get("catalog").unwrap().clone()
    }
}
fn capture(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn equal(actual: &IvyJson, expected: &IvyJson, label: &IvyJson) {
    assert_eq!(
        hash_json(actual).unwrap(),
        hash_json(expected).unwrap(),
        "{}\nactual={}\nexpected={}",
        label.as_str().unwrap(),
        stringify(actual, MANAGEMENT_FRAME_BYTES).unwrap(),
        stringify(expected, MANAGEMENT_FRAME_BYTES).unwrap()
    );
}

#[test]
fn all_retained_registries_preserve_metadata_schemas_order_and_definition_hashes() {
    let rows = array(data(), "registry");
    assert_eq!(
        rows.iter().filter(|row| row.get("path").is_some()).count(),
        7
    );
    for row in rows {
        let actual = agent_registry(&catalog(row)).and_then(|registry| {
            let definitions = array(&registry, "namespaces")
                .iter()
                .map(|namespace| {
                    let tools = array(namespace, "tools")
                        .iter()
                        .map(|tool| {
                            Ok(IvyJson::object([
                                ("name", tool.get("name").unwrap().clone()),
                                ("hash", tool_definition_hash(tool)?.into()),
                            ]))
                        })
                        .collect::<IvyResult<Vec<_>>>()?;
                    Ok(IvyJson::object([
                        ("namespace", namespace.get("namespace").unwrap().clone()),
                        ("definitions", IvyJson::Array(tools)),
                    ]))
                })
                .collect::<IvyResult<Vec<_>>>()?;
            Ok(IvyJson::object([
                ("hash", hash_json(&registry)?.into()),
                ("definitions", IvyJson::Array(definitions)),
            ]))
        });
        equal(
            &capture(actual),
            row.get("expected").unwrap(),
            row.get("label").unwrap(),
        );
    }
    for row in array(data(), "schemas") {
        let actual = agent_schema(row.get("name").unwrap().as_js_string().unwrap()).unwrap();
        assert_eq!(
            hash_json(&actual).unwrap(),
            row.get("hash").unwrap().as_str().unwrap()
        );
    }
}

#[test]
fn cached_registry_values_and_provider_schemas_are_independent_and_uninterpreted() {
    let row = array(data(), "registry")
        .iter()
        .find(|row| row.get("label") == Some(&"group-order-duplicates-utf16-opaque-schemas".into()))
        .unwrap();
    let catalog = catalog(row);
    let registry = agent_registry(&catalog).unwrap();
    let tools = array(&array(&registry, "namespaces")[0], "tools");
    assert_eq!(
        tools[0].get("inputSchema"),
        catalog.get("clientRequests").unwrap().as_array().unwrap()[0].get("inputSchema")
    );
    let inventories = array(&array(&registry, "namespaces")[0], "inventoryKinds");
    assert_eq!(
        inventories[0].get("version"),
        Some(&NATIVE_INVENTORY_SCHEMA_VERSION.into())
    );
    assert_eq!(
        inventories[1].get("version"),
        Some(&NATIVE_PROJECT_SCHEMA_VERSION.into())
    );
    let expected = hash_json(&registry).unwrap();
    let mut changed = registry;
    changed.insert("discoveryHint", "changed".into());
    assert_eq!(
        hash_json(&agent_registry(&catalog).unwrap()).unwrap(),
        expected
    );
}

#[test]
fn discovery_matches_original_search_exact_schemas_constraints_and_errors() {
    for pair in array(data(), "lowercase") {
        let pair = pair.as_array().unwrap();
        assert_eq!(
            pair[0].as_js_string().unwrap().to_english_lowercase(),
            *pair[1].as_js_string().unwrap()
        );
    }
    let mut registries = HashMap::<String, IvyJson>::new();
    for row in array(data(), "discovery") {
        let catalog = catalog(row);
        let hash = hash_json(&catalog).unwrap();
        if !registries.contains_key(&hash) {
            registries.insert(hash.clone(), agent_registry(&catalog).unwrap());
        }
        let definitions = row
            .get("definitions")
            .and_then(IvyJson::as_array)
            .unwrap_or_else(|| {
                array(
                    &array(registries.get(&hash).unwrap(), "namespaces")[0],
                    "tools",
                )
            });
        let actual = discover_native_tools(
            &catalog,
            definitions,
            row.get("input").unwrap(),
            &row.get("platform").unwrap().as_str().unwrap(),
        )
        .and_then(|page| {
            Ok(IvyJson::object([
                ("hash", hash_json(&page)?.into()),
                ("count", IvyJson::Number(array(&page, "items").len() as f64)),
            ]))
        });
        equal(
            &capture(actual),
            row.get("expected").unwrap(),
            row.get("label").unwrap(),
        );
    }
}
