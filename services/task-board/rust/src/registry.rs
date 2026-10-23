//! Native registration from one trusted operation/schema catalog. Deployment
//! composition supplies the matching operation descriptions and workflow guide.
use crate::{
    journal_store::Contracts,
    schema::{RuntimeContracts, task_board_contracts},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, browser_notice::browser_notice_topic,
    canonical_bundle::Bundle,
};
use ivy_sdk::service::Dependency;
use std::{collections::HashMap, sync::Arc};

pub struct RegistryText {
    pub descriptions: HashMap<String, JsString>,
    pub guide_markdown: JsString,
}

/// The SDK advertisement and local handler names come from the same selection.
pub struct TaskBoardRegistry {
    value: Json,
    tool_names: Vec<JsString>,
}
impl TaskBoardRegistry {
    pub fn new(
        catalog: &Json,
        schemas: &Bundle,
        contracts: &RuntimeContracts,
        text: RegistryText,
    ) -> IvyResult<Self> {
        let document = catalog.required("schema")?.as_str()?;
        let operations = catalog
            .required("operations")?
            .as_object()
            .ok_or_else(IvyError::internal)?;
        let mut tools = Vec::new();
        let mut tool_names = Vec::new();
        for (name, operation) in operations {
            let name = name.to_utf8()?;
            if matches!(name.as_str(), "review" | "saveResult") {
                continue;
            }
            let description = text
                .descriptions
                .get(&name)
                .ok_or_else(IvyError::internal)?;
            let definition = |property: &str| -> IvyResult<Json> {
                let reference = operation.required(property)?.as_str()?;
                let name = reference
                    .strip_prefix("#/$defs/")
                    .ok_or_else(IvyError::internal)?;
                schemas.definition(&document, name)
            };
            let mut discovery = Json::Object(vec![]);
            if !matches!(name.as_str(), "workspace" | "markCommentsRead") {
                let mcp_name = match name.as_str() {
                    "edit" | "transition" | "defer" | "reassign" | "newThread" | "cancel"
                    | "archive" => "task_update".to_owned(),
                    "read" | "operation" => "task_read".to_owned(),
                    "list" | "categories" => "task_list".to_owned(),
                    _ => {
                        let mut mcp_name = String::from("task_");
                        for letter in name.chars() {
                            if letter.is_ascii_uppercase() {
                                mcp_name.push('_');
                                mcp_name.push(letter.to_ascii_lowercase());
                            } else {
                                mcp_name.push(letter);
                            }
                        }
                        mcp_name
                    }
                };
                discovery.insert(
                    "mcp",
                    Json::object([
                        ("name", mcp_name.into()),
                        (
                            "surface",
                            if name == "savePlan" { "ivy_dev" } else { "ivy" }.into(),
                        ),
                    ]),
                );
            }
            discovery.insert(
                "keywords",
                Json::Array(vec!["kanban".into(), "collaboration".into()]),
            );
            let mutation = operation
                .required("mutation")?
                .as_bool()
                .ok_or_else(IvyError::internal)?;
            tools.push(Json::object([
                ("namespace", "task-board".into()),
                ("name", name.as_str().into()),
                ("interfaceVersion", "1.0.0".into()),
                ("description", Json::String(description.clone())),
                ("discovery", discovery),
                ("inputSchema", definition("input")?),
                ("outputSchema", definition("output")?),
                (
                    "annotations",
                    Json::object([
                        ("readOnlyHint", Json::Bool(!mutation)),
                        ("idempotentHint", Json::Bool(true)),
                    ]),
                ),
            ]));
            tool_names.push(name.into());
        }
        let published = task_board_contracts()?;
        let mut requirements = Vec::new();
        let mut keys = Vec::new();
        for contract in &published {
            let key = contract.required("key")?.as_str()?;
            if keys.contains(&key) {
                continue;
            }
            requirements.push(Json::object([
                ("key", key.as_str().into()),
                (
                    "readVersions",
                    Json::Array(
                        contracts
                            .readable_versions(&key)?
                            .iter()
                            .cloned()
                            .map(Json::String)
                            .collect(),
                    ),
                ),
                (
                    "writeVersions",
                    Json::Array(vec![contracts.version(&key)?.into()]),
                ),
            ]));
            keys.push(key);
        }
        Ok(Self {
            value: Json::object([
                ("mcpPrefix", "task".into()),
                (
                    "discoveryHint",
                    "TaskBoard is a collaborative Kanban system for tracking work as tasks that the user and agents carry out together, with progress updates, discussion and result review.".into(),
                ),
                (
                    "namespaces",
                    Json::Array(vec![Json::object([
                        ("namespace", "task-board".into()),
                        (
                            "description",
                            "Collaborative Kanban tasks, agent work and user review".into(),
                        ),
                        ("guideMarkdown", Json::String(text.guide_markdown)),
                        ("tools", Json::Array(tools)),
                        (
                            "topics",
                            Json::Array(vec![browser_notice_topic(&"task-board".into())]),
                        ),
                        ("inventoryKinds", Json::Array(vec![])),
                    ])]),
                ),
                ("contracts", Json::Array(published)),
                ("requiredContracts", Json::Array(requirements)),
            ]),
            tool_names,
        })
    }

    pub fn value(&self) -> &Json {
        &self.value
    }

    pub fn tool_names(&self) -> &[JsString] {
        &self.tool_names
    }

    pub fn dependency(&self) -> Dependency<Json> {
        let value = Arc::new(self.value.clone());
        Arc::new(move || {
            let value = value.clone();
            Box::pin(async move { Ok((*value).clone()) })
        })
    }
}
