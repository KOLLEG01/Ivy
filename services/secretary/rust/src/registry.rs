//! Secretary-owned registration metadata. Schema bundling and browser event
//! envelopes stay in the shared contracts/SDK implementation.
use crate::store::Record;
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::parse, schema::bundle_local_schema};
use ivy_sdk::browser_notice::browser_notice_topic;
use serde_json::json;
use std::sync::OnceLock;

const VERSION: &str = "1.0.0";
fn fixed(value: serde_json::Value) -> IvyResult<IvyJson> {
    IvyJson::from_serde(&value)
}
fn schemas() -> IvyResult<&'static IvyJson> {
    static VALUE: OnceLock<IvyResult<IvyJson>> = OnceLock::new();
    VALUE
        .get_or_init(|| {
            parse(
                include_str!("../../../../specs/schemas/secretary.schema.json"),
                1024 * 1024,
            )
        })
        .as_ref()
        .map_err(Clone::clone)
}
fn catalog() -> IvyResult<&'static IvyJson> {
    static VALUE: OnceLock<IvyResult<IvyJson>> = OnceLock::new();
    VALUE
        .get_or_init(|| {
            parse(
                include_str!("../../../../specs/schemas/secretary.contracts.json"),
                1024 * 1024,
            )
        })
        .as_ref()
        .map_err(Clone::clone)
}
fn bundled(name: &str) -> IvyResult<IvyJson> {
    bundle_local_schema(schemas()?, &name.into())
}
fn description(record: Record) -> IvyResult<&'static str> {
    Ok(match record {
        Record::Item => {
            "Final selected message for the user-facing inbox. Source identity, assessment and useful notice state are materialized in its current revision. Source cursors and operation receipts live in the local Secretary journal."
        }
        Record::Configuration => {
            "Global Secretary execution target and contact rules. Assignments inherit fields from this current revision."
        }
        Record::Assignment => {
            "One revisioned system-originated schedule or event assignment, including its prompt, hidden optional preflight process and sparse rule overrides."
        }
        Record::ScheduleProgress => {
            "Durable per-assignment schedule cursor. A trigger is acknowledged only after its exact execution has been saved."
        }
        Record::EventProgress => {
            "Durable per-assignment event position and assignment snapshot. Each Hive event is acknowledged after matching executions and progress have been saved."
        }
        Record::Execution => {
            "Durable Secretary execution journal from admitted trigger through native task archival and seven-day deletion."
        }
        _ => return Err(IvyError::internal()),
    })
}
fn tool_description(name: &str) -> IvyResult<&'static str> {
    Ok(match name {
        "capture" => "Capture one original source observation.",
        "assess" => "Record the decision for one original unassessed item.",
        "attach" => "Attach acquired media evidence to one original inbox item.",
        "checkpoint" => "Advance one source cursor after saved capture receipts.",
        "operation" => "Read the recorded outcome of an inbox action by its operation ID.",
        "operationRead" => {
            "Read the caller-owned outcome of an inbox or assignment operation by its original ID."
        }
        "status" => {
            "Read source collection checkpoints, notification settings, and recovery issues."
        }
        "binding" => {
            "Read the current service node, host and expected scope without a prior scope value."
        }
        "listAssignments" => {
            "List assignments triggered by schedules or events, with current revisions for updates."
        }
        "getAssignment" => "Read one current Secretary assignment and its exact revision pin.",
        "saveConfiguration" => {
            "Set default execution targets and contact rules for Secretary assignments."
        }
        "saveAssignment" => {
            "Create or update one exact Secretary schedule, reminder or event-triggered assignment."
        }
        _ => return Err(IvyError::internal()),
    })
}
fn assignment_input(creating: bool) -> IvyResult<IvyJson> {
    let mut input = bundled("SaveAssignmentRequest")?;
    let mut definitions = input.required("$defs")?.clone();
    let mut assignment = definitions.required("SaveAssignmentRequest")?.clone();
    let mut properties = assignment.required("properties")?.clone();
    let selected = if creating {
        fixed(json!({"type":"null"}))?
    } else {
        properties
            .required("assignment")?
            .required("anyOf")?
            .as_array()
            .and_then(|v| v.first())
            .cloned()
            .ok_or_else(IvyError::internal)?
    };
    properties.insert("assignment", selected);
    assignment.insert("properties", properties);
    definitions.insert("SaveAssignmentRequest", assignment);
    input.insert("$defs", definitions);
    Ok(input)
}
pub fn secretary_registry() -> IvyResult<IvyJson> {
    let mut contracts = Vec::new();
    let mut required = Vec::new();
    for value in catalog()?
        .required("contracts")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        let key = value.required("key")?.as_str()?;
        let record = [
            Record::Item,
            Record::Configuration,
            Record::Assignment,
            Record::ScheduleProgress,
            Record::EventProgress,
            Record::Execution,
        ]
        .into_iter()
        .find(|record| record.key() == key)
        .ok_or_else(IvyError::internal)?;
        let versions = record.versions();
        let version = versions.last().ok_or_else(IvyError::internal)?;
        let retention = match record {
            Record::Item => {
                json!({"objects":{"mode":"expire","maximumAgeDays":90},"revisions":{"mode":"all"}})
            }
            Record::Assignment | Record::Execution => {
                json!({"objects":{"mode":"retain"},"revisions":{"mode":"all"}})
            }
            _ => {
                json!({"objects":{"mode":"retain"},"revisions":{"mode":"bounded","maximumCount":100}})
            }
        };
        let mut contract = fixed(
            json!({"key":key,"version":version,"owner":{"kind":"service","serviceName":"secretary"},"mediaType":"application/json","retention":retention,"specMarkdown":description(record)?}),
        )?;
        contract.insert(
            "jsonSchema",
            bundled(&value.required("definition")?.as_str()?)?,
        );
        contracts.push(contract);
        required.push(fixed(
            json!({"key":key,"readVersions":versions,"writeVersions":[version]}),
        )?);
    }
    let mut tools = Vec::new();
    let IvyJson::Object(operations) = catalog()?.required("operations")? else {
        return Err(IvyError::internal());
    };
    for (name, operation) in operations {
        let name = name.to_utf8()?;
        let mut tool = fixed(
            json!({"namespace":"secretary","name":name,"interfaceVersion":VERSION,"description":tool_description(&name)?,"annotations":{"readOnlyHint":operation.get("mutation")!=Some(&true.into()),"idempotentHint":true}}),
        )?;
        tool.insert(
            "inputSchema",
            bundled(&operation.required("input")?.as_str()?)?,
        );
        tool.insert(
            "outputSchema",
            bundled(&operation.required("output")?.as_str()?)?,
        );
        if let Some((mcp, surface, title)) = match name.as_str() {
            "operation" => Some(("secretary_history", "ivy_dev", "Read inbox action outcome")),
            "operationRead" => Some((
                "secretary_operation_read",
                "ivy",
                "Read Secretary operation outcome",
            )),
            "status" => Some(("secretary_status", "ivy_dev", "Check Secretary sources")),
            "binding" => Some(("secretary_binding", "ivy", "Read Secretary binding")),
            "listAssignments" => Some(("secretary_list_assignments", "ivy", "List assignments")),
            "saveConfiguration" => Some((
                "secretary_configure_execution",
                "ivy_dev",
                "Configure assignments",
            )),
            _ => None,
        } {
            tool.insert(
                "discovery",
                fixed(json!({"keywords":[],"mcp":{"name":mcp,"surface":surface}}))?,
            );
            let mut annotations = tool.required("annotations")?.clone();
            annotations.insert("title", title.into());
            annotations.insert("openWorldHint", false.into());
            tool.insert("annotations", annotations);
        }
        tools.push(tool);
    }
    for creating in [true, false] {
        let mut tool = if creating {
            fixed(
                json!({"namespace":"secretary","name":"createAssignment","interfaceVersion":VERSION,"discovery":{"keywords":["schedule","reminder","Zeitplan"],"mcp":{"name":"secretary_create_assignment","surface":"ivy"}},"description":"Create an assignment that runs agent work on a schedule or a service event.","annotations":{"title":"Create assignment","readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}}),
            )?
        } else {
            fixed(
                json!({"namespace":"secretary","name":"updateAssignment","interfaceVersion":VERSION,"discovery":{"keywords":[],"mcp":{"name":"secretary_update_assignment","surface":"ivy"}},"description":"Change an assignment's prompt, schedule, or event trigger using its current revision.","annotations":{"title":"Update assignment","readOnlyHint":false,"idempotentHint":true,"openWorldHint":false}}),
            )?
        };
        tool.insert("inputSchema", assignment_input(creating)?);
        tool.insert("outputSchema", bundled("Pin")?);
        tools.push(tool);
    }
    let mut follow = fixed(
        json!({"namespace":"secretary","name":"followUp","interfaceVersion":VERSION,"discovery":{"keywords":["follow-up"],"mcp":{"name":"secretary_follow_up","surface":"ivy"}},"description":"Accept a Main or Voice question about a Secretary item or execution object ID. Returns promptly; poll secretary_follow_up_read with the original operationId for the answer.",
        "inputSchema":{"type":"object","additionalProperties":false,"required":["operationId","itemId","question"],"properties":{"operationId":{"type":"string","minLength":1,"maxLength":256},"itemId":{"type":"string","minLength":1,"maxLength":256},"question":{"type":"string","minLength":1,"maxLength":16384}}},
        "outputSchema":{"type":"object","additionalProperties":false,"required":["operationId","itemId","phase","threadId","turnId","answer","errorCode"],"properties":{"operationId":{"type":"string"},"itemId":{"type":"string"},"phase":{"enum":["prepared","running","succeeded","outcome_unknown"]},"threadId":{"type":["string","null"]},"turnId":{"type":["string","null"]},"answer":{"type":["string","null"]},"errorCode":{"type":["string","null"]}}},"annotations":{"readOnlyHint":false,"idempotentHint":true}}),
    )?;
    tools.push(follow.clone());
    follow.insert("name", "followUpRead".into());
    follow.insert("description","Read the caller-owned result of an accepted Secretary follow-up using its original operationId.".into());
    follow.insert(
        "discovery",
        fixed(json!({"keywords":[],"mcp":{"name":"secretary_follow_up_read","surface":"ivy"}}))?,
    );
    follow.insert("inputSchema",fixed(json!({"type":"object","additionalProperties":false,"required":["operationId"],"properties":{"operationId":{"type":"string","minLength":1,"maxLength":256}}}))?);
    follow.insert(
        "annotations",
        fixed(json!({"readOnlyHint":true,"idempotentHint":true}))?,
    );
    tools.push(follow);
    for tool in &mut tools {
        if let Some(discovery) = tool.get("discovery") {
            let mut discovery = discovery.clone();
            if discovery.get("mcp").is_some() {
                discovery.insert("summary", tool.required("description")?.clone());
                tool.insert("discovery", discovery);
            }
        }
    }
    let mut namespace = fixed(
        json!({"namespace":"secretary","description":"System assignment execution, durable inbox and agent triage","guideMarkdown":"Start with secretary_binding for the selected serviceNodeId; copy its expectedScope into secretary_list_assignments to page current assignments or select an exact assignmentId. Create with secretary_create_assignment and update from the returned revision pin with secretary_update_assignment. The routed and inner operationId must match. After an uncertain reply, use secretary_operation_read with the original operationId; unknown or not_found does not prove that no effect occurred. Assignments start empty and run from schedules or Ivy service events. Use hive_event_topics to discover registered triggers, event kinds and payload schemas. Events advertising messageChannel are grouped by source and channel for five minutes; Secretary asks the source message-channel.unread tool for fresh read state before assessment. On ivy_dev, secretary_status reports source collection progress, secretary_history retrieves a full inbox action, and secretary_configure_execution sets assignment defaults. User follow-up continues through ChatBridge and the shared Main conversation.","inventoryKinds":[]}),
    )?;
    namespace.insert(
        "topics",
        IvyJson::Array(vec![browser_notice_topic(&"secretary".into())]),
    );
    namespace.insert("tools", IvyJson::Array(tools));
    Ok(IvyJson::object([("discoveryHint","Secretary manages scheduled and event-triggered assignments, the inbox and follow-up questions.".into()),("namespaces",IvyJson::Array(vec![namespace])),("contracts",IvyJson::Array(contracts)),("requiredContracts",IvyJson::Array(required))]))
}
/// Dormant triage deliberately advertises no contracts or namespaces.
pub fn triage_registry() -> IvyJson {
    IvyJson::object([
        ("namespaces", IvyJson::Array(vec![])),
        ("contracts", IvyJson::Array(vec![])),
        ("requiredContracts", IvyJson::Array(vec![])),
    ])
}
/// The service scopes every required contract to its configured original root.
pub fn service_registry(root: &IvyJson) -> IvyResult<IvyJson> {
    let mut registry = secretary_registry()?;
    let mut required = registry
        .required("requiredContracts")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .to_vec();
    for contract in &mut required {
        contract.insert(
            "readScope",
            IvyJson::object([
                ("roots", IvyJson::Array(vec![root.clone()])),
                ("history", "all".into()),
                ("includeArchived", true.into()),
                ("references", IvyJson::Array(vec![])),
            ]),
        );
    }
    registry.insert("requiredContracts", IvyJson::Array(required));
    Ok(registry)
}
