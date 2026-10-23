//! Finite registration inputs, not the finalized current runtime bundle.
use ivy_contracts::{
    IvyJson as Json, JsString, builtin_schema::SchemaBundle, canonical_bundle::Bundle,
    core_schema::validate_shared, json::parse,
};
use ivy_task_board::{
    journal_store::Contracts,
    registry::{RegistryText, TaskBoardRegistry},
    schema::{RuntimeContracts, task_board_contracts},
};

fn json(text: &str) -> Json {
    parse(text, 32 * 1024 * 1024).unwrap()
}
fn documents() -> Vec<(String, Json)> {
    [
        (
            "TaskBoard",
            include_str!("../../specs/schemas/task-board.schema.json"),
        ),
        ("Chat", include_str!("../../specs/schemas/chat.schema.json")),
        (
            "Agent",
            include_str!("../../specs/schemas/agent.schema.json"),
        ),
        (
            "Wire",
            include_str!("../../specs/schemas/hive-wire.schema.json"),
        ),
        (
            "Hive",
            include_str!("../../specs/schemas/hive-operations.schema.json"),
        ),
    ]
    .map(|(prefix, text)| (prefix.into(), json(text)))
    .into()
}
fn catalog() -> Json {
    json(include_str!(
        "../../specs/schemas/task-board.operations.json"
    ))
}
fn text(catalog: &Json) -> RegistryText {
    RegistryText {
        descriptions: catalog
            .required("operations")
            .unwrap()
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, _)| {
                (
                    name.to_utf8().unwrap(),
                    JsString::from(format!("Finite description: {}", name.to_utf8().unwrap())),
                )
            })
            .collect(),
        guide_markdown: "Finite workflow guide supplied by the caller. 日本語".into(),
    }
}
pub(super) fn fixture(contracts: &RuntimeContracts) -> TaskBoardRegistry {
    let catalog = catalog();
    TaskBoardRegistry::new(
        &catalog,
        &Bundle::from_documents(documents()).unwrap(),
        contracts,
        text(&catalog),
    )
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn native_registry_keeps_management_private_and_preserves_schema_and_reader_contracts() {
    let contracts = RuntimeContracts::from_documents(
        documents()
            .into_iter()
            .map(|(_, document)| document)
            .collect(),
    )
    .unwrap();
    let registry = fixture(&contracts);
    assert_eq!((registry.dependency())().await.unwrap(), *registry.value());
    let namespace = &registry
        .value()
        .required("namespaces")
        .unwrap()
        .as_array()
        .unwrap()[0];
    validate_shared("RegistrySync", registry.value()).unwrap();
    validate_shared("NamespaceDefinition", namespace).unwrap();
    let guarded = registry
        .value()
        .required("contracts")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter(|contract| {
            contract
                .get("retention")
                .and_then(|v| v.get("awaitOwnerReadiness"))
                .and_then(Json::as_bool)
                == Some(true)
        })
        .count();
    assert_eq!(guarded, 6);
    assert_eq!(
        namespace.required("guideMarkdown").unwrap(),
        &Json::String(text(&catalog()).guide_markdown)
    );
    let tools = namespace.required("tools").unwrap().as_array().unwrap();
    assert_eq!(tools.len(), 19);
    let expected = [
        ("configuration", Some("task_configuration"), "ivy", true),
        ("configure", Some("task_configure"), "ivy", false),
        ("savePlan", Some("task_save_plan"), "ivy_dev", false),
        ("create", Some("task_create"), "ivy", false),
        ("edit", Some("task_update"), "ivy", false),
        ("transition", Some("task_update"), "ivy", false),
        ("defer", Some("task_update"), "ivy", false),
        ("reassign", Some("task_update"), "ivy", false),
        ("newThread", Some("task_update"), "ivy", false),
        ("cancel", Some("task_update"), "ivy", false),
        ("comment", Some("task_comment"), "ivy", false),
        (
            "uploadAttachment",
            Some("task_upload_attachment"),
            "ivy",
            false,
        ),
        ("archive", Some("task_update"), "ivy", false),
        ("operation", Some("task_read"), "ivy", true),
        ("read", Some("task_read"), "ivy", true),
        ("list", Some("task_list"), "ivy", true),
        ("categories", Some("task_list"), "ivy", true),
        ("workspace", None, "", true),
        ("markCommentsRead", None, "", false),
    ];
    for (name, mcp, surface, read_only) in expected {
        let tool = tools
            .iter()
            .find(|tool| tool.get("name") == Some(&name.into()))
            .unwrap();
        let discovery = tool.required("discovery").unwrap();
        if let Some(mcp) = mcp {
            assert_eq!(
                discovery.required("mcp").unwrap().required("name").unwrap(),
                &mcp.into()
            );
            assert_eq!(
                discovery
                    .required("mcp")
                    .unwrap()
                    .required("surface")
                    .unwrap(),
                &surface.into()
            );
        } else {
            assert!(discovery.get("mcp").is_none());
        }
        assert_eq!(
            tool.required("description").unwrap(),
            &Json::String(text(&catalog()).descriptions.remove(name).unwrap())
        );
        assert_eq!(
            tool.required("annotations")
                .unwrap()
                .required("readOnlyHint")
                .unwrap(),
            &Json::Bool(read_only)
        );
        assert_eq!(
            tool.required("annotations")
                .unwrap()
                .required("idempotentHint")
                .unwrap(),
            &Json::Bool(true)
        );
        // Bundled schemas resolve without loading the source documents.
        for property in ["inputSchema", "outputSchema"] {
            let bundled = tool.required(property).unwrap();
            let mut pending = vec![bundled];
            while let Some(value) = pending.pop() {
                match value {
                    Json::Object(properties) => {
                        if let Some(reference) = value.get("$ref") {
                            let reference = reference.as_str().unwrap();
                            let name = reference
                                .strip_prefix("#/$defs/")
                                .expect("Nonlocal schema reference");
                            assert!(bundled.required("$defs").unwrap().get(name).is_some());
                        }
                        pending.extend(properties.iter().map(|(_, value)| value));
                    }
                    Json::Array(values) => pending.extend(values),
                    _ => {}
                }
            }
            SchemaBundle::from_documents(vec![bundled.clone()])
                .unwrap()
                .matches("#", &Json::Null)
                .unwrap();
        }
    }
    assert_eq!(
        registry.tool_names(),
        tools
            .iter()
            .map(|tool| tool
                .required("name")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone())
            .collect::<Vec<_>>()
    );
    assert!(!registry.tool_names().iter().any(|name| {
        ["review", "saveResult", "start", "continue", "nativeUpdate"]
            .iter()
            .any(|hidden| name == &JsString::from(*hidden))
    }));
    let read = tools
        .iter()
        .find(|tool| tool.get("name") == Some(&"read".into()))
        .unwrap();
    let schema =
        SchemaBundle::from_documents(vec![read.required("inputSchema").unwrap().clone()]).unwrap();
    assert!(
        schema
            .matches(
                "#",
                &json(r#"{"task":"11111111-1111-4111-8111-111111111111"}"#)
            )
            .unwrap()
    );
    assert!(
        !schema
            .matches(
                "#",
                &json(r#"{"task":"11111111-1111-4111-8111-111111111111","unknown":true}"#)
            )
            .unwrap()
    );
    assert_eq!(
        registry.value().required("contracts").unwrap(),
        &Json::Array(task_board_contracts().unwrap())
    );
    let requirements = registry
        .value()
        .required("requiredContracts")
        .unwrap()
        .as_array()
        .unwrap();
    let mut seen = Vec::new();
    for requirement in requirements {
        validate_shared("ContractRequirement", requirement).unwrap();
        let key = requirement.required("key").unwrap().as_str().unwrap();
        assert!(!seen.contains(&key));
        assert_eq!(
            requirement.required("readVersions").unwrap(),
            &Json::Array(
                contracts
                    .readable_versions(&key)
                    .unwrap()
                    .iter()
                    .cloned()
                    .map(Json::String)
                    .collect()
            )
        );
        assert_eq!(
            requirement.required("writeVersions").unwrap(),
            &Json::Array(vec![contracts.version(&key).unwrap().into()])
        );
        seen.push(key);
    }
    let mut published = Vec::new();
    for contract in task_board_contracts().unwrap() {
        let key = contract.required("key").unwrap().as_str().unwrap();
        if !published.contains(&key) {
            published.push(key);
        }
    }
    assert_eq!(seen, published);
    let task = requirements
        .iter()
        .find(|value| value.get("key") == Some(&"task-board/task".into()))
        .unwrap();
    assert_eq!(
        task.required("writeVersions").unwrap(),
        &json(r#"["2.3.0"]"#)
    );
    assert!(
        task.required("readVersions")
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&"1.12.0".into())
    );

    let catalog = catalog();
    let schemas = Bundle::from_documents(documents()).unwrap();
    let mut missing_text = text(&catalog);
    missing_text.descriptions.remove("comment");
    assert!(TaskBoardRegistry::new(&catalog, &schemas, &contracts, missing_text).is_err());
    for (property, value) in [("input", "#/$defs/Unavailable"), ("mutation", "false")] {
        let mut operation = catalog
            .required("operations")
            .unwrap()
            .required("read")
            .unwrap()
            .clone();
        operation.insert(property, value.into());
        let mut operations = catalog.required("operations").unwrap().clone();
        operations.insert("read", operation);
        let mut broken = catalog.clone();
        broken.insert("operations", operations);
        assert!(TaskBoardRegistry::new(&broken, &schemas, &contracts, text(&catalog)).is_err());
    }
}
