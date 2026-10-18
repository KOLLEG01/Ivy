use crate::schema::{READABLE_VERSIONS, catalog, chat_contracts, final_contract};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, chat_bundle::bundled_schema};
use ivy_sdk::main_chat::CHAT_INTERFACE_VERSION;

fn description(name: &str) -> IvyResult<&'static str> {
    Ok(match name {
        "retryResult" => {
            "Retry reading the result of the original turn after a failed collection; does not start another turn."
        }
        "workspace" => "Check the WhatsApp transport and the shared Main conversation binding.",
        "createMain" => "Create or replace the shared Main conversation at its observed revision.",
        "send" => {
            "Queue input for Main to process. This does not send the supplied text directly to WhatsApp. Main must answer normally and must never use this tool to send its own answer or forward a notice to itself. Set expectedBinding=null only when creating the first Main."
        }
        "cancel" => "Request cancellation of a queued or running input at its current revision.",
        "receive" => "Offer saved Main replies to the WhatsApp transport.",
        "acknowledge" => {
            "Mark the exact offered replies as delivered after WhatsApp confirms them."
        }
        "operation" => "Check the original outcome of a ChatBridge action by operationId.",
        "history" => {
            "Read message history for a configured channel without marking replies as delivered."
        }
        "input" => "Check whether a message input is queued, running or finished in its channel.",
        "notify" => {
            "Give Main a service notification for the user. Main answers through its single WhatsApp transport; requires an authenticated producer."
        }
        "notice" => {
            "Read the Main reply and WhatsApp delivery state for this producer's original notification operation."
        }
        _ => return Err(IvyError::internal()),
    })
}
fn strings(values: &[&str]) -> Json {
    Json::Array(values.iter().map(|v| (*v).into()).collect())
}
pub fn chat_tools() -> IvyResult<Vec<Json>> {
    catalog()
        .required("operations")?
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, operation)| {
            let name = name.to_utf8()?;
            let mut discovery = Json::object(Vec::<(&str, Json)>::new());
            if !matches!(name.as_str(), "receive" | "acknowledge") {
                let (mcp, surface) = match name.as_str() {
                    "send" | "notify" => ("chat_bridge_send".to_owned(), "ivy"),
                    "history" | "input" | "notice" | "operation" => {
                        ("chat_bridge_read".to_owned(), "ivy")
                    }
                    "workspace" => ("chat_bridge_status".to_owned(), "ivy_dev"),
                    _ => {
                        let snake: String = name
                            .chars()
                            .flat_map(|c| {
                                if c.is_ascii_uppercase() {
                                    vec!['_', c.to_ascii_lowercase()]
                                } else {
                                    vec![c]
                                }
                            })
                            .collect();
                        (format!("chat_bridge_{snake}"), "ivy_dev")
                    }
                };
                discovery.insert(
                    "mcp",
                    Json::object([("name", mcp.into()), ("surface", surface.into())]),
                );
            }
            discovery.insert(
                "keywords",
                strings(match name.as_str() {
                    "workspace" | "history" => &["WhatsApp"],
                    "send" => &["WhatsApp", "message"],
                    "notify" => &["WhatsApp", "send", "message", "notification"],
                    "notice" => &["notification"],
                    _ => &[],
                }),
            );
            Ok(Json::object([
                ("namespace", "chat".into()),
                ("name", name.as_str().into()),
                ("interfaceVersion", CHAT_INTERFACE_VERSION.into()),
                ("description", description(&name)?.into()),
                ("discovery", discovery),
                (
                    "inputSchema",
                    bundled_schema(
                        operation.required("input")?.as_js_string().unwrap(),
                        catalog().get("schema").and_then(Json::as_js_string),
                    )?,
                ),
                (
                    "outputSchema",
                    bundled_schema(
                        operation.required("output")?.as_js_string().unwrap(),
                        catalog().get("schema").and_then(Json::as_js_string),
                    )?,
                ),
                (
                    "annotations",
                    Json::object([
                        (
                            "readOnlyHint",
                            Json::Bool(operation.get("mutation") != Some(&Json::Bool(true))),
                        ),
                        ("idempotentHint", Json::Bool(true)),
                    ]),
                ),
            ]))
        })
        .collect()
}
pub fn chat_registry() -> IvyResult<Json> {
    let contracts = chat_contracts()?;
    let mut required = Vec::new();
    for contract in &contracts {
        let key = contract.required("key")?;
        if required
            .iter()
            .any(|entry: &Json| entry.get("key") == Some(key))
        {
            continue;
        }
        let versions: Vec<Json> = contracts
            .iter()
            .filter(|c| c.get("key") == Some(key))
            .map(|c| c.get("version").unwrap().clone())
            .collect();
        required.push(Json::object([
            ("key", key.clone()),
            (
                "readVersions",
                if final_contract(&key.as_str()?) {
                    strings(&READABLE_VERSIONS)
                } else {
                    Json::Array(versions.clone())
                },
            ),
            (
                "writeVersions",
                Json::Array(vec![versions.last().unwrap().clone()]),
            ),
        ]));
    }
    Ok(Json::object([
        ("discoveryHint", "ChatBridge lets agents exchange messages with the user through WhatsApp via the shared Main conversation.".into()),
        ("namespaces", Json::Array(vec![Json::object([
            ("namespace", "chat".into()), ("description", "Messages and service notifications through the shared Main conversation".into()),
            ("guideMarkdown", concat!(
                "Use chat_bridge_send to give Main user input or authenticated service notifications. It queues an input; it is not direct WhatsApp delivery. Main answers normally, and the configured channel automatically delivers that answer. Main must never call chat_bridge_send to deliver its own answer or forward a notice to itself. ",
                "If no Main exists, send with expectedBinding=null. Keep operationId and messageId for the same message. A successful send means queued, not delivered; use chat_bridge_read for the original operation, input, notice or channel history after an uncertain reply. ",
                "Service notifications supply only operationId, source and text; Main selects WhatsApp. Native pending inputs belong to the owning AgentManager."
            ).into()),
            ("tools", Json::Array(chat_tools()?)), ("topics", Json::Array(vec![])), ("inventoryKinds", Json::Array(vec![])),
        ])])),
        ("contracts", Json::Array(contracts)), ("requiredContracts", Json::Array(required)),
    ]))
}
